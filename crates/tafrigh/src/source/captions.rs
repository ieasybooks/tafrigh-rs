use serde::Deserialize;
use serde_json::Value;

use crate::{Error, Result, Segment, Word};

/// Whether `url` is an HTTP(S) [YouTube](https://www.youtube.com) URL, including short links and subdomains.
pub fn is_youtube_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return false;
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = host.to_ascii_lowercase();
    host == "youtu.be" || host == "youtube.com" || host.ends_with(".youtube.com")
}

/// Whether `language` names one subtitle track, rather than a yt-dlp language pattern.
pub fn is_subtitle_language(language: &str) -> bool {
    language != "all"
        && language
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
        && language
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
}

/// Keep exactly one JSON3 track: manual captions first, otherwise original automatic captions.
/// The saved info is consumed by yt-dlp, which retains responsibility for HTTP and cookies.
pub(super) fn select_track(info: &mut Value, language: &str) -> Result<()> {
    if info.get("extractor_key").and_then(Value::as_str) != Some("Youtube") {
        return Err(Error::Source(
            "the youtube transcriber requires a YouTube video".into(),
        ));
    }
    let manual = info["subtitles"][language]
        .as_array()
        .filter(|tracks| !tracks.is_empty());
    let automatic = &info["automatic_captions"];
    let tracks = manual.or_else(|| {
        automatic[format!("{language}-orig")]
            .as_array()
            .or_else(|| automatic[language].as_array())
    });
    let track = tracks
        .and_then(|tracks| {
            tracks.iter().find(|track| {
                track["ext"] == "json3"
                    && track["url"].as_str().is_some_and(|url| {
                        // Translated captions carry a tlang query parameter. Do not mistake them
                        // for speech in the requested language, even under an unsuffixed key.
                        !url.split_once('?').is_some_and(|(_, query)| {
                            query.split('&').any(|part| part.starts_with("tlang="))
                        })
                    })
            })
        })
        .cloned()
        .ok_or_else(|| {
            Error::Source(format!(
                "no original captions available for language {language}"
            ))
        })?;
    info["subtitles"] = serde_json::json!({language: [track]});
    info["automatic_captions"] = serde_json::json!({});
    if let Some(info) = info.as_object_mut() {
        info.remove("requested_subtitles");
    }
    Ok(())
}

#[derive(Deserialize)]
struct Captions {
    events: Vec<CaptionEvent>,
}

#[derive(Deserialize)]
struct CaptionEvent {
    #[serde(rename = "tStartMs")]
    start: Option<u64>,
    #[serde(rename = "dDurationMs")]
    duration: Option<u64>,
    #[serde(rename = "aAppend", default)]
    append: u8,
    #[serde(rename = "wWinId", default)]
    window: u64,
    #[serde(default)]
    segs: Vec<CaptionText>,
}

#[derive(Deserialize)]
struct CaptionText {
    utf8: String,
    #[serde(rename = "tOffsetMs")]
    offset: Option<i64>,
}

/// Timed JSON3 fragments are usable as words only when each fragment contains one word.
/// Missing internal offsets or multiword fragments must not create invented word boundaries.
fn timed_words(parts: &[CaptionText], start: u64, end: f64) -> Vec<Word> {
    if !parts.iter().any(|part| part.offset.is_some()) {
        return Vec::new();
    }
    let mut words: Vec<Word> = Vec::new();
    for part in parts.iter().filter(|part| !part.utf8.trim().is_empty()) {
        let text = part.utf8.trim();
        let Some(offset) = part.offset.or_else(|| words.is_empty().then_some(0)) else {
            return Vec::new();
        };
        let Ok(offset) = u64::try_from(offset) else {
            return Vec::new();
        };
        let Some(time) = start.checked_add(offset) else {
            return Vec::new();
        };
        let time = time as f64 / 1000.0;
        if text.split_whitespace().count() != 1 || time >= end {
            return Vec::new();
        }
        if let Some(previous) = words.last_mut() {
            if time <= previous.start {
                return Vec::new();
            }
            previous.end = time;
        }
        words.push(Word {
            start: time,
            end,
            text: text.to_owned(),
            confidence: None,
        });
    }
    words
}

/// Read cue text and timing directly; JSON3 avoids VTT's rolling display markup.
pub(super) fn parse_json3(bytes: &[u8]) -> Result<Vec<Segment>> {
    let captions: Captions = serde_json::from_slice(bytes)
        .map_err(|err| Error::Source(format!("invalid YouTube JSON3 captions: {err}")))?;
    let mut cues: Vec<(u64, Segment)> = Vec::new();
    for event in captions.events {
        let text: String = event.segs.iter().map(|part| part.utf8.as_str()).collect();
        let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            continue; // Window definitions and newline-only display events carry no speech.
        }
        let start = event
            .start
            .ok_or_else(|| Error::Source("caption is missing its start time".into()))?;
        let end = event
            .duration
            .and_then(|duration| start.checked_add(duration));
        if event.duration.is_some() && end.is_none_or(|end| end <= start) {
            return Err(Error::Source("caption has no valid duration".into()));
        }
        if event.append == 1 {
            let previous = cues
                .iter_mut()
                .rev()
                .find(|(window, _)| *window == event.window)
                .map(|(_, cue)| cue)
                .ok_or_else(|| Error::Source("caption append event has no preceding cue".into()))?;
            if (start as f64 / 1000.0) < previous.start {
                return Err(Error::Source(
                    "caption append event precedes its cue".into(),
                ));
            }
            previous.text.push(' ');
            previous.text.push_str(&text);
            if let Some(end) = end {
                previous.end = previous.end.max(end as f64 / 1000.0);
            } else if start as f64 / 1000.0 >= previous.end {
                return Err(Error::Source("caption append event has no end time".into()));
            }
            let words = timed_words(&event.segs, start, previous.end);
            match (previous.words.last_mut(), words.first()) {
                (Some(last), Some(first)) if first.start > last.start => {
                    last.end = first.start;
                    previous.words.extend(words);
                }
                _ => previous.words.clear(),
            }
        } else {
            let end = end
                .filter(|end| *end > start)
                .ok_or_else(|| Error::Source("caption has no valid duration".into()))?;
            let mut segment = Segment::new(start as f64 / 1000.0, end as f64 / 1000.0, text);
            segment.words = timed_words(&event.segs, start, segment.end);
            cues.push((event.window, segment));
        }
    }
    let mut segments: Vec<_> = cues.into_iter().map(|(_, cue)| cue).collect();
    segments.sort_by(|a, b| a.start.total_cmp(&b.start));
    // Remove only exact duplicate cues; repeated speech at another time must survive.
    segments.dedup();
    if segments.is_empty() {
        return Err(Error::Source("YouTube returned empty captions".into()));
    }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn selects_manual_then_original_automatic_captions() {
        let track = |name: &str| json!([{"ext":"json3", "url":format!("https://www.youtube.com/api/timedtext?lang=ar&name={name}")}]);
        let info = json!({"extractor_key":"Youtube", "subtitles":{"ar":track("manual")},
            "automatic_captions":{"ar-orig":track("automatic")}});
        let mut manual = info.clone();
        select_track(&mut manual, "ar").unwrap();
        assert_eq!(manual["subtitles"]["ar"], track("manual"));
        assert_eq!(manual["automatic_captions"], json!({}));
        let mut auto = info;
        auto["subtitles"] = json!({});
        select_track(&mut auto, "ar").unwrap();
        assert_eq!(auto["subtitles"]["ar"], track("automatic"));
    }

    #[test]
    fn rejects_translations_missing_tracks_and_other_extractors() {
        for mut info in [
            json!({"extractor_key":"Youtube", "automatic_captions":{"ar":[{"ext":"json3", "url":"https://www.youtube.com/api/timedtext?lang=en&tlang=ar"}]}}),
            json!({"extractor_key":"Youtube", "subtitles":{}}),
            json!({"extractor_key":"Vimeo"}),
        ] {
            assert!(select_track(&mut info, "ar").is_err());
        }
    }

    #[test]
    fn parses_unicode_fragments_and_append_events_without_losing_repeated_speech() {
        let segments = parse_json3(
            br#"{"events":[
            {"tStartMs":0,"wWinId":1},
            {"tStartMs":1000,"dDurationMs":1500,"segs":[{"utf8":"hello "},{"utf8":"world"}]},
            {"tStartMs":1200,"aAppend":1,"segs":[{"utf8":"again"}]},
            {"tStartMs":2500,"segs":[{"utf8":"\n"}]},
            {"tStartMs":3000,"dDurationMs":1000,"segs":[{"utf8":"hello world again"}]},
            {"tStartMs":4500,"dDurationMs":500,"segs":[{"utf8":"\u0645\u0631\u062d\u0628\u0627"}]}
        ]}"#,
        )
        .unwrap();
        assert_eq!(
            segments,
            [
                Segment::new(1.0, 2.5, "hello world again"),
                Segment::new(3.0, 4.0, "hello world again"),
                Segment::new(4.5, 5.0, "مرحبا")
            ]
        );
        assert!(segments.iter().all(|segment| segment.words.is_empty()));
    }

    #[test]
    fn rejects_empty_malformed_and_untimed_captions() {
        for json in [
            "{}",
            "not json",
            r#"{"events":[]}"#,
            r#"{"events":[{"tStartMs":0,"segs":[{"utf8":"hello"}]}]}"#,
            r#"{"events":[{"tStartMs":0,"dDurationMs":0,"segs":[{"utf8":"hello"}]}]}"#,
        ] {
            assert!(parse_json3(json.as_bytes()).is_err(), "{json}");
        }
    }

    #[test]
    fn derives_word_ends_from_next_start_and_caption_end() {
        let segments = parse_json3(
            br#"{"events":[{"tStartMs":1000,"dDurationMs":2000,
            "segs":[{"utf8":"hello"},{"utf8":" world","tOffsetMs":700}]}]}"#,
        )
        .unwrap();
        assert_eq!(segments[0].text, "hello world");
        assert_eq!(
            segments[0].words,
            [
                Word {
                    start: 1.0,
                    end: 1.7,
                    text: "hello".into(),
                    confidence: None
                },
                Word {
                    start: 1.7,
                    end: 3.0,
                    text: "world".into(),
                    confidence: None
                },
            ]
        );
    }

    #[test]
    fn appends_timed_words_and_rejects_unusable_word_boundaries() {
        let segments = parse_json3(
            br#"{"events":[
            {"tStartMs":0,"dDurationMs":1000,"segs":[{"utf8":"hello","tOffsetMs":0}]},
            {"tStartMs":500,"dDurationMs":1000,"aAppend":1,"segs":[{"utf8":"world","tOffsetMs":0}]}
        ]}"#,
        )
        .unwrap();
        assert_eq!(segments[0].words.len(), 2);
        assert_eq!(segments[0].words[0].end, 0.5);
        assert_eq!(segments[0].words[1].end, 1.5);
        for parts in [
            json!([{"utf8":"two words", "tOffsetMs":0}]),
            json!([{"utf8":"hello", "tOffsetMs":100}, {"utf8":" world"}]),
            json!([{"utf8":"hello", "tOffsetMs":100}, {"utf8":" world", "tOffsetMs":50}]),
            json!([{"utf8":"hello", "tOffsetMs":1000}]),
            json!([{"utf8":"hello", "tOffsetMs":-1}]),
        ] {
            let data = json!({"events":[{"tStartMs":0, "dDurationMs":1000, "segs":parts}]});
            let segments = parse_json3(&serde_json::to_vec(&data).unwrap()).unwrap();
            assert_eq!(segments[0].words, []);
        }
    }

    #[test]
    fn validates_urls_and_exact_language_codes() {
        for url in [
            "https://youtu.be/abc",
            "https://www.youtube.com/watch?v=abc",
            "https://music.youtube.com/playlist?list=abc",
        ] {
            assert!(is_youtube_url(url));
        }
        for url in [
            "file:///youtube.com/abc",
            "https://youtube.com.evil.test/abc",
            "https://youtube.com@evil.test/abc",
            "lecture.mp3",
        ] {
            assert!(!is_youtube_url(url));
        }
        for language in ["ar", "en-US", "pt-BR"] {
            assert!(is_subtitle_language(language));
        }
        for language in ["", "all", "../ar", "ar.*", "ar,en", " ar", "-ar"] {
            assert!(!is_subtitle_language(language));
        }
    }
}
