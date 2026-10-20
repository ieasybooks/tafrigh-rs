//! Turning media files into segments.

mod chunked;

use std::path::Path;

use async_trait::async_trait;

pub use chunked::{ChunkOptions, ChunkedTranscriber, OnChunkError};

use crate::audio::AudioChunk;
use crate::event::{Progress, TranscribeEvent};
use crate::{BoxError, Result, Segment, Word};

/// Anything that turns a media file into timed text.
///
/// Implemented by [`ChunkedTranscriber`] for chunk-at-a-time recognizers. Engines that consume
/// whole files and segment internally (e.g. Whisper) implement this directly.
#[async_trait]
pub trait Transcriber: Send + Sync {
    /// Transcribes `media`, reporting progress along the way. Segments are in time order.
    async fn transcribe(
        &self,
        media: &Path,
        progress: &dyn Progress<TranscribeEvent>,
    ) -> Result<Vec<Segment>>;
}

/// Recognizes one chunk of audio at a time, e.g. a cloud speech API.
#[async_trait]
pub trait ChunkRecognizer: Send + Sync {
    /// Recognizes `chunk`. Word times in the result are relative to the start of
    /// `chunk.samples` (padding included). Implementations handle their own retries.
    async fn recognize(&self, chunk: &AudioChunk) -> Result<Recognition, BoxError>;

    /// Longest audio (padding included) accepted per request, in seconds.
    fn max_audio_duration(&self) -> Option<f64> {
        None
    }

    /// How many requests may usefully be in flight at once.
    fn max_in_flight(&self) -> usize {
        1
    }
}

/// What a recognizer heard in one chunk: zero or more utterances, in order.
///
/// When every utterance carries word timings, each becomes its own [`Segment`] bounded by its
/// first and last word; otherwise the chunk becomes one segment spanning the chunk.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Recognition {
    /// Utterances in the order they were spoken.
    pub utterances: Vec<Utterance>,
}

impl Recognition {
    /// A single utterance without word timings.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            utterances: vec![Utterance {
                text: text.into(),
                words: Vec::new(),
            }],
        }
    }

    /// All utterance texts joined with spaces.
    pub fn joined_text(&self) -> String {
        let parts: Vec<&str> = self
            .utterances
            .iter()
            .map(|u| u.text.trim())
            .filter(|t| !t.is_empty())
            .collect();
        parts.join(" ")
    }
}

/// One stretch of recognized speech.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Utterance {
    /// Recognized text.
    pub text: String,
    /// Word timings relative to the start of the chunk audio (padding included).
    pub words: Vec<Word>,
}

#[async_trait]
impl<T: Transcriber + ?Sized> Transcriber for std::sync::Arc<T> {
    async fn transcribe(
        &self,
        media: &Path,
        progress: &dyn Progress<TranscribeEvent>,
    ) -> Result<Vec<Segment>> {
        (**self).transcribe(media, progress).await
    }
}

#[async_trait]
impl<T: ChunkRecognizer + ?Sized> ChunkRecognizer for std::sync::Arc<T> {
    async fn recognize(&self, chunk: &AudioChunk) -> Result<Recognition, BoxError> {
        (**self).recognize(chunk).await
    }

    fn max_audio_duration(&self) -> Option<f64> {
        (**self).max_audio_duration()
    }

    fn max_in_flight(&self) -> usize {
        (**self).max_in_flight()
    }
}
