use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::{ACCEPT, CONTENT_TYPE, RETRY_AFTER};
use tafrigh::audio::AudioChunk;
use tafrigh::{BoxError, ChunkRecognizer, ErrorChain, Recognition, Utterance};

use crate::scheduler::Scheduler;
use crate::{WitConfig, WitError, language, response};

/// Rate-limit refusals tolerated per chunk before they count as failed attempts.
const MAX_RATE_LIMIT_RETRIES: u32 = 30;

/// Wit.ai recognizer. Share it through an `Arc`; all state is internal.
pub struct WitRecognizer {
    client: reqwest::Client,
    config: WitConfig,
    language: String,
    scheduler: Scheduler,
}

impl fmt::Debug for WitRecognizer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WitRecognizer")
            .field("config", &self.config)
            .field("language", &self.language)
            .finish_non_exhaustive()
    }
}

impl WitRecognizer {
    /// Validates the configuration and looks up the apps' language when not given.
    pub async fn new(config: WitConfig) -> Result<Self, WitError> {
        config.validate()?;
        let client = reqwest::Client::builder()
            .timeout(config.request_timeout)
            // Wit intermittently rejects concurrent HTTP/2 uploads as `no-body`.
            .http1_only()
            .user_agent(concat!("tafrigh/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(WitError::Client)?;
        let language = match &config.language {
            Some(language) => language.clone(),
            None => language::detect(&client, &config).await?,
        };
        let scheduler = Scheduler::new(
            config.tokens.len(),
            config.max_in_flight_per_token,
            config.request_interval()?,
        );
        Ok(Self {
            client,
            config,
            language,
            scheduler,
        })
    }

    /// Language of the Wit apps behind the tokens.
    pub fn language(&self) -> &str {
        &self.language
    }

    /// The configuration in effect.
    pub fn config(&self) -> &WitConfig {
        &self.config
    }

    async fn request(
        &self,
        token: usize,
        body: Vec<u8>,
        timeout: Duration,
    ) -> Result<Vec<Utterance>, WitError> {
        let url = format!(
            "{}/{}?v={}",
            self.config.api_url.trim_end_matches('/'),
            self.config.endpoint.path(),
            self.config.api_version
        );
        let response = self
            .client
            .post(url)
            .bearer_auth(&self.config.tokens[token])
            .timeout(timeout)
            .header(CONTENT_TYPE, "audio/wav")
            // Same request shape as tafrigh's JS client: versioned query plus this header.
            .header(ACCEPT, "application/vnd.wit.20200513+json")
            .body(body)
            .send()
            .await
            .map_err(WitError::Request)?;
        let status = response.status();
        let retry_after = response
            .headers()
            .get(RETRY_AFTER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let bytes = response.bytes().await.map_err(WitError::Request)?;
        if !status.is_success() {
            return Err(WitError::status(
                status,
                &String::from_utf8_lossy(&bytes),
                retry_after,
            ));
        }
        response::parse(&bytes)
    }

    fn backoff(&self, attempt: u32) -> Duration {
        let exponential = self
            .config
            .retry_base_delay
            .saturating_mul(1u32 << (attempt - 1).min(16))
            .min(self.config.retry_max_delay);
        exponential.mul_f64(rand::random_range(0.75..=1.25))
    }

    fn normalize(&self, mut utterance: Utterance) -> Utterance {
        utterance.text = language::normalize(&self.language, &utterance.text);
        for word in &mut utterance.words {
            word.text = language::normalize(&self.language, &word.text);
        }
        utterance
    }
}

#[async_trait]
impl ChunkRecognizer for WitRecognizer {
    async fn recognize(&self, chunk: &AudioChunk) -> Result<Recognition, BoxError> {
        let body = chunk.wav_bytes();
        let timeout = self.config.timeout_for(chunk.audio_duration());
        let mut attempt = 0;
        let mut rate_limited = 0;
        loop {
            let slot = self.scheduler.acquire().await;
            let err = match self.request(slot.token(), body.clone(), timeout).await {
                Ok(utterances) => {
                    return Ok(Recognition {
                        utterances: utterances.into_iter().map(|u| self.normalize(u)).collect(),
                    });
                }
                Err(err) => err,
            };
            // Rate limits slow the token (or the whole account) down instead of using up
            // attempts; the next try usually goes to another token.
            if err.is_rate_limited() && rate_limited < MAX_RATE_LIMIT_RETRIES {
                rate_limited += 1;
                let delay = err
                    .retry_after()
                    .unwrap_or_else(|| self.backoff(rate_limited));
                if err.is_account_limit() {
                    self.scheduler.hold_all(delay);
                } else {
                    self.scheduler.hold(slot.token(), delay);
                }
                tracing::debug!(
                    "chunk #{} rate-limited ({rate_limited}): {}; holding for {delay:.1?}",
                    chunk.index,
                    ErrorChain(&err)
                );
                continue;
            }
            attempt += 1;
            if !err.is_retryable() || attempt >= self.config.max_attempts {
                return Err(Box::new(err));
            }
            if let Some(delay) = err.retry_after() {
                self.scheduler.hold(slot.token(), delay);
            }
            drop(slot);
            let delay = self.backoff(attempt);
            tracing::debug!(
                "chunk #{} attempt {attempt} failed: {}; retrying in {delay:.1?}",
                chunk.index,
                ErrorChain(&err)
            );
            tokio::time::sleep(delay).await;
        }
    }

    fn max_audio_duration(&self) -> Option<Duration> {
        Some(self.config.audio_limit())
    }

    fn max_in_flight(&self) -> usize {
        self.config.tokens.len() * self.config.max_in_flight_per_token
    }
}
