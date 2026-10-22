use std::fmt;
use std::num::NonZero;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use reqwest::header::{ACCEPT, CONTENT_TYPE, RETRY_AFTER};
use tafrigh::audio::AudioChunk;
use tafrigh::{BoxError, ChunkRecognizer, ErrorChain, Recognition, Utterance};
use tokio::sync::Semaphore;

use crate::scheduler::Scheduler;
use crate::{AudioFormat, WitConfig, WitError, audio, language, response};

/// Rate-limit refusals tolerated per chunk before they count as failed attempts.
const MAX_RATE_LIMIT_RETRIES: u32 = 30;

/// Wit.ai recognizer. Share it through an `Arc`; all state is internal.
pub struct WitRecognizer {
    client: reqwest::Client,
    config: WitConfig,
    language: String,
    scheduler: Scheduler,
    /// Opus encodings running at once, at most one per CPU core.
    encoders: Semaphore,
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
    /// Validates the configuration, checks that ffmpeg can encode Opus when Opus is sent, and
    /// looks up the apps' language when not given.
    pub async fn new(config: WitConfig) -> Result<Self, WitError> {
        config.validate()?;
        if config.audio_format == AudioFormat::Opus {
            audio::check_opus(&config.ffmpeg).await?;
        }
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
        let cores = std::thread::available_parallelism().map_or(1, NonZero::get);
        Ok(Self {
            client,
            config,
            language,
            scheduler,
            encoders: Semaphore::new(cores),
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
            .header(CONTENT_TYPE, self.config.audio_format.content_type())
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

    /// The chunk's audio in the configured format.
    async fn body(&self, chunk: &AudioChunk) -> Result<Vec<u8>, WitError> {
        match self.config.audio_format {
            AudioFormat::Wav => Ok(chunk.wav_bytes()),
            AudioFormat::Opus => {
                let _encoder = self
                    .encoders
                    .acquire()
                    .await
                    .expect("semaphore is never closed");
                audio::opus(&self.config.ffmpeg, &chunk.samples).await
            }
        }
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
        let body = self.body(chunk).await?;
        let timeout = self.config.timeout_for(chunk.audio_duration());
        let mut attempt = 0;
        let mut rate_limited = 0;
        let mut requests = 0;
        loop {
            let slot = self.scheduler.acquire().await;
            requests += 1;
            let load = self.scheduler.load();
            let sent = Instant::now();
            let result = self.request(slot.token(), body.clone(), timeout).await;
            tracing::debug!(
                "chunk #{} request {requests}: {:.2}s of audio in {} bytes on token {} ({load}): \
                 {} after {:.2?}",
                chunk.index,
                chunk.audio_duration().as_secs_f64(),
                body.len(),
                slot.token() + 1,
                outcome(&result),
                sent.elapsed()
            );
            let err = match result {
                Ok(utterances) => {
                    slot.succeeded();
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
            if err.is_overload()
                && let Some(limit) = slot.overloaded()
            {
                tracing::debug!(
                    "chunk #{}: Wit or the network seems overloaded ({}); sending at most \
                     {limit} request(s) at once",
                    chunk.index,
                    ErrorChain(&err)
                );
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

/// A short description of a request's outcome, for logs.
fn outcome(result: &Result<Vec<Utterance>, WitError>) -> String {
    match result {
        Ok(utterances) => format!("{} utterance(s)", utterances.len()),
        Err(WitError::Status { status, .. }) => format!("HTTP {status}"),
        Err(WitError::Request(err)) if err.is_timeout() => "timed out".into(),
        Err(_) => "failed".into(),
    }
}
