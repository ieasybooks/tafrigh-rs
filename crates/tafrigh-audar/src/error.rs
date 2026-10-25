/// Errors resolving or running an Audar model.
#[derive(Debug, thiserror::Error)]
pub enum AudarError {
    /// Invalid settings or an unavailable requested device.
    #[error("invalid Audar configuration: {0}")]
    Config(String),
    /// Model or fixture filesystem failure.
    #[error("Audar file error: {0}")]
    Io(#[from] std::io::Error),
    /// Cache, download or integrity verification failure.
    #[error("Audar model download failed: {0}")]
    Download(#[from] tafrigh::models::ModelDownloadError),
    /// Native loading, audio preparation or inference failure.
    #[error("Audar inference failed: {0}")]
    Native(String),
    /// The inference worker stopped before returning a result.
    #[error("the Audar inference worker stopped before returning a result")]
    WorkerStopped,
}

pub(crate) fn native<E: std::fmt::Display>(context: &str) -> impl FnOnce(E) -> AudarError + '_ {
    move |error| AudarError::Native(format!("{context}: {error}"))
}
