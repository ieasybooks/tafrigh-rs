//! Progress reporting.
//!
//! Library calls take a `&dyn Progress`; any `Fn(E) + Send + Sync` closure works, and
//! [`NoProgress`] ignores everything.

use std::path::PathBuf;
use std::sync::Arc;

/// Receives progress events of type `E`.
pub trait Progress<E = TranscribeEvent>: Send + Sync {
    /// Handles one event. Called from the task doing the work, so keep it cheap.
    fn report(&self, event: E);
}

impl<E, F> Progress<E> for F
where
    F: Fn(E) + Send + Sync,
{
    fn report(&self, event: E) {
        self(event);
    }
}

/// Discards all events.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoProgress;

impl<E> Progress<E> for NoProgress {
    fn report(&self, _event: E) {}
}

/// Progress of transcribing a single media file. Times are in seconds.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TranscribeEvent {
    /// Decoding started.
    Started {
        /// Container duration, when the file reports one.
        duration: Option<f64>,
    },
    /// Audio has been decoded and analyzed up to `seconds`.
    Decoded {
        /// Position reached in the source.
        seconds: f64,
    },
    /// A chunk of speech was found and queued for recognition.
    ChunkQueued {
        /// Position of the chunk in the file, starting at 0.
        index: usize,
        /// Start of the chunk in the source.
        start: f64,
        /// End of the chunk in the source.
        end: f64,
    },
    /// A chunk was recognized.
    ChunkRecognized {
        /// Position of the chunk in the file, starting at 0.
        index: usize,
        /// Start of the chunk in the source.
        start: f64,
        /// End of the chunk in the source.
        end: f64,
        /// No text came back.
        empty: bool,
    },
    /// A chunk failed after all retries and was skipped (see [`crate::OnChunkError`]).
    ChunkSkipped {
        /// Position of the chunk in the file, starting at 0.
        index: usize,
        /// Start of the chunk in the source.
        start: f64,
        /// End of the chunk in the source.
        end: f64,
        /// Why recognition failed.
        error: String,
    },
    /// All audio was decoded and segmented.
    Segmented {
        /// Final number of chunks.
        chunks: usize,
    },
}

/// Identifies a media item in pipeline events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ItemInfo {
    /// Output file stem, unique within a run.
    pub id: Arc<str>,
    /// Human-readable name, e.g. a video title or file name.
    pub title: Option<Arc<str>>,
}

/// Progress of a whole [`crate::Tafrigh`] run.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum Event {
    /// An input (file, folder or URL) is being expanded into items.
    InputResolving {
        /// The input as given.
        input: String,
    },
    /// An input was expanded.
    InputResolved {
        /// The input as given.
        input: String,
        /// Number of items found.
        items: usize,
    },
    /// An input could not be expanded; its items are not processed.
    InputFailed {
        /// The input as given.
        input: String,
        /// Why it failed.
        error: String,
    },
    /// Work on an item started.
    ItemStarted {
        /// The item.
        item: ItemInfo,
        /// 1-based position within the current input.
        position: usize,
        /// Number of items in the current input.
        total: usize,
    },
    /// An item was skipped.
    ItemSkipped {
        /// The item.
        item: ItemInfo,
        /// Why it was skipped.
        reason: String,
    },
    /// An item's audio is being downloaded.
    Downloading {
        /// The item.
        item: ItemInfo,
    },
    /// Transcription progress of an item.
    Transcribing {
        /// The item.
        item: ItemInfo,
        /// What happened.
        event: TranscribeEvent,
    },
    /// An item was transcribed and its outputs written.
    ItemFinished {
        /// The item.
        item: ItemInfo,
        /// Number of segments before compaction.
        segments: usize,
        /// Files written.
        outputs: Vec<PathBuf>,
    },
    /// An item failed; the run continues with the next one.
    ItemFailed {
        /// The item.
        item: ItemInfo,
        /// Why it failed.
        error: String,
    },
}
