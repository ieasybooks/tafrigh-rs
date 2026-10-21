use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use crate::{CohereError, ModelSource};

/// The model's two supported language hints.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Language {
    /// Arabic, including Arabic-English code switching.
    #[default]
    Arabic,
    /// English.
    English,
}

impl Language {
    /// The ISO language code sent to the model.
    pub fn code(self) -> &'static str {
        match self {
            Self::Arabic => "ar",
            Self::English => "en",
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl FromStr for Language {
    type Err = CohereError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "ar" => Ok(Self::Arabic),
            "en" => Ok(Self::English),
            _ => Err(CohereError::Config(format!(
                "unsupported Cohere language {value:?}; expected ar or en"
            ))),
        }
    }
}

/// Where the model runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Device {
    /// Try the build's GPU, falling back to CPU if GPU loading fails or none is available.
    #[default]
    Auto,
    /// Require CPU execution.
    Cpu,
    /// Require an available GPU; never fall back to CPU.
    Gpu,
    /// Require the zero-based Nth GPU in transcribe.cpp's enumerated GPU list.
    GpuIndex(usize),
}

impl fmt::Display for Device {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Cpu => f.write_str("cpu"),
            Self::Gpu => f.write_str("gpu"),
            Self::GpuIndex(index) => write!(f, "gpu:{index}"),
        }
    }
}

impl FromStr for Device {
    type Err = CohereError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::Auto),
            "cpu" => Ok(Self::Cpu),
            "gpu" => Ok(Self::Gpu),
            _ => value
                .strip_prefix("gpu:")
                .filter(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
                .and_then(|index| index.parse().ok())
                .map(Self::GpuIndex)
                .ok_or_else(|| {
                    CohereError::Config(format!(
                        "invalid Cohere device {value:?}; expected auto, cpu, gpu or gpu:N"
                    ))
                }),
        }
    }
}

/// Settings for [`crate::CohereRecognizer`].
#[derive(Debug, Clone)]
pub struct CohereConfig {
    /// A model variant (`Q8_0` by default) or a local GGUF file.
    pub model: ModelSource,
    /// Override the platform's `tafrigh/models` cache directory.
    pub model_dir: Option<PathBuf>,
    /// Language hint; Arabic by default.
    pub language: Language,
    /// Compute device; GPU when available, otherwise CPU by default.
    pub device: Device,
    /// Maximum concurrent requests in a GPU batch; 8 by default. CPU uses one at a time.
    pub batch_size: usize,
    /// Maximum time to collect a partial GPU batch; 10 ms by default.
    pub batch_wait: Duration,
    /// CPU threads used by transcribe.cpp. 0, the default, picks them: every core but two
    /// when the model runs on a GPU, leaving room for decoding and voice detection, and every
    /// core on the CPU.
    pub threads: usize,
}

impl Default for CohereConfig {
    fn default() -> Self {
        Self {
            model: ModelSource::default(),
            model_dir: None,
            language: Language::default(),
            device: Device::default(),
            batch_size: 8,
            batch_wait: Duration::from_millis(10),
            threads: 0,
        }
    }
}

impl CohereConfig {
    /// A variant name or local model with Arabic decoding and automatic device selection.
    pub fn new(model: impl Into<ModelSource>) -> Self {
        Self {
            model: model.into(),
            ..Self::default()
        }
    }

    pub(crate) fn validate(&self) -> Result<(), CohereError> {
        if self.batch_size == 0 || self.batch_size > 64 {
            return Err(CohereError::Config(
                "batch_size must be between 1 and 64".into(),
            ));
        }
        if self.threads > i32::MAX as usize {
            return Err(CohereError::Config(
                "threads must fit in a signed 32-bit integer".into(),
            ));
        }
        if self.batch_wait > Duration::from_secs(1) {
            return Err(CohereError::Config(
                "batch_wait must not exceed one second".into(),
            ));
        }
        if let ModelSource::Local(path) = &self.model
            && path.extension().is_none_or(|extension| extension != "gguf")
        {
            return Err(CohereError::Config(
                "the local model must be a .gguf file".into(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_supported_languages_and_devices() {
        for language in ["ar", "en"] {
            assert_eq!(language.parse::<Language>().unwrap().to_string(), language);
        }
        assert!("fr".parse::<Language>().is_err());
        for device in ["auto", "cpu", "gpu", "gpu:0", "gpu:12"] {
            assert_eq!(device.parse::<Device>().unwrap().to_string(), device);
        }
        for invalid in ["cuda", "gpu:", "gpu:-1", "gpu:+1", "gpu:one"] {
            assert!(invalid.parse::<Device>().is_err(), "{invalid}");
        }
    }

    #[test]
    fn validates_configuration_before_loading() {
        let mut config = CohereConfig::new("model.gguf");
        config.validate().unwrap();
        config.batch_size = 0;
        assert!(config.validate().is_err());
        config.batch_size = 65;
        assert!(config.validate().is_err());
        config.batch_size = 8;
        config.threads = usize::MAX;
        assert!(config.validate().is_err());
        config.threads = 0;
        config.model = "model.bin".into();
        assert!(config.validate().is_err());
    }
}
