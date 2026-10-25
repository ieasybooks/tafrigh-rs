use std::num::NonZeroU32;
use std::sync::OnceLock;

use llama_cpp_2::context::LlamaContext;
use llama_cpp_2::context::params::{KvCacheType, LlamaContextParams};
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::LlamaModel;
use llama_cpp_2::model::params::{LlamaModelParams, LlamaSplitMode};
use llama_cpp_2::mtmd::{
    MtmdBitmap, MtmdContext, MtmdContextParams, MtmdInputChunks, MtmdInputText, mtmd_default_marker,
};
use llama_cpp_2::sampling::LlamaSampler;
use llama_cpp_2::{LlamaBackendDevice, LlamaBackendDeviceType};
use tafrigh::Recognition;
use tokio::sync::{mpsc, oneshot};

use crate::error::native;
use crate::recognizer::Request;
use crate::text::GeneratedText;
use crate::{AudarConfig, AudarError, Device, ModelFiles};

const CONTEXT: u32 = 2048;
const MAX_TOKENS: usize = 440;
static BACKEND: OnceLock<Result<LlamaBackend, String>> = OnceLock::new();

pub(crate) struct Loaded {
    pub backend: String,
    pub device_name: String,
}

type Ready = oneshot::Sender<Result<Loaded, AudarError>>;

pub(crate) fn worker(
    config: &AudarConfig,
    files: &ModelFiles,
    threads: usize,
    jobs: mpsc::Receiver<Request>,
    ready: Ready,
    runtime: &tokio::runtime::Handle,
) {
    let mut ready = Some(ready);
    let mut jobs = Some(jobs);
    let result = (|| {
        // Fail on local files before initializing backends or allocating model weights.
        for file in [&files.decoder, &files.mmproj] {
            if !std::fs::metadata(file)?.is_file() {
                return Err(AudarError::Config(format!(
                    "{} is not a regular file",
                    file.display()
                )));
            }
            if file.to_str().is_none_or(|name| name.contains('\0')) {
                return Err(AudarError::Config(
                    "model paths must be UTF-8 without NUL bytes".into(),
                ));
            }
        }
        let backend = BACKEND
            .get_or_init(|| {
                llama_cpp_2::send_logs_to_tracing(llama_cpp_2::LogOptions::default());
                LlamaBackend::init().map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(native("backend"))?;
        let devices = llama_cpp_2::list_llama_ggml_backend_devices();
        let index = match config.device {
            Device::GpuIndex(index) => index,
            _ => 0,
        };
        let gpu = devices
            .iter()
            .filter(|device| {
                matches!(
                    device.device_type,
                    LlamaBackendDeviceType::Gpu | LlamaBackendDeviceType::IntegratedGpu
                )
            })
            .nth(index);
        if config.device != Device::Cpu {
            if let Some(gpu) = gpu {
                let result = serve(
                    backend,
                    files,
                    threads,
                    Some(gpu),
                    &mut jobs,
                    &mut ready,
                    runtime,
                );
                match result {
                    Ok(()) => return Ok(()),
                    Err(error) if config.device == Device::Auto => {
                        tracing::warn!(%error, "Audar GPU loading failed; retrying on CPU");
                    }
                    Err(error) => return Err(error),
                }
            } else if config.device != Device::Auto {
                return Err(AudarError::Config(format!(
                    "GPU {index} is unavailable; use a GPU-enabled build and a working driver, or select cpu"
                )));
            }
        }
        serve(
            backend, files, threads, None, &mut jobs, &mut ready, runtime,
        )
    })();
    if let Err(error) = result
        && let Some(ready) = ready
    {
        let _ = ready.send(Err(error));
    }
}

// The worker owns every native context. The decoder borrows the model only within this call.
#[allow(clippy::too_many_arguments)]
fn serve(
    backend: &LlamaBackend,
    files: &ModelFiles,
    threads: usize,
    gpu: Option<&LlamaBackendDevice>,
    jobs: &mut Option<mpsc::Receiver<Request>>,
    ready: &mut Option<Ready>,
    runtime: &tokio::runtime::Handle,
) -> Result<(), AudarError> {
    let mut params =
        LlamaModelParams::default().with_n_gpu_layers(if gpu.is_some() { 999 } else { 0 });
    if let Some(gpu) = gpu {
        params = params
            .with_devices(&[gpu.index])
            .map_err(native("device"))?
            .with_split_mode(LlamaSplitMode::None)
            .with_main_gpu(0);
    }
    let model =
        LlamaModel::load_from_file(backend, &files.decoder, &params).map_err(native("decoder"))?;
    let mtmd_params = MtmdContextParams {
        use_gpu: gpu.is_some(),
        n_threads: threads as i32,
        print_timings: false,
        flash_attn_type: -1,
        warmup: true,
        ..MtmdContextParams::default()
    };
    let projector = files.mmproj.to_str().expect("validated model path");
    let mut encoder = MtmdContext::init_from_file_with_device(
        projector,
        &model,
        &mtmd_params,
        gpu.map(|device| device.index),
    )
    .map_err(native("mmproj"))?;
    if !encoder.support_audio() || encoder.get_audio_sample_rate() != Some(16_000) {
        return Err(AudarError::Config(
            "the mmproj file must have a 16 kHz audio encoder".into(),
        ));
    }
    let preparation = MtmdContext::init_from_file(
        projector,
        &model,
        &MtmdContextParams {
            use_gpu: false,
            warmup: false,
            ..mtmd_params
        },
    )
    .map_err(native("audio preprocessor"))?;
    let ctx_params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(CONTEXT))
        .with_n_seq_max(1)
        .with_n_batch(CONTEXT)
        .with_n_ubatch(512)
        .with_n_threads(threads as i32)
        .with_n_threads_batch(threads as i32)
        .with_flash_attention_policy(-1)
        .with_type_k(KvCacheType::F16)
        .with_type_v(KvCacheType::F16)
        .with_offload_kqv(gpu.is_some())
        .with_op_offload(gpu.is_some());
    let mut ctx = model
        .new_context(backend, ctx_params)
        .map_err(native("decoder context"))?;
    let loaded = Loaded {
        backend: gpu.map_or_else(|| "CPU".into(), |device| device.name.clone()),
        device_name: gpu.map_or_else(|| "CPU".into(), |device| device.description.clone()),
    };
    if ready
        .take()
        .expect("startup reports once")
        .send(Ok(loaded))
        .is_err()
    {
        return Ok(());
    }
    let prompt = format!(
        "<|im_start|>system\n<|im_end|>\n<|im_start|>user\n{}<|im_end|>\n<|im_start|>assistant\nlanguage Arabic<asr_text>",
        mtmd_default_marker()
    );
    let jobs = jobs.take().expect("one inference worker");
    prepare_and_decode(jobs, preparation, &prompt, runtime, |prepared| {
        let result = decode(
            &prepared.chunks,
            &model,
            &mut encoder,
            &mut ctx,
            prepared.request.chunk,
        );
        let _ = prepared.request.reply.send(result);
    });
    Ok(())
}

struct Prepared {
    chunks: MtmdInputChunks,
    request: Request,
}

fn prepare(request: Request, context: &MtmdContext, prompt: &str) -> Option<Prepared> {
    let result = (|| {
        let bitmap = MtmdBitmap::from_audio_data(&request.pcm).map_err(native("audio bitmap"))?;
        context
            .tokenize(
                MtmdInputText {
                    text: prompt.to_owned(),
                    add_special: false,
                    parse_special: true,
                },
                &[&bitmap],
            )
            .map_err(native("audio features"))
    })();
    match result {
        Ok(chunks) => Some(Prepared { chunks, request }),
        Err(error) => {
            let _ = request.reply.send(Err(error));
            None
        }
    }
}

fn prepare_and_decode(
    mut jobs: mpsc::Receiver<Request>,
    preparation: MtmdContext,
    prompt: &str,
    runtime: &tokio::runtime::Handle,
    mut decode: impl FnMut(Prepared),
) {
    std::thread::scope(|scope| {
        let (sender, mut receiver) = mpsc::channel(1);
        scope.spawn(move || {
            loop {
                // Decoder shutdown also wakes a preparer waiting for input. This prevents
                // a worker panic from leaving the scoped thread and callers waiting forever.
                let request = runtime.block_on(async {
                    tokio::select! {
                        request = jobs.recv() => request,
                        () = sender.closed() => None,
                    }
                });
                let Some(request) = request else {
                    break;
                };
                if request.reply.is_closed() {
                    continue;
                }
                if let Some(prepared) = prepare(request, &preparation, prompt)
                    && sender.blocking_send(prepared).is_err()
                {
                    break;
                }
            }
        });
        while let Some(prepared) = receiver.blocking_recv() {
            if !prepared.request.reply.is_closed() {
                decode(prepared);
            }
        }
    });
}

fn decode(
    chunks: &MtmdInputChunks,
    model: &LlamaModel,
    encoder: &mut MtmdContext,
    context: &mut LlamaContext<'_>,
    (index, start, end): (usize, f64, f64),
) -> Result<Recognition, AudarError> {
    if chunks.total_positions() as usize + MAX_TOKENS > CONTEXT as usize {
        return Err(AudarError::Config(
            "audio prompt and token budget exceed the decoder context".into(),
        ));
    }
    context.clear_kv_cache();
    let mut position = chunks
        .eval_chunks(encoder, context, 0, 0, CONTEXT as i32, true)
        .map_err(native("encoder and prefill"))?;
    let mut sampler = LlamaSampler::greedy();
    let mut batch = LlamaBatch::new(1, 1);
    let vocab = model.vocab();
    let mut output = GeneratedText::default();
    let mut tokens = 0;
    let mut token = sampler.sample(context, -1);
    let stop = loop {
        if vocab.is_eog(token) {
            break "end_of_text";
        }
        output.push(token.0, &vocab.token_to_piece(token, true, None));
        tokens += 1;
        if let Some((bytes_before, bytes_after)) = output.trim_repetition() {
            tracing::warn!(
                bytes_before,
                bytes_after,
                "chunk #{index} ({start:.2}s–{end:.2}s): Audar stopped at repetition; dropped the repeats"
            );
            break "repetition";
        }
        if tokens >= MAX_TOKENS {
            break "token_limit";
        }
        batch.clear();
        batch
            .add(token, position, &[0], true)
            .map_err(native("decoder batch"))?;
        position += 1;
        context.decode(&mut batch).map_err(native("decode"))?;
        token = sampler.sample(context, 0);
    };
    if stop == "token_limit" {
        tracing::warn!(
            "chunk #{index} ({start:.2}s–{end:.2}s): Audar stopped at {stop}; kept the text produced so far"
        );
    }
    let text = output.finish();
    tracing::debug!(chunk = index, tokens, stop, "recognized Audar chunk");
    Ok(if text.is_empty() {
        Recognition::default()
    } else {
        Recognition::text(text)
    })
}
