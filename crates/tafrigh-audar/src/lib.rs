//! Local Arabic recognition with Audar-ASR V1 Turbo and Flash through the bundled
//! `tafrigh-llama` runtime. Both the decoder and its BF16 audio encoder are required.
//!
//! [`AudarRecognizer`] implements [`tafrigh::ChunkRecognizer`] without word timings. Use
//! merged chunks of up to 29 seconds with 0.5 seconds of padding on each side, and disable
//! gap resends. The encoder accepts at most [`MAX_AUDIO_DURATION`] including padding.
//!
//! Clones share a bounded queue. A CPU worker prepares the next request's audio features
//! while one decoder sequence runs. Decoding uses the evaluated Arabic assistant prefill,
//! an empty system body, greedy sampling and a 440-token cap with repetition handling.
//!
//! The `cuda` feature enables NVIDIA GPUs; Metal is enabled on Apple by the native runtime.
//! Building requires `CMake`, a C++ compiler and libclang. Model weights are downloaded on
//! first use and are not bundled; see this crate's third-party license notes.
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use tafrigh_audar::{AudarConfig, AudarRecognizer};
//!
//! let recognizer = AudarRecognizer::load(AudarConfig::default()).await?;
//! println!("Using {}", recognizer.backend());
//! // Or: AudarConfig::local("decoder.gguf", "mmproj.gguf").
//! # Ok(())
//! # }
//! ```

mod config;
mod error;
mod model;
mod recognizer;
mod runtime;
mod text;

pub use config::{AudarConfig, Device, MAX_THREADS};
pub use error::AudarError;
pub use model::{
    DownloadProgress, FLASH_REVISION, ModelFiles, ModelMetadata, ModelSource, ModelVariant,
    TURBO_REVISION,
};
pub use recognizer::{AudarRecognizer, MAX_AUDIO_DURATION};
pub use tafrigh::models::default_model_dir;
