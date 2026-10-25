use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use tafrigh::models::download;
pub use tafrigh::models::{DownloadProgress, ModelMetadata};

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

impl fmt::Display for ModelSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Variant(variant) => variant.fmt(f),
            Self::Local(path) => path.display().fmt(f),
        }
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
                .map_err(Into::into)
            }
        }
    }
}

/// Platform cache folder with `tafrigh/models` appended.
pub fn default_model_dir() -> Result<PathBuf, CohereError> {
    tafrigh::models::default_model_dir().map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

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
