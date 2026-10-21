//! Audio decoding, padding and the chunks handed to recognizers.

mod decode;
mod padding;

use std::time::Duration;

pub use decode::{Decoder, PcmReader};
pub use padding::{Padding, PaddingFill};

/// Sample rate of all PCM handled by this crate (mono, signed 16-bit).
pub const SAMPLE_RATE: u32 = 16_000;

/// Duration of `samples` samples at [`SAMPLE_RATE`], in seconds.
pub(crate) fn samples_to_seconds(samples: usize) -> f64 {
    samples as f64 / f64::from(SAMPLE_RATE)
}

/// Number of samples in `seconds` at [`SAMPLE_RATE`], rounded; negative input gives 0.
pub(crate) fn seconds_to_samples(seconds: f64) -> usize {
    (seconds.max(0.0) * f64::from(SAMPLE_RATE)).round() as usize
}

/// Number of samples in `duration` at [`SAMPLE_RATE`], rounded.
pub(crate) fn duration_to_samples(duration: Duration) -> usize {
    seconds_to_samples(duration.as_secs_f64())
}

/// A piece of the source audio sent to a recognizer.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    /// Position of this chunk in the file, starting at 0.
    pub index: usize,
    /// Start of the speech in the source, in seconds.
    pub start: f64,
    /// End of the speech in the source, in seconds.
    pub end: f64,
    /// Seconds of padding before the speech in `samples`; recognizer timings are offset by it.
    pub lead_in: f64,
    /// Mono 16 kHz signed 16-bit samples, including any padding.
    pub samples: Vec<i16>,
}

impl AudioChunk {
    /// Duration of the audio actually sent, padding included.
    pub fn audio_duration(&self) -> Duration {
        Duration::from_secs_f64(samples_to_seconds(self.samples.len()))
    }

    /// The samples as a complete WAV file (PCM, mono, 16-bit, 16 kHz).
    pub fn wav_bytes(&self) -> Vec<u8> {
        let data_len = u32::try_from(self.samples.len() * 2).expect("chunk under 4 GiB");
        let mut wav = Vec::with_capacity(44 + data_len as usize);
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36 + data_len).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
        wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
        wav.extend_from_slice(&1u16.to_le_bytes()); // mono
        wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
        wav.extend_from_slice(&2u16.to_le_bytes()); // block align
        wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&data_len.to_le_bytes());
        wav.extend(self.samples.iter().flat_map(|sample| sample.to_le_bytes()));
        wav
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_describes_the_samples() {
        let chunk = AudioChunk {
            index: 0,
            start: 0.0,
            end: 0.0,
            lead_in: 0.0,
            samples: vec![1, -2, 3],
        };
        let wav = chunk.wav_bytes();
        assert_eq!(wav.len(), 44 + 6);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(wav[4..8].try_into().unwrap()), 36 + 6);
        assert_eq!(
            u32::from_le_bytes(wav[24..28].try_into().unwrap()),
            SAMPLE_RATE
        );
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 6);
        assert_eq!(&wav[44..], [1, 0, 0xfe, 0xff, 3, 0]);
    }
}
