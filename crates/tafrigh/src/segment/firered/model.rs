//! Official causal `FireRed` ONNX export, with its cache carried between inference batches.

use std::io::Cursor;
use std::sync::Arc;

use tract_onnx::prelude::*;

use super::features::{FeatureFrame, Filterbank, HOP_SAMPLES, MEL_BINS, WINDOW_SAMPLES};
use crate::{Error, Result};

static MODEL_ONNX: &[u8] = include_bytes!("../../../assets/fireredvad_stream_vad_with_cache.onnx");
pub(super) const BATCH_FRAMES: usize = 128;
const CACHE_SHAPE: [usize; 4] = [8, 1, 128, 19];

/// Loaded streaming `FireRedVAD` network and filterbank. Cheap to clone and share.
#[derive(Clone)]
pub struct FireRedModel {
    plan: Arc<TypedRunnableModel>,
    filterbank: Arc<Filterbank>,
}

impl std::fmt::Debug for FireRedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FireRedModel").finish_non_exhaustive()
    }
}

impl FireRedModel {
    /// Loads the bundled official streaming model and fixed normalization statistics.
    pub fn bundled() -> Result<Self> {
        let load = || -> TractResult<Arc<TypedRunnableModel>> {
            tract_onnx::onnx()
                // The export's symbolic intermediate annotations conflict with concrete inputs.
                .with_ignore_value_info(true)
                .model_for_read(&mut Cursor::new(MODEL_ONNX))?
                .with_input_fact(0, f32::fact([1, BATCH_FRAMES, MEL_BINS]).into())?
                .with_input_fact(1, f32::fact(CACHE_SHAPE).into())?
                .into_optimized()?
                .into_runnable()
        };
        let plan = load().map_err(|err| Error::Vad(err.context("loading FireRed model").into()))?;
        Ok(Self {
            plan,
            filterbank: Arc::new(Filterbank::new()),
        })
    }

    pub(super) fn stream(&self) -> Result<ProbabilityStream> {
        Ok(ProbabilityStream {
            model: self.clone(),
            cache: Tensor::zero::<f32>(&CACHE_SHAPE).map_err(|err| Error::Vad(err.into()))?,
            features: self.filterbank.stream(),
            pending: Vec::with_capacity(WINDOW_SAMPLES),
            batch: Vec::with_capacity(BATCH_FRAMES * MEL_BINS),
        })
    }
}

pub(super) struct ProbabilityStream {
    model: FireRedModel,
    cache: Tensor,
    features: FeatureFrame,
    pending: Vec<f32>,
    batch: Vec<f32>,
}

impl ProbabilityStream {
    pub fn push(&mut self, samples: &[f32], out: &mut Vec<f32>) -> Result<()> {
        let mut pending = std::mem::take(&mut self.pending);
        pending.extend_from_slice(samples);
        let mut offset = 0;
        while pending.len() - offset >= WINDOW_SAMPLES {
            self.batch.extend_from_slice(
                &self
                    .features
                    .extract(&pending[offset..offset + WINDOW_SAMPLES]),
            );
            offset += HOP_SAMPLES;
            if self.batch.len() == BATCH_FRAMES * MEL_BINS {
                self.run_batch(out, true)?;
            }
        }
        pending.drain(..offset);
        self.pending = pending;
        Ok(())
    }

    pub fn finish(&mut self, out: &mut Vec<f32>) -> Result<()> {
        // Kaldi snip_edges: a final incomplete window produces no feature frame.
        self.pending.clear();
        if !self.batch.is_empty() {
            self.run_batch(out, false)?;
        }
        Ok(())
    }

    fn run_batch(&mut self, out: &mut Vec<f32>, keep_state: bool) -> Result<()> {
        let valid = self.batch.len() / MEL_BINS;
        // Only EOF pads a batch. The graph is causal, so trailing rows cannot affect valid ones.
        self.batch.resize(BATCH_FRAMES * MEL_BINS, 0.0);
        let run = || -> TractResult<TVec<TValue>> {
            let input = Tensor::from_shape(&[1, BATCH_FRAMES, MEL_BINS], &self.batch)?;
            self.model
                .plan
                .run(tvec!(input.into(), self.cache.clone().into()))
        };
        let outputs = run().map_err(|err| Error::Vad(err.into()))?;
        let probs = outputs[0]
            .to_plain_array_view::<f32>()
            .map_err(|err| Error::Vad(err.into()))?;
        for &prob in probs.iter().take(valid) {
            if !(0.0..=1.0).contains(&prob) {
                return Err(Error::Vad(
                    format!("invalid speech probability {prob}").into(),
                ));
            }
            out.push(prob);
        }
        if keep_state {
            self.cache = outputs[1].clone().into_tensor();
        }
        self.batch.clear();
        Ok(())
    }
}
