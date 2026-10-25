//! Shared model cache and verified downloads, enabled by the `models` feature.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

/// Integrity metadata pinned from the repository's Git LFS pointers.
#[derive(Debug, Clone, Copy)]
pub struct ModelMetadata {
    /// Filename inside the model repository and cache folder.
    pub filename: &'static str,
    /// SHA-256 of the complete GGUF file, verified before cache publication.
    pub sha256: &'static str,
    /// Exact expected file length in bytes.
    pub size: u64,
}

/// Errors resolving the cache or downloading a pinned model file.
#[derive(Debug, thiserror::Error)]
pub enum ModelDownloadError {
    /// No platform cache directory is available.
    #[error("could not determine a cache directory; set model_dir")]
    CacheDirectory,
    /// Filesystem operation failed.
    #[error("model file error: {0}")]
    Io(#[from] std::io::Error),
    /// Network, HTTP status or response-stream failure.
    #[error("model download failed: {0}")]
    Download(#[from] reqwest::Error),
    /// Download length differs from the pinned size.
    #[error("model {filename} has {actual} downloaded bytes; expected {expected}")]
    DownloadSize {
        /// Model filename.
        filename: String,
        /// Pinned size.
        expected: u64,
        /// Received size.
        actual: u64,
    },
    /// Download digest differs from the pinned SHA-256.
    #[error("model {filename} failed SHA-256 verification: expected {expected}, got {actual}")]
    DownloadHash {
        /// Model filename.
        filename: String,
        /// Pinned digest.
        expected: String,
        /// Received digest.
        actual: String,
    },
}

/// Bytes received for one model download. `total` is the pinned expected file size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    /// Bytes received so far.
    pub downloaded: u64,
    /// Expected total bytes.
    pub total: u64,
}

/// Platform cache folder with `tafrigh/models` appended.
///
/// Linux respects `XDG_CACHE_HOME` and otherwise uses `~/.cache`; macOS uses
/// `~/Library/Caches`. No tafrigh-specific environment variable is read.
pub fn default_model_dir() -> Result<PathBuf, ModelDownloadError> {
    dirs::cache_dir()
        .map(|directory| directory.join("tafrigh/models"))
        .ok_or(ModelDownloadError::CacheDirectory)
}

async fn cached(path: &Path, size: u64) -> Result<bool, ModelDownloadError> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.is_file() && metadata.len() == size),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Downloads a pinned file, checking its length and SHA-256 before publishing it.
///
/// Existing files with the expected length are reused without hashing. Concurrent callers
/// share a filesystem lock; stale partial downloads are restarted. `base_url` names the
/// directory containing `metadata.filename`. The callback should return promptly.
pub async fn download(
    directory: &Path,
    metadata: ModelMetadata,
    base_url: &str,
    progress: &(dyn Fn(DownloadProgress) + Send + Sync),
) -> Result<PathBuf, ModelDownloadError> {
    let destination = directory.join(metadata.filename);
    if cached(&destination, metadata.size).await? {
        return Ok(destination);
    }
    tokio::fs::create_dir_all(directory).await?;
    let _lock = lock(&directory.join(format!("{}.lock", metadata.filename))).await?;
    // Another process may have completed the download while this one waited.
    if cached(&destination, metadata.size).await? {
        return Ok(destination);
    }
    let part = directory.join(format!("{}.part", metadata.filename));
    let result = download_part(&part, metadata, base_url, progress).await;
    if let Err(error) = result {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(error);
    }
    // An old wrong-sized file is not usable. Remove it for Windows rename semantics too.
    match tokio::fs::remove_file(&destination).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    tokio::fs::rename(&part, &destination).await?;
    Ok(destination)
}

/// Locks `path` for this process, waiting while another download holds it.
///
/// Waiting polls instead of blocking a thread: the download that holds the lock needs Tokio's
/// blocking threads for its file operations, so blocking one per waiter could leave it none.
async fn lock(path: &Path) -> Result<std::fs::File, ModelDownloadError> {
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .await?
        .into_std()
        .await;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
        }
    }
}

async fn download_part(
    part: &Path,
    metadata: ModelMetadata,
    base_url: &str,
    progress: &(dyn Fn(DownloadProgress) + Send + Sync),
) -> Result<(), ModelDownloadError> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(60))
        .user_agent(concat!("tafrigh/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let url = format!("{}/{}", base_url.trim_end_matches('/'), metadata.filename);
    let mut response = client.get(url).send().await?.error_for_status()?;
    // Restart a stale partial file; a completed file is published only after verification.
    let mut file = tokio::fs::File::create(part).await?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0;
    progress(DownloadProgress {
        downloaded,
        total: metadata.size,
    });
    while let Some(bytes) = response.chunk().await? {
        downloaded += bytes.len() as u64;
        if downloaded > metadata.size {
            return Err(ModelDownloadError::DownloadSize {
                filename: metadata.filename.into(),
                expected: metadata.size,
                actual: downloaded,
            });
        }
        hasher.update(&bytes);
        file.write_all(&bytes).await?;
        progress(DownloadProgress {
            downloaded,
            total: metadata.size,
        });
    }
    if downloaded != metadata.size {
        return Err(ModelDownloadError::DownloadSize {
            filename: metadata.filename.into(),
            expected: metadata.size,
            actual: downloaded,
        });
    }
    let mut actual = String::with_capacity(64);
    for byte in hasher.finalize() {
        write!(actual, "{byte:02x}").expect("writing to a String cannot fail");
    }
    if actual != metadata.sha256 {
        return Err(ModelDownloadError::DownloadHash {
            filename: metadata.filename.into(),
            expected: metadata.sha256.into(),
            actual,
        });
    }
    file.flush().await?;
    file.sync_all().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const TEST_MODEL: ModelMetadata = ModelMetadata {
        filename: "tiny.gguf",
        sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        size: 3,
    };

    #[tokio::test]
    async fn download_verifies_publishes_and_reports_progress() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/tiny.gguf"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"abc"))
            .expect(1)
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("tiny.gguf.part"), b"stale partial").unwrap();
        let updates = Mutex::new(Vec::new());
        let result = download(directory.path(), TEST_MODEL, &server.uri(), &|value| {
            updates.lock().unwrap().push(value);
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read(result).unwrap(), b"abc");
        assert!(!directory.path().join("tiny.gguf.part").exists());
        let updates = updates.into_inner().unwrap();
        assert_eq!(updates.first().unwrap().downloaded, 0);
        assert_eq!(
            updates.last().unwrap(),
            &DownloadProgress {
                downloaded: 3,
                total: 3
            }
        );
    }

    #[tokio::test]
    async fn hash_mismatch_deletes_the_partial_file() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"bad"))
            .expect(1)
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        let error = download(directory.path(), TEST_MODEL, &server.uri(), &|_| {})
            .await
            .unwrap_err();
        assert!(matches!(error, ModelDownloadError::DownloadHash { .. }));
        assert!(!directory.path().join("tiny.gguf").exists());
        assert!(!directory.path().join("tiny.gguf.part").exists());
    }

    #[tokio::test]
    async fn short_stream_is_never_published() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ab"))
            .expect(1)
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        let error = download(directory.path(), TEST_MODEL, &server.uri(), &|_| {})
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ModelDownloadError::DownloadSize {
                expected: 3,
                actual: 2,
                ..
            }
        ));
        assert!(!directory.path().join("tiny.gguf").exists());
        assert!(!directory.path().join("tiny.gguf.part").exists());
    }

    #[tokio::test]
    async fn cached_file_with_expected_size_skips_network_and_hashing() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        let expected = directory.path().join("tiny.gguf");
        std::fs::write(&expected, b"xyz").unwrap();
        assert_eq!(
            download(directory.path(), TEST_MODEL, &server.uri(), &|_| panic!(
                "cached file should not report a download"
            ))
            .await
            .unwrap(),
            expected
        );
    }

    #[test]
    fn waiting_for_the_lock_leaves_blocking_threads_to_the_download() {
        // With one blocking thread, a waiter that blocked on the lock would keep the
        // download holding it from writing its file.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(
                    ResponseTemplate::new(200)
                        .set_body_bytes(b"abc")
                        .set_delay(Duration::from_millis(200)),
                )
                .expect(1)
                .mount(&server)
                .await;
            let directory = tempfile::tempdir().unwrap().keep();
            let start = |directory: PathBuf, url: String| {
                tokio::spawn(async move { download(&directory, TEST_MODEL, &url, &|_| {}).await })
            };
            let first = start(directory.clone(), server.uri());
            // Start the second download once the first holds the lock and waits for the server.
            while server
                .received_requests()
                .await
                .unwrap_or_default()
                .is_empty()
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            let second = start(directory.clone(), server.uri());
            let both = tokio::time::timeout(Duration::from_secs(10), async {
                (first.await.unwrap(), second.await.unwrap())
            })
            .await
            .expect("the downloads deadlocked");
            assert_eq!(both.0.unwrap(), both.1.unwrap());
            std::fs::remove_dir_all(directory).unwrap();
        });
    }

    #[tokio::test]
    async fn concurrent_downloads_share_one_verified_file() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_bytes(b"abc")
                    .set_delay(Duration::from_millis(30)),
            )
            .expect(1)
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        let url = server.uri();
        let (first, second) = tokio::join!(
            download(directory.path(), TEST_MODEL, &url, &|_| {}),
            download(directory.path(), TEST_MODEL, &url, &|_| {})
        );
        assert_eq!(first.unwrap(), second.unwrap());
    }

    #[tokio::test]
    async fn http_error_keeps_existing_invalid_file_and_never_publishes() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503))
            .expect(1)
            .mount(&server)
            .await;
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("tiny.gguf"), b"invalid").unwrap();
        let error = download(directory.path(), TEST_MODEL, &server.uri(), &|_| {})
            .await
            .unwrap_err();
        assert!(matches!(error, ModelDownloadError::Download(_)));
        assert_eq!(
            std::fs::read(directory.path().join("tiny.gguf")).unwrap(),
            b"invalid"
        );
    }
}
