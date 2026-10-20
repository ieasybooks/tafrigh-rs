//! Turns parsed arguments into library objects, and checks the external programs they use.

use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, bail};
use tafrigh::ChunkOptions;
use tafrigh::audio::{Decoder, Padding};
use tafrigh::output::{BuiltinFormat, Format, OutputWriter};
use tafrigh::segment::silero::{SileroConfig, SileroSegmenter};
use tafrigh::segment::{FixedWindows, Segmenter};
use tafrigh::source::YtDlp;
use tafrigh_wit::{Endpoint, WitConfig};

use crate::cli::{self, FormatChoice, Output, Segmentation, Tools, Wit};

impl Output {
    /// The output writer for these options.
    pub fn writer(&self) -> anyhow::Result<OutputWriter> {
        Ok(
            OutputWriter::new(self.dir.as_deref().unwrap_or_else(|| Path::new(".")))
                .formats(self.formats()?)
                .min_words(self.min_words)
                .keep_uncompacted(self.keep_uncompacted),
        )
    }

    /// Selected formats in order, without duplicates, plus word-level ones with `--words`.
    pub(super) fn formats(&self) -> anyhow::Result<Vec<Arc<dyn Format>>> {
        let mut formats: Vec<BuiltinFormat> = Vec::new();
        for choice in &self.formats {
            match choice {
                FormatChoice::All => formats.extend(BuiltinFormat::ALL),
                FormatChoice::One(format) => formats.push(*format),
            }
        }
        if self.words {
            let word_formats: Vec<BuiltinFormat> =
                formats.iter().filter_map(|f| f.words()).collect();
            if word_formats.is_empty() {
                bail!("--words needs at least one of srt, vtt, csv, tsv or json in --format");
            }
            formats.extend(word_formats);
        }
        let mut seen = HashSet::new();
        formats.retain(|format| seen.insert(*format));
        if formats.is_empty() {
            bail!("no output formats given");
        }
        Ok(formats
            .into_iter()
            .map(|format| Arc::new(format) as Arc<dyn Format>)
            .collect())
    }
}

impl Segmentation {
    /// The segmenter selected by `--vad`.
    pub fn segmenter(&self) -> anyhow::Result<Arc<dyn Segmenter>> {
        if !(self.max_chunk.is_finite() && self.max_chunk > 0.0) {
            bail!("--max-chunk must be positive");
        }
        if !(self.pad.is_finite() && self.pad >= 0.0) {
            bail!("--pad must be zero or positive");
        }
        if !(self.resend_gaps.is_finite() && self.resend_gaps >= 0.0) {
            bail!("--resend-gaps must be zero or positive");
        }
        Ok(match self.vad {
            cli::Vad::Silero => Arc::new(SileroSegmenter::new(SileroConfig {
                threshold: self.vad_threshold,
                min_speech_ms: self.min_speech_ms,
                max_speech_s: self.max_chunk,
                min_silence_ms: self.min_silence_ms,
                speech_pad_ms: self.speech_pad_ms,
            })?),
            cli::Vad::None => Arc::new(FixedWindows::new(self.max_chunk)?),
        })
    }

    /// Longest audio sent per request, padding included, in seconds.
    pub fn request_seconds(&self) -> f64 {
        self.max_chunk + 2.0 * self.pad
    }

    /// Chunking options matching the segmenter.
    pub fn chunk_options(&self) -> ChunkOptions {
        ChunkOptions {
            max_chunk_s: self.max_chunk,
            merge: self.merge,
            padding: (self.pad > 0.0).then(|| match self.pad_noise {
                Some(dbfs) => Padding::white_noise(self.pad, dbfs),
                None => Padding::silence(self.pad),
            }),
            on_error: self.on_chunk_error.into(),
            max_in_flight: None,
            resend_gaps: (self.resend_gaps > 0.0).then_some(self.resend_gaps),
        }
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

impl Wit {
    /// The recognizer configuration for requests of up to `request_seconds` of audio.
    pub fn config(&self, request_seconds: f64) -> WitConfig {
        let mut config = WitConfig::new(self.tokens.iter().cloned());
        config.language.clone_from(&self.language);
        config.endpoint = self.endpoint.into();
        config.api_url.clone_from(&self.api_url);
        config.api_version.clone_from(&self.api_version);
        config.requests_per_second = self.requests_per_second;
        config.max_in_flight_per_token = self
            .concurrency
            .unwrap_or_else(|| default_concurrency(self.requests_per_second, request_seconds));
        config.max_attempts = self.attempts;
        config.max_audio_seconds = self.max_audio;
        config
    }
}

/// Requests per token needed to start `rate` requests a second when each takes about its
/// audio's length plus 2s (upload and latency).
fn default_concurrency(rate: f64, request_seconds: f64) -> usize {
    (rate * (request_seconds + 2.0)).ceil().clamp(1.0, 64.0) as usize
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
    pub fn ytdlp(&self, output_dir: &Path) -> anyhow::Result<YtDlp> {
        let download_dir = self.download_dir.as_deref().unwrap_or(output_dir);
        let ytdlp = YtDlp::new(download_dir)
            .program(&self.yt_dlp)
            .save_metadata(self.save_metadata);
        Ok(match &self.yt_dlp_args {
            Some(extra) => ytdlp.args(shell_words::split(extra).context("parsing --yt-dlp-args")?),
            None => ytdlp,
        })
    }
}

/// Checks the external programs this run needs, reporting every unavailable one at once
/// with how to fix it. yt-dlp is only checked when given (i.e. when there are URL inputs).
pub async fn check_tools(decoder: &Decoder, ytdlp: Option<&YtDlp>) -> anyhow::Result<()> {
    let mut problems = Vec::new();
    if let Err(err) = decoder.check() {
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
    use crate::cli::{Cli, Command};

    fn formats(flags: &[&str]) -> anyhow::Result<Vec<String>> {
        let mut argv = vec!["tafrigh", "-w", "t"];
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
            .formats()?
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
        assert_eq!(super::default_concurrency(1.0, 16.0), 18);
        assert_eq!(super::default_concurrency(0.5, 5.0), 4);
        assert_eq!(super::default_concurrency(1e-9, 5.0), 1);
        assert_eq!(super::default_concurrency(100.0, 60.0), 64);
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
