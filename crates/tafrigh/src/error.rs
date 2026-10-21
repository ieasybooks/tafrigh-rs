//! Error types.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

/// Boxed error used at extension points (recognizers, custom sources).
pub type BoxError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Result with [`Error`] as the default error type.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors produced by this crate.
///
/// Messages leave out their underlying cause, which is available through
/// [`source`](std::error::Error::source); use [`ErrorChain`] (or `anyhow`'s `{:#}`) to print
/// the whole chain on one line.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A file system or process I/O operation failed.
    #[error("{context}")]
    Io {
        /// What was being done.
        context: String,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// A required external program (`ffmpeg`, `yt-dlp`) could not be run.
    ///
    /// This is a setup problem rather than a problem with one input, so
    /// [`Tafrigh::run`](crate::Tafrigh::run) stops on it instead of failing every item.
    #[error("could not run {tool} (`{}`)", program.display())]
    ToolUnavailable {
        /// The tool's name, e.g. `"ffmpeg"`.
        tool: &'static str,
        /// The executable that was tried.
        program: PathBuf,
        /// Why it could not be run, e.g. [`std::io::ErrorKind::NotFound`].
        #[source]
        source: std::io::Error,
    },

    /// `ffmpeg` ran but could not decode a file.
    #[error("could not decode {}: {message}", path.display())]
    Decode {
        /// The media file.
        path: PathBuf,
        /// ffmpeg's last error line.
        message: String,
    },

    /// The voice activity model failed.
    #[error("voice activity detection failed: {0}")]
    Vad(String),

    /// A chunk failed after the recognizer's own retries.
    #[error("chunk #{index} ({start:.2}s–{end:.2}s) could not be recognized")]
    Recognition {
        /// Position of the chunk in the file, starting at 0.
        index: usize,
        /// Start of the chunk in the source, in seconds.
        start: f64,
        /// End of the chunk in the source, in seconds.
        end: f64,
        /// The recognizer's error.
        #[source]
        source: BoxError,
    },

    /// An input could not be resolved or downloaded.
    #[error("{0}")]
    Source(String),

    /// Options are inconsistent or out of range.
    #[error("invalid configuration: {0}")]
    Config(String),

    /// The work was cancelled before it finished.
    #[error("cancelled")]
    Cancelled,
}

/// Displays an error followed by its causes, separated by `: `, like `anyhow`'s `{:#}`.
///
/// ```
/// use tafrigh::{Error, ErrorChain};
///
/// let err = Error::Io {
///     context: "writing a.srt".into(),
///     source: std::io::Error::other("disk full"),
/// };
/// assert_eq!(err.to_string(), "writing a.srt");
/// assert_eq!(ErrorChain(&err).to_string(), "writing a.srt: disk full");
/// ```
#[derive(Debug, Clone, Copy)]
pub struct ErrorChain<'a>(pub &'a (dyn std::error::Error + 'static));

impl fmt::Display for ErrorChain<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)?;
        let mut source = self.0.source();
        while let Some(cause) = source {
            write!(f, ": {cause}")?;
            source = cause.source();
        }
        Ok(())
    }
}

impl Error {
    /// Adds context to an I/O error from the pipeline or a backend.
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }

    pub(crate) fn tool(tool: &'static str, program: &Path, source: std::io::Error) -> Self {
        Self::ToolUnavailable {
            tool,
            program: program.to_owned(),
            source,
        }
    }

    /// Turns the outcome of running `program flag` (e.g. `ffmpeg -version`) into `Ok` or
    /// [`Error::ToolUnavailable`].
    pub(crate) fn check_tool(
        tool: &'static str,
        program: &Path,
        flag: &str,
        status: std::io::Result<ExitStatus>,
    ) -> Result<()> {
        let source = match status {
            Ok(status) if status.success() => return Ok(()),
            Ok(status) => std::io::Error::other(format!("`{flag}` exited with {status}")),
            Err(err) => err,
        };
        Err(Self::tool(tool, program, source))
    }
}
