//! Integration tests against a mock Wit server.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tafrigh::audio::AudioChunk;
use tafrigh::segment::silero::{SileroConfig, SileroSegmenter};
use tafrigh::{
    ChunkOptions, ChunkRecognizer, ChunkedTranscriber, Error, OnChunkError, Recognition,
    TranscribeEvent, Transcriber,
};
use tafrigh_wit::{Endpoint, WitConfig, WitRecognizer};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn config(server: &MockServer, tokens: &[&str]) -> WitConfig {
    let mut config = WitConfig::new(tokens.iter().copied());
    config.api_url = server.uri();
    config.language = Some("en".into());
    config.requests_per_second = 1000.0;
    config.retry_base_delay = Duration::from_millis(5);
    config.retry_max_delay = Duration::from_millis(20);
    config
}

fn chunk(samples: usize) -> AudioChunk {
    AudioChunk {
        index: 0,
        start: 1.0,
        end: 1.0 + samples as f64 / 16_000.0,
        lead_in: 0.0,
        samples: vec![7; samples],
    }
}

/// A dictation stream: one partial and one final transcription per `(text, start_ms, end_ms)`.
/// All utterance texts joined with spaces.
fn joined_text(recognition: &Recognition) -> String {
    let texts: Vec<&str> = recognition
        .utterances
        .iter()
        .map(|utterance| utterance.text.as_str())
        .collect();
    texts.join(" ")
}

fn dictation_stream(utterances: &[(&str, u64, u64)]) -> ResponseTemplate {
    let mut objects = Vec::new();
    for (text, start, end) in utterances {
        objects.push(serde_json::json!({
            "speech": {"confidence": 0.5, "tokens": []},
            "text": text.split(' ').next().unwrap(),
            "type": "PARTIAL_TRANSCRIPTION"
        }));
        objects.push(serde_json::json!({
            "is_final": true,
            "speech": {"confidence": 0.9, "tokens": [
                {"token": text, "start": start, "end": end, "confidence": 0.9}
            ]},
            "text": text,
            "type": "FINAL_TRANSCRIPTION"
        }));
    }
    let body: Vec<String> = objects.iter().map(ToString::to_string).collect();
    ResponseTemplate::new(200).set_body_string(body.join("\r\n"))
}

fn dictation(text: &str) -> ResponseTemplate {
    dictation_stream(&[(text, 0, 250)])
}

/// Splits each request's audio into two utterances (first and second half) and records
/// requests. The first utterance's text carries the PCM byte count.
struct Echo {
    seen: Arc<Mutex<Vec<(String, usize)>>>,
}

impl Respond for Echo {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let auth = request
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        assert_eq!(&request.body[..4], b"RIFF", "body must be a WAV file");
        let pcm = request.body.len() - 44;
        self.seen.lock().unwrap().push((auth, request.body.len()));
        let duration_ms = pcm as u64 / 32;
        let half = duration_ms / 2;
        let first = format!("bytes {pcm}");
        dictation_stream(&[(&first, 0, half), ("tail", half, duration_ms)])
    }
}

#[tokio::test]
async fn sends_wav_like_js_tafrigh_and_parses_utterances() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/dictation"))
        .and(query_param("v", "20240304"))
        .and(header("authorization", "Bearer token-a"))
        .and(header("content-type", "audio/wav"))
        .and(header("accept", "application/vnd.wit.20200513+json"))
        .respond_with(dictation_stream(&[
            ("hello there", 0, 250),
            ("again", 400, 700),
        ]))
        .expect(1)
        .mount(&server)
        .await;

    let recognizer = WitRecognizer::new(config(&server, &["token-a"]))
        .await
        .unwrap();
    let recognition = recognizer.recognize(&chunk(1600)).await.unwrap();
    assert_eq!(joined_text(&recognition), "hello there again");
    assert_eq!(recognition.utterances.len(), 2);
    assert!((recognition.utterances[1].words[0].end - 0.7).abs() < 1e-9);

    let body = &server.received_requests().await.unwrap()[0].body;
    assert_eq!(body.len(), 44 + 1600 * 2);
    assert_eq!(&body[..4], b"RIFF");
}

#[tokio::test]
async fn retries_rate_limits_and_server_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(502))
        .up_to_n_times(1)
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(dictation("finally"))
        .with_priority(3)
        .mount(&server)
        .await;

    let recognizer = WitRecognizer::new(config(&server, &["t"])).await.unwrap();
    let recognition = recognizer.recognize(&chunk(160)).await.unwrap();
    assert_eq!(joined_text(&recognition), "finally");
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

fn rate_limit(scope: &str) -> ResponseTemplate {
    ResponseTemplate::new(429).set_body_string(
        serde_json::json!({
            "code": "rate-limit",
            "error": format!("You have exceeded the limit of 60 requests per minute for your {scope}")
        })
        .to_string(),
    )
}

#[tokio::test]
async fn rate_limits_do_not_use_up_attempts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(rate_limit("app"))
        .up_to_n_times(2)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(rate_limit("account"))
        .up_to_n_times(2)
        .with_priority(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(dictation("patience"))
        .with_priority(3)
        .mount(&server)
        .await;
    let mut config = config(&server, &["a", "b"]);
    config.max_attempts = 1;
    let recognizer = WitRecognizer::new(config).await.unwrap();
    let recognition = recognizer.recognize(&chunk(160)).await.unwrap();
    assert_eq!(joined_text(&recognition), "patience");
    assert_eq!(server.received_requests().await.unwrap().len(), 5);
}

#[tokio::test]
async fn endless_rate_limits_eventually_fail() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(rate_limit("app"))
        .expect(31)
        .mount(&server)
        .await;
    let mut config = config(&server, &["t"]);
    config.max_attempts = 1;
    config.retry_max_delay = Duration::from_millis(5);
    let recognizer = WitRecognizer::new(config).await.unwrap();
    let err = recognizer.recognize(&chunk(160)).await.unwrap_err();
    assert!(err.to_string().contains("429"), "{err}");
}

#[tokio::test]
async fn retries_missing_body_responses() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_string(r#"{"code": "no-body", "error": "No request body provided"}"#),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(dictation("arrived"))
        .with_priority(2)
        .mount(&server)
        .await;

    let recognizer = WitRecognizer::new(config(&server, &["t"])).await.unwrap();
    let recognition = recognizer.recognize(&chunk(160)).await.unwrap();
    assert_eq!(joined_text(&recognition), "arrived");
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn stalled_requests_time_out_by_audio_length() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(dictation("stalled").set_delay(Duration::from_secs(5)))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(dictation("prompt"))
        .with_priority(2)
        .mount(&server)
        .await;

    let mut config = config(&server, &["t"]);
    // 0.1s of audio: the request may take 0.2s plus twice that, far below the 60s cap.
    config.request_timeout_base = Duration::from_millis(200);
    let recognizer = WitRecognizer::new(config).await.unwrap();
    let started = std::time::Instant::now();
    let recognition = recognizer.recognize(&chunk(1600)).await.unwrap();
    assert_eq!(joined_text(&recognition), "prompt");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn gives_up_after_max_attempts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .expect(3)
        .mount(&server)
        .await;
    let mut config = config(&server, &["t"]);
    config.max_attempts = 3;
    let recognizer = WitRecognizer::new(config).await.unwrap();
    let err = recognizer.recognize(&chunk(160)).await.unwrap_err();
    assert!(err.to_string().contains("503"), "{err}");
}

#[tokio::test]
async fn does_not_retry_client_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad token"))
        .expect(1)
        .mount(&server)
        .await;
    let recognizer = WitRecognizer::new(config(&server, &["t"])).await.unwrap();
    let err = recognizer.recognize(&chunk(160)).await.unwrap_err();
    assert!(err.to_string().contains("401"), "{err}");
}

#[tokio::test]
async fn normalizes_arabic_question_marks() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(dictation("ما هذا?"))
        .mount(&server)
        .await;
    let mut config = config(&server, &["t"]);
    config.language = Some("ar".into());
    let recognizer = WitRecognizer::new(config).await.unwrap();
    let recognition = recognizer.recognize(&chunk(160)).await.unwrap();
    assert_eq!(recognition.utterances[0].text, "ما هذا؟");
    assert_eq!(recognition.utterances[0].words[0].text, "ما هذا؟");
}

#[tokio::test]
async fn detects_language_and_rejects_mixed_apps() {
    let server = MockServer::start().await;
    for (token, lang) in [("ar-1", "ar"), ("ar-2", "ar"), ("en-1", "en")] {
        Mock::given(method("GET"))
            .and(path("/apps"))
            .and(header("authorization", format!("Bearer {token}").as_str()))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "other", "lang": "fr"},
                {"name": "mine", "lang": lang, "is_app_for_token": true}
            ])))
            .mount(&server)
            .await;
    }
    let mut same = config(&server, &["ar-1", "ar-2"]);
    same.language = None;
    assert_eq!(WitRecognizer::new(same).await.unwrap().language(), "ar");

    let mut mixed = config(&server, &["ar-1", "en-1"]);
    mixed.language = None;
    let err = WitRecognizer::new(mixed).await.unwrap_err();
    assert!(err.to_string().contains("same language"), "{err}");
}

// ---- Full pipeline: real speech → Silero → Wit mock ---------------------------------------

/// Writes the 11s speech fixture as a WAV file and returns its path.
fn wav_fixture() -> tempfile::TempPath {
    let pcm = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tafrigh/tests/fixtures/short.s16le"
    ))
    .unwrap();
    let chunk = AudioChunk {
        index: 0,
        start: 0.0,
        end: 0.0,
        lead_in: 0.0,
        samples: pcm
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&b| i16::from_le_bytes(b))
            .collect(),
    };
    let path = tempfile::Builder::new()
        .suffix(".wav")
        .tempfile()
        .unwrap()
        .into_temp_path();
    std::fs::write(&path, chunk.wav_bytes()).unwrap();
    path
}

/// Spans the Python reference produces for the fixture with a 60 ms speech pad.
const EXPECTED_SPANS: [(usize, usize); 4] = [
    (4_672, 36_800),
    (51_776, 71_616),
    (85_568, 123_328),
    (130_112, 176_000),
];

async fn transcriber(server: &MockServer, options: ChunkOptions) -> ChunkedTranscriber {
    let recognizer = WitRecognizer::new(config(server, &["a", "b"]))
        .await
        .unwrap();
    let config = SileroConfig {
        speech_pad: Duration::from_millis(60),
        ..SileroConfig::default()
    };
    let segmenter = SileroSegmenter::new(config).unwrap();
    ChunkedTranscriber::new(segmenter, recognizer, options).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn transcribes_real_audio_through_silero_and_wit() {
    let server = MockServer::start().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .respond_with(Echo { seen: seen.clone() })
        .mount(&server)
        .await;

    let options = ChunkOptions {
        padding: None,
        ..ChunkOptions::default()
    };
    let transcriber = transcriber(&server, options).await;
    let events = Mutex::new(Vec::new());
    let segments = transcriber
        .transcribe(&wav_fixture(), &|e: TranscribeEvent| {
            events.lock().unwrap().push(e);
        })
        .await
        .unwrap();

    // Each chunk's two utterances become two segments, bounded by their words.
    assert_eq!(segments.len(), 2 * EXPECTED_SPANS.len());
    for (pair, (start, end)) in segments.chunks(2).zip(EXPECTED_SPANS) {
        let (start, end) = (start as f64 / 16_000.0, end as f64 / 16_000.0);
        let middle = start + ((end - start) * 1000.0 / 2.0).floor() / 1000.0;
        assert!((pair[0].start - start).abs() < 1e-3, "{pair:?}");
        assert!((pair[0].end - middle).abs() < 2e-3, "{pair:?}");
        assert!((pair[1].start - middle).abs() < 2e-3, "{pair:?}");
        assert!((pair[1].end - end).abs() < 2e-3, "{pair:?}");
        let pcm_bytes = ((end - start) * 32_000.0).round() as usize;
        assert_eq!(pair[0].text, format!("bytes {pcm_bytes}"));
        assert_eq!(pair[1].text, "tail");
    }
    // Requests were spread over both tokens.
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 4);
    assert!(seen.iter().any(|(auth, _)| auth == "Bearer a"));
    assert!(seen.iter().any(|(auth, _)| auth == "Bearer b"));

    let events = events.lock().unwrap();
    assert!(
        matches!(events[0], TranscribeEvent::Started { duration: Some(d) } if (d - 11.0).abs() < 0.01)
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, TranscribeEvent::Segmented { chunks: 4 }))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn merges_chunks_and_pads_with_lead_in() {
    let server = MockServer::start().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .respond_with(Echo { seen: seen.clone() })
        .mount(&server)
        .await;

    let options = ChunkOptions {
        merge: true,
        padding: Some(tafrigh::audio::Padding::white_noise(
            Duration::from_secs(1),
            -30.0,
        )),
        ..ChunkOptions::default()
    };
    let segments = transcriber(&server, options)
        .await
        .transcribe(&wav_fixture(), &tafrigh::NoProgress)
        .await
        .unwrap();

    // The whole 11s clip fits in one 15s chunk: one request, two utterances.
    assert_eq!(seen.lock().unwrap().len(), 1);
    let expected_bytes = 44 + (176_000 - 4672 + 2 * 16_000) * 2;
    assert_eq!(seen.lock().unwrap()[0].1, expected_bytes);
    assert_eq!(segments.len(), 2);
    // Word times inside the 1s lead-in/tail padding are clamped onto the speech.
    assert!((segments[0].start - 4672.0 / 16_000.0).abs() < 1e-9);
    assert!((segments[1].end - 176_000.0 / 16_000.0).abs() < 1e-9);
}

/// Hears only the first third of each request's audio (padding included), like Wit stopping
/// early. The text carries the PCM byte count.
struct FirstThird {
    seen: Arc<Mutex<Vec<usize>>>,
}

impl Respond for FirstThird {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let pcm = request.body.len() - 44;
        self.seen.lock().unwrap().push(pcm);
        let third_ms = pcm as u64 / 32 / 3;
        dictation_stream(&[(&format!("bytes {pcm}"), 0, third_ms)])
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn resends_speech_the_recognizer_skipped() {
    let server = MockServer::start().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .respond_with(FirstThird { seen: seen.clone() })
        .mount(&server)
        .await;

    // With 0.5s padding, the words leave 1.5s, 1.0s, 1.7s and 2.1s of the four chunks
    // uncovered: all but the second reach 1.2s and go out again.
    let options = ChunkOptions {
        resend_gaps: Some(Duration::from_millis(1200)),
        ..ChunkOptions::default()
    };
    let segments = transcriber(&server, options)
        .await
        .transcribe(&wav_fixture(), &tafrigh::NoProgress)
        .await
        .unwrap();

    assert_eq!(seen.lock().unwrap().len(), 4 + 3);
    assert_eq!(segments.len(), 7);
    assert!(
        segments.windows(2).all(|w| w[0].start <= w[1].start),
        "{segments:?}"
    );
    let texts: Vec<&str> = segments.iter().map(|s| s.text.as_str()).collect();
    for (resent, original) in [(1, 0), (4, 3), (6, 5)] {
        // A resend starts where the chunk's words stopped and carries less audio.
        assert!(
            (segments[resent].start - segments[original].end).abs() < 2e-3,
            "{segments:?}"
        );
        let bytes = |i: usize| {
            texts[i]
                .trim_start_matches("bytes ")
                .parse::<usize>()
                .unwrap()
        };
        assert!(bytes(resent) < bytes(original), "{texts:?}");
    }
    // Resent stretches end where their chunk's speech ends.
    for (resent, (_, end)) in [
        (1, EXPECTED_SPANS[0]),
        (4, EXPECTED_SPANS[2]),
        (6, EXPECTED_SPANS[3]),
    ] {
        let words_end = segments[resent].end;
        assert!(words_end <= end as f64 / 16_000.0 + 1e-9, "{segments:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn chunk_failures_abort_or_skip() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    Mock::given(method("POST"))
        .respond_with(move |_: &Request| {
            // The second request fails permanently.
            if counter.fetch_add(1, Ordering::SeqCst) == 1 {
                ResponseTemplate::new(400)
            } else {
                dictation("ok")
            }
        })
        .mount(&server)
        .await;

    let one_at_a_time = ChunkOptions {
        max_in_flight: Some(1),
        // Each reply covers 0.25s; resends would shift which request fails.
        resend_gaps: None,
        ..ChunkOptions::default()
    };
    let err = transcriber(&server, one_at_a_time.clone())
        .await
        .transcribe(&wav_fixture(), &tafrigh::NoProgress)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Recognition { index: 1, .. }), "{err}");

    calls.store(0, Ordering::SeqCst);
    let skipped = Mutex::new(Vec::new());
    let segments = transcriber(
        &server,
        ChunkOptions {
            on_error: OnChunkError::Skip,
            ..one_at_a_time
        },
    )
    .await
    .transcribe(&wav_fixture(), &|e: TranscribeEvent| {
        if let TranscribeEvent::ChunkSkipped { index, .. } = e {
            skipped.lock().unwrap().push(index);
        }
    })
    .await
    .unwrap();
    assert_eq!(segments.len(), 3);
    assert_eq!(*skipped.lock().unwrap(), vec![1]);
}

#[tokio::test]
async fn audio_limits_depend_on_the_endpoint() {
    let server = MockServer::start().await;
    let options = |max_chunk| ChunkOptions {
        max_chunk,
        padding: Some(tafrigh::audio::Padding::silence(Duration::from_secs(1))),
        ..ChunkOptions::default()
    };
    let build = |config: WitConfig, seconds| async move {
        let max_chunk = Duration::from_secs(seconds);
        let recognizer = WitRecognizer::new(config).await.unwrap();
        let segmenter = tafrigh::segment::FixedWindows::new(max_chunk).unwrap();
        ChunkedTranscriber::new(segmenter, recognizer, options(max_chunk))
    };

    // Dictation: up to 60s per request.
    assert!(build(config(&server, &["t"]), 58).await.is_ok());
    let err = build(config(&server, &["t"]), 59).await.err().unwrap();
    assert!(err.to_string().contains("60.00s limit"), "{err}");

    // Speech: up to 20s per request.
    let mut speech = config(&server, &["t"]);
    speech.endpoint = Endpoint::Speech;
    let err = build(speech, 19).await.err().unwrap();
    assert!(err.to_string().contains("20.00s limit"), "{err}");
}
