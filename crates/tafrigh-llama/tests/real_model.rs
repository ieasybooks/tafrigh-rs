//! Opt-in Audar Flash Q8_0 smoke test. The model and projector are never downloaded.

#![cfg(feature = "mtmd")]

use std::num::NonZeroU32;
use std::path::Path;
use std::process::Command;

use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::{params::LlamaModelParams, LlamaModel};
use llama_cpp_2::mtmd::{
    mtmd_default_marker, MtmdBitmap, MtmdContext, MtmdContextParams, MtmdInputText,
};
use llama_cpp_2::sampling::LlamaSampler;

#[test]
fn audar_transcribes_when_models_are_configured() {
    let Some(model_path) = std::env::var_os("TAFRIGH_TEST_AUDAR_MODEL") else {
        eprintln!("skipping real Audar model test; set TAFRIGH_TEST_AUDAR_MODEL and TAFRIGH_TEST_AUDAR_MMPROJ");
        return;
    };
    let projector = std::env::var("TAFRIGH_TEST_AUDAR_MMPROJ")
        .expect("TAFRIGH_TEST_AUDAR_MMPROJ must accompany TAFRIGH_TEST_AUDAR_MODEL");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample1.wav");
    let audio = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(fixture)
        .args(["-f", "f32le", "-ac", "1", "-ar", "16000", "pipe:1"])
        .output()
        .expect("ffmpeg is required for the real-model test");
    assert!(
        audio.status.success(),
        "{}",
        String::from_utf8_lossy(&audio.stderr)
    );
    let samples: Vec<f32> = audio
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| f32::from_le_bytes(*bytes))
        .collect();
    assert!(!samples.is_empty());
    let gpu = cfg!(feature = "cuda") || (cfg!(feature = "metal") && cfg!(target_os = "macos"));
    let backend = LlamaBackend::init().unwrap();
    let params = LlamaModelParams::default().with_n_gpu_layers(if gpu { 999 } else { 0 });
    let model = LlamaModel::load_from_file(&backend, model_path, &params).unwrap();
    let mut mtmd = MtmdContext::init_from_file(
        &projector,
        &model,
        &MtmdContextParams {
            use_gpu: gpu,
            n_threads: 3,
            print_timings: false,
            flash_attn_type: -1,
            warmup: true,
            ..MtmdContextParams::default()
        },
    )
    .unwrap();
    assert!(mtmd.support_audio());
    let context_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(2048))
        .with_n_batch(2048)
        .with_n_ubatch(512)
        .with_n_threads(3)
        .with_n_threads_batch(3);
    let mut context = model.new_context(&backend, context_params).unwrap();
    let bitmap = MtmdBitmap::from_audio_data(&samples).unwrap();
    let prompt = format!(
        "<|im_start|>system\n<|im_end|>\n<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\nlanguage Arabic<asr_text>",
        mtmd_default_marker()
    );
    let chunks = mtmd
        .tokenize(
            MtmdInputText {
                text: prompt,
                add_special: false,
                parse_special: true,
            },
            &[&bitmap],
        )
        .unwrap();
    let start_position = chunks
        .eval_chunks(&mut mtmd, &mut context, 0, 0, 2048, true)
        .unwrap();
    let mut sampler = LlamaSampler::greedy();
    let mut batch = LlamaBatch::new(1, 1);
    let vocab = model.vocab();
    let mut output = Vec::new();
    let mut ended = false;
    for position in start_position..start_position + 128 {
        let token = sampler.sample(&context, -1);
        if vocab.is_eog(token) {
            ended = true;
            break;
        }
        output.extend(vocab.token_to_piece(token, true, None));
        batch.clear();
        batch.add(token, position, &[0], true).unwrap();
        context.decode(&mut batch).unwrap();
    }
    let text = String::from_utf8(output).unwrap();
    eprintln!("Audar transcript: {text}");
    assert!(
        ended,
        "Audar did not finish within the fixture's token budget"
    );
    assert!(
        text.chars()
            .any(|ch| ('\u{0600}'..='\u{06ff}').contains(&ch)),
        "{text}"
    );
}
