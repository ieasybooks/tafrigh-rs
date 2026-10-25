//! Kaldi filterbanks used by the official `FireRed` frontend, followed by fixed CMVN.

use std::sync::Arc;

use rustfft::{Fft, FftPlanner, num_complex::Complex};

pub(super) const WINDOW_SAMPLES: usize = 400;
pub(super) const HOP_SAMPLES: usize = 160;
pub(super) const MEL_BINS: usize = 80;
const FFT_SIZE: usize = 512;

/// The original Kaldi binary double matrix: header, two rows, 81 columns.
const CMVN: &[u8; 1311] = include_bytes!("../../../assets/firered_cmvn.ark");

pub(super) struct Filterbank {
    fft: Arc<dyn Fft<f32>>,
    window: [f32; WINDOW_SAMPLES],
    filters: [Vec<(usize, f32)>; MEL_BINS],
    means: [f64; MEL_BINS],
    inverse_std: [f64; MEL_BINS],
}

impl Filterbank {
    pub fn new() -> Self {
        let window = std::array::from_fn(|i| {
            let phase = std::f64::consts::TAU / (WINDOW_SAMPLES - 1) as f64 * i as f64;
            (0.5 - 0.5 * phase.cos()).powf(0.85) as f32
        });
        let mel = |hz: f32| 1127.0 * (1.0 + hz / 700.0).ln();
        let low = mel(20.0);
        let step = (mel(8000.0) - low) / (MEL_BINS + 1) as f32;
        let filters = std::array::from_fn(|bin| {
            let left = low + bin as f32 * step;
            let center = low + (bin + 1) as f32 * step;
            let right = low + (bin + 2) as f32 * step;
            (0..FFT_SIZE / 2)
                .filter_map(|i| {
                    let frequency = mel(16000.0 / FFT_SIZE as f32 * i as f32);
                    if frequency <= left || frequency >= right {
                        return None;
                    }
                    let weight = if frequency <= center {
                        (frequency - left) / (center - left)
                    } else {
                        (right - frequency) / (right - center)
                    };
                    Some((i, weight))
                })
                .collect()
        });
        // Keep CMVN arithmetic in f64, as the publisher's numpy implementation does.
        let stat =
            |i: usize| f64::from_le_bytes(CMVN[15 + i * 8..15 + (i + 1) * 8].try_into().unwrap());
        let count = stat(MEL_BINS);
        let means = std::array::from_fn(|i| stat(i) / count);
        let inverse_std = std::array::from_fn(|i| {
            let variance = stat(MEL_BINS + 1 + i) / count - means[i] * means[i];
            1.0 / variance.max(1e-20).sqrt()
        });
        Self {
            fft: FftPlanner::new().plan_fft_forward(FFT_SIZE),
            window,
            filters,
            means,
            inverse_std,
        }
    }

    pub fn stream(self: &Arc<Self>) -> FeatureFrame {
        FeatureFrame {
            bank: Arc::clone(self),
            buffer: [Complex::new(0.0, 0.0); FFT_SIZE],
            scratch: vec![Complex::new(0.0, 0.0); self.fft.get_inplace_scratch_len()],
        }
    }
}

/// Per-stream FFT scratch, reused for every complete 25 ms window.
pub(super) struct FeatureFrame {
    bank: Arc<Filterbank>,
    buffer: [Complex<f32>; FFT_SIZE],
    scratch: Vec<Complex<f32>>,
}

impl FeatureFrame {
    pub fn extract(&mut self, samples: &[f32]) -> [f32; MEL_BINS] {
        debug_assert_eq!(samples.len(), WINDOW_SAMPLES);
        self.buffer.fill(Complex::new(0.0, 0.0));
        for (value, sample) in self.buffer.iter_mut().zip(samples) {
            // The model was trained on signed-16-bit PCM scale, not normalized samples.
            value.re = sample * 32768.0;
        }
        let mean = self.buffer[..WINDOW_SAMPLES]
            .iter()
            .map(|x| x.re)
            .sum::<f32>()
            / WINDOW_SAMPLES as f32;
        for value in &mut self.buffer[..WINDOW_SAMPLES] {
            value.re -= mean;
        }
        for i in (1..WINDOW_SAMPLES).rev() {
            self.buffer[i].re -= 0.97 * self.buffer[i - 1].re;
        }
        self.buffer[0].re -= 0.97 * self.buffer[0].re;
        for (value, window) in self.buffer.iter_mut().zip(&self.bank.window) {
            value.re *= window;
        }
        self.bank
            .fft
            .process_with_scratch(&mut self.buffer, &mut self.scratch);
        let power: [f32; FFT_SIZE / 2] = std::array::from_fn(|i| {
            let x = self.buffer[i];
            x.re * x.re + x.im * x.im
        });
        std::array::from_fn(|bin| {
            let energy: f32 = self.bank.filters[bin]
                .iter()
                .map(|&(i, weight)| power[i] * weight)
                .sum();
            let log_energy = energy.max(f32::EPSILON).ln();
            ((f64::from(log_energy) - self.bank.means[bin]) * self.bank.inverse_std[bin]) as f32
        })
    }
}
