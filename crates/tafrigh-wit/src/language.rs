//! App language lookup and language-specific text normalization.

use serde::Deserialize;
use tafrigh::ErrorChain;

use crate::{WitConfig, WitError};

#[derive(Deserialize)]
struct App {
    lang: Option<String>,
    #[serde(default)]
    is_app_for_token: bool,
}

/// Asks Wit which app each token belongs to and returns their shared language.
pub(crate) async fn detect(
    client: &reqwest::Client,
    config: &WitConfig,
) -> Result<String, WitError> {
    let url = format!(
        "{}/apps?limit=10000&v={}",
        config.api_url.trim_end_matches('/'),
        config.api_version
    );
    let lookups = config.tokens.iter().map(|token| {
        let request = client.get(&url).bearer_auth(token).send();
        async move {
            let response = request.await.map_err(WitError::Request)?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(WitError::status(status, &body, None));
            }
            let apps: Vec<App> = response.json().await.map_err(WitError::Request)?;
            apps.into_iter()
                .find(|app| app.is_app_for_token)
                .and_then(|app| app.lang)
                .ok_or_else(|| WitError::Language("no app found for a token".into()))
        }
    });
    let mut languages = futures::future::try_join_all(lookups)
        .await
        .map_err(|err| {
            WitError::Language(format!(
                "could not detect the Wit app language ({}); set it explicitly",
                ErrorChain(&err)
            ))
        })?;
    languages.sort();
    languages.dedup();
    match languages.as_slice() {
        [language] => Ok(language.clone()),
        _ => Err(WitError::Language(format!(
            "all tokens must belong to apps of the same language, found {}",
            languages.join(", ")
        ))),
    }
}

/// Adjusts recognized text for the app language (Arabic question marks for `ar`).
pub(crate) fn normalize(language: &str, text: &str) -> String {
    match language {
        "ar" => text.replace('?', "؟"),
        _ => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn arabic_question_marks() {
        assert_eq!(normalize("ar", "ما هذا?"), "ما هذا؟");
        assert_eq!(normalize("en", "what?"), "what?");
    }
}
