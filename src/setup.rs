//! Turns parsed arguments into a plan of library settings, and checks the external programs
//! a run uses.
//!
//! Each option's own range is checked here, while parsing, so those mistakes fail with Usage's
//! diagnostics and exit code before any program, model or network request is touched. Library
//! constructors still check how settings fit together when the run starts, such as whether
//! `--max-chunk` leaves room for the detector's speech padding.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::bail;
use tafrigh::ChunkOptions;
use tafrigh::audio::{Decoder, Padding};
use tafrigh::output::{BuiltinFormat, OutputMode, OutputWriter};
use tafrigh::segment::firered::{FireRedConfig, FireRedSegmenter};
use tafrigh::segment::silero::{SileroConfig, SileroSegmenter};
use tafrigh::segment::{FixedWindows, Segmenter};
use tafrigh::source::{Input, YtDlp};
#[cfg(feature = "cohere")]
use tafrigh_cohere::{
    CohereConfig, Device, Language, MAX_BATCH_SIZE, MAX_THREADS, ModelSource, ModelVariant,
};
use tafrigh_wit::{AudioFormat, Endpoint, WitConfig};
use tafrigh_youtube::YoutubeCaptions;
use usage_rs::{Event, Parser, ValidationError};

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
    #[cfg(feature = "cohere")]
    Cohere(Box<CohereRun>),
    Youtube(YoutubeCaptions),
}

/// How audio backends decode, segment and chunk recordings.
pub struct Audio {
    pub decoder: Decoder,
    pub segmenter: SegmenterConfig,
    pub chunks: ChunkOptions,
}

/// Settings for transcribing with Wit.
pub struct WitRun {
    pub audio: Audio,
    pub wit: WitConfig,
}

/// Settings for transcribing with Cohere Transcribe Arabic.
#[cfg(feature = "cohere")]
pub struct CohereRun {
    pub audio: Audio,
    pub cohere: CohereConfig,
}

/// The segmenter `--vad` selects, configured. Built when the run starts, because loading
/// the speech models takes a moment.
pub enum SegmenterConfig {
    Silero(SileroConfig),
    FireRed(FireRedConfig),
    FixedWindows(Duration),
}

impl SegmenterConfig {
    pub fn build(self) -> tafrigh::Result<Arc<dyn Segmenter>> {
        Ok(match self {
            Self::Silero(config) => Arc::new(SileroSegmenter::new(config)?),
            Self::FireRed(config) => Arc::new(FireRedSegmenter::new(config)?),
            Self::FixedWindows(window) => Arc::new(FixedWindows::new(window)?),
        })
    }
}

impl Plan {
    /// Parse options and reject explicit flags the selected backend does not use.
    pub fn parse_from<'v>(argv: &[&'v OsStr]) -> Result<Self, usage_rs::Error<'static, 'v>> {
        let cli = Cli::parse_from(argv)?;
        let typed = typed_options(argv)?;
        Self::new(cli, &typed).map_err(ValidationError::into_parse_error)
    }

    fn new(cli: Cli, typed: &Typed) -> Result<Self, ValidationError> {
        Ok(match cli.command {
            Command::Transcribe(args) => Self::Transcribe(Box::new(Run::new(&args, typed)?)),
            Command::Completions(args) => Self::Completions(args),
            Command::Spec => Self::Spec,
        })
    }
}

/// Long names of the options typed on the command line, such as `merge` for `--no-merge`.
type Typed = HashSet<&'static str>;

/// Usage's binding events contain only argv flags, including negations and short forms.
/// Environment fallbacks and declared defaults never produce these events. Argument files
/// have already been expanded by `command::parse`.
fn typed_options<'v>(argv: &[&'v OsStr]) -> Result<Typed, usage_rs::Error<'static, 'v>> {
    let mut typed = Typed::new();
    let mut parser = Parser::new(Cli::command(), argv);
    while let Some(event) = parser.next_event() {
        if let Event::Flag { flag, .. } = event? {
            typed.insert(flag.name);
        }
    }
    Ok(typed)
}

/// Fails when an option typed on the command line isn't used by `transcriber`.
fn check_backend_options(transcriber: Transcriber, typed: &Typed) -> Result<(), ValidationError> {
    let (backend, unused): (&str, &[&'static str]) = match transcriber {
        Transcriber::Wit => (
            "wit",
            &[
                "--model",
                "--device",
                "--model-dir",
                "--cohere-batch-size",
                "--threads",
            ],
        ),
        Transcriber::Cohere => (
            "cohere",
            &[
                "--words",
                "--resend-gaps",
                "--wit-token",
                "--wit-rps",
                "--wit-concurrency",
                "--wit-attempts",
                "--wit-max-audio",
                "--wit-endpoint",
                "--wit-api-url",
                "--wit-api-version",
                "--wit-audio",
            ],
        ),
        Transcriber::Youtube => (
            "youtube",
            &[
                "--recursive",
                "--vad",
                "--max-chunk",
                "--merge",
                "--vad-threshold",
                "--min-speech-ms",
                "--min-silence-ms",
                "--speech-pad-ms",
                "--pad",
                "--pad-noise",
                "--resend-gaps",
                "--on-chunk-error",
                "--wit-token",
                "--wit-rps",
                "--wit-concurrency",
                "--wit-attempts",
                "--wit-max-audio",
                "--wit-endpoint",
                "--wit-api-url",
                "--wit-api-version",
                "--wit-audio",
                "--ffmpeg",
                "--ffprobe",
                "--model",
                "--device",
                "--model-dir",
                "--cohere-batch-size",
                "--threads",
            ],
        ),
    };
    match unused
        .iter()
        .find(|option| typed.contains(&option["--".len()..]))
    {
        Some(option) => {
            Err(ValidationError::field(option)
                .reason(format!("isn't used by --transcriber {backend}")))
        }
        None => Ok(()),
    }
}

impl Run {
    fn new(args: &Args, typed: &Typed) -> Result<Self, ValidationError> {
        check_backend_options(args.transcriber, typed)?;
        let output_mode = args.output.mode()?;
        let output = args.output.writer()?;
        let ytdlp = args
            .tools
            .ytdlp(output_mode.dir().unwrap_or(Path::new(".")))?;
        let backend = match args.transcriber {
            Transcriber::Wit => Backend::Wit(Box::new(WitRun::new(args, typed)?)),
            #[cfg(feature = "cohere")]
            Transcriber::Cohere => Backend::Cohere(Box::new(CohereRun::new(args, typed)?)),
            #[cfg(not(feature = "cohere"))]
            Transcriber::Cohere => {
                return Err(ValidationError::field("--transcriber")
                    .value("cohere")
                    .reason(
                        "this build of tafrigh doesn't include Cohere; reinstall with \
                         `cargo install tafrigh-cli --features cohere` or use a release archive",
                    ));
            }
            Transcriber::Youtube => Backend::Youtube(youtube(args)?),
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
    let language = args.language.as_deref().ok_or_else(|| {
        ValidationError::field("--language").reason("is required with --transcriber youtube")
    })?;
    let captions = YoutubeCaptions::new(language).map_err(|_| {
        ValidationError::field("--language").reason(
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

/// How a backend chunks audio unless the command line says otherwise.
#[derive(Clone, Copy)]
struct Chunking {
    /// `--max-chunk`, in seconds.
    max_chunk: f64,
    /// `--merge`.
    merge: bool,
}

/// Wit drops words near chunk edges and returns its own utterances, so short chunks work best.
const WIT_CHUNKING: Chunking = Chunking {
    max_chunk: 15.0,
    merge: false,
};

/// Cohere was more accurate and faster with speech merged into chunks of up to 30 s than with
/// 15 s chunks; each chunk becomes one segment.
#[cfg(feature = "cohere")]
const COHERE_CHUNKING: Chunking = Chunking {
    max_chunk: 30.0,
    merge: true,
};

impl Audio {
    /// Decoding, segmentation and chunking from the segmentation options, using `default`
    /// for chunking options that weren't typed. `word_timings` says whether the recognizer
    /// returns word timings, which resending gaps needs.
    fn new(
        args: &Args,
        typed: &Typed,
        default: Chunking,
        word_timings: bool,
    ) -> Result<Self, ValidationError> {
        let segmentation = &args.segmentation;
        let max_chunk = duration(
            "--max-chunk",
            segmentation.max_chunk.unwrap_or(default.max_chunk),
            Zero::Invalid,
        )?;
        let merge = if typed.contains("merge") {
            segmentation.merge
        } else {
            default.merge
        };
        let pad = duration("--pad", segmentation.pad, Zero::Disables)?;
        let resend_gaps = duration("--resend-gaps", segmentation.resend_gaps, Zero::Disables)?;
        let threshold = segmentation
            .vad_threshold
            .unwrap_or_else(|| match segmentation.vad {
                cli::Vad::Silero | cli::Vad::None => SileroConfig::default().threshold,
                cli::Vad::Firered => FireRedConfig::default().threshold,
            });
        if !(0.0..=1.0).contains(&threshold) {
            return Err(ValidationError::field("--vad-threshold")
                .value(threshold.to_string())
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
        let segmenter = match segmentation.vad {
            cli::Vad::Silero => SegmenterConfig::Silero(SileroConfig {
                threshold,
                min_speech: Duration::from_millis(segmentation.min_speech_ms.into()),
                max_speech: max_chunk,
                min_silence: Duration::from_millis(segmentation.min_silence_ms.into()),
                speech_pad: Duration::from_millis(segmentation.speech_pad_ms.into()),
            }),
            cli::Vad::Firered => SegmenterConfig::FireRed(FireRedConfig {
                threshold,
                min_speech: Duration::from_millis(segmentation.min_speech_ms.into()),
                max_speech: max_chunk,
                min_silence: Duration::from_millis(segmentation.min_silence_ms.into()),
                speech_pad: Duration::from_millis(segmentation.speech_pad_ms.into()),
            }),
            cli::Vad::None => SegmenterConfig::FixedWindows(max_chunk),
        };
        let chunks = ChunkOptions {
            max_chunk,
            merge,
            padding,
            on_error: segmentation.on_chunk_error.into(),
            max_in_flight: None,
            resend_gaps: (word_timings && !resend_gaps.is_zero()).then_some(resend_gaps),
        };
        Ok(Self {
            decoder: args.tools.decoder(),
            segmenter,
            chunks,
        })
    }

    /// Fails when a chunk plus padding, in the whole samples sent, exceeds the recognizer's
    /// `limit`.
    fn check_limit(&self, limit: Duration, name: &str) -> Result<(), ValidationError> {
        if self.chunks.longest_request() > limit {
            return Err(ValidationError::field("--max-chunk")
                .value(self.chunks.max_chunk.as_secs_f64().to_string())
                .reason(format!(
                    "chunk plus padding exceeds the {limit:?} {name} audio limit"
                )));
        }
        Ok(())
    }
}

impl WitRun {
    fn new(args: &Args, typed: &Typed) -> Result<Self, ValidationError> {
        let audio = Audio::new(args, typed, WIT_CHUNKING, true)?;
        let mut wit = args.wit.config(audio.chunks.longest_request())?;
        wit.ffmpeg.clone_from(&args.tools.ffmpeg);
        wit.language.clone_from(&args.language);
        audio.check_limit(wit.audio_limit(), "Wit")?;
        Ok(Self { audio, wit })
    }
}

#[cfg(feature = "cohere")]
impl CohereRun {
    fn new(args: &Args, typed: &Typed) -> Result<Self, ValidationError> {
        if let Some(format) = args
            .output
            .formats()?
            .into_iter()
            .find(|f| f.is_word_level())
        {
            return Err(ValidationError::field("--format")
                .value(format.to_string())
                .reason("needs word timings, which --transcriber cohere doesn't provide"));
        }
        let audio = Audio::new(args, typed, COHERE_CHUNKING, false)?;
        audio.check_limit(tafrigh_cohere::MAX_AUDIO_DURATION, "Cohere")?;
        let language = match args.language.as_deref() {
            None => Language::default(),
            Some(code) => code.parse().map_err(|_| {
                ValidationError::field("--language")
                    .value(code)
                    .reason("expected ar or en with --transcriber cohere")
            })?,
        };
        let models = &args.models;
        if let Some(size) = models.cohere_batch_size
            && !(1..=MAX_BATCH_SIZE).contains(&size)
        {
            return Err(ValidationError::field("--cohere-batch-size")
                .value(size.to_string())
                .reason(format!("must be between 1 and {MAX_BATCH_SIZE}")));
        }
        let max_threads = std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(MAX_THREADS);
        if let Some(threads) = models.threads
            && threads > max_threads
        {
            return Err(ValidationError::field("--threads")
                .value(threads.to_string())
                .reason(format!(
                    "must be 0 (automatic) or at most {max_threads}, limited by available CPU parallelism and the native limit"
                )));
        }
        let device: Device = models.device.parse().map_err(|_| {
            ValidationError::field("--device")
                .value(&models.device)
                .reason("expected auto, cpu, gpu or gpu:N")
        })?;
        let model = ModelSource::from(models.model.as_str());
        if let ModelSource::Local(path) = &model
            && !path.is_file()
        {
            let variants: Vec<&str> = ModelVariant::ALL.iter().map(|v| v.name()).collect();
            return Err(ValidationError::field("--model")
                .value(&models.model)
                .reason(format!(
                    "expected a variant ({}) or an existing .gguf file",
                    variants.join(", ")
                )));
        }
        let mut cohere = CohereConfig::new(model);
        cohere.model_dir.clone_from(&models.model_dir);
        cohere.language = language;
        cohere.device = device;
        if let Some(size) = models.cohere_batch_size {
            cohere.batch_size = size;
        }
        if let Some(threads) = models.threads {
            cohere.threads = threads;
        }
        Ok(Self { audio, cohere })
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
        config.endpoint = self.endpoint.into();
        config.audio_format = self.audio.into();
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

impl From<cli::WitAudio> for AudioFormat {
    fn from(audio: cli::WitAudio) -> Self {
        match audio {
            cli::WitAudio::Opus => Self::Opus,
            cli::WitAudio::Wav => Self::Wav,
        }
    }
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

    use crate::cli::{Args, Cli, Command};

    /// Wit transcription arguments with `flags`.
    fn parse(flags: &[&str]) -> anyhow::Result<Args> {
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
        Ok(*parsed)
    }

    fn formats(flags: &[&str]) -> anyhow::Result<Vec<String>> {
        Ok(parse(flags)?
            .output
            .formats()
            .map_err(|error| anyhow::anyhow!("{error:?}"))?
            .iter()
            .map(|f| f.extension().to_owned())
            .collect())
    }

    /// The chunk limit and merging a run uses for `argv` after the program name.
    fn chunking(argv: &[&str]) -> (Duration, bool) {
        let argv: Vec<_> = std::iter::once("tafrigh")
            .chain(argv.iter().copied())
            .map(std::ffi::OsStr::new)
            .collect();
        let Ok(super::Plan::Transcribe(run)) = super::Plan::parse_from(&argv) else {
            panic!("expected a transcription run");
        };
        let audio = match run.backend {
            super::Backend::Wit(wit) => wit.audio,
            #[cfg(feature = "cohere")]
            super::Backend::Cohere(cohere) => cohere.audio,
            super::Backend::Youtube(_) => panic!("expected an audio backend"),
        };
        (audio.chunks.max_chunk, audio.chunks.merge)
    }

    #[test]
    fn wit_sends_15_second_chunks_without_merging_by_default() {
        let wit = ["x.mp3", "--transcriber", "wit", "-w", "t"];
        assert_eq!(chunking(&wit), (Duration::from_secs(15), false));
        assert_eq!(
            chunking(&[&wit[..], &["--merge", "--max-chunk", "30"]].concat()),
            (Duration::from_secs(30), true)
        );
    }

    #[cfg(feature = "cohere")]
    #[test]
    fn cohere_merges_speech_up_to_30_seconds_by_default() {
        let cohere = ["x.mp3", "--transcriber", "cohere"];
        assert_eq!(chunking(&cohere), (Duration::from_secs(30), true));
        assert_eq!(
            chunking(&[&cohere[..], &["--no-merge", "--max-chunk", "15"]].concat()),
            (Duration::from_secs(15), false)
        );
    }

    #[cfg(feature = "cohere")]
    #[test]
    fn cohere_batch_and_thread_options_reach_the_recognizer() {
        let argv: Vec<_> = [
            "tafrigh",
            "x.mp3",
            "--transcriber",
            "cohere",
            "--cohere-batch-size",
            "24",
            "--threads",
            "1",
        ]
        .into_iter()
        .map(std::ffi::OsStr::new)
        .collect();
        let Ok(super::Plan::Transcribe(run)) = super::Plan::parse_from(&argv) else {
            panic!("expected a transcription run");
        };
        let super::Backend::Cohere(run) = run.backend else {
            panic!("expected Cohere");
        };
        assert_eq!(run.cohere.batch_size, 24);
        assert_eq!(run.cohere.threads, 1);
    }

    #[cfg(feature = "cohere")]
    #[test]
    fn omitted_cohere_knobs_preserve_library_defaults_and_zero_is_automatic() {
        for flags in [vec![], vec!["--threads", "0"]] {
            let argv: Vec<_> = [vec!["tafrigh", "x.mp3", "--transcriber", "cohere"], flags]
                .concat()
                .into_iter()
                .map(std::ffi::OsStr::new)
                .collect();
            let Ok(super::Plan::Transcribe(run)) = super::Plan::parse_from(&argv) else {
                panic!("expected a transcription run");
            };
            let super::Backend::Cohere(run) = run.backend else {
                panic!("expected Cohere");
            };
            let defaults = tafrigh_cohere::CohereConfig::default();
            assert_eq!(run.cohere.batch_size, defaults.batch_size);
            assert_eq!(run.cohere.threads, defaults.threads);
        }
    }

    #[test]
    fn sends_opus_encoded_by_the_configured_ffmpeg_unless_asked_for_wav() {
        let wit = |flags: &[&str]| {
            super::WitRun::new(&parse(flags).unwrap(), &super::Typed::new())
                .unwrap()
                .wit
        };
        let opus = wit(&["--ffmpeg", "/opt/ffmpeg/bin/ffmpeg"]);
        assert_eq!(opus.audio_format, super::AudioFormat::Opus);
        assert_eq!(opus.ffmpeg, std::path::Path::new("/opt/ffmpeg/bin/ffmpeg"));
        let wav = wit(&["--wit-audio", "wav"]);
        assert_eq!(wav.audio_format, super::AudioFormat::Wav);
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

#[cfg(test)]
mod vad_tests {
    use super::*;

    fn audio(extra: &[&str]) -> Audio {
        let mut argv = vec![
            "tafrigh",
            "recording.wav",
            "--transcriber",
            "wit",
            "--wit-token",
            "test-token",
        ];
        argv.extend_from_slice(extra);
        let argv: Vec<_> = argv.into_iter().map(OsStr::new).collect();
        let Plan::Transcribe(run) = Plan::parse_from(&argv).unwrap() else {
            panic!("transcription plan")
        };
        let Backend::Wit(run) = run.backend else {
            panic!("Wit plan")
        };
        run.audio
    }

    #[test]
    fn detector_defaults_are_independent() {
        let SegmenterConfig::Silero(config) = audio(&[]).segmenter else {
            panic!("default Silero")
        };
        assert_eq!(config, SileroConfig::default());
        let SegmenterConfig::FireRed(config) = audio(&["--vad", "firered"]).segmenter else {
            panic!("FireRed")
        };
        assert_eq!(config, FireRedConfig::default());
        assert!(matches!(
            audio(&["--vad", "none"]).segmenter,
            SegmenterConfig::FixedWindows(_)
        ));
    }

    #[test]
    fn explicit_vad_settings_apply_to_firered() {
        let configured = audio(&[
            "--vad",
            "firered",
            "--vad-threshold",
            "0.7",
            "--min-speech-ms",
            "200",
            "--min-silence-ms",
            "400",
            "--speech-pad-ms",
            "80",
            "--max-chunk",
            "12",
            "--merge",
        ]);
        let SegmenterConfig::FireRed(config) = configured.segmenter else {
            panic!("FireRed")
        };
        assert_eq!(
            config,
            FireRedConfig {
                threshold: 0.7,
                min_speech: Duration::from_millis(200),
                min_silence: Duration::from_millis(400),
                speech_pad: Duration::from_millis(80),
                max_speech: Duration::from_secs(12),
            }
        );
        assert!(configured.chunks.merge);
        assert_eq!(configured.chunks.max_chunk, Duration::from_secs(12));
        let SegmenterConfig::Silero(config) =
            audio(&["--vad", "silero", "--vad-threshold", "0.7"]).segmenter
        else {
            panic!("Silero")
        };
        assert_eq!(
            config,
            SileroConfig {
                threshold: 0.7,
                ..Default::default()
            }
        );
    }
}
