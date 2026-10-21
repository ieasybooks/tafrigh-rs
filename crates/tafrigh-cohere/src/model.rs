use std::fmt;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::CohereError;

/// Immutable revision of the official transcribe.cpp GGUF distribution.
pub const MODEL_REVISION: &str = "11a54d6895f94cce78d0ece142b8e552e7822539";
const MODEL_REPOSITORY: &str = "handy-computer/cohere-transcribe-arabic-07-2026-gguf";
const MODEL_FOLDER: &str = "cohere-transcribe-arabic-07-2026";

/// Published Cohere GGUF variants. `Q8_0` is the default.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ModelVariant {
    /// Brain floating point, 16-bit weights.
    Bf16,
    /// Half-precision floating point weights.
    F16,
    /// Eight-bit quantization, the default balance of size and accuracy.
    #[default]
    Q8,
    /// Six-bit K quantization.
    Q6K,
    /// Mixed five-bit K quantization.
    Q5KM,
    /// Mixed four-bit K quantization, the smallest variant.
    Q4KM,
}

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

impl ModelVariant {
    /// All supported variants, in decreasing precision order.
    pub const ALL: [Self; 6] = [
        Self::Bf16,
        Self::F16,
        Self::Q8,
        Self::Q6K,
        Self::Q5KM,
        Self::Q4KM,
    ];

    /// Name accepted by model selection.
    pub fn name(self) -> &'static str {
        match self {
            Self::Bf16 => "bf16",
            Self::F16 => "f16",
            Self::Q8 => "q8_0",
            Self::Q6K => "q6_k",
            Self::Q5KM => "q5_k_m",
            Self::Q4KM => "q4_k_m",
        }
    }

    /// Pinned size and SHA-256 for this variant at [`MODEL_REVISION`].
    pub fn metadata(self) -> ModelMetadata {
        let (filename, sha256, size) = match self {
            Self::Bf16 => (
                "cohere-transcribe-arabic-07-2026-BF16.gguf",
                "4eade55ddc6ef7d9d1caeee0fa412d161212910c30ee2aa82d95725e0dfdf0a2",
                4_105_263_008,
            ),
            Self::F16 => (
                "cohere-transcribe-arabic-07-2026-F16.gguf",
                "c8a37345c10d1cb3b85d7d1779905e63051f39694bd6fdd5002d9434c09226fc",
                4_106_644_896,
            ),
            Self::Q8 => (
                "cohere-transcribe-arabic-07-2026-Q8_0.gguf",
                "910de5c9c57f9fd8a280e1701f9cd96f63768878c1ca9e4ecf23638a5e0fef16",
                2_410_655_136,
            ),
            Self::Q6K => (
                "cohere-transcribe-arabic-07-2026-Q6_K.gguf",
                "f0fc615d27da3236166cd68cf9968c3be7d257b953bbba3b2e10e5d9f3fd1ff4",
                1_972_524_448,
            ),
            Self::Q5KM => (
                "cohere-transcribe-arabic-07-2026-Q5_K_M.gguf",
                "55e61c9b047e36f0e084d367f6b0bfecc71a6a0527da6eea4f0c687f3584775f",
                1_770_270_112,
            ),
            Self::Q4KM => (
                "cohere-transcribe-arabic-07-2026-Q4_K_M.gguf",
                "4671080cd10ffc6166e1e1b36216f9b51e1455f170d55c59cff7b81332ad4621",
                1_558_162_848,
            ),
        };
        ModelMetadata {
            filename,
            sha256,
            size,
        }
    }
}

impl fmt::Display for ModelVariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for ModelVariant {
    type Err = CohereError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL.into_iter().find(|variant| variant.name() == value).ok_or_else(|| {
            CohereError::Config(format!("unknown Cohere variant {value:?}; expected bf16, f16, q8_0, q6_k, q5_k_m or q4_k_m"))
        })
    }
}

/// A downloaded variant or a caller-supplied GGUF file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSource {
    /// Download the pinned variant into the model cache on first use.
    Variant(ModelVariant),
    /// Use this file directly without downloading or copying it.
    Local(PathBuf),
}

impl Default for ModelSource {
    fn default() -> Self {
        Self::Variant(ModelVariant::default())
    }
}

impl From<ModelVariant> for ModelSource {
    fn from(variant: ModelVariant) -> Self {
        Self::Variant(variant)
    }
}

impl From<PathBuf> for ModelSource {
    fn from(path: PathBuf) -> Self {
        Self::Local(path)
    }
}

impl From<&Path> for ModelSource {
    fn from(path: &Path) -> Self {
        Self::Local(path.to_owned())
    }
}

impl From<&str> for ModelSource {
    fn from(value: &str) -> Self {
        value
            .parse::<ModelVariant>()
            .map_or_else(|_| Self::Local(value.into()), Self::Variant)
    }
}

impl From<String> for ModelSource {
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}

impl ModelSource {
    /// Resolves a local path or downloads and verifies a variant.
    ///
    /// `model_dir` overrides the platform model cache directory. The callback receives byte
    /// counts while downloading and should return promptly. An existing final file with the
    /// expected size is reused without hashing; only downloads are SHA-256 verified.
    pub async fn resolve(
        &self,
        model_dir: Option<&Path>,
        progress: &(dyn Fn(DownloadProgress) + Send + Sync),
    ) -> Result<PathBuf, CohereError> {
        match self {
            Self::Local(path) => Ok(path.clone()),
            Self::Variant(variant) => {
                let directory = match model_dir {
                    Some(directory) => directory.to_owned(),
                    None => default_model_dir()?,
                };
                let base_url =
                    format!("https://huggingface.co/{MODEL_REPOSITORY}/resolve/{MODEL_REVISION}");
                download(
                    &directory.join(MODEL_FOLDER),
                    variant.metadata(),
                    &base_url,
                    progress,
                )
                .await
            }
        }
    }
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
pub fn default_model_dir() -> Result<PathBuf, CohereError> {
    dirs::cache_dir()
        .map(|directory| directory.join("tafrigh/models"))
        .ok_or_else(|| {
            CohereError::Config(
                "could not determine a cache directory; set CohereConfig::model_dir".into(),
            )
        })
}

async fn cached(path: &Path, size: u64) -> Result<bool, CohereError> {
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.is_file() && metadata.len() == size),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

async fn download(
    directory: &Path,
    metadata: ModelMetadata,
    base_url: &str,
    progress: &(dyn Fn(DownloadProgress) + Send + Sync),
) -> Result<PathBuf, CohereError> {
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
async fn lock(path: &Path) -> Result<std::fs::File, CohereError> {
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
) -> Result<(), CohereError> {
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
            return Err(CohereError::DownloadSize {
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
        return Err(CohereError::DownloadSize {
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
        return Err(CohereError::DownloadHash {
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
        assert!(matches!(error, CohereError::DownloadHash { .. }));
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
            CohereError::DownloadSize {
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
        assert!(matches!(error, CohereError::Download(_)));
        assert_eq!(
            std::fs::read(directory.path().join("tiny.gguf")).unwrap(),
            b"invalid"
        );
    }

    #[test]
    fn variants_are_exact_and_pinned() {
        assert_eq!(ModelVariant::default(), ModelVariant::Q8);
        for variant in ModelVariant::ALL {
            assert_eq!(variant.name().parse::<ModelVariant>().unwrap(), variant);
            let metadata = variant.metadata();
            assert_eq!(Path::new(metadata.filename).extension().unwrap(), "gguf");
            assert_eq!(metadata.sha256.len(), 64);
            assert!(metadata.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
            assert!(metadata.size > 1_000_000_000);
        }
        assert!("q3".parse::<ModelVariant>().is_err());
    }
}
