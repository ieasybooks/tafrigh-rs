use rand::{RngExt, SeedableRng, rngs::SmallRng};

use super::{samples_to_seconds, seconds_to_samples};

/// Extra audio added on both sides of every chunk before recognition.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Padding {
    /// Length added on each side, in seconds.
    pub seconds: f64,
    /// What the added audio contains.
    pub fill: PaddingFill,
}

/// Content of [`Padding`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PaddingFill {
    /// Digital silence.
    Silence,
    /// Uniform white noise.
    WhiteNoise {
        /// Peak amplitude in dBFS; 0 is full scale.
        dbfs: f64,
    },
}

impl Padding {
    /// Pads with `seconds` of silence on each side.
    pub fn silence(seconds: f64) -> Self {
        Self {
            seconds,
            fill: PaddingFill::Silence,
        }
    }

    /// Pads with `seconds` of white noise peaking at `dbfs` on each side.
    pub fn white_noise(seconds: f64, dbfs: f64) -> Self {
        Self {
            seconds,
            fill: PaddingFill::WhiteNoise { dbfs },
        }
    }

    /// Returns the padded samples and the lead-in duration in seconds. `seed` makes the noise
    /// reproducible.
    pub fn apply(&self, samples: &[i16], seed: u64) -> (Vec<i16>, f64) {
        let pad = seconds_to_samples(self.seconds);
        if pad == 0 {
            return (samples.to_vec(), 0.0);
        }
        let mut out = Vec::with_capacity(samples.len() + 2 * pad);
        let fill = |out: &mut Vec<i16>, rng: &mut SmallRng| match self.fill {
            PaddingFill::Silence => out.extend(std::iter::repeat_n(0, pad)),
            PaddingFill::WhiteNoise { dbfs } => {
                let peak =
                    (f64::from(i16::MAX) * 10f64.powf(dbfs / 20.0)).clamp(0.0, f64::from(i16::MAX));
                out.extend((0..pad).map(|_| (rng.random_range(-1.0..=1.0) * peak) as i16));
            }
        };
        let mut rng = SmallRng::seed_from_u64(seed);
        fill(&mut out, &mut rng);
        out.extend_from_slice(samples);
        fill(&mut out, &mut rng);
        (out, samples_to_seconds(pad))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pads_both_sides() {
        let (out, lead_in) = Padding::silence(0.001).apply(&[5, 6], 0);
        assert_eq!(
            out,
            [0; 16]
                .iter()
                .chain(&[5, 6])
                .chain(&[0; 16])
                .copied()
                .collect::<Vec<_>>()
        );
        assert!((lead_in - 0.001).abs() < 1e-12);

        let (noisy, _) = Padding::white_noise(0.01, -6.0).apply(&[0; 10], 7);
        let peak = noisy.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak > 0 && f64::from(peak) <= 32767.0 * 0.502, "{peak}");
        assert_eq!(noisy, Padding::white_noise(0.01, -6.0).apply(&[0; 10], 7).0);
    }
}
