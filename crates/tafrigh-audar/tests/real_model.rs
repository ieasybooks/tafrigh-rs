//! Opt-in model test; ordinary tests never download or load Audar weights.

use std::path::PathBuf;
use std::time::Duration;

use tafrigh::audio::{AudioChunk, Decoder, SAMPLE_RATE};
use tafrigh::{ChunkRecognizer, Recognition};
use tafrigh_audar::{AudarConfig, AudarError, AudarRecognizer, Device};

#[tokio::test]
async fn recognizes_arabic_with_local_decoder_and_projector() {
    let Some(model) = std::env::var_os("TAFRIGH_TEST_AUDAR_MODEL") else {
        eprintln!(
            "skipping real Audar model test; set TAFRIGH_TEST_AUDAR_MODEL and TAFRIGH_TEST_AUDAR_MMPROJ"
        );
        return;
    };
    let projector = std::env::var_os("TAFRIGH_TEST_AUDAR_MMPROJ")
        .expect("TAFRIGH_TEST_AUDAR_MMPROJ must accompany TAFRIGH_TEST_AUDAR_MODEL");
    let mut config = AudarConfig::local(PathBuf::from(model), PathBuf::from(projector));
    config.device = if cfg!(feature = "cuda") {
        Device::Gpu
    } else {
        Device::Cpu
    };
    config.threads = 3;
    let mut unavailable = config.clone();
    unavailable.device = Device::GpuIndex(usize::MAX);
    assert!(matches!(AudarRecognizer::load(unavailable).await,
        Err(AudarError::Config(message)) if message.contains("unavailable")));
    let recognizer = AudarRecognizer::load(config).await.unwrap();
    assert_eq!(recognizer.threads(), 3);
    assert_eq!(recognizer.max_in_flight(), 2);
    assert_eq!(
        recognizer.max_audio_duration(),
        Some(Duration::from_secs(30))
    );
    assert!(!recognizer.word_timings());
    assert_eq!(recognizer.backend() == "CPU", !cfg!(feature = "cuda"));
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample1.wav");
    let samples = tokio::task::spawn_blocking(move || {
        let mut reader = Decoder::default().open(&fixture).unwrap();
        let mut samples = Vec::new();
        let mut buffer = Vec::new();
        while reader.read(&mut buffer, SAMPLE_RATE as usize).unwrap() > 0 {
            samples.extend_from_slice(&buffer);
        }
        samples
    })
    .await
    .unwrap();
    let chunk = AudioChunk {
        index: 0,
        start: 0.0,
        end: samples.len() as f64 / f64::from(SAMPLE_RATE),
        lead_in: 0.0,
        samples,
    };
    let (first, second) = tokio::join!(recognizer.recognize(&chunk), recognizer.recognize(&chunk));
    let first = first.unwrap();
    assert_eq!(first, second.unwrap());
    let Recognition { utterances } = first;
    assert_eq!(utterances.len(), 1);
    assert_eq!(utterances[0].words, []);
    assert!(
        utterances[0]
            .text
            .chars()
            .any(|ch| ('\u{0600}'..='\u{06ff}').contains(&ch)),
        "{}",
        utterances[0].text
    );
    assert!(!utterances[0].text.contains("<asr_text>"));
}
