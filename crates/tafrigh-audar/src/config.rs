use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use crate::{AudarError, ModelSource};

/// Native ggml thread-mask capacity.
pub const MAX_THREADS: usize = 512;

/// Where the decoder and audio encoder run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Device {
    /// Try the first GPU and retry on CPU if loading fails or no GPU is available.
    #[default]
    Auto,
    /// Require CPU execution.
    Cpu,
    /// Require the first visible GPU; never fall back to CPU.
    Gpu,
    /// Require the zero-based Nth GPU for both decoder and audio encoder.
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
    type Err = AudarError;

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
                    AudarError::Config(format!(
                        "invalid Audar device {value:?}; expected auto, cpu, gpu or gpu:N"
                    ))
                }),
        }
    }
}

/// Settings for [`crate::AudarRecognizer`].
#[derive(Debug, Clone, Default)]
pub struct AudarConfig {
    /// Pinned model variant (Turbo `Q8_0` by default), or a local decoder and projector.
    pub model: ModelSource,
    /// Override the platform's `tafrigh/models` cache directory.
    pub model_dir: Option<PathBuf>,
    /// Compute device; defaults to automatic GPU selection with CPU fallback.
    pub device: Device,
    /// Threads for decoding, prefill and audio encoding. Zero uses available CPU parallelism,
    /// capped at [`MAX_THREADS`]. This is not a limit on all process threads.
    pub threads: usize,
}

impl AudarConfig {
    /// Select a model with Arabic decoding and automatic device selection.
    pub fn new(model: impl Into<ModelSource>) -> Self {
        Self {
            model: model.into(),
            ..Self::default()
        }
    }

    /// Use local GGUF decoder and audio projector files without downloading or copying them.
    pub fn local(decoder: impl Into<PathBuf>, mmproj: impl Into<PathBuf>) -> Self {
        Self::new(ModelSource::Local {
            decoder: decoder.into(),
            mmproj: mmproj.into(),
        })
    }

    /// Check settings without loading models or accessing the network.
    pub fn validate(&self) -> Result<(), AudarError> {
        if self.threads > MAX_THREADS {
            return Err(AudarError::Config(format!(
                "threads must not exceed {MAX_THREADS}"
            )));
        }
        if let ModelSource::Local { decoder, mmproj } = &self.model {
            for (name, file) in [("decoder", decoder), ("mmproj", mmproj)] {
                if file.extension().is_none_or(|extension| extension != "gguf") {
                    return Err(AudarError::Config(format!(
                        "the local {name} must be a .gguf file"
                    )));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_devices_threads_and_both_local_files() {
        for device in ["auto", "cpu", "gpu", "gpu:0", "gpu:12"] {
            let config = AudarConfig {
                device: device.parse().unwrap(),
                ..Default::default()
            };
            assert_eq!(config.device.to_string(), device);
            config.validate().unwrap();
        }
        for value in ["cuda", "gpu:", "gpu:-1", "gpu:+1", "gpu:one"] {
            assert!(value.parse::<Device>().is_err());
        }
        let mut config = AudarConfig::local("decoder.gguf", "mmproj.gguf");
        for threads in [0, 1, MAX_THREADS] {
            config.threads = threads;
            config.validate().unwrap();
        }
        config.threads = MAX_THREADS + 1;
        assert!(config.validate().is_err());
        config.threads = 0;
        assert!(
            AudarConfig::local("decoder.bin", "mmproj.gguf")
                .validate()
                .is_err()
        );
        assert!(
            AudarConfig::local("decoder.gguf", "mmproj.bin")
                .validate()
                .is_err()
        );
    }
}
