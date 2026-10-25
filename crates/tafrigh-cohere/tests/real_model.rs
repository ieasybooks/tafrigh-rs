//! Opt-in smoke test against the real Cohere Arabic model.

use std::path::PathBuf;
use std::time::Duration;

use tafrigh::audio::{AudioChunk, Decoder, SAMPLE_RATE};
use tafrigh::{ChunkRecognizer, Recognition};
use tafrigh_cohere::{CohereConfig, CohereError, CohereRecognizer, Device};

#[tokio::test]
async fn recognizes_short_arabic_clip_when_model_is_configured() {
    let Some(model) = std::env::var_os("TAFRIGH_TEST_COHERE_MODEL") else {
        eprintln!("skipping real Cohere model test; set TAFRIGH_TEST_COHERE_MODEL");
        return;
    };
    let mut config = CohereConfig::new(PathBuf::from(model));
    config.device = if cfg!(feature = "cuda") {
        Device::GpuIndex(0)
    } else {
        Device::Cpu
    };
    config.threads = 4;
    let mut missing_gpu = config.clone();
    missing_gpu.device = Device::GpuIndex(usize::MAX);
    assert!(matches!(
        CohereRecognizer::load(missing_gpu).await,
        Err(CohereError::Config(message)) if message.contains("unavailable")
    ));
    let recognizer = CohereRecognizer::load(config).await.unwrap();
    assert_eq!(recognizer.threads(), 4);
    assert_eq!(
        recognizer.batch_size(),
        if cfg!(feature = "cuda") { 8 } else { 1 }
    );
    let backend = recognizer.backend().to_ascii_lowercase();
    assert!(if cfg!(feature = "cuda") {
        backend.starts_with("cuda")
    } else {
        backend == "cpu"
    });
    assert!(!recognizer.word_timings());
    // The supported recognition length is smaller than the native encoder capacity.
    assert_eq!(
        recognizer.max_audio_duration(),
        Some(Duration::from_secs(31))
    );
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
    for result in [first, second] {
        let Recognition { utterances } = result.unwrap();
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
    }
}
