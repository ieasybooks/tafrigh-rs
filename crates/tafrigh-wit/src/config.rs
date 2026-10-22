use std::fmt;
use std::time::Duration;

use crate::WitError;

/// Which Wit endpoint transcribes audio.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Endpoint {
    /// Transcription only (what tafrigh's JS client uses).
    #[default]
    Dictation,
    /// Transcription plus intent and entity extraction.
    Speech,
}

impl Endpoint {
    pub(crate) fn path(self) -> &'static str {
        match self {
            Self::Dictation => "dictation",
            Self::Speech => "speech",
        }
    }

    /// Longest audio per request used when [`WitConfig::max_audio`] is unset: 60s for
    /// dictation (what tafrigh's JS client sends), 20s for speech.
    pub fn default_max_audio(self) -> Duration {
        match self {
            Self::Dictation => Duration::from_mins(1),
            Self::Speech => Duration::from_secs(20),
        }
    }
}

/// Configuration for [`crate::WitRecognizer`]. Start from [`WitConfig::new`] and adjust fields.
#[derive(Clone)]
pub struct WitConfig {
    /// Client access tokens of Wit apps; all must share a language.
    pub tokens: Vec<String>,
    /// The apps' language (e.g. `"ar"`). Looked up from Wit when `None`.
    pub language: Option<String>,
    /// Endpoint that transcribes the audio.
    pub endpoint: Endpoint,
    /// Base URL of the API, e.g. a proxy or a local mock.
    pub api_url: String,
    /// API version, sent as the `v` query parameter.
    pub api_version: String,
    /// Request starts allowed per second for each token.
    pub requests_per_second: f64,
    /// Most concurrent requests per token. Wit answers at about the pace of the audio, so
    /// keeping up with [`Self::requests_per_second`] takes about the rate times (seconds of
    /// audio per request + 2). The default, 18, suits 1 request a second with chunks up to 16s.
    /// Fewer requests run at once while Wit or the network seems overloaded: timeouts and server
    /// errors halve the requests being sent, and successes raise the limit again.
    pub max_in_flight_per_token: usize,
    /// Attempts per chunk, including the first.
    pub max_attempts: u32,
    /// Delay before the first retry; doubles on each further retry (with jitter).
    pub retry_base_delay: Duration,
    /// Upper bound for the retry delay.
    pub retry_max_delay: Duration,
    /// Longest a request may take, upload and response included.
    pub request_timeout: Duration,
    /// Time a request may take beyond twice its audio's duration; see
    /// [`WitConfig::timeout_for`].
    pub request_timeout_base: Duration,
    /// Longest audio per request; `None` uses [`Endpoint::default_max_audio`].
    pub max_audio: Option<Duration>,
}

impl WitConfig {
    /// Defaults for the given tokens; blank tokens are dropped.
    pub fn new<I, S>(tokens: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            tokens: tokens
                .into_iter()
                .map(|token| token.into().trim().to_owned())
                .filter(|token| !token.is_empty())
                .collect(),
            language: None,
            endpoint: Endpoint::default(),
            api_url: "https://api.wit.ai".into(),
            api_version: "20240304".into(),
            requests_per_second: 1.0,
            max_in_flight_per_token: 18,
            max_attempts: 5,
            retry_base_delay: Duration::from_secs(1),
            retry_max_delay: Duration::from_secs(30),
            request_timeout: Duration::from_mins(1),
            request_timeout_base: Duration::from_secs(15),
            max_audio: None,
        }
    }

    /// Longest audio per request in effect.
    pub fn audio_limit(&self) -> Duration {
        self.max_audio
            .unwrap_or_else(|| self.endpoint.default_max_audio())
    }

    /// Timeout for a request carrying `audio`: [`Self::request_timeout_base`] plus twice the
    /// audio's duration, at most [`Self::request_timeout`]. Wit answers at about the pace of
    /// the audio, so a request far past that has stalled and is better retried.
    pub fn timeout_for(&self, audio: Duration) -> Duration {
        self.request_timeout_base
            .saturating_add(audio.saturating_mul(2))
            .min(self.request_timeout)
    }

    pub(crate) fn request_interval(&self) -> Result<Duration, WitError> {
        Duration::try_from_secs_f64(1.0 / self.requests_per_second).map_err(|_| {
            WitError::Config("the request rate is too small to represent an interval".into())
        })
    }

    pub(crate) fn validate(&self) -> Result<(), WitError> {
        let fail = |message: &str| Err(WitError::Config(message.to_owned()));
        if self.tokens.is_empty() {
            return fail("at least one Wit client access token is required");
        }
        if !(self.requests_per_second.is_finite() && self.requests_per_second > 0.0) {
            return fail("requests per second must be positive");
        }
        self.request_interval()?;
        if self.max_in_flight_per_token == 0 || self.max_attempts == 0 {
            return fail("in-flight requests and attempts must be at least 1");
        }
        if self.audio_limit().is_zero() {
            return fail("the maximum audio per request must be positive");
        }
        Ok(())
    }
}

impl fmt::Debug for WitConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WitConfig")
            .field("tokens", &format_args!("[{} redacted]", self.tokens.len()))
            .field("language", &self.language)
            .field("endpoint", &self.endpoint)
            .field("api_url", &self.api_url)
            .field("api_version", &self.api_version)
            .field("requests_per_second", &self.requests_per_second)
            .field("max_in_flight_per_token", &self.max_in_flight_per_token)
            .field("max_attempts", &self.max_attempts)
            .field("retry_base_delay", &self.retry_base_delay)
            .field("retry_max_delay", &self.retry_max_delay)
            .field("request_timeout", &self.request_timeout)
            .field("request_timeout_base", &self.request_timeout_base)
            .field("max_audio", &self.max_audio)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_tokens_and_redacts_them() {
        let config = WitConfig::new([" secret-a ", "", "secret-b"]);
        assert_eq!(config.tokens, ["secret-a", "secret-b"]);
        let debug = format!("{config:?}");
        assert!(
            debug.contains("[2 redacted]") && !debug.contains("secret"),
            "{debug}"
        );
    }

    #[test]
    fn validates() {
        assert!(WitConfig::new(["t"]).validate().is_ok());
        assert!(WitConfig::new([""; 0]).validate().is_err());
        let mut config = WitConfig::new(["t"]);
        config.requests_per_second = 0.0;
        assert!(config.validate().is_err());
        config.requests_per_second = 1e-30;
        assert!(config.validate().is_err());
        let mut config = WitConfig::new(["t"]);
        config.endpoint = Endpoint::Speech;
        assert_eq!(config.audio_limit(), Duration::from_secs(20));
        config.max_audio = Some(Duration::ZERO);
        assert!(config.validate().is_err());
    }

    #[test]
    fn timeouts_scale_with_audio() {
        let config = WitConfig::new(["t"]);
        let timeout = |seconds| config.timeout_for(Duration::from_secs(seconds));
        assert_eq!(timeout(0), Duration::from_secs(15));
        assert_eq!(timeout(4), Duration::from_secs(23));
        assert_eq!(timeout(16), Duration::from_secs(47));
        assert_eq!(timeout(60), Duration::from_mins(1));
    }
}
