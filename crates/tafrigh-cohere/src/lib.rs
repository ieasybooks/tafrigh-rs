//! Local [Cohere Transcribe Arabic 07-2026](https://huggingface.co/CohereLabs/cohere-transcribe-arabic-07-2026)
//! speech recognition through transcribe.cpp.
//!
//! [`CohereRecognizer`] implements [`tafrigh::ChunkRecognizer`]. It accepts mono 16 kHz
//! PCM chunks and returns punctuated Arabic or English text without word timings. Decoding,
//! Silero VAD, padding and output formats stay in tafrigh. Disable gap resends when constructing
//! a [`tafrigh::ChunkedTranscriber`]. Each recognized chunk becomes one segment.
//!
//! Concurrent requests share one model and session. GPU requests are collected into bounded
//! batches; CPU requests run individually. Loading and inference run on blocking threads.
//! The `cuda` feature enables NVIDIA GPUs; the upstream default enables Metal on Apple.
//! Building requires `CMake` and a C++ compiler. Model weights are not bundled.
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use tafrigh_cohere::{CohereConfig, CohereRecognizer};
//!
//! let recognizer = CohereRecognizer::load(CohereConfig::new("model.gguf")).await?;
//! println!("Using {}", recognizer.backend());
//! # Ok(())
//! # }
//! ```

mod config;
mod error;
mod recognizer;

pub use config::{CohereConfig, Device, Language};
pub use error::CohereError;
pub use recognizer::CohereRecognizer;
