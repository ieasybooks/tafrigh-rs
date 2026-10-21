//! Tafrigh: transcribe audio and video into subtitles and text.
//!
//! The crate is organized as a small set of composable pieces:
//!
//! - [`audio`]: decoding any media file to 16 kHz mono PCM through `ffmpeg`, streamed.
//! - [`segment`]: splitting that stream into speech spans ([`segment::Segmenter`]), with
//!   Silero VAD v6 ([`segment::silero`]) as the default and fixed windows as a fallback.
//! - [`Transcriber`] turns a media file into [`Segment`]s, and [`CaptionProvider`] retrieves
//!   existing captions. [`ChunkedTranscriber`] pairs a segmenter with a [`ChunkRecognizer`]
//!   (for example a cloud speech API) and runs requests concurrently while the file is still
//!   being decoded.
//! - [`source`]: resolving inputs (local files, folders, URLs through `yt-dlp`) into media items.
//! - [`output`]: compaction and transcript formats (txt, srt, vtt, csv, tsv, json).
//! - [`Tafrigh`] ties sources, a transcriber and outputs together and reports progress as
//!   [`Event`]s. [`Tafrigh::captions`] uses a caption provider without audio.
//!
//! Backends live in their own crates: `tafrigh-wit` implements [`ChunkRecognizer`] and
//! `tafrigh-youtube` implements [`CaptionProvider`]. The CLI selects and constructs them.
//!
//! ```no_run
//! # async fn demo(recognizer: impl tafrigh::ChunkRecognizer + 'static) -> tafrigh::Result<()> {
//! use tafrigh::segment::silero::{SileroConfig, SileroSegmenter};
//! use tafrigh::{ChunkOptions, ChunkedTranscriber, NoProgress, Transcriber};
//!
//! let segmenter = SileroSegmenter::new(SileroConfig::default())?;
//! let transcriber = ChunkedTranscriber::new(segmenter, recognizer, ChunkOptions::default())?;
//! let segments = transcriber.transcribe("lecture.mp3".as_ref(), &NoProgress).await?;
//! for segment in segments {
//!     println!("[{:.2} → {:.2}] {}", segment.start, segment.end, segment.text);
//! }
//! # Ok(())
//! # }
//! ```

pub mod audio;
mod error;
mod event;
pub mod output;
mod pipeline;
pub mod segment;
pub mod source;
mod transcribe;
mod types;

pub use error::{BoxError, Error, ErrorChain, Result};
pub use event::{Event, ItemInfo, NoProgress, Progress, TranscribeEvent};
pub use pipeline::{FailedInput, ItemReport, ItemStatus, RunReport, Tafrigh, TafrighBuilder};
pub use transcribe::{
    CaptionProvider, ChunkOptions, ChunkRecognizer, ChunkedTranscriber, OnChunkError, Recognition,
    Transcriber, Utterance,
};
pub use types::{Segment, Word};
