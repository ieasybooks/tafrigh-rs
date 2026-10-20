//! Command-line arguments. Doc comments here are the `--help` text.
#![allow(
    clippy::doc_markdown,
    reason = "doc comments are --help text, where backticks would show literally"
)]

use std::path::PathBuf;

use clap::{CommandFactory, Parser, ValueEnum};
use tafrigh::output::BuiltinFormat;

/// The --help footer. Keep in sync with the exit codes in main.rs.
const HELP_FOOTER: &str = "\
Requires ffmpeg, plus yt-dlp for URLs (and a JavaScript runtime such as Deno for YouTube).

Exit codes: 0 success, 1 error (e.g. a missing program), 2 invalid arguments,
3 finished but some inputs or items failed, 130 interrupted.";

/// Transcribe audio and video into subtitles and text with Wit.ai.
///
/// Inputs can be media files, folders of media files, or URLs (videos, playlists,
/// channels; anything yt-dlp supports).
#[derive(Debug, Parser)]
#[command(
    name = "tafrigh",
    version,
    about,
    max_term_width = 100,
    after_help = HELP_FOOTER
)]
pub struct Args {
    /// Files, folders or URLs to transcribe.
    #[arg(required = true, value_name = "INPUT")]
    pub inputs: Vec<String>,

    /// Include media in subdirectories. Directory symlinks are not followed.
    #[arg(short = 'r', long)]
    pub recursive: bool,

    /// Hide progress bars.
    #[arg(short, long)]
    pub quiet: bool,

    /// More logging (-v debug, -vv trace). RUST_LOG overrides.
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(flatten)]
    pub output: Output,

    #[command(flatten)]
    pub segmentation: Segmentation,

    #[command(flatten)]
    pub wit: Wit,

    #[command(flatten)]
    pub tools: Tools,
}

/// Where transcripts go and what they contain.
#[derive(Debug, clap::Args)]
#[command(next_help_heading = "Output")]
pub struct Output {
    /// Directory for transcripts; selects tree-to-tree mode unless --output-mode is given.
    #[arg(short = 'o', long = "output-dir", value_name = "DIR")]
    pub dir: Option<PathBuf>,

    /// Destination layout [default: side-by-side without -o, tree-to-tree with -o].
    #[arg(long = "output-mode", value_enum, value_name = "MODE")]
    pub mode: Option<OutputMode>,

    /// Output formats, comma-separated: txt, srt, vtt, csv, tsv, json, all, or word-level
    /// words-srt, words-vtt, words-csv, words-tsv, words-json.
    #[arg(
        short,
        long = "format",
        value_name = "FORMATS",
        value_delimiter = ',',
        default_value = "all",
        value_parser = parse_format
    )]
    pub formats: Vec<FormatChoice>,

    /// Also write word timings as <id>.words.<ext> for each chosen srt, vtt, csv, tsv or json
    /// format (one cue or row per word).
    #[arg(long)]
    pub words: bool,

    /// Merge segments with fewer words into the following ones (0 disables).
    #[arg(long, value_name = "N", default_value_t = 1)]
    pub min_words: usize,

    /// Also write uncompacted transcripts as <id>.uncompacted.<ext>.
    #[arg(long)]
    pub keep_uncompacted: bool,

    /// Skip items when every requested output file already exists.
    #[arg(long)]
    pub skip_existing: bool,

    /// Write a sample CSV with this many random segments; suffix its name on collisions.
    #[arg(long, value_name = "N", default_value_t = 0)]
    pub sample: usize,
}

/// Value of --output-mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputMode {
    /// Write beside each local or downloaded media file; cannot be used with -o.
    SideBySide,
    /// Mirror input folders in -o, keeping multiple roots separate; URLs go flat.
    TreeToTree,
    /// Put all transcripts directly in -o.
    Flat,
}

impl Output {
    /// Resolve and validate the destination mode before any tools or network requests.
    pub fn mode(&self) -> Result<tafrigh::output::OutputMode, clap::Error> {
        use clap::error::ErrorKind;
        use tafrigh::output::OutputMode as Mode;

        match (self.mode, self.dir.is_some()) {
            (Some(OutputMode::SideBySide), true) => Err(Args::command().error(
                ErrorKind::ArgumentConflict,
                "--output-mode side-by-side cannot be used with --output-dir",
            )),
            (Some(OutputMode::TreeToTree | OutputMode::Flat), false) => Err(Args::command().error(
                ErrorKind::MissingRequiredArgument,
                "--output-mode tree-to-tree and flat require --output-dir",
            )),
            (Some(OutputMode::SideBySide) | None, false) => Ok(Mode::SideBySide),
            (Some(OutputMode::TreeToTree) | None, true) => Ok(Mode::TreeToTree),
            (Some(OutputMode::Flat), true) => Ok(Mode::Flat),
        }
    }
}

/// A value of `--format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatChoice {
    /// Every segment-level format.
    All,
    /// One format.
    One(BuiltinFormat),
}

fn parse_format(value: &str) -> Result<FormatChoice, String> {
    if value.trim().eq_ignore_ascii_case("all") {
        Ok(FormatChoice::All)
    } else {
        value.trim().parse().map(FormatChoice::One)
    }
}

/// How audio is cut into chunks.
#[derive(Debug, clap::Args)]
#[command(next_help_heading = "Segmentation")]
pub struct Segmentation {
    /// How to find speech.
    #[arg(long, value_enum, default_value_t = Vad::Silero)]
    pub vad: Vad,

    /// Longest chunk sent for recognition, in seconds (Wit allows up to 60 on dictation and
    /// 20 on speech, padding included).
    #[arg(long, value_name = "SECONDS", default_value_t = 15.0)]
    pub max_chunk: f64,

    /// Merge consecutive speech up to --max-chunk: fewer requests and more context per
    /// request; segments still follow Wit's utterances.
    #[arg(long)]
    pub merge: bool,

    /// Speech probability threshold.
    #[arg(long, value_name = "P", default_value_t = 0.5)]
    pub vad_threshold: f32,

    /// Shortest speech kept, in milliseconds.
    #[arg(long, value_name = "MS", default_value_t = 500)]
    pub min_speech_ms: u32,

    /// Silence that ends a chunk, in milliseconds.
    #[arg(long, value_name = "MS", default_value_t = 300)]
    pub min_silence_ms: u32,

    /// Context kept around detected speech, in milliseconds.
    #[arg(long, value_name = "MS", default_value_t = 250)]
    pub speech_pad_ms: u32,

    /// Pad each chunk on both sides with this many seconds of silence (0 disables). Wit drops
    /// words that start or end right at a chunk edge.
    #[arg(long, value_name = "SECONDS", default_value_t = 0.5)]
    pub pad: f64,

    /// Pad with white noise peaking at this level instead of silence (0 = full scale,
    /// as in the Python tafrigh).
    #[arg(
        long,
        value_name = "DBFS",
        allow_negative_numbers = true,
        requires = "pad"
    )]
    pub pad_noise: Option<f64>,

    /// Resend stretches of speech that Wit returned no words for, when at least this many
    /// seconds long (0 disables). Wit sometimes stops transcribing partway through a chunk.
    #[arg(long, value_name = "SECONDS", default_value_t = 1.0)]
    pub resend_gaps: f64,

    /// What to do when a chunk fails after all retries.
    #[arg(long, value_enum, value_name = "POLICY", default_value_t = OnChunkError::Abort)]
    pub on_chunk_error: OnChunkError,
}

/// Value of `--vad`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Vad {
    /// Silero VAD v6; long speech is split at its longest pause.
    Silero,
    /// Fixed windows of --max-chunk seconds, no voice detection.
    None,
}

/// Value of `--on-chunk-error`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OnChunkError {
    /// Fail the file, so a rerun produces a complete transcript.
    Abort,
    /// Leave the chunk out and continue.
    Skip,
}

/// Wit.ai settings.
#[derive(Debug, clap::Args)]
#[command(next_help_heading = "Wit.ai")]
pub struct Wit {
    /// Wit client access tokens (repeat or comma-separate). All apps must share a language.
    #[arg(
        short = 'w',
        long = "wit-token",
        value_name = "TOKEN",
        env = "TAFRIGH_WIT_TOKENS",
        hide_env_values = true,
        value_delimiter = ',',
        required = true
    )]
    pub tokens: Vec<String>,

    /// Language of the Wit apps (e.g. ar); detected from the tokens when omitted.
    #[arg(long = "wit-language", value_name = "LANG")]
    pub language: Option<String>,

    /// Request starts per second for each token.
    #[arg(long = "wit-rps", value_name = "N", default_value_t = 1.0)]
    pub requests_per_second: f64,

    /// Concurrent requests per token [default: enough to keep up with --wit-rps; Wit answers
    /// at about the pace of the audio, so longer chunks need more].
    #[arg(long = "wit-concurrency", value_name = "N")]
    pub concurrency: Option<usize>,

    /// Attempts per chunk, including the first.
    #[arg(long = "wit-attempts", value_name = "N", default_value_t = 5)]
    pub attempts: u32,

    /// Longest audio per request in seconds [default: 60 for dictation, 20 for speech].
    #[arg(long = "wit-max-audio", value_name = "SECONDS")]
    pub max_audio: Option<f64>,

    /// Wit endpoint: dictation (transcription only) or speech (also extracts intents).
    #[arg(long = "wit-endpoint", value_enum, default_value_t = WitEndpoint::Dictation)]
    pub endpoint: WitEndpoint,

    /// Wit API base URL (e.g. a proxy or a local mock server).
    #[arg(
        long = "wit-api-url",
        value_name = "URL",
        default_value = "https://api.wit.ai"
    )]
    pub api_url: String,

    /// Wit API version, sent as the v query parameter.
    #[arg(
        long = "wit-api-version",
        value_name = "VERSION",
        default_value = "20240304"
    )]
    pub api_version: String,
}

/// Value of `--wit-endpoint`.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum WitEndpoint {
    /// Transcription only.
    Dictation,
    /// Transcription plus intents.
    Speech,
}

/// Downloads and external programs.
#[derive(Debug, clap::Args)]
#[command(next_help_heading = "Downloads and tools")]
pub struct Tools {
    /// Extra yt-dlp arguments, e.g. "--cookies cookies.txt --playlist-items 1-10".
    #[arg(long, value_name = "ARGS", allow_hyphen_values = true)]
    pub yt_dlp_args: Option<String>,

    /// Where downloaded audio goes [default: -o, or the current directory].
    #[arg(long, value_name = "DIR")]
    pub download_dir: Option<PathBuf>,

    /// Save yt-dlp's metadata for each URL as <id>.info.json.
    #[arg(long)]
    pub save_metadata: bool,

    /// yt-dlp executable, needed for URL inputs.
    #[arg(long, value_name = "PATH", default_value = "yt-dlp")]
    pub yt_dlp: PathBuf,

    /// ffmpeg executable (required).
    #[arg(long, value_name = "PATH", default_value = "ffmpeg")]
    pub ffmpeg: PathBuf,

    /// ffprobe executable (optional; only used for progress percentages).
    #[arg(long, value_name = "PATH", default_value = "ffprobe")]
    pub ffprobe: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_is_valid() {
        Args::command().debug_assert();
    }

    #[test]
    fn parses_typical_invocation() {
        let args = Args::try_parse_from([
            "tafrigh",
            "-w",
            "a,b",
            "-w",
            "c",
            "-f",
            "srt,txt",
            "--merge",
            "--pad",
            "1",
            "--pad-noise",
            "-6",
            "--yt-dlp-args",
            "--playlist-items 1-3",
            "lecture.mp3",
        ])
        .unwrap();
        assert_eq!(args.wit.tokens, ["a", "b", "c"]);
        assert_eq!(
            args.output.formats,
            [
                FormatChoice::One(BuiltinFormat::Srt),
                FormatChoice::One(BuiltinFormat::Txt)
            ]
        );
        assert_eq!(args.segmentation.pad_noise, Some(-6.0));
        assert_eq!(
            args.tools.yt_dlp_args.as_deref(),
            Some("--playlist-items 1-3")
        );
        assert_eq!(args.inputs, ["lecture.mp3"]);
        assert!(!args.output.words);
        assert!(!args.recursive);
    }

    #[test]
    fn parses_recursive_flags() {
        for flag in ["-r", "--recursive"] {
            let args = Args::try_parse_from(["tafrigh", "-w", "token", flag, "media/"]).unwrap();
            assert!(args.recursive);
        }
    }

    #[test]
    fn output_mode_defaults_and_constraints() {
        use tafrigh::output::OutputMode as Mode;
        for (flags, expected) in [
            (vec![], Ok(Mode::SideBySide)),
            (vec!["-o", "out"], Ok(Mode::TreeToTree)),
            (vec!["--output-mode", "side-by-side"], Ok(Mode::SideBySide)),
            (
                vec!["--output-mode", "tree-to-tree", "-o", "out"],
                Ok(Mode::TreeToTree),
            ),
            (vec!["--output-mode", "flat", "-o", "out"], Ok(Mode::Flat)),
            (
                vec!["--output-mode", "side-by-side", "-o", "out"],
                Err(clap::error::ErrorKind::ArgumentConflict),
            ),
            (
                vec!["--output-mode", "tree-to-tree"],
                Err(clap::error::ErrorKind::MissingRequiredArgument),
            ),
            (
                vec!["--output-mode", "flat"],
                Err(clap::error::ErrorKind::MissingRequiredArgument),
            ),
        ] {
            let mut argv = vec!["tafrigh", "-w", "token", "media"];
            argv.extend(flags);
            let parsed = Args::try_parse_from(argv).unwrap();
            assert_eq!(parsed.output.mode().map_err(|err| err.kind()), expected);
        }
    }

    #[test]
    fn parses_formats() {
        let args =
            Args::try_parse_from(["tafrigh", "-w", "a", "--words", "-f", "all,words-json", "x"])
                .unwrap();
        assert!(args.output.words);
        assert_eq!(
            args.output.formats,
            [
                FormatChoice::All,
                FormatChoice::One(BuiltinFormat::WordsJson)
            ]
        );
        let err = Args::try_parse_from(["tafrigh", "-w", "a", "-f", "docx", "x"]).unwrap_err();
        assert!(err.to_string().contains("unknown format"), "{err}");
    }
}
