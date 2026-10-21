//! Golden tests against the Python reference (silero-vad 6.2.1's `get_speech_timestamps` and
//! onnxruntime). Regenerate fixtures with `tests/fixtures/generate.py`.

use base64::Engine;
use serde_json::Value;

use super::timestamps::speech_timestamps;
use super::*;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn golden() -> Value {
    let text = std::fs::read_to_string(format!("{FIXTURES}/silero_golden.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn params(value: &Value) -> TimestampParams {
    TimestampParams {
        threshold: value["threshold"].as_f64().unwrap() as f32,
        min_speech_ms: value["min_speech_ms"].as_u64().unwrap() as u32,
        max_speech_s: value["max_speech_s"].as_f64().unwrap_or(f64::INFINITY),
        min_silence_ms: value["min_silence_ms"].as_u64().unwrap() as u32,
        speech_pad_ms: value["speech_pad_ms"].as_u64().unwrap() as u32,
        use_max_possible_silence_at_max_speech: value["use_max_possible_silence_at_max_speech"]
            .as_bool()
            .unwrap(),
        ..TimestampParams::default()
    }
}

fn spans(value: &Value) -> Vec<SpeechSpan> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| SpeechSpan {
            start: pair[0].as_u64().unwrap() as usize,
            end: pair[1].as_u64().unwrap() as usize,
        })
        .collect()
}

fn short_audio() -> Vec<f32> {
    std::fs::read(format!("{FIXTURES}/short.s16le"))
        .unwrap()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&b| f32::from(i16::from_le_bytes(b)) / 32768.0)
        .collect()
}

#[test]
fn timestamps_match_python_reference() {
    let golden = golden();
    let cases = golden["cases"].as_array().unwrap();
    assert!(cases.len() >= 60);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(case["probs_u8"].as_str().unwrap())
            .unwrap();
        // Same rounding as numpy: f64 division, then f32.
        let probs: Vec<f32> = bytes
            .iter()
            .map(|&b| (f64::from(b) / 255.0) as f32)
            .collect();
        let audio_len = case["audio_len"].as_u64().unwrap() as usize;
        let params = params(&case["params"]);
        let expected = spans(&case["spans"]);

        assert_eq!(
            speech_timestamps(&probs, audio_len, &params),
            expected,
            "batch: {name}"
        );

        let mut stream = StreamingTimestamps::new(&params);
        let mut got = Vec::new();
        for chunk in probs.chunks(37) {
            stream.push(chunk, &mut got);
        }
        stream.finish(audio_len, &mut got);
        assert_eq!(got, expected, "streaming: {name}");
    }
}

#[test]
fn network_matches_onnxruntime() {
    let audio = short_audio();
    let expected: Vec<f32> = std::fs::read(format!("{FIXTURES}/short.probs.f32"))
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&b| f32::from_le_bytes(b))
        .collect();

    let model = SileroModel::bundled().unwrap();
    let mut stream = model.stream().unwrap();
    let mut probs = Vec::new();
    // Odd block sizes exercise frame and batch boundaries.
    for block in audio.chunks(1234) {
        stream.push(block, &mut probs).unwrap();
    }
    stream.finish(&mut probs).unwrap();

    assert_eq!(probs.len(), expected.len());
    let max_diff = probs
        .iter()
        .zip(&expected)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(max_diff < 1e-4, "max probability difference {max_diff}");
}

#[test]
fn segmenter_matches_python_end_to_end() {
    let golden = golden();
    let p = params(&golden["short"]["params"]);
    let config = SileroConfig {
        threshold: p.threshold,
        min_speech: Duration::from_millis(p.min_speech_ms.into()),
        max_speech: Duration::from_secs_f64(p.max_speech_s),
        min_silence: Duration::from_millis(p.min_silence_ms.into()),
        speech_pad: Duration::from_millis(p.speech_pad_ms.into()),
    };
    let segmenter = SileroSegmenter::new(config).unwrap();
    let mut session = segmenter.begin().unwrap();
    let mut got = Vec::new();
    for block in short_audio().chunks(16_000) {
        session.push(block, &mut got).unwrap();
    }
    session.finish(&mut got).unwrap();
    assert_eq!(got, spans(&golden["short"]["spans"]));
}
