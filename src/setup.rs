//! Turns parsed arguments into a plan of library settings, and checks the external programs
//! a run uses.
//!
//! Each option's own range is checked here, while parsing, so those mistakes fail with Usage's
//! diagnostics and exit code before any program, model or network request is touched. Library
//! constructors still check how settings fit together when the run starts, such as whether
//! `--max-chunk` leaves room for Silero's speech padding.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::bail;
use tafrigh::ChunkOptions;
use tafrigh::audio::{Decoder, Padding};
use tafrigh::output::{BuiltinFormat, OutputMode, OutputWriter};
use tafrigh::segment::silero::{SileroConfig, SileroSegmenter};
use tafrigh::segment::{FixedWindows, Segmenter};
use tafrigh::source::{Input, YtDlp};
use tafrigh_wit::{Endpoint, WitConfig};
use tafrigh_youtube::YoutubeCaptions;
use usage_rs::ValidationError;

use crate::cli::{self, Args, Cli, Command, Completions, FormatChoice, Output, Tools, Transcriber};

/// What the command line asks for, with each option checked.
pub enum Plan {
    /// Transcribe or retrieve captions.
    Transcribe(Box<Run>),
    /// Generate or install shell completions.
    Completions(Completions),
    /// Print the Usage specification.
    Spec,
}

/// A transcription run built from checked options.
pub struct Run {
    pub inputs: Vec<Input>,
    pub backend: Backend,
    pub output: OutputWriter,
    pub output_mode: OutputMode,
    pub ytdlp: YtDlp,
    pub recursive: bool,
    pub skip_existing: bool,
    pub sample: usize,
    pub quiet: bool,
    pub verbose: u8,
}

/// The backend a run uses, with its settings.
pub enum Backend {
    Wit(Box<WitRun>),
    Youtube(YoutubeCaptions),
}

/// Settings for transcribing with Wit.
pub struct WitRun {
    pub decoder: Decoder,
    pub segmenter: SegmenterConfig,
    pub chunks: ChunkOptions,
    pub wit: WitConfig,
}

/// The segmenter `--vad` selects, configured. Built when the run starts, because loading
/// Silero takes a moment.
pub enum SegmenterConfig {
    Silero(SileroConfig),
    FixedWindows(Duration),
}

impl SegmenterConfig {
    pub fn build(self) -> tafrigh::Result<Arc<dyn Segmenter>> {
        Ok(match self {
            Self::Silero(config) => Arc::new(SileroSegmenter::new(config)?),
            Self::FixedWindows(window) => Arc::new(FixedWindows::new(window)?),
        })
    }
}

impl TryFrom<Cli> for Plan {
    type Error = ValidationError;

    fn try_from(cli: Cli) -> Result<Self, ValidationError> {
        Ok(match cli.command {
            Command::Transcribe(args) => Self::Transcribe(Box::new(Run::try_from(*args)?)),
            Command::Completions(args) => Self::Completions(args),
            Command::Spec => Self::Spec,
        })
    }
}

impl TryFrom<Args> for Run {
    type Error = ValidationError;

    fn try_from(args: Args) -> Result<Self, ValidationError> {
        let output_mode = args.output.mode()?;
        let output = args.output.writer()?;
        let ytdlp = args
            .tools
            .ytdlp(output_mode.dir().unwrap_or(Path::new(".")))?;
        let backend = match args.transcriber {
            Transcriber::Wit => Backend::Wit(Box::new(WitRun::try_from(&args)?)),
            Transcriber::Youtube => Backend::Youtube(youtube(&args)?),
        };
        Ok(Self {
            inputs: args
                .inputs
                .iter()
                .map(|input| Input::parse(input))
                .collect(),
            backend,
            output,
            output_mode,
            ytdlp,
            recursive: args.recursive,
            skip_existing: args.output.skip_existing,
            sample: args.output.sample,
            quiet: args.quiet,
            verbose: args.verbose,
        })
    }
}

/// The captions backend: one exact language, and only URLs it supports.
fn youtube(args: &Args) -> Result<YoutubeCaptions, ValidationError> {
    let language = args.subtitle_language.as_deref().ok_or_else(|| {
        ValidationError::field("--subtitle-language")
            .reason("is required with --transcriber youtube")
    })?;
    let captions = YoutubeCaptions::new(language).map_err(|_| {
        ValidationError::field("--subtitle-language").reason(
            "expected one language code, such as ar or en-US; patterns and lists are not supported",
        )
    })?;
    if args
        .inputs
        .iter()
        .any(|input| !tafrigh_youtube::is_youtube_url(input))
    {
        return Err(
            ValidationError::field("INPUT").reason("--transcriber youtube requires YouTube URLs")
        );
    }
    Ok(captions)
}

impl TryFrom<&Args> for WitRun {
    type Error = ValidationError;

    fn try_from(args: &Args) -> Result<Self, ValidationError> {
        if args.subtitle_language.is_some() {
            return Err(ValidationError::field("--subtitle-language")
                .reason("requires --transcriber youtube; use --wit-language for Wit"));
        }
        let segmentation = &args.segmentation;
        let max_chunk = duration("--max-chunk", segmentation.max_chunk, Zero::Invalid)?;
        let pad = duration("--pad", segmentation.pad, Zero::Disables)?;
        let resend_gaps = duration("--resend-gaps", segmentation.resend_gaps, Zero::Disables)?;
        if !(0.0..=1.0).contains(&segmentation.vad_threshold) {
            return Err(ValidationError::field("--vad-threshold")
                .value(segmentation.vad_threshold.to_string())
                .reason("must be between 0 and 1"));
        }
        let padding = match (segmentation.pad_noise, pad.is_zero()) {
            (Some(dbfs), _) if !dbfs.is_finite() || dbfs > 0.0 => {
                return Err(ValidationError::field("--pad-noise")
                    .value(dbfs.to_string())
                    .reason("must be finite and at most 0 dBFS"));
            }
            (Some(_), true) => {
                return Err(ValidationError::field("--pad-noise")
                    .reason("requires --pad greater than zero"));
            }
            (None, true) => None,
            (Some(dbfs), false) => Some(Padding::white_noise(pad, dbfs)),
            (None, false) => Some(Padding::silence(pad)),
        };

        let request = max_chunk.saturating_add(pad.saturating_mul(2));
        let wit = args.wit.config(request)?;
        if request > wit.audio_limit() {
            return Err(ValidationError::field("--max-chunk")
                .value(segmentation.max_chunk.to_string())
                .reason(format!(
                    "chunk plus padding exceeds the {:?} Wit audio limit",
                    wit.audio_limit()
                )));
        }

        let segmenter = match segmentation.vad {
            cli::Vad::Silero => SegmenterConfig::Silero(SileroConfig {
                threshold: segmentation.vad_threshold,
                min_speech: Duration::from_millis(segmentation.min_speech_ms.into()),
                max_speech: max_chunk,
                min_silence: Duration::from_millis(segmentation.min_silence_ms.into()),
                speech_pad: Duration::from_millis(segmentation.speech_pad_ms.into()),
            }),
            cli::Vad::None => SegmenterConfig::FixedWindows(max_chunk),
        };
        let chunks = ChunkOptions {
            max_chunk,
            merge: segmentation.merge,
            padding,
            on_error: segmentation.on_chunk_error.into(),
            max_in_flight: None,
            resend_gaps: (!resend_gaps.is_zero()).then_some(resend_gaps),
        };
        Ok(Self {
            decoder: args.tools.decoder(),
            segmenter,
            chunks,
            wit,
        })
    }
}

/// Whether zero is a valid value for a duration option.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Zero {
    Invalid,
    /// Zero turns the option's feature off.
    Disables,
}

/// `seconds` given to `flag`, as a duration.
fn duration(flag: &'static str, seconds: f64, zero: Zero) -> Result<Duration, ValidationError> {
    match Duration::try_from_secs_f64(seconds) {
        Ok(duration) if zero == Zero::Disables || !duration.is_zero() => Ok(duration),
        _ => Err(ValidationError::field(flag)
            .value(seconds.to_string())
            .reason(match zero {
                Zero::Invalid => "must be finite and positive",
                Zero::Disables => "must be finite and zero or positive",
            })),
    }
}

impl Output {
    /// The destination for `--output-dir` and `--output-mode`.
    pub fn mode(&self) -> Result<OutputMode, ValidationError> {
        match (self.mode, self.dir.clone()) {
            (Some(cli::OutputMode::SideBySide) | None, None) => Ok(OutputMode::SideBySide),
            (Some(cli::OutputMode::TreeToTree) | None, Some(dir)) => {
                Ok(OutputMode::TreeToTree(dir))
            }
            (Some(cli::OutputMode::Flat), Some(dir)) => Ok(OutputMode::Flat(dir)),
            (Some(cli::OutputMode::SideBySide), Some(_)) => {
                Err(ValidationError::field("--output-mode")
                    .value("side-by-side")
                    .reason("cannot be used with --output-dir"))
            }
            (Some(mode @ (cli::OutputMode::TreeToTree | cli::OutputMode::Flat)), None) => {
                Err(ValidationError::field("--output-mode")
                    .value(if mode == cli::OutputMode::Flat {
                        "flat"
                    } else {
                        "tree-to-tree"
                    })
                    .reason("tree-to-tree and flat require --output-dir"))
            }
        }
    }

    /// The output writer for these options.
    pub fn writer(&self) -> Result<OutputWriter, ValidationError> {
        Ok(OutputWriter::new()
            .formats(self.formats()?)
            .min_words(self.min_words)
            .keep_uncompacted(self.keep_uncompacted))
    }

    /// Selected formats in order, without duplicates, plus word-level ones with `--words`.
    pub(super) fn formats(&self) -> Result<Vec<BuiltinFormat>, ValidationError> {
        let mut formats: Vec<BuiltinFormat> = Vec::new();
        for choice in &self.formats {
            match choice {
                FormatChoice::All => formats.extend(BuiltinFormat::SEGMENT_LEVEL),
                FormatChoice::One(format) => formats.push(*format),
            }
        }
        if self.words {
            let word_formats: Vec<BuiltinFormat> =
                formats.iter().filter_map(|f| f.words()).collect();
            if word_formats.is_empty() {
                return Err(ValidationError::field("--words")
                    .reason("needs at least one of srt, vtt, csv, tsv or json in --format"));
            }
            formats.extend(word_formats);
        }
        let mut seen = HashSet::new();
        formats.retain(|format| seen.insert(*format));
        if formats.is_empty() {
            return Err(ValidationError::field("--format").reason("no output formats given"));
        }
        Ok(formats)
    }
}

impl From<cli::OnChunkError> for tafrigh::OnChunkError {
    fn from(policy: cli::OnChunkError) -> Self {
        match policy {
            cli::OnChunkError::Abort => Self::Abort,
            cli::OnChunkError::Skip => Self::Skip,
        }
    }
}

impl cli::Wit {
    /// The recognizer configuration for requests of up to `request` of audio.
    fn config(&self, request: Duration) -> Result<WitConfig, ValidationError> {
        let rate = self.requests_per_second;
        if !(rate.is_finite() && rate > 0.0) {
            return Err(ValidationError::field("--wit-rps")
                .value(rate.to_string())
                .reason("must be finite and positive"));
        }
        if self.tokens.iter().all(|token| token.trim().is_empty()) {
            return Err(ValidationError::field("--wit-token")
                .reason("at least one nonblank token is required"));
        }
        let at_least_one = |flag| {
            ValidationError::field(flag)
                .value("0")
                .reason("must be at least 1")
        };
        if self.attempts == 0 {
            return Err(at_least_one("--wit-attempts"));
        }
        if self.concurrency == Some(0) {
            return Err(at_least_one("--wit-concurrency"));
        }
        let mut config = WitConfig::new(self.tokens.iter().cloned());
        config.language.clone_from(&self.language);
        config.endpoint = self.endpoint.into();
        config.api_url.clone_from(&self.api_url);
        config.api_version.clone_from(&self.api_version);
        config.requests_per_second = rate;
        config.max_in_flight_per_token = self
            .concurrency
            .unwrap_or_else(|| default_concurrency(rate, request));
        config.max_attempts = self.attempts;
        config.max_audio = self
            .max_audio
            .map(|seconds| duration("--wit-max-audio", seconds, Zero::Invalid))
            .transpose()?;
        Ok(config)
    }
}

/// Requests per token needed to start `rate` requests a second when each takes about its
/// audio's length plus 2s (upload and latency).
fn default_concurrency(rate: f64, request: Duration) -> usize {
    (rate * (request.as_secs_f64() + 2.0))
        .ceil()
        .clamp(1.0, 64.0) as usize
}

impl From<cli::WitEndpoint> for Endpoint {
    fn from(endpoint: cli::WitEndpoint) -> Self {
        match endpoint {
            cli::WitEndpoint::Dictation => Self::Dictation,
            cli::WitEndpoint::Speech => Self::Speech,
        }
    }
}

impl Tools {
    /// The decoder using the configured ffmpeg and ffprobe.
    pub fn decoder(&self) -> Decoder {
        Decoder::new(&self.ffmpeg, &self.ffprobe)
    }

    /// The yt-dlp source, downloading into `--download-dir` or `output_dir`.
    pub fn ytdlp(&self, output_dir: &Path) -> Result<YtDlp, ValidationError> {
        let download_dir = self.download_dir.as_deref().unwrap_or(output_dir);
        let ytdlp = YtDlp::new(download_dir)
            .program(&self.yt_dlp)
            .save_metadata(self.save_metadata);
        let Some(extra) = &self.yt_dlp_args else {
            return Ok(ytdlp);
        };
        let extra = shell_words::split(extra)
            .map_err(|error| ValidationError::field("--yt-dlp-args").reason(error.to_string()))?;
        Ok(ytdlp.args(extra))
    }
}

/// Checks the external programs this run needs, reporting every unavailable one at once
/// with how to fix it. Only supplied tools are checked: captions need yt-dlp but no decoder.
pub async fn check_tools(decoder: Option<&Decoder>, ytdlp: Option<&YtDlp>) -> anyhow::Result<()> {
    let mut problems = Vec::new();
    if let Some(decoder) = decoder
        && let Err(err) = decoder.check().await
    {
        problems.push(describe_unavailable(
            &err,
            "",
            "https://ffmpeg.org/download.html",
            "--ffmpeg",
        ));
    }
    if let Some(ytdlp) = ytdlp
        && let Err(err) = ytdlp.check().await
    {
        problems.push(describe_unavailable(
            &err,
            " (needed for URLs)",
            "https://github.com/yt-dlp/yt-dlp/wiki/Installation",
            "--yt-dlp",
        ));
    }
    if problems.is_empty() {
        return Ok(());
    }
    let heading = if problems.len() == 1 {
        "a required program is unavailable"
    } else {
        "required programs are unavailable"
    };
    bail!("{heading}:\n{}", problems.join("\n"));
}

/// One line of the unavailable-programs report, e.g.
/// `  - ffmpeg: `ffmpeg` was not found. Install ffmpeg (…) or pass --ffmpeg PATH.`
fn describe_unavailable(err: &tafrigh::Error, note: &str, install: &str, flag: &str) -> String {
    let tafrigh::Error::ToolUnavailable {
        tool,
        program,
        source,
    } = err
    else {
        return format!("  - {}", tafrigh::ErrorChain(err));
    };
    let program = program.display();
    let reason = match source.kind() {
        ErrorKind::NotFound => format!("`{program}` was not found"),
        ErrorKind::PermissionDenied => format!("`{program}` is not executable"),
        _ => format!("`{program}` could not be run ({source})"),
    };
    format!("  - {tool}{note}: {reason}. Install {tool} ({install}) or pass {flag} PATH.")
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tafrigh::output::Format;

    use crate::cli::{Cli, Command};

    fn formats(flags: &[&str]) -> anyhow::Result<Vec<String>> {
        let mut argv = vec!["tafrigh", "--transcriber", "wit", "-w", "t"];
        argv.extend_from_slice(flags);
        argv.push("x.mp3");
        let argv: Vec<_> = argv.iter().map(std::ffi::OsStr::new).collect();
        let Command::Transcribe(parsed) = Cli::try_parse_from(&argv)
            .map_err(|error| anyhow::anyhow!("{error:?}"))?
            .command
        else {
            panic!("expected transcription");
        };
        Ok(parsed
            .output
            .formats()
            .map_err(|error| anyhow::anyhow!("{error:?}"))?
            .iter()
            .map(|f| f.extension().to_owned())
            .collect())
    }

    #[test]
    fn describes_unavailable_programs() {
        let missing = tafrigh::Error::ToolUnavailable {
            tool: "ffmpeg",
            program: "ffmpeg".into(),
            source: std::io::ErrorKind::NotFound.into(),
        };
        assert_eq!(
            super::describe_unavailable(&missing, "", "https://ffmpeg.org", "--ffmpeg"),
            "  - ffmpeg: `ffmpeg` was not found. Install ffmpeg (https://ffmpeg.org) or pass \
             --ffmpeg PATH."
        );
    }

    #[test]
    fn concurrency_keeps_up_with_the_request_rate() {
        let concurrency =
            |rate, seconds| super::default_concurrency(rate, Duration::from_secs(seconds));
        assert_eq!(concurrency(1.0, 16), 18);
        assert_eq!(concurrency(0.5, 5), 4);
        assert_eq!(concurrency(1e-9, 5), 1);
        assert_eq!(concurrency(100.0, 60), 64);
    }

    #[test]
    fn expands_and_dedups_formats() {
        assert_eq!(
            formats(&["-f", "srt,all,txt"]).unwrap(),
            ["srt", "txt", "vtt", "csv", "tsv", "json"]
        );
        assert_eq!(
            formats(&["-f", "srt,txt", "--words"]).unwrap(),
            ["srt", "txt", "words.srt"]
        );
        assert!(formats(&["-f", "txt", "--words"]).is_err());
    }
}
