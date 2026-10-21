//! Command-line arguments. Doc comments here are the `--help` text.
#![allow(
    clippy::doc_markdown,
    reason = "doc comments are --help text, where backticks would show literally"
)]

use std::path::PathBuf;

use tafrigh::output::BuiltinFormat;
use usage_rs::{ValidationError, ValueEnum};

/// Tool requirements and reusable argument-file syntax.
const HELP_FOOTER: &str = "\
Wit requires ffmpeg and Wit tokens. YouTube captions require yt-dlp, without audio processing.
URLs require yt-dlp; YouTube may also need a JavaScript runtime such as Deno.

Use @FILE to read options and inputs from a file of shell-style words; @@ escapes a literal @.
Boolean transcription flags accept a --no- prefix, such as --no-merge, to override presets.";

/// Transcribe with Wit.ai or retrieve existing YouTube captions.
///
/// Transcription is the default command. Use completions to set up your shell, or
/// spec to export the CLI definition for documentation and other Usage tools.
#[derive(Debug, Clone, usage_rs::Cli)]
#[usage(
    bin = "tafrigh",
    version,
    repository = env!("CARGO_PKG_REPOSITORY"),
    license = env!("CARGO_PKG_LICENSE"),
    completion,
    default_subcommand = "transcribe",
    default_subcommand_flags,
    default_subcommand_help,
    arg_required_else_help,
    unknown_flags = "error",
    max_term_width = 100,
    validate_with = validate,
    exit_code(0, "Success"),
    exit_code(1, "Setup, filesystem or other runtime error"),
    exit_code(2, "Invalid arguments"),
    example("tafrigh lecture.mp3 --transcriber wit -w TOKEN", help = "Transcribe a local recording"),
    example("tafrigh completions zsh --install", help = "Install shell completions")
)]
pub struct Cli {
    #[usage(subcommand)]
    pub command: Command,
}

/// Operations available from the command line.
#[derive(Debug, Clone, usage_rs::Subcommands)]
pub enum Command {
    /// Transcribe files, folders, videos or playlists.
    Transcribe(Box<Args>),
    /// Generate or install shell completions.
    Completions(Completions),
    /// Print the Usage specification as KDL for docs, man pages and tooling.
    Spec,
}

/// Generate a completion script, or install it in the shell's user directory.
///
/// Scripts call tafrigh directly; no separate usage executable is needed.
/// Installation prints any shell setup still required and preserves unrelated files.
#[derive(Debug, Clone, usage_rs::Args)]
#[usage(
    unknown_flags = "error",
    example("tafrigh completions bash", help = "Print a script to stdout"),
    example(
        "tafrigh completions zsh --install --dry-run",
        help = "Preview installation"
    )
)]
pub struct Completions {
    /// Shell to generate completions for.
    #[usage(arg, choices("bash", "elvish", "fish", "nu", "powershell", "zsh"))]
    pub shell: String,

    /// Install into this shell's user completion directory.
    #[usage(long)]
    pub install: bool,

    /// Show the destination and setup instructions without writing a file.
    #[usage(long, requires("--install"))]
    pub dry_run: bool,
}

/// Transcribe with Wit.ai or retrieve existing YouTube captions.
///
/// Inputs can be media files, folders of media files, or URLs (videos, playlists,
/// channels; anything yt-dlp supports). The youtube transcriber accepts only YouTube
/// URLs and existing captions in --subtitle-language; it never invokes Wit.
#[derive(Debug, Clone, usage_rs::Args)]
#[usage(
    unknown_flags = "error",
    max_term_width = 100,
    after_help = HELP_FOOTER,
    exit_code(3, "Finished, but some inputs or items failed"),
    exit_code(130, "Interrupted with Ctrl-C"),
    example("tafrigh transcribe lectures/ --transcriber wit -r -o transcripts -f srt,txt -w TOKEN", help = "Mirror a folder of recordings"),
    example("tafrigh transcribe lecture.mp3 --transcriber wit -f srt --words -w TOKEN", help = "Include word-level subtitles"),
    example("tafrigh transcribe https://www.youtube.com/watch?v=VIDEO_ID --transcriber youtube --subtitle-language ar", help = "Retrieve existing Arabic captions")
)]
pub struct Args {
    /// Files, folders or URLs to transcribe.
    #[usage(arg, required, name = "INPUT", value_hint = usage_rs::ValueHint::AnyPath)]
    pub inputs: Vec<String>,

    /// Backend to use for the entire run. Required; no automatic switching.
    #[usage(long, value_enum, value_name = "BACKEND")]
    pub transcriber: Transcriber,

    /// One exact YouTube subtitle language (e.g. ar or en-US); required with --transcriber
    /// youtube. Prefers manual captions, then original automatic captions; never translates.
    #[usage(long, value_name = "LANG")]
    pub subtitle_language: Option<String>,

    /// Include media in subdirectories. Directory symlinks are not followed.
    #[usage(short = 'r', long, negate = "no-recursive")]
    pub recursive: bool,

    /// Hide progress bars.
    #[usage(short, long, negate = "no-quiet")]
    pub quiet: bool,

    /// More logging (-v debug, -vv trace). RUST_LOG overrides.
    #[usage(short, long, count)]
    pub verbose: u8,

    #[usage(flatten, next_help_heading = "Output")]
    pub output: Output,

    #[usage(flatten, next_help_heading = "Segmentation")]
    pub segmentation: Segmentation,

    #[usage(flatten, next_help_heading = "Wit.ai")]
    pub wit: Wit,

    #[usage(flatten, next_help_heading = "Downloads and tools")]
    pub tools: Tools,
}

/// Explicit transcription backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Transcriber {
    /// Recognize audio with Wit.ai; requires Wit tokens and ffmpeg.
    Wit,
    /// Retrieve existing YouTube captions; missing captions fail the item.
    Youtube,
}

/// Where transcripts go and what they contain.
#[derive(Debug, Clone, usage_rs::Args)]
pub struct Output {
    /// Directory for transcripts; selects tree-to-tree mode unless --output-mode is given.
    #[usage(short = 'o', long = "output-dir", value_name = "DIR", value_hint = usage_rs::ValueHint::DirPath)]
    pub dir: Option<PathBuf>,

    /// Destination layout [default: side-by-side without -o, tree-to-tree with -o].
    #[usage(long = "output-mode", value_enum, value_name = "MODE")]
    pub mode: Option<OutputMode>,

    /// Output formats, comma-separated: txt, srt, vtt, csv, tsv, json, all, or word-level
    /// words-srt, words-vtt, words-csv, words-tsv, words-json.
    #[usage(
        short,
        long = "format",
        value_name = "FORMATS",
        delimiter = ',',
        default = "all",
        choices(
            "all",
            "txt",
            "srt",
            "vtt",
            "csv",
            "tsv",
            "json",
            "words-srt",
            "words-vtt",
            "words-csv",
            "words-tsv",
            "words-json"
        )
    )]
    pub formats: Vec<FormatChoice>,

    /// Also write word timings as <id>.words.<ext> for each chosen srt, vtt, csv, tsv or json
    /// format (one cue or row per word).
    #[usage(long, negate = "no-words")]
    pub words: bool,

    /// Merge segments with fewer words into the following ones (0 disables).
    #[usage(long, value_name = "N", default = "1")]
    pub min_words: usize,

    /// Also write uncompacted transcripts as <id>.uncompacted.<ext>.
    #[usage(long, negate = "no-keep-uncompacted")]
    pub keep_uncompacted: bool,

    /// Skip items when every requested output file already exists.
    #[usage(long, negate = "no-skip-existing")]
    pub skip_existing: bool,

    /// Write a sample CSV with this many random segments; suffix its name on collisions.
    #[usage(long, value_name = "N", default = "0")]
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
    pub fn mode(&self) -> Result<tafrigh::output::OutputMode, ValidationError> {
        use tafrigh::output::OutputMode as Mode;

        match (self.mode, self.dir.clone()) {
            (Some(OutputMode::SideBySide) | None, None) => Ok(Mode::SideBySide),
            (Some(OutputMode::TreeToTree) | None, Some(dir)) => Ok(Mode::TreeToTree(dir)),
            (Some(OutputMode::Flat), Some(dir)) => Ok(Mode::Flat(dir)),
            (Some(OutputMode::SideBySide), Some(_)) => Err(ValidationError::field("--output-mode")
                .value("side-by-side")
                .reason("cannot be used with --output-dir")),
            (Some(mode @ (OutputMode::TreeToTree | OutputMode::Flat)), None) => {
                Err(ValidationError::field("--output-mode")
                    .value(if mode == OutputMode::Flat {
                        "flat"
                    } else {
                        "tree-to-tree"
                    })
                    .reason("tree-to-tree and flat require --output-dir"))
            }
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

impl std::str::FromStr for FormatChoice {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.trim().eq_ignore_ascii_case("all") {
            Ok(FormatChoice::All)
        } else {
            value.trim().parse().map(FormatChoice::One)
        }
    }
}

/// How audio is cut into chunks.
#[derive(Debug, Clone, usage_rs::Args)]
pub struct Segmentation {
    /// How to find speech.
    #[usage(long, value_enum, default = "silero")]
    pub vad: Vad,

    /// Longest chunk sent for recognition, in seconds (Wit allows up to 60 on dictation and
    /// 20 on speech, padding included).
    #[usage(long, value_name = "SECONDS", default = "15.0")]
    pub max_chunk: f64,

    /// Merge consecutive speech up to --max-chunk: fewer requests and more context per
    /// request; segments still follow Wit's utterances.
    #[usage(long, negate = "no-merge")]
    pub merge: bool,

    /// Speech probability threshold.
    #[usage(long, value_name = "P", default = "0.5")]
    pub vad_threshold: f32,

    /// Shortest speech kept, in milliseconds.
    #[usage(long, value_name = "MS", default = "500")]
    pub min_speech_ms: u32,

    /// Silence that ends a chunk, in milliseconds.
    #[usage(long, value_name = "MS", default = "300")]
    pub min_silence_ms: u32,

    /// Context kept around detected speech, in milliseconds.
    #[usage(long, value_name = "MS", default = "250")]
    pub speech_pad_ms: u32,

    /// Pad each chunk on both sides with this many seconds of silence (0 disables). Wit drops
    /// words that start or end right at a chunk edge.
    #[usage(long, value_name = "SECONDS", default = "0.5")]
    pub pad: f64,

    /// Pad with white noise peaking at this level instead of silence (0 = full scale,
    /// as in the Python tafrigh).
    #[usage(long, value_name = "DBFS", allow_negative_numbers)]
    pub pad_noise: Option<f64>,

    /// Resend stretches of speech that Wit returned no words for, when at least this many
    /// seconds long (0 disables). Wit sometimes stops transcribing partway through a chunk.
    #[usage(long, value_name = "SECONDS", default = "1.0")]
    pub resend_gaps: f64,

    /// What to do when a chunk fails after all retries.
    #[usage(long, value_enum, value_name = "POLICY", default = "abort")]
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
#[derive(Debug, Clone, usage_rs::Args)]
pub struct Wit {
    /// Wit client access tokens (repeat or comma-separate). All apps must share a language.
    #[usage(
        short = 'w',
        long = "wit-token",
        value_name = "TOKEN",
        env = "TAFRIGH_WIT_TOKENS",
        value_hint = usage_rs::ValueHint::Other,
        hide_env_values,
        delimiter = ','
    )]
    pub tokens: Vec<String>,

    /// Language of the Wit apps (e.g. ar); detected from the tokens when omitted.
    #[usage(
        long = "wit-language",
        value_name = "LANG",
        env = "TAFRIGH_WIT_LANGUAGE"
    )]
    pub language: Option<String>,

    /// Request starts per second for each token.
    #[usage(long = "wit-rps", value_name = "N", default = "1.0")]
    pub requests_per_second: f64,

    /// Concurrent requests per token [default: enough to keep up with --wit-rps; Wit answers
    /// at about the pace of the audio, so longer chunks need more].
    #[usage(long = "wit-concurrency", value_name = "N")]
    pub concurrency: Option<usize>,

    /// Attempts per chunk, including the first.
    #[usage(long = "wit-attempts", value_name = "N", default = "5")]
    pub attempts: u32,

    /// Longest audio per request in seconds [default: 60 for dictation, 20 for speech].
    #[usage(long = "wit-max-audio", value_name = "SECONDS")]
    pub max_audio: Option<f64>,

    /// Wit endpoint: dictation (transcription only) or speech (also extracts intents).
    #[usage(long = "wit-endpoint", value_enum, default = "dictation")]
    pub endpoint: WitEndpoint,

    /// Wit API base URL (e.g. a proxy or a local mock server).
    #[usage(
        long = "wit-api-url",
        value_name = "URL",
        default = "https://api.wit.ai",
        value_hint = usage_rs::ValueHint::Url
    )]
    pub api_url: String,

    /// Wit API version, sent as the v query parameter.
    #[usage(long = "wit-api-version", value_name = "VERSION", default = "20240304")]
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
#[derive(Debug, Clone, usage_rs::Args)]
pub struct Tools {
    /// Extra yt-dlp arguments, e.g. "--cookies cookies.txt --playlist-items 1-10".
    #[usage(long, value_name = "ARGS", allow_hyphen_values)]
    pub yt_dlp_args: Option<String>,

    /// Where downloaded audio goes [default: -o, or the current directory].
    #[usage(long, value_name = "DIR", value_hint = usage_rs::ValueHint::DirPath)]
    pub download_dir: Option<PathBuf>,

    /// Save yt-dlp's metadata for each URL as <id>.info.json.
    #[usage(long, negate = "no-save-metadata")]
    pub save_metadata: bool,

    /// yt-dlp executable, needed for URL inputs.
    #[usage(long, value_name = "PATH", default = "yt-dlp", value_hint = usage_rs::ValueHint::ExecutablePath, env = "TAFRIGH_YT_DLP")]
    pub yt_dlp: PathBuf,

    /// ffmpeg executable (required with --transcriber wit).
    #[usage(long, value_name = "PATH", default = "ffmpeg", value_hint = usage_rs::ValueHint::ExecutablePath, env = "TAFRIGH_FFMPEG")]
    pub ffmpeg: PathBuf,

    /// ffprobe executable (optional; only used for progress percentages).
    #[usage(long, value_name = "PATH", default = "ffprobe", value_hint = usage_rs::ValueHint::ExecutablePath, env = "TAFRIGH_FFPROBE")]
    pub ffprobe: PathBuf,
}

/// Validate user options before constructing tools, models or HTTP clients.
fn validate(cli: &Cli) -> Result<(), ValidationError> {
    let Command::Transcribe(args) = &cli.command else {
        return Ok(());
    };
    args.output.mode()?;
    args.output
        .formats()
        .map_err(|error| ValidationError::field("--format").reason(error.to_string()))?;

    if let Some(extra) = &args.tools.yt_dlp_args {
        shell_words::split(extra)
            .map_err(|error| ValidationError::field("--yt-dlp-args").reason(error.to_string()))?;
    }
    if args.transcriber == Transcriber::Youtube {
        let language = args.subtitle_language.as_deref().ok_or_else(|| {
            ValidationError::field("--subtitle-language")
                .reason("is required with --transcriber youtube")
        })?;
        if !tafrigh_youtube::is_subtitle_language(language) {
            return Err(ValidationError::field("--subtitle-language")
                .reason("expected one language code, such as ar or en-US; patterns and lists are not supported"));
        }
        if args
            .inputs
            .iter()
            .any(|input| !tafrigh_youtube::is_youtube_url(input))
        {
            return Err(ValidationError::field("INPUT")
                .reason("--transcriber youtube requires YouTube URLs"));
        }
        return Ok(());
    }
    if args.subtitle_language.is_some() {
        return Err(ValidationError::field("--subtitle-language")
            .reason("requires --transcriber youtube; use --wit-language for Wit"));
    }

    let segmentation = &args.segmentation;
    for (name, value, allow_zero) in [
        ("--max-chunk", segmentation.max_chunk, false),
        ("--pad", segmentation.pad, true),
        ("--resend-gaps", segmentation.resend_gaps, true),
        ("--wit-rps", args.wit.requests_per_second, false),
    ] {
        if !value.is_finite() || value < 0.0 || (!allow_zero && value == 0.0) {
            return Err(ValidationError::field(name)
                .value(value.to_string())
                .reason(if allow_zero {
                    "must be finite and zero or positive"
                } else {
                    "must be finite and positive"
                }));
        }
    }
    if !(0.0..=1.0).contains(&segmentation.vad_threshold) {
        return Err(ValidationError::field("--vad-threshold")
            .value(segmentation.vad_threshold.to_string())
            .reason("must be between 0 and 1"));
    }
    if let Some(noise) = segmentation.pad_noise {
        if !noise.is_finite() || noise > 0.0 {
            return Err(ValidationError::field("--pad-noise")
                .value(noise.to_string())
                .reason("must be finite and at most 0 dBFS"));
        }
        if segmentation.pad == 0.0 {
            return Err(
                ValidationError::field("--pad-noise").reason("requires --pad greater than zero")
            );
        }
    }
    if args.wit.tokens.iter().all(|token| token.trim().is_empty()) {
        return Err(
            ValidationError::field("--wit-token").reason("at least one nonblank token is required")
        );
    }
    if args.wit.concurrency == Some(0) || args.wit.attempts == 0 {
        return Err(ValidationError::field(if args.wit.attempts == 0 {
            "--wit-attempts"
        } else {
            "--wit-concurrency"
        })
        .value("0")
        .reason("must be at least 1"));
    }
    let max_audio = args.wit.config(segmentation.request_seconds()).max_audio();
    if !max_audio.is_finite() || max_audio <= 0.0 {
        return Err(ValidationError::field("--wit-max-audio")
            .value(max_audio.to_string())
            .reason("must be finite and positive"));
    }
    if segmentation.request_seconds() > max_audio + 1e-9 {
        return Err(ValidationError::field("--max-chunk")
            .value(segmentation.max_chunk.to_string())
            .reason(format!(
                "chunk plus padding exceeds the {max_audio}s Wit audio limit"
            )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::*;

    fn parse(argv: &[&str]) -> Args {
        let argv: Vec<_> = argv.iter().map(OsStr::new).collect();
        let Command::Transcribe(parsed) = Cli::parse_from(&argv).unwrap().command else {
            panic!("expected transcription");
        };
        *parsed
    }

    #[test]
    fn parses_lists_counts_defaults_and_hyphenated_values() {
        let args = parse(&[
            "--transcriber",
            "wit",
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
            "-vv",
            "lecture.mp3",
        ]);
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
        assert_eq!(args.verbose, 2);
        assert_eq!(args.inputs, ["lecture.mp3"]);
        assert!(!args.output.words);
        assert!(!args.recursive);
    }

    #[test]
    fn direct_and_explicit_transcription_share_defaults() {
        for command in [
            vec!["recording.mp3", "--transcriber", "wit", "-w", "test"],
            vec![
                "transcribe",
                "recording.mp3",
                "--transcriber",
                "wit",
                "-w",
                "test",
            ],
        ] {
            let args = parse(&command);
            assert_eq!(args.output.formats, [FormatChoice::All]);
            assert_eq!(
                args.output.mode().unwrap(),
                tafrigh::output::OutputMode::SideBySide
            );
            assert_eq!(args.segmentation.max_chunk, 15.0);
        }
    }

    #[test]
    fn later_options_override_presets() {
        let args = parse(&[
            "recording.mp3",
            "--transcriber",
            "wit",
            "-w",
            "test",
            "-o",
            "first",
            "-o",
            "last",
            "--merge",
            "--no-merge",
            "-r",
            "--no-recursive",
            "--no-quiet",
            "-q",
            "--words",
            "--no-words",
            "--keep-uncompacted",
            "--no-keep-uncompacted",
            "--skip-existing",
            "--no-skip-existing",
            "--save-metadata",
            "--no-save-metadata",
        ]);
        assert_eq!(args.output.dir, Some("last".into()));
        assert_eq!(
            args.output.mode().unwrap(),
            tafrigh::output::OutputMode::TreeToTree("last".into())
        );
        assert!(!args.segmentation.merge);
        assert!(!args.recursive);
        assert!(args.quiet);
        assert!(!args.output.words);
        assert!(!args.output.keep_uncompacted);
        assert!(!args.output.skip_existing);
        assert!(!args.tools.save_metadata);
    }

    #[test]
    fn completions_offer_choices_and_appropriate_path_kinds() {
        use usage_rs::test::{self as harness, Files};

        assert_eq!(
            harness::candidates(Cli::spec(), "tafrigh --transcriber y"),
            ["youtube"]
        );

        assert!(
            harness::candidates(Cli::spec(), "tafrigh transcribe --format w")
                .contains(&"words-srt".to_owned())
        );
        assert_eq!(
            harness::candidates(Cli::spec(), "tafrigh transcribe --output-mode tr"),
            ["tree-to-tree"]
        );
        assert_eq!(
            harness::completion(Cli::spec(), "tafrigh transcribe --output-dir ").files,
            Some(Files::Dirs)
        );
        assert_eq!(
            harness::completion(Cli::spec(), "tafrigh transcribe ").files,
            Some(Files::Any)
        );
        assert_eq!(
            harness::completion(Cli::spec(), "tafrigh transcribe --wit-token ").files,
            None
        );
    }
}
