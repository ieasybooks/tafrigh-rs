use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::SAMPLE_RATE;
use crate::{Error, Result};

/// Maximum time spent waiting for an `ffprobe` duration query.
const FFPROBE_TIMEOUT: Duration = Duration::from_secs(30);

/// Decodes media files to 16 kHz mono signed 16-bit PCM by streaming from `ffmpeg`.
#[derive(Debug, Clone)]
pub struct Decoder {
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    probe_timeout: Duration,
}

impl Default for Decoder {
    fn default() -> Self {
        Self {
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            probe_timeout: FFPROBE_TIMEOUT,
        }
    }
}

impl Decoder {
    /// Uses the given `ffmpeg` and `ffprobe` executables instead of the ones on `PATH`.
    pub fn new(ffmpeg: impl Into<PathBuf>, ffprobe: impl Into<PathBuf>) -> Self {
        Self {
            ffmpeg: ffmpeg.into(),
            ffprobe: ffprobe.into(),
            probe_timeout: FFPROBE_TIMEOUT,
        }
    }

    /// Sets the ffprobe deadline for crate-internal tests.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_probe_timeout(mut self, timeout: Duration) -> Self {
        self.probe_timeout = timeout;
        self
    }

    /// Checks that `ffmpeg` can be run, failing with [`Error::ToolUnavailable`] otherwise.
    ///
    /// `ffprobe` is optional (it only provides durations for progress percentages) and is not
    /// checked.
    pub fn check(&self) -> Result<()> {
        let status = Command::new(&self.ffmpeg)
            .arg("-version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        Error::check_tool("ffmpeg", &self.ffmpeg, "-version", status)
    }

    /// Starts decoding `path`. Samples are read incrementally from the returned reader.
    ///
    /// Fails with [`Error::ToolUnavailable`] when `ffmpeg` cannot be started.
    pub fn open(&self, path: &Path) -> Result<PcmReader> {
        let mut child = Command::new(&self.ffmpeg)
            .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(path)
            .args(["-vn", "-sn", "-dn", "-ac", "1", "-ar"])
            .arg(SAMPLE_RATE.to_string())
            .args(["-f", "s16le", "-acodec", "pcm_s16le", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| Error::tool("ffmpeg", &self.ffmpeg, err))?;

        let stdout = child.stdout.take().expect("stdout is piped");
        let mut stderr = child.stderr.take().expect("stderr is piped");
        let stderr = std::thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            text
        });

        Ok(PcmReader {
            path: path.to_owned(),
            child,
            stdout,
            stderr: Some(stderr),
            carry: None,
            bytes: Vec::new(),
            finished: false,
        })
    }

    /// Container duration in seconds, if `ffprobe` is available and the file reports one.
    pub fn probe_duration(&self, path: &Path) -> Option<f64> {
        let mut child = Command::new(&self.ffprobe)
            .args(["-v", "error", "-show_entries", "format=duration"])
            .args(["-of", "default=noprint_wrappers=1:nokey=1"])
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut stdout = child.stdout.take().expect("stdout is piped");
        let reader = std::thread::spawn(move || {
            let mut output = Vec::new();
            let _ = stdout.read_to_end(&mut output);
            output
        });
        let deadline = Instant::now() + self.probe_timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        let _ = child.kill();
                        let _ = child.wait();
                        drop(reader);
                        return None;
                    }
                    std::thread::sleep(remaining.min(Duration::from_millis(10)));
                }
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    drop(reader);
                    return None;
                }
            }
        };
        let output = reader.join().ok()?;
        if !status.success() {
            return None;
        }
        String::from_utf8_lossy(&output)
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
    }
}

/// Streaming PCM reader over a running `ffmpeg` process. Kills the process when dropped early.
///
/// Reading is blocking; use it from a blocking thread.
pub struct PcmReader {
    path: PathBuf,
    child: Child,
    stdout: ChildStdout,
    stderr: Option<JoinHandle<String>>,
    carry: Option<u8>,
    bytes: Vec<u8>,
    finished: bool,
}

impl PcmReader {
    /// Reads up to `max` samples into `out` (cleared first). Returns `Ok(0)` at end of stream.
    pub fn read(&mut self, out: &mut Vec<i16>, max: usize) -> Result<usize> {
        out.clear();
        if self.finished {
            return Ok(0);
        }
        let want = max.max(1) * 2;
        self.bytes.resize(want, 0);
        loop {
            let mut filled = 0;
            if let Some(byte) = self.carry.take() {
                self.bytes[0] = byte;
                filled = 1;
            }
            let read = self.stdout.read(&mut self.bytes[filled..]).map_err(|err| {
                Error::io(format!("reading audio of {}", self.path.display()), err)
            })?;
            filled += read;
            if read == 0 {
                self.finished = true;
                self.finish_process()?;
                return Ok(0);
            }
            let whole = filled / 2 * 2;
            if filled > whole {
                self.carry = Some(self.bytes[whole]);
            }
            out.extend(
                self.bytes[..whole]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|&pair| i16::from_le_bytes(pair)),
            );
            if !out.is_empty() {
                return Ok(out.len());
            }
        }
    }

    fn finish_process(&mut self) -> Result<()> {
        let status = self
            .child
            .wait()
            .map_err(|err| Error::io("waiting for ffmpeg", err))?;
        let stderr = self
            .stderr
            .take()
            .and_then(|handle| handle.join().ok())
            .unwrap_or_default();
        if status.success() {
            return Ok(());
        }
        let message = stderr
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .map_or("ffmpeg failed", str::trim)
            .to_owned();
        Err(Error::Decode {
            path: self.path.clone(),
            message: format!("{message} ({status})"),
        })
    }
}

impl Drop for PcmReader {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use super::*;

    fn kind(result: Result<impl Sized>) -> Option<ErrorKind> {
        match result {
            Err(Error::ToolUnavailable {
                tool: "ffmpeg",
                source,
                ..
            }) => Some(source.kind()),
            _ => None,
        }
    }

    #[test]
    fn missing_or_broken_ffmpeg_is_unavailable() {
        let missing = Decoder::new("/nonexistent/ffmpeg", "/nonexistent/ffprobe");
        assert_eq!(kind(missing.check()), Some(ErrorKind::NotFound));
        assert_eq!(
            kind(missing.open(Path::new("a.mp3"))),
            Some(ErrorKind::NotFound)
        );
        assert_eq!(missing.probe_duration(Path::new("a.mp3")), None);

        #[cfg(unix)]
        {
            let broken = Decoder::new("false", "false");
            assert_eq!(kind(broken.check()), Some(ErrorKind::Other));
        }
    }

    #[cfg(unix)]
    #[test]
    fn probe_timeout_returns_without_waiting_for_ffprobe() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "tafrigh-fake-ffprobe-{}-{}.sh",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::write(&path, b"#!/bin/sh\nexec sleep 10\n").unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();

        let decoder = Decoder::new("true", &path).with_probe_timeout(Duration::from_millis(200));
        let started = Instant::now();
        assert_eq!(decoder.probe_duration(Path::new("audio.mp3")), None);
        assert!(started.elapsed() < Duration::from_secs(2));
        std::fs::remove_file(path).unwrap();
    }
}
