//! Streaming `FireRedVAD` on tract, with Kaldi filterbanks and Silero's timestamp rules.
//!
//! Every recording starts with a fresh cache. Complete 25 ms windows produce probabilities
//! at 10 ms intervals; incomplete windows at EOF are discarded (Kaldi's snipped edges).

mod features;
#[cfg(test)]
mod golden_tests;
mod model;

use std::time::Duration;

use features::HOP_SAMPLES;
pub use model::FireRedModel;
use model::ProbabilityStream;

use super::timestamps::{StreamingTimestamps, TimestampParams};
use super::{SegmentationSession, Segmenter, SpeechSpan};
use crate::audio::SAMPLE_RATE;
use crate::{Error, Result};

/// Speech detection settings for streaming `FireRedVAD`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FireRedConfig {
    /// Speech probability at or above which a frame counts as speech (default 0.4).
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

impl Default for FireRedConfig {
    fn default() -> Self {
        Self {
            threshold: 0.4,
            min_speech: Duration::from_millis(500),
            max_speech: Duration::from_secs(15),
            min_silence: Duration::from_millis(300),
            speech_pad: Duration::from_millis(250),
        }
    }
}

impl FireRedConfig {
    fn timestamp_params(&self) -> TimestampParams {
        let millis = |duration: Duration| u32::try_from(duration.as_millis()).unwrap_or(u32::MAX);
        TimestampParams {
            frame_samples: HOP_SAMPLES,
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
        let min_span = HOP_SAMPLES as f64 / f64::from(SAMPLE_RATE)
            + 2.0 * f64::from(params.speech_pad_ms) / 1000.0;
        if params.max_speech_s <= min_span {
            return Err(Error::Config(format!(
                "maximum speech duration must be above {min_span:.3}s"
            )));
        }
        Ok(())
    }
}

/// Segments recordings with the official streaming `FireRedVAD` model.
#[derive(Debug, Clone)]
pub struct FireRedSegmenter {
    model: FireRedModel,
    params: TimestampParams,
}

impl FireRedSegmenter {
    /// Uses the bundled model and normalization statistics.
    pub fn new(config: FireRedConfig) -> Result<Self> {
        config.validate()?;
        Self::with_model(FireRedModel::bundled()?, config)
    }

    /// Uses an already loaded model, shared with other segmenters if desired.
    pub fn with_model(model: FireRedModel, config: FireRedConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            model,
            params: config.timestamp_params(),
        })
    }
}

impl Segmenter for FireRedSegmenter {
    fn begin(&self) -> Result<Box<dyn SegmentationSession>> {
        Ok(Box::new(FireRedSession {
            probs: self.model.stream()?,
            timestamps: StreamingTimestamps::new(&self.params),
            scratch: Vec::with_capacity(model::BATCH_FRAMES),
            samples: 0,
            max_samples: (self.params.max_speech_s * f64::from(SAMPLE_RATE)) as usize,
        }))
    }
}

struct FireRedSession {
    probs: ProbabilityStream,
    timestamps: StreamingTimestamps,
    scratch: Vec<f32>,
    samples: usize,
    max_samples: usize,
}

impl SegmentationSession for FireRedSession {
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
        let mut final_spans = Vec::new();
        self.timestamps.finish(self.samples, &mut final_spans);
        // A snipped 25 ms window reaches 15 ms beyond the 10 ms probability stride.
        // With little or no speech padding, that EOF tail can cross the split limit.
        // Keep all source samples, using the same hard limit as downstream chunking.
        for span in final_spans {
            for start in (span.start..span.end).step_by(self.max_samples) {
                out.push(SpeechSpan {
                    start,
                    end: start.saturating_add(self.max_samples).min(span.end),
                });
            }
        }
        Ok(())
    }

    fn retain_from(&self) -> usize {
        self.timestamps.retain_from()
    }
}
