//! Silero VAD v6 network on tract.
//!
//! Uses the sequence-form ONNX export distributed by faster-whisper, which evaluates many
//! 512-sample frames per call while carrying the LSTM state between calls.

use std::io::Cursor;
use std::sync::Arc;

use tract_onnx::prelude::*;

use super::{CONTEXT_SAMPLES, WINDOW_SAMPLES};
use crate::{Error, Result};

/// The bundled model (MIT, Silero Team; export from SYSTRAN/faster-whisper).
static MODEL_ONNX: &[u8] = include_bytes!("../../../assets/silero_vad_v6.onnx");

/// Frames per network call.
pub(super) const BATCH_FRAMES: usize = 256;
const STATE: usize = 128;
const ROW: usize = WINDOW_SAMPLES + CONTEXT_SAMPLES;

type Plan = TypedRunnableModel;

/// Loaded, optimized network. Cheap to clone and share between threads.
#[derive(Clone)]
pub struct SileroModel {
    plan: Arc<Plan>,
}

impl std::fmt::Debug for SileroModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SileroModel").finish_non_exhaustive()
    }
}

impl SileroModel {
    /// Loads the bundled model.
    pub fn bundled() -> Result<Self> {
        Self::from_onnx(MODEL_ONNX)
    }

    /// Loads a compatible sequence-form Silero ONNX export.
    pub fn from_onnx(bytes: &[u8]) -> Result<Self> {
        let load = || -> TractResult<Arc<Plan>> {
            tract_onnx::onnx()
                .model_for_read(&mut Cursor::new(bytes))?
                .with_input_fact(0, f32::fact([BATCH_FRAMES, ROW]).into())?
                .with_input_fact(1, f32::fact([1, 1, STATE]).into())?
                .with_input_fact(2, f32::fact([1, 1, STATE]).into())?
                .into_optimized()?
                .into_runnable()
        };
        let plan = load().map_err(|err| Error::Vad(format!("loading Silero model: {err:#}")))?;
        Ok(Self { plan })
    }

    /// Starts a fresh stream (zeroed recurrent state) for one recording.
    pub(super) fn stream(&self) -> Result<ProbabilityStream> {
        let zero =
            || Tensor::zero::<f32>(&[1, 1, STATE]).map_err(|err| Error::Vad(err.to_string()));
        Ok(ProbabilityStream {
            model: self.clone(),
            h: zero()?,
            c: zero()?,
            context: [0.0; CONTEXT_SAMPLES],
            pending: Vec::with_capacity(WINDOW_SAMPLES * 2),
            batch: Vec::with_capacity(BATCH_FRAMES * ROW),
            batch_frames: 0,
        })
    }
}

/// Turns a stream of samples into one speech probability per 512-sample frame.
pub(super) struct ProbabilityStream {
    model: SileroModel,
    h: Tensor,
    c: Tensor,
    context: [f32; CONTEXT_SAMPLES],
    pending: Vec<f32>,
    batch: Vec<f32>,
    batch_frames: usize,
}

impl ProbabilityStream {
    /// Feeds samples; probabilities of completed batches are appended to `out`.
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<f32>) -> Result<()> {
        let mut pending = std::mem::take(&mut self.pending);
        pending.extend_from_slice(samples);
        let whole = pending.len() - pending.len() % WINDOW_SAMPLES;
        for frame in pending[..whole].as_chunks::<WINDOW_SAMPLES>().0 {
            self.add_frame(frame);
            if self.batch_frames == BATCH_FRAMES {
                self.run_batch(out, true)?;
            }
        }
        pending.drain(..whole);
        self.pending = pending;
        Ok(())
    }

    /// Flushes buffered audio, zero-padding the final partial frame.
    pub fn finish(&mut self, out: &mut Vec<f32>) -> Result<()> {
        if !self.pending.is_empty() {
            let mut frame = [0.0; WINDOW_SAMPLES];
            frame[..self.pending.len()].copy_from_slice(&self.pending);
            self.pending.clear();
            self.add_frame(&frame);
        }
        if self.batch_frames > 0 {
            self.run_batch(out, false)?;
        }
        Ok(())
    }

    fn add_frame(&mut self, frame: &[f32]) {
        debug_assert_eq!(frame.len(), WINDOW_SAMPLES);
        self.batch.extend_from_slice(&self.context);
        self.batch.extend_from_slice(frame);
        self.context
            .copy_from_slice(&frame[WINDOW_SAMPLES - CONTEXT_SAMPLES..]);
        self.batch_frames += 1;
    }

    fn run_batch(&mut self, out: &mut Vec<f32>, keep_state: bool) -> Result<()> {
        let valid = self.batch_frames;
        // The network is causal across frames, so zero rows after the valid ones cannot
        // change their outputs. Only the final (partial) batch is padded.
        self.batch.resize(BATCH_FRAMES * ROW, 0.0);
        let run = || -> TractResult<TVec<TValue>> {
            let input = Tensor::from_shape(&[BATCH_FRAMES, ROW], &self.batch)?;
            self.model.plan.run(tvec!(
                input.into(),
                self.h.clone().into(),
                self.c.clone().into()
            ))
        };
        let outputs = run().map_err(|err| Error::Vad(format!("{err:#}")))?;
        let probs = outputs[0]
            .to_plain_array_view::<f32>()
            .map_err(|err| Error::Vad(err.to_string()))?;
        for &p in probs.iter().take(valid) {
            if !(0.0..=1.0).contains(&p) {
                return Err(Error::Vad(format!("invalid speech probability {p}")));
            }
            out.push(p);
        }
        if keep_state {
            self.h = outputs[1].clone().into_tensor();
            self.c = outputs[2].clone().into_tensor();
        }
        self.batch.clear();
        self.batch_frames = 0;
        Ok(())
    }
}
