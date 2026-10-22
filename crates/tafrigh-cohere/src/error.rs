use std::sync::Arc;

/// Errors loading or running a Cohere model.
#[derive(Debug, thiserror::Error)]
pub enum CohereError {
    /// Invalid settings or an unavailable requested device.
    #[error("invalid Cohere configuration: {0}")]
    Config(String),
    /// Model or fixture filesystem failure.
    #[error("Cohere file error: {0}")]
    Io(#[from] std::io::Error),
    /// The native model or session could not be loaded.
    #[error("could not load Cohere model: {0}")]
    Load(#[from] transcribe_cpp::Error),
    /// Native inference failed; includes truncated or repetition-stopped output.
    #[error("Cohere recognition failed: {0}")]
    Inference(#[source] Arc<transcribe_cpp::Error>),
    /// A blocking task panicked or was cancelled.
    #[error("Cohere task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
    /// The inference worker stopped before returning a result.
    #[error("the Cohere inference worker stopped before returning a result")]
    WorkerStopped,
    /// Network, HTTP status or response-stream failure while downloading a model.
    #[error("Cohere model download failed: {0}")]
    Download(#[from] reqwest::Error),
    /// The download did not match the pinned file length.
    #[error("Cohere model {filename} has {actual} downloaded bytes; expected {expected}")]
    DownloadSize {
        /// Model filename.
        filename: String,
        /// Pinned expected byte count.
        expected: u64,
        /// Received byte count.
        actual: u64,
    },
    /// The download's SHA-256 did not match the pinned digest.
    #[error(
        "Cohere model {filename} failed SHA-256 verification: expected {expected}, got {actual}"
    )]
    DownloadHash {
        /// Model filename.
        filename: String,
        /// Pinned digest.
        expected: String,
        /// Downloaded file digest.
        actual: String,
    },
}
