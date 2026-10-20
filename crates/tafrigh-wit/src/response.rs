//! Parsing Wit transcription responses.
//!
//! Recent API versions stream JSON objects separated by `\r\n`. Each object carries `text`, a
//! `type` (`PARTIAL_TRANSCRIPTION`, `FINAL_TRANSCRIPTION`, and on `/speech` also
//! `FINAL_UNDERSTANDING`) and usually `is_final` and `speech.tokens` with word timings in
//! milliseconds. Wit emits one final transcription per utterance it hears. Older API versions
//! return a single object with `text`.

use serde_json::Value;
use tafrigh::{Utterance, Word};

use crate::WitError;

const FINAL_TRANSCRIPTION: &str = "FINAL_TRANSCRIPTION";

pub(crate) fn parse(body: &[u8]) -> Result<Vec<Utterance>, WitError> {
    let mut objects = Vec::new();
    for value in serde_json::Deserializer::from_slice(body).into_iter::<Value>() {
        let value = value.map_err(|err| WitError::InvalidResponse(err.to_string()))?;
        if let Some(error) = value.get("error") {
            return Err(WitError::Api {
                code: value
                    .get("code")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned(),
                message: error
                    .as_str()
                    .map_or_else(|| error.to_string(), str::to_owned),
            });
        }
        if value.get("text").is_some() {
            objects.push(value);
        }
    }

    // Prefer typed final transcriptions (one per utterance). On /speech this skips the
    // FINAL_UNDERSTANDING object, which repeats the text. Fall back to `is_final` objects, then
    // to the last object (legacy single-object responses).
    let typed: Vec<&Value> = objects
        .iter()
        .filter(|o| o.get("type").and_then(Value::as_str) == Some(FINAL_TRANSCRIPTION))
        .collect();
    let chosen = if typed.is_empty() {
        let finals: Vec<&Value> = objects
            .iter()
            .filter(|o| o.get("is_final").and_then(Value::as_bool) == Some(true))
            .collect();
        if finals.is_empty() {
            objects.last().into_iter().collect()
        } else {
            finals
        }
    } else {
        typed
    };

    Ok(chosen
        .into_iter()
        .map(|object| Utterance {
            text: object["text"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_owned(),
            words: object
                .pointer("/speech/tokens")
                .and_then(Value::as_array)
                .map(|tokens| tokens.iter().filter_map(word).collect())
                .unwrap_or_default(),
        })
        .filter(|utterance| !utterance.text.is_empty())
        .collect())
}

fn word(token: &Value) -> Option<Word> {
    Some(Word {
        text: token.get("token")?.as_str()?.to_owned(),
        start: token.get("start")?.as_f64()? / 1000.0,
        end: token.get("end")?.as_f64()? / 1000.0,
        confidence: token
            .get("confidence")
            .and_then(Value::as_f64)
            .map(|c| c as f32),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dictation_stream_with_utterances() {
        // Shape as consumed by tafrigh's JS client: is_final, speech, text, type.
        let body = concat!(
            r#"{"speech":{"confidence":0.5,"tokens":[]},"text":"hello","type":"PARTIAL_TRANSCRIPTION"}"#,
            "\r\n",
            r#"{"is_final":true,"speech":{"confidence":0.9,"tokens":["#,
            r#"{"token":"hello","start":120,"end":480,"confidence":0.95},"#,
            r#"{"token":"world","start":500,"end":900,"confidence":0.85}]},"#,
            r#""text":"hello world","type":"FINAL_TRANSCRIPTION"}"#,
            "\r\n",
            r#"{"speech":{"tokens":[]},"text":"ag","type":"PARTIAL_TRANSCRIPTION"}"#,
            "\r\n",
            r#"{"is_final":true,"speech":{"tokens":[{"token":"again","start":2000,"end":2400}]},"#,
            r#""text":"again","type":"FINAL_TRANSCRIPTION"}"#,
        );
        let utterances = parse(body.as_bytes()).unwrap();
        assert_eq!(utterances.len(), 2);
        assert_eq!(utterances[0].text, "hello world");
        assert_eq!(utterances[0].words.len(), 2);
        assert!((utterances[0].words[1].start - 0.5).abs() < 1e-9);
        assert_eq!(utterances[1].text, "again");
        assert_eq!(utterances[1].words[0].confidence, None);
    }

    #[test]
    fn speech_stream_skips_understanding() {
        let body = concat!(
            r#"{"is_final":true,"text":"turn on","type":"FINAL_TRANSCRIPTION","speech":{"tokens":[]}}"#,
            "\r\n",
            r#"{"is_final":true,"text":"turn on","type":"FINAL_UNDERSTANDING","intents":[]}"#,
        );
        let utterances = parse(body.as_bytes()).unwrap();
        assert_eq!(utterances.len(), 1);
        assert_eq!(utterances[0].text, "turn on");
    }

    #[test]
    fn untyped_finals_and_legacy_object() {
        let body = concat!(
            r#"{"text":"a","is_final":false}"#,
            "\r\n",
            r#"{"text":"a b","is_final":true}"#
        );
        assert_eq!(parse(body.as_bytes()).unwrap()[0].text, "a b");
        let legacy = parse(br#"{"text":" hi there ","intents":[],"entities":{}}"#).unwrap();
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].text, "hi there");
        assert_eq!(legacy[0].words, []);
    }

    #[test]
    fn empty_and_error_bodies() {
        assert_eq!(parse(b"").unwrap(), []);
        assert_eq!(
            parse(br#"{"text":"","type":"FINAL_TRANSCRIPTION"}"#).unwrap(),
            []
        );
        let err = parse(br#"{"error":"Too many requests","code":"rate-limit"}"#).unwrap_err();
        assert!(err.is_retryable(), "{err}");
        assert!(parse(br#"{"text": "trunc"#).is_err());
    }
}
