//! [Wit.ai](https://wit.ai) speech recognition for tafrigh.
//!
//! [`WitRecognizer`] implements [`tafrigh::ChunkRecognizer`]: it sends each chunk as Ogg Opus
//! (encoded with ffmpeg) or WAV to Wit's `/dictation` (or `/speech`) endpoint, spreads requests
//! over several client access tokens with a per-token rate limit, retries transient failures
//! with backoff, and returns one utterance per final transcription Wit streams back, with word
//! timings.
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use tafrigh_wit::{WitConfig, WitRecognizer};
//!
//! let recognizer = WitRecognizer::new(WitConfig::new(["TOKEN_A", "TOKEN_B"])).await?;
//! println!("Wit app language: {}", recognizer.language());
//! # Ok(())
//! # }
//! ```

mod audio;
mod config;
mod error;
mod language;
mod recognizer;
mod response;
mod scheduler;

pub use audio::AudioFormat;
pub use config::{Endpoint, WitConfig};
pub use error::WitError;
pub use recognizer::WitRecognizer;
