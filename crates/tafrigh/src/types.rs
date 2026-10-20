use serde::{Deserialize, Serialize};

/// A transcribed span of the source timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    /// Start, in seconds from the beginning of the media.
    pub start: f64,
    /// End, in seconds from the beginning of the media.
    pub end: f64,
    /// Recognized text.
    pub text: String,
    /// Word-level timings, when the recognizer provides them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<Word>,
}

impl Segment {
    /// A segment without word timings.
    pub fn new(start: f64, end: f64, text: impl Into<String>) -> Self {
        Self {
            start,
            end,
            text: text.into(),
            words: Vec::new(),
        }
    }

    /// Length in seconds.
    pub fn duration(&self) -> f64 {
        self.end - self.start
    }

    /// Number of whitespace-separated words in the text.
    pub fn word_count(&self) -> usize {
        self.text.split_whitespace().count()
    }
}

/// A recognized word.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Word {
    /// Start, in seconds from the beginning of the media.
    pub start: f64,
    /// End, in seconds from the beginning of the media.
    pub end: f64,
    /// The word as recognized.
    pub text: String,
    /// Recognizer confidence between 0 and 1, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}
