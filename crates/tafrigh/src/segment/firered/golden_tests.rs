//! Independent fixtures from official `FireRed` Python features and ONNX Runtime.

use std::sync::OnceLock;

use serde_json::Value;

use super::features::{Filterbank, MEL_BINS, WINDOW_SAMPLES};
use super::*;
use crate::segment::Merger;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/firered");

fn model() -> FireRedModel {
    static MODEL: OnceLock<FireRedModel> = OnceLock::new();
    MODEL
        .get_or_init(|| FireRedModel::bundled().unwrap())
        .clone()
}

fn golden() -> Value {
    serde_json::from_str(&std::fs::read_to_string(format!("{FIXTURES}/golden.json")).unwrap())
        .unwrap()
}

fn floats(name: &str) -> Vec<f32> {
    std::fs::read(format!("{FIXTURES}/{name}"))
        .unwrap()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&bytes| f32::from_le_bytes(bytes))
        .collect()
}

fn audio(case: &Value) -> Vec<f32> {
    std::fs::read(format!("{FIXTURES}/{}", case["pcm"].as_str().unwrap()))
        .unwrap()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&bytes| f32::from(i16::from_le_bytes(bytes)) / 32768.0)
        .collect::<Vec<_>>()
        .repeat(case["repeat"].as_u64().unwrap() as usize)
}

fn assert_close(got: &[f32], expected: &[f32], tolerance: f32, name: &str) {
    assert_eq!(got.len(), expected.len(), "{name}");
    let (i, max) = got
        .iter()
        .zip(expected)
        .enumerate()
        .map(|(i, (a, b))| (i, (a - b).abs()))
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    eprintln!("{name}: maximum absolute difference {max}");
    assert!(
        max < tolerance,
        "{name}: max difference {max} at {i}: {} vs {}",
        got[i],
        expected[i]
    );
}

#[test]
fn features_match_official_kaldi_frontend() {
    let mut frame = std::sync::Arc::new(Filterbank::new()).stream();
    for case in golden()["cases"].as_array().unwrap() {
        if case["repeat"] != 1 {
            continue;
        }
        let name = case["name"].as_str().unwrap();
        let samples = audio(case);
        let expected = floats(&format!("{name}.features.f32"));
        let got: Vec<_> = samples
            .windows(WINDOW_SAMPLES)
            .step_by(HOP_SAMPLES)
            .flat_map(|samples| frame.extract(samples))
            .collect();
        assert_eq!(
            got.len(),
            case["frames"].as_u64().unwrap() as usize * MEL_BINS
        );
        assert_close(&got, &expected, 1e-3, name);
    }
    let constant = frame.extract(&[1234.0 / 32768.0; WINDOW_SAMPLES]);
    assert_close(
        &constant,
        &floats("constant.features.f32"),
        1e-6,
        "DC removal and log floor",
    );
}

#[test]
fn probabilities_match_reference_across_cache_boundaries() {
    for case in golden()["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let mut stream = model().stream().unwrap();
        let mut got = Vec::new();
        for block in audio(case).chunks(1234) {
            stream.push(block, &mut got).unwrap();
        }
        stream.finish(&mut got).unwrap();
        assert_close(&got, &floats(&format!("{name}.probs.f32")), 1e-4, name);
    }
}

#[test]
fn sessions_match_both_chunk_policies_and_preserve_retained_audio() {
    for case in golden()["cases"].as_array().unwrap() {
        let samples = audio(case);
        for (seconds, merge, key) in [(15, false, "spans_15"), (30, true, "spans_30")] {
            let segmenter = FireRedSegmenter::with_model(
                model(),
                FireRedConfig {
                    max_speech: Duration::from_secs(seconds),
                    ..Default::default()
                },
            )
            .unwrap();
            let expected: Vec<_> = case[key]
                .as_array()
                .unwrap()
                .iter()
                .map(|pair| SpeechSpan {
                    start: pair[0].as_u64().unwrap() as usize,
                    end: pair[1].as_u64().unwrap() as usize,
                })
                .collect();
            // Reusing one model must reset both frontend and cache for each recording.
            for block_size in [1, 317, 16000, samples.len()] {
                let mut session = segmenter.begin().unwrap();
                let mut got = Vec::new();
                let mut watermark = 0;
                let mut seen = 0;
                for block in samples.chunks(block_size) {
                    let before = got.len();
                    session.push(&[], &mut got).unwrap();
                    session.push(block, &mut got).unwrap();
                    seen += block.len();
                    assert!(
                        got[before..]
                            .iter()
                            .all(|span| span.start >= watermark && span.end <= seen)
                    );
                    let next = session.retain_from();
                    assert!(next >= watermark && next <= seen);
                    watermark = next;
                }
                let before = got.len();
                session.finish(&mut got).unwrap();
                assert!(
                    got[before..]
                        .iter()
                        .all(|span| span.start >= watermark && span.end <= seen)
                );
                assert!(
                    got.iter()
                        .all(|span| !span.is_empty() && span.len() <= seconds as usize * 16000)
                );
                assert!(got.windows(2).all(|pair| pair[0].end <= pair[1].start));
                if merge {
                    let mut merger = Merger::new(seconds as usize * 16000);
                    got = got
                        .into_iter()
                        .filter_map(|span| merger.push(span))
                        .collect();
                    got.extend(merger.finish());
                }
                assert_eq!(got, expected, "{} {key} block {block_size}", case["name"]);
                let before = got.len();
                session.finish(&mut got).unwrap();
                assert_eq!(
                    before,
                    got.len(),
                    "finishing twice must not emit duplicates"
                );
            }
        }
    }
}

#[test]
fn snipped_edges_empty_input_and_silence() {
    let segmenter = FireRedSegmenter::with_model(model(), FireRedConfig::default()).unwrap();
    for length in [0, 1, 159, 399, 400, 401, 559, 560, 561, 16000] {
        let samples = vec![0.0; length];
        let mut stream = model().stream().unwrap();
        let mut probabilities = Vec::new();
        for block in samples.chunks(79) {
            stream.push(block, &mut probabilities).unwrap();
        }
        stream.finish(&mut probabilities).unwrap();
        let expected_frames = if length < WINDOW_SAMPLES {
            0
        } else {
            1 + (length - WINDOW_SAMPLES) / HOP_SAMPLES
        };
        assert_eq!(probabilities.len(), expected_frames, "length {length}");
        let mut session = segmenter.begin().unwrap();
        let mut spans = Vec::new();
        session.push(&samples, &mut spans).unwrap();
        session.finish(&mut spans).unwrap();
        assert!(spans.is_empty(), "silence length {length}");
    }
}

#[test]
fn rejects_invalid_configuration() {
    for threshold in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        assert!(
            FireRedConfig {
                threshold,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        FireRedConfig {
            max_speech: Duration::from_millis(510),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        FireRedConfig {
            max_speech: Duration::from_millis(511),
            ..Default::default()
        }
        .validate()
        .is_ok()
    );
}

#[test]
fn eof_tail_respects_maximum_without_speech_padding() {
    let segmenter = FireRedSegmenter::with_model(
        model(),
        FireRedConfig {
            // Every complete frame is speech, so the final window crosses exactly one second.
            threshold: 0.0,
            min_speech: Duration::ZERO,
            max_speech: Duration::from_secs(1),
            speech_pad: Duration::ZERO,
            ..Default::default()
        },
    )
    .unwrap();
    let mut session = segmenter.begin().unwrap();
    let mut spans = Vec::new();
    session.push(&vec![0.0; 16240], &mut spans).unwrap();
    session.finish(&mut spans).unwrap();
    assert_eq!(
        spans,
        [
            SpeechSpan {
                start: 0,
                end: 16000
            },
            SpeechSpan {
                start: 16000,
                end: 16240
            }
        ]
    );
}
