//! Silero VAD v6 segmenter.
//!
//! Speech probabilities come from the bundled Silero v6 network running on tract; spans follow
//! Silero 6.2.1's rules. When speech runs longer than the maximum, it is cut at the longest
//! pause inside it rather than mid-word.

#[cfg(test)]
mod golden_tests;
mod model;

#[cfg(test)]
use super::timestamps;
use super::timestamps::{StreamingTimestamps, TimestampParams};
use model::ProbabilityStream;
pub use model::SileroModel;

use std::time::Duration;

use super::{SegmentationSession, Segmenter, SpeechSpan};
use crate::audio::SAMPLE_RATE;
use crate::{Error, Result};

/// Samples per Silero frame at 16 kHz.
const WINDOW_SAMPLES: usize = 512;
/// Samples of the previous frame prepended to each frame.
const CONTEXT_SAMPLES: usize = 64;

/// Settings tuned for chunked cloud recognition. Durations below the longest span are used
/// in whole milliseconds, as in Silero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SileroConfig {
    /// Speech probability at or above which a frame counts as speech.
    pub threshold: f32,
    /// Shortest speech kept.
    pub min_speech: Duration,
    /// Longest span. Longer speech is split at its longest pause.
    pub max_speech: Duration,
    /// Silence that ends a span.
    pub min_silence: Duration,
    /// Padding on each side of a span.
    pub speech_pad: Duration,
}

impl Default for SileroConfig {
    /// The threshold is 0.15 rather than Silero's 0.5: at 0.5, Silero scores some Quran
    /// recitation, especially melodic recitation, as silence, so it never reaches the recognizer.
    fn default() -> Self {
        Self {
            threshold: 0.15,
            min_speech: Duration::from_millis(500),
            max_speech: Duration::from_secs(15),
            min_silence: Duration::from_millis(300),
            speech_pad: Duration::from_millis(250),
        }
    }
}

impl SileroConfig {
    /// The full timestamp rules these settings stand for.
    fn timestamp_params(&self) -> TimestampParams {
        let millis = |duration: Duration| u32::try_from(duration.as_millis()).unwrap_or(u32::MAX);
        TimestampParams {
            frame_samples: WINDOW_SAMPLES,
            threshold: self.threshold,
            min_speech_ms: millis(self.min_speech),
            max_speech_s: self.max_speech.as_secs_f64(),
            min_silence_ms: millis(self.min_silence),
            speech_pad_ms: millis(self.speech_pad),
            ..TimestampParams::default()
        }
    }

    fn validate(&self) -> Result<()> {
        if !(0.0..=1.0).contains(&self.threshold) {
            return Err(Error::Config(
                "VAD threshold must be between 0 and 1".into(),
            ));
        }
        let params = self.timestamp_params();
        let rate = f64::from(SAMPLE_RATE);
        let min_span =
            (WINDOW_SAMPLES as f64 + 2.0 * f64::from(params.speech_pad_ms) * rate / 1000.0) / rate;
        if params.max_speech_s <= min_span {
            return Err(Error::Config(format!(
                "maximum speech duration must be above {min_span:.3}s"
            )));
        }
        Ok(())
    }
}

/// Segments with Silero VAD v6.
#[derive(Debug, Clone)]
pub struct SileroSegmenter {
    model: SileroModel,
    params: TimestampParams,
}

impl SileroSegmenter {
    /// Uses the bundled model.
    pub fn new(config: SileroConfig) -> Result<Self> {
        Self::with_model(SileroModel::bundled()?, config)
    }

    /// Uses an already loaded model, e.g. one shared with other segmenters.
    pub fn with_model(model: SileroModel, config: SileroConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            model,
            params: config.timestamp_params(),
        })
    }
}

impl Segmenter for SileroSegmenter {
    fn begin(&self) -> Result<Box<dyn SegmentationSession>> {
        Ok(Box::new(SileroSession {
            probs: self.model.stream()?,
            timestamps: StreamingTimestamps::new(&self.params),
            scratch: Vec::with_capacity(model::BATCH_FRAMES),
            samples: 0,
        }))
    }
}

struct SileroSession {
    probs: ProbabilityStream,
    timestamps: StreamingTimestamps,
    scratch: Vec<f32>,
    samples: usize,
}

impl SegmentationSession for SileroSession {
    fn push(&mut self, samples: &[f32], out: &mut Vec<SpeechSpan>) -> Result<()> {
        self.samples += samples.len();
        self.scratch.clear();
        self.probs.push(samples, &mut self.scratch)?;
        self.timestamps.push(&self.scratch, out);
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<SpeechSpan>) -> Result<()> {
        self.scratch.clear();
        self.probs.finish(&mut self.scratch)?;
        self.timestamps.push(&self.scratch, out);
        self.timestamps.finish(self.samples, out);
        Ok(())
    }

    fn retain_from(&self) -> usize {
        self.timestamps.retain_from()
    }
}
