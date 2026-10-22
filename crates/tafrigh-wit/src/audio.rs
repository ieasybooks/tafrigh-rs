//! Audio formats for requests to Wit.

use std::path::Path;
use std::process::Stdio;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::WitError;

/// Format of the audio sent to Wit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AudioFormat {
    /// Ogg Opus at 24 kbps, encoded with ffmpeg's libopus: about a twelfth of the WAV size. In
    /// tests on Arabic speech, Wit was as accurate as with WAV and word timings matched.
    #[default]
    Opus,
    /// 16-bit PCM WAV, as tafrigh's JS client sends.
    Wav,
}

impl AudioFormat {
    /// The requests' `Content-Type`. Ogg Opus must be sent as `audio/opus`: as `audio/ogg`,
    /// Wit answers with empty transcriptions.
    pub(crate) fn content_type(self) -> &'static str {
        match self {
            Self::Opus => "audio/opus",
            Self::Wav => "audio/wav",
        }
    }
}

/// Encodes 16 kHz mono samples as Ogg Opus at 24 kbps with `ffmpeg`.
pub(crate) async fn opus(ffmpeg: &Path, samples: &[i16]) -> Result<Vec<u8>, WitError> {
    let failed = |reason: String| WitError::Encode(format!("{}: {reason}", ffmpeg.display()));
    let mut child = Command::new(ffmpeg)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .args(["-f", "s16le", "-ar", "16000", "-ac", "1", "-i", "pipe:0"])
        // Complexity 5 takes about half the CPU of the default 10 for the same size.
        .args(["-map_metadata", "-1", "-c:a", "libopus", "-b:a", "24k"])
        .args(["-compression_level", "5", "-f", "ogg", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| failed(err.to_string()))?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let pcm: Vec<u8> = samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect();
    // Dropping `stdin` after writing closes it, so ffmpeg finishes.
    let write = async move { stdin.write_all(&pcm).await };
    let (written, output) = tokio::join!(write, child.wait_with_output());
    let output = output.map_err(|err| failed(err.to_string()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(failed(match stderr.trim() {
            "" => output.status.to_string(),
            message => message.to_owned(),
        }));
    }
    written.map_err(|err| failed(err.to_string()))?;
    Ok(output.stdout)
}

/// Checks that `ffmpeg` can encode Opus, by encoding a moment of silence.
pub(crate) async fn check_opus(ffmpeg: &Path) -> Result<(), WitError> {
    opus(ffmpeg, &[0; 1600]).await.map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn encodes_ogg_opus() {
        let samples: Vec<i16> = (0..16_000)
            .map(|i| ((i % 64) * 256 - 8192) as i16)
            .collect();
        let ogg = opus(Path::new("ffmpeg"), &samples).await.unwrap();
        assert_eq!(&ogg[..4], b"OggS");
        assert!(ogg.windows(8).any(|window| window == b"OpusHead"));
        assert!(ogg.len() < samples.len() * 2 / 4, "{} bytes", ogg.len());
    }

    #[tokio::test]
    async fn missing_ffmpeg_cannot_encode() {
        let err = check_opus(Path::new("/nonexistent/ffmpeg"))
            .await
            .unwrap_err();
        assert!(matches!(err, WitError::Encode(_)), "{err:?}");
        assert!(err.to_string().contains("/nonexistent/ffmpeg"), "{err}");
    }
}
