use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub use tafrigh::models::{DownloadProgress, ModelMetadata};
use tafrigh::models::{default_model_dir, download};

use crate::AudarError;

/// Immutable official Turbo model revision.
pub const TURBO_REVISION: &str = "371428bea487c7aec82b27dc21f8d4324002e98e";
/// Immutable official Flash model revision.
pub const FLASH_REVISION: &str = "54274d39245beba38f30048128e231d4176bd91d";

/// Official Audar-ASR V1 decoder variants, each paired with its tier's BF16 audio encoder.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ModelVariant {
    /// turbo-bf16.
    TurboBf16,
    /// turbo-q8_0.
    #[default]
    TurboQ8,
    /// turbo-q4_k_m.
    TurboQ4KM,
    /// flash-bf16.
    FlashBf16,
    /// flash-q8_0.
    FlashQ8,
    /// flash-q4_k_m.
    FlashQ4KM,
}

impl ModelVariant {
    /// Every supported tier and decoder precision.
    pub const ALL: [Self; 6] = [
        Self::TurboBf16,
        Self::TurboQ8,
        Self::TurboQ4KM,
        Self::FlashBf16,
        Self::FlashQ8,
        Self::FlashQ4KM,
    ];

    /// Name accepted by the CLI.
    pub fn name(self) -> &'static str {
        match self {
            Self::TurboBf16 => "turbo-bf16",
            Self::TurboQ8 => "turbo-q8_0",
            Self::TurboQ4KM => "turbo-q4_k_m",
            Self::FlashBf16 => "flash-bf16",
            Self::FlashQ8 => "flash-q8_0",
            Self::FlashQ4KM => "flash-q4_k_m",
        }
    }

    /// Official model repository.
    pub fn repository(self) -> &'static str {
        if self.is_turbo() {
            "audarai/Audar-ASR-V1-Turbo"
        } else {
            "audarai/Audar-ASR-V1-Flash"
        }
    }

    /// Pinned repository revision for both files.
    pub fn revision(self) -> &'static str {
        if self.is_turbo() {
            TURBO_REVISION
        } else {
            FLASH_REVISION
        }
    }

    fn is_turbo(self) -> bool {
        matches!(self, Self::TurboBf16 | Self::TurboQ8 | Self::TurboQ4KM)
    }

    /// Expected decoder length and SHA-256 from the official Git LFS pointer.
    pub fn metadata(self) -> ModelMetadata {
        match self {
            Self::TurboBf16 => ModelMetadata {
                filename: "Audar-ASR-V1-Turbo.gguf",
                sha256: "35ccb4204f3d1abbe6b2511ff9449a2268f175e5bc0b176b0f065915db5ea793",
                size: 4_069_674_848,
            },
            Self::TurboQ8 => ModelMetadata {
                filename: "Audar-ASR-V1-Turbo-Q8_0.gguf",
                sha256: "0a91ab40f6a30db06c4186e2f621f504f4625ba6058e639cc09f1cbefded10d2",
                size: 2_165_034_848,
            },
            Self::TurboQ4KM => ModelMetadata {
                filename: "Audar-ASR-V1-Turbo-Q4_K_M.gguf",
                sha256: "c55e3c28225ef6e9b56906a6463af62d34ed417803c45f3b7b20f463af2e8cf4",
                size: 1_282_434_912,
            },
            Self::FlashBf16 => ModelMetadata {
                filename: "Audar-ASR-V1-Flash.gguf",
                sha256: "ab22f6426d510eb43c2975ae6b07ac15d9c414cd0b07ae537fb4986f75590b95",
                size: 1_198_178_144,
            },
            Self::FlashQ8 => ModelMetadata {
                filename: "Audar-ASR-V1-Flash-Q8_0.gguf",
                sha256: "1b01c707fcf162ef8e844a89f0833bd0d005933342e0c37fa8da164497a9fb38",
                size: 639_442_784,
            },
            Self::FlashQ4KM => ModelMetadata {
                filename: "Audar-ASR-V1-Flash-Q4_K_M.gguf",
                sha256: "8b0008ada2f47aaf7c82c10a115f6f964a70ebad71062d491db8c2f6ec8054ed",
                size: 396_700_512,
            },
        }
    }

    /// Required BF16 audio encoder, shared by every decoder precision of this tier.
    pub fn mmproj_metadata(self) -> ModelMetadata {
        if self.is_turbo() {
            ModelMetadata {
                filename: "mmproj-Audar-ASR-V1-Turbo.gguf",
                sha256: "190459e806938175711779847eb62ea609cd78b8d2ec06fb96a94d69ab37a9be",
                size: 641_773_856,
            }
        } else {
            ModelMetadata {
                filename: "mmproj-Audar-ASR-V1-Flash.gguf",
                sha256: "73f06fc82a009b4a9d6c825782a3676cb553402b3a5ecc27ae77a92caa6b7fa9",
                size: 378_575_424,
            }
        }
    }
}

impl fmt::Display for ModelVariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for ModelVariant {
    type Err = AudarError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|variant| variant.name() == value)
            .ok_or_else(|| {
                AudarError::Config(format!(
                    "unknown Audar variant {value:?}; expected {}",
                    Self::ALL.map(Self::name).join(", ")
                ))
            })
    }
}

/// A pinned official variant or an explicit pair of local GGUF files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSource {
    /// Download the decoder and BF16 audio encoder into the shared model cache.
    Variant(ModelVariant),
    /// Use both files in place, without downloads, copying or hash verification.
    Local {
        /// Audar decoder GGUF.
        decoder: PathBuf,
        /// Matching BF16 mmproj audio encoder GGUF.
        mmproj: PathBuf,
    },
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

impl fmt::Display for ModelSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Variant(variant) => variant.fmt(f),
            Self::Local { decoder, .. } => decoder.display().fmt(f),
        }
    }
}

/// Resolved decoder and its required audio encoder.
#[derive(Debug, Clone)]
pub struct ModelFiles {
    /// Decoder file.
    pub decoder: PathBuf,
    /// BF16 audio encoder file.
    pub mmproj: PathBuf,
}

impl ModelSource {
    /// Resolve both files. The progress callback reports each downloaded file separately;
    /// cached files of the expected size are reused without hashing, as with Cohere.
    pub async fn resolve(
        &self,
        model_dir: Option<&Path>,
        progress: &(dyn Fn(DownloadProgress) + Send + Sync),
    ) -> Result<ModelFiles, AudarError> {
        match self {
            Self::Local { decoder, mmproj } => Ok(ModelFiles {
                decoder: decoder.clone(),
                mmproj: mmproj.clone(),
            }),
            Self::Variant(variant) => {
                let root = match model_dir {
                    Some(directory) => directory.to_owned(),
                    None => default_model_dir()?,
                };
                let folder = if variant.is_turbo() {
                    "audar-asr-v1-turbo"
                } else {
                    "audar-asr-v1-flash"
                };
                let directory = root.join(folder);
                let base = format!(
                    "https://huggingface.co/{}/resolve/{}",
                    variant.repository(),
                    variant.revision()
                );
                let decoder = download(&directory, variant.metadata(), &base, progress).await?;
                let mmproj =
                    download(&directory, variant.mmproj_metadata(), &base, progress).await?;
                Ok(ModelFiles { decoder, mmproj })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_pin_both_files_and_the_default_is_turbo_q8() {
        assert_eq!(ModelVariant::default(), ModelVariant::TurboQ8);
        for variant in ModelVariant::ALL {
            assert_eq!(variant.name().parse::<ModelVariant>().unwrap(), variant);
            assert_eq!(variant.revision().len(), 40);
            for file in [variant.metadata(), variant.mmproj_metadata()] {
                assert_eq!(Path::new(file.filename).extension().unwrap(), "gguf");
                assert_eq!(file.sha256.len(), 64);
                assert!(file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()));
                assert!(file.size > 300_000_000);
            }
            assert_eq!(
                variant.mmproj_metadata().filename.contains("Turbo"),
                variant.is_turbo()
            );
        }
        for invalid in ["q8_0", "turbo-q3", "flash-f16"] {
            assert!(invalid.parse::<ModelVariant>().is_err());
        }
    }

    #[tokio::test]
    async fn local_pair_is_used_in_place_without_download_progress() {
        let source = ModelSource::Local {
            decoder: "decoder.gguf".into(),
            mmproj: "mmproj.gguf".into(),
        };
        let files = source
            .resolve(None, &|_| panic!("no download for local files"))
            .await
            .unwrap();
        assert_eq!(files.decoder, Path::new("decoder.gguf"));
        assert_eq!(files.mmproj, Path::new("mmproj.gguf"));
    }
}
