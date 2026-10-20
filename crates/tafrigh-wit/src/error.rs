use std::time::Duration;

use reqwest::StatusCode;

/// Longest response body kept in [`WitError::Status`].
const MAX_BODY_CHARS: usize = 500;

/// Errors from the Wit recognizer.
///
/// As with [`tafrigh::Error`], messages leave out the underlying cause; print them with
/// [`tafrigh::ErrorChain`] to include it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum WitError {
    /// The configuration is invalid.
    #[error("{0}")]
    Config(String),

    /// The HTTP client could not be built.
    #[error("could not create HTTP client")]
    Client(#[source] reqwest::Error),

    /// The request could not be sent or its response could not be read.
    #[error("request failed")]
    Request(#[source] reqwest::Error),

    /// Wit answered with an unsuccessful HTTP status.
    #[error("Wit returned HTTP {status}: {body}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// Start of the response body.
        body: String,
        /// Delay requested by a `Retry-After` header.
        retry_after: Option<Duration>,
    },

    /// Wit returned an error object.
    #[error("Wit error {code}: {message}")]
    Api {
        /// Wit's error code, e.g. `rate-limit`.
        code: String,
        /// Wit's error message.
        message: String,
    },

    /// The response could not be parsed.
    #[error("unexpected Wit response: {0}")]
    InvalidResponse(String),

    /// The apps' language could not be determined, or tokens belong to different languages.
    #[error("{0}")]
    Language(String),
}

impl WitError {
    pub(crate) fn status(status: StatusCode, body: &str, retry_after: Option<Duration>) -> Self {
        Self::Status {
            status: status.as_u16(),
            body: body.chars().take(MAX_BODY_CHARS).collect(),
            retry_after,
        }
    }

    /// Whether retrying the same request may succeed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Request(_) | Self::InvalidResponse(_) => true,
            // Wit sometimes reports a dropped upload as a missing body.
            Self::Status {
                status: 400, body, ..
            } => body_code(body).as_deref() == Some("no-body"),
            Self::Status { status, .. } => matches!(status, 408 | 425 | 429 | 500..=599),
            Self::Api { code, .. } => {
                let code = code.to_ascii_lowercase();
                code.contains("rate") || code.contains("timeout") || code.contains("unavailable")
            }
            Self::Config(_) | Self::Client(_) | Self::Language(_) => false,
        }
    }

    /// Delay the server asked for before retrying, if any.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// Whether Wit refused the request because a rate limit was reached.
    pub(crate) fn is_rate_limited(&self) -> bool {
        match self {
            Self::Status { status: 429, .. } => true,
            Self::Api { code, .. } => code.to_ascii_lowercase().contains("rate"),
            _ => false,
        }
    }

    /// Whether the rate limit is the account's, shared by all of its apps (tokens).
    pub(crate) fn is_account_limit(&self) -> bool {
        let message = match self {
            Self::Status { body, .. } => body_field(body, "error"),
            Self::Api { message, .. } => Some(message.clone()),
            _ => None,
        };
        message.is_some_and(|message| message.contains("for your account"))
    }
}

/// The `code` field of a JSON error body.
fn body_code(body: &str) -> Option<String> {
    body_field(body, "code")
}

/// A string field of a JSON error body.
fn body_field(body: &str, field: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value.get(field)?.as_str().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(code: u16, body: &str) -> WitError {
        WitError::status(StatusCode::from_u16(code).unwrap(), body, None)
    }

    #[test]
    fn classifies_rate_limits() {
        let app = status(
            429,
            r#"{"code": "rate-limit", "error": "You have exceeded the limit of 60 requests per minute for your app"}"#,
        );
        let account = status(
            429,
            r#"{"code": "rate-limit", "error": "You have exceeded the limit of 240 requests per minute for your account"}"#,
        );
        assert!(app.is_rate_limited() && !app.is_account_limit());
        assert!(account.is_rate_limited() && account.is_account_limit());
        assert!(!status(503, "").is_rate_limited());
        assert!(!status(429, "not json").is_account_limit());
    }

    #[test]
    fn retries_only_missing_body_client_errors() {
        assert!(
            status(
                400,
                r#"{"code": "no-body", "error": "No request body provided"}"#
            )
            .is_retryable()
        );
        assert!(!status(400, r#"{"code": "bad-request"}"#).is_retryable());
        assert!(!status(400, "no-body").is_retryable());
    }
}
