//! Resolving user inputs into media items.

mod captions;
mod local;
mod ytdlp;

use std::convert::Infallible;
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

pub use captions::{is_subtitle_language, is_youtube_url};
pub(crate) use local::discover_local;
pub use local::{MEDIA_EXTENSIONS, expand_local, is_media_path};
pub use ytdlp::YtDlp;

/// Something to transcribe, as given by the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Input {
    /// A media file, or a folder of media files (recursive discovery is opt-in).
    Path(PathBuf),
    /// A URL handled by yt-dlp (videos, playlists, channels).
    Url(String),
}

impl Input {
    /// True for [`Input::Url`].
    pub fn is_url(&self) -> bool {
        matches!(self, Self::Url(_))
    }

    /// Treats `scheme://…` as a URL and anything else as a path.
    pub fn parse(value: &str) -> Self {
        match value.split_once("://") {
            Some((scheme, _))
                if !scheme.is_empty()
                    && scheme
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')) =>
            {
                Self::Url(value.to_owned())
            }
            _ => Self::Path(PathBuf::from(value)),
        }
    }
}

impl FromStr for Input {
    type Err = Infallible;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse(value))
    }
}

impl From<&str> for Input {
    fn from(value: &str) -> Self {
        Self::parse(value)
    }
}

impl From<PathBuf> for Input {
    fn from(value: PathBuf) -> Self {
        Self::Path(value)
    }
}

impl fmt::Display for Input {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Path(path) => write!(f, "{}", path.display()),
            Self::Url(url) => f.write_str(url),
        }
    }
}

/// A single recording to transcribe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaItem {
    /// Output file stem; unique per item within a run.
    pub id: Arc<str>,
    /// Human-readable name, e.g. a video title or file name.
    pub title: Option<Arc<str>>,
    /// Where the audio comes from.
    pub media: Media,
    /// A link to the original, used in samples (e.g. a video page).
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Where an item's audio comes from.
pub enum Media {
    /// A local file.
    File(PathBuf),
    /// Downloaded on demand through yt-dlp.
    Remote {
        /// Page or media URL passed to yt-dlp.
        url: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inputs() {
        assert_eq!(
            Input::parse("https://youtu.be/x"),
            Input::Url("https://youtu.be/x".into())
        );
        assert_eq!(
            Input::parse("file:///tmp/a.mp3"),
            Input::Url("file:///tmp/a.mp3".into())
        );
        assert_eq!(Input::parse("a/b.mp3"), Input::Path("a/b.mp3".into()));
        assert_eq!(Input::parse("C:\\x.mp3"), Input::Path("C:\\x.mp3".into()));
    }
}
