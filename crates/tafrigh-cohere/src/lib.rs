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
//! Select a pinned [`ModelVariant`] (`Q8_0` by default) or a local [`ModelSource::Local`] file.
//! Variants download on first use into [`default_model_dir`], with streaming SHA-256
//! verification before publication. [`CohereConfig::model_dir`] overrides the cache location;
//! [`CohereRecognizer::load_with_progress`] reports bytes for a progress bar.
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
mod model;
mod recognizer;

pub use config::{CohereConfig, Device, Language};
pub use error::CohereError;
pub use model::{
    DownloadProgress, MODEL_REVISION, ModelMetadata, ModelSource, ModelVariant, default_model_dir,
};
pub use recognizer::CohereRecognizer;
