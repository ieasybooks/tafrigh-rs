//! Existing [YouTube](https://www.youtube.com) captions for tafrigh.
//!
//! [`YoutubeCaptions`] implements [`CaptionProvider`], using yt-dlp to resolve URLs and
//! retrieve original captions in one language. It never downloads or recognizes audio.
//!
//! ```no_run
//! # async fn demo() -> tafrigh::Result<()> {
//! use tafrigh::source::{Input, YtDlp};
//! use tafrigh::{NoProgress, Tafrigh};
//! use tafrigh_youtube::YoutubeCaptions;
//!
//! let pipeline = Tafrigh::captions(YoutubeCaptions::new("ar")?)
//!     .ytdlp(YtDlp::new("."))
//!     .build();
//! let report = pipeline
//!     .run([Input::parse("https://youtu.be/VIDEO_ID")], &NoProgress)
//!     .await?;
//! assert_eq!(report.failures(), 0);
//! # Ok(())
//! # }
//! ```

mod captions;

use std::ffi::OsStr;

use async_trait::async_trait;
use serde_json::Value;
use tafrigh::source::{Media, MediaItem, YtDlp};
use tafrigh::{CaptionProvider, Error, Result, Segment};

pub use captions::{is_subtitle_language, is_youtube_url};

/// Retrieves manual or original automatic captions in one exact language.
/// Available word starts are preserved; word ends use the next start or the caption end.
#[derive(Debug, Clone)]
pub struct YoutubeCaptions {
    language: String,
}

impl YoutubeCaptions {
    /// Creates a provider for a single language code, such as `ar` or `en-US`.
    /// Language patterns, lists and automatic translation are not supported.
    pub fn new(language: impl Into<String>) -> Result<Self> {
        let language = language.into();
        if !is_subtitle_language(&language) {
            return Err(Error::Config(
                "expected one subtitle language code, such as ar or en-US".into(),
            ));
        }
        Ok(Self { language })
    }
}

/// Options for every yt-dlp run: no audio, and no output besides what is asked for.
const CAPTIONS_ONLY: [&str; 4] = [
    "--skip-download",
    "--ignore-no-formats-error",
    "--no-progress",
    "--quiet",
];

#[async_trait]
impl CaptionProvider for YoutubeCaptions {
    /// Resolves [YouTube](https://www.youtube.com) videos, playlists and channels without requiring audio formats.
    async fn resolve(&self, url: &str, ytdlp: &YtDlp) -> Result<Vec<MediaItem>> {
        if !captions::is_youtube_url(url) {
            return Err(Error::Source(
                "the youtube transcriber requires YouTube URLs".into(),
            ));
        }
        let resolve = [
            "--flat-playlist",
            "--dump-single-json",
            "--ignore-errors",
            "--simulate",
            "--",
            url,
        ];
        ytdlp
            .list(CAPTIONS_ONLY.into_iter().chain(resolve), url)
            .await
    }

    /// Fetches existing captions in the configured language, preferring manual captions to
    /// original automatic captions. Missing or translated-only tracks fail the item.
    /// Audio is never downloaded. Temporary caption files are removed after parsing.
    async fn fetch(&self, item: &MediaItem, ytdlp: &YtDlp) -> Result<Vec<Segment>> {
        let language = self.language.as_str();
        let Media::Remote { url } = &item.media else {
            return Err(Error::Source(
                "the youtube transcriber requires YouTube URLs".into(),
            ));
        };
        if !captions::is_youtube_url(url) {
            return Err(Error::Source(
                "the youtube transcriber requires YouTube URLs".into(),
            ));
        }
        let dump_info = [
            "--no-playlist",
            "--dump-single-json",
            "--simulate",
            "--",
            url,
        ];
        let output = ytdlp
            .run(CAPTIONS_ONLY.into_iter().chain(dump_info), url)
            .await?;
        let mut info: Value = serde_json::from_slice(&output).map_err(|err| {
            Error::Source(format!("yt-dlp returned invalid JSON for {url}: {err}"))
        })?;
        captions::select_track(&mut info, language)?;

        let temporary =
            tempfile::tempdir().map_err(|err| Error::io("creating caption directory", err))?;
        let metadata = temporary.path().join("selected.info.json");
        tokio::fs::write(
            &metadata,
            serde_json::to_vec(&info).expect("JSON value serializes"),
        )
        .await
        .map_err(|err| Error::io("writing selected caption metadata", err))?;
        // Escape literal percent signs in TMPDIR before adding yt-dlp's extension template.
        let template = format!(
            "{}/captions.%(ext)s",
            temporary.path().to_string_lossy().replace('%', "%%")
        );
        let subtitle_template = format!("subtitle:{template}");
        let download = [
            "--no-playlist",
            "--no-simulate",
            "--write-subs",
            "--no-write-auto-subs",
            "--sub-langs",
            language,
            "--sub-format",
            "json3",
            "--convert-subs",
            "none",
            "--no-embed-subs",
            "--no-write-info-json",
            "--no-write-thumbnail",
            "--output",
            &template,
            "--output",
            &subtitle_template,
            "--load-info-json",
        ];
        let args = CAPTIONS_ONLY
            .into_iter()
            .chain(download)
            .map(OsStr::new)
            .chain([metadata.as_os_str()]);
        ytdlp.run(args, url).await?;
        let path = temporary.path().join(format!("captions.{language}.json3"));
        let bytes = tokio::fs::read(&path).await.map_err(|err| {
            Error::io(
                format!("reading downloaded {language} captions for {url}"),
                err,
            )
        })?;
        captions::parse_json3(&bytes)
    }
}
