# Cohere CUDA performance and output checks — 2026-10-23

The complete change set combines bounded batch prefetching, two native CUDA graph fixes, and CLI batch/thread controls. On an RTX 4070, the Q8_0 default batch and thread settings took **13.52 seconds per audio hour**, compared with **58.00** for the official tafrigh 0.4.0 CUDA13 release. Tuned BF16 took **12.12 seconds per audio hour**. These are local Cohere transcription times, including preparation and output writing; they are not Wit or download/import timings.

## Machine, models and measurement boundary

NVIDIA RTX 4070 12 GiB; Intel Core i7-14700F; 30 GiB RAM; CUDA 13.4; Rust 1.99.1. The patched binary was installed with a fresh Cargo target directory using `cargo install --path . --locked --features cuda`, without `TRANSCRIBE_DIR`. It statically contains transcribe/ggml and uses the system CUDA runtime libraries. The native runtime is pinned to transcribe.cpp 0.4.1 with the local source patches; the original 0.4.0 release used native 0.4.0. Archive checksums, build flags, model revisions and measured aggregates are in the [JSON results](cohere-cuda-2026-10-23.json).

Both implementations use [Cohere Transcribe Arabic 07-2026](https://huggingface.co/CohereLabs/cohere-transcribe-arabic-07-2026), revision `0a8193caa4f3f92131471ab08824e488141cb392`. GGUF variants come from revision `11a54d6895f94cce78d0ece142b8e552e7822539` of the pinned GGUF distribution. BF16 comparisons use the same model variant; GGUF conversion and different numerical kernels can still produce small text differences.

Timing uses 16 local recordings totaling 26,378.247 seconds (7.3273 audio hours). Each pass starts a fresh CLI process and ends after SRT/TXT output is written. It includes model loading, FFmpeg decoding, Silero VAD, inference and rendering. It excludes installation, model downloads, input staging, YouTube waits and application imports. The recordings and their transcripts are private and are not distributed, so the published timing corpus cannot be independently replayed. The accuracy corpus below is public.

All passes ran sequentially on the same GPU. Resource measurements include desktop GPU use and were sampled approximately once per second. Fresh processes do not imply cold filesystem caches. Common settings were Arabic, Silero threshold 0.5, minimum speech 500 ms, minimum silence 300 ms, speech padding 250 ms, merging up to 30 seconds, no extra chunk padding, minimum words 1, and abort on a chunk error.

## Throughput

| Build and settings | GPU processes | Passes | Seconds per audio hour | Range |
|---|---:|---:|---:|---:|
| Original 0.4.0, Q8_0, batch 8, automatic threads | 1 | 1 | 58.00 | single baseline |
| Patched Q8_0, batch 8, automatic threads | 1 | 3 | 13.52 | 13.51–13.72 |
| Original 0.4.0, BF16, batch 8 per process, automatic threads | 2 | 3 | 47.02 | 46.67–47.68 |
| Patched BF16, batch 32, 8 CPU threads | 1 | 3 | 12.12 | 12.06–12.33 |
| cohere-transcribe 0.1.4, BF16, batch 24 | 1 | 3 | 10.35 | 10.19–10.59 |

Values with three passes are medians. Q8_0 improves by 4.29× against its single measured baseline with the same batch/thread settings. The BF16 comparison also changes the number of GPU processes and batch/thread settings; its 3.88× gain is for the complete setup, not an isolated estimate of the native patch or prefetch effect. The earlier BF16 and cohere-transcribe measurements used the same frozen recordings and machine; their individual times are retained in the JSON.

Q8_0, batch 8 and automatic threads remain the defaults. The tuned BF16 setting is an explicit choice for this GPU, not a new universal default. It uses more GPU memory. The cohere-transcribe comparison used the original BF16 safetensors, one process, fixed batch 24, FFmpeg decoding, PyTorch Silero, and the same chunking parameters; matching parameters does not guarantee identical VAD boundaries. Its PyTorch build used CUDA 12.8.

## Human-reference accuracy

All 428 clips from [FLEURS `ar_eg` test](https://huggingface.co/datasets/google/fleurs), revision `70bb2e84b976b7e960aa89f1c648e09c59f894dd`, CC BY 4.0: 78.09 minutes, 283 distinct reference sentences, 8,008 normalized words. Each setting was checked with the normal Silero path and with a whole-clip control that disables VAD and merging. These short clips have substantially different overhead from long recordings; their runtime is not used for the throughput table above.

| Variant / segmentation | WER, original → patched | CER, original → patched | Identical normalized outputs | 95% CI, WER change |
|---|---:|---:|---:|---:|
| Q8_0 / silero | 4.820% → 4.770% | 2.172% → 2.148% | 425/428 | -0.146 to +0.000 pp |
| Q8_0 / whole | 4.795% → 4.883% | 2.200% → 2.237% | 418/428 | -0.050 to +0.291 pp |
| BF16 / silero | 4.995% → 4.995% | 2.260% → 2.260% | 427/428 | +0.000 to +0.000 pp |
| BF16 / whole | 4.720% → 4.720% | 2.169% → 2.174% | 427/428 | +0.000 to +0.000 pp |

The intervals use paired bootstrap resampling by reference sentence, 2,000 resamples. “pp” means percentage points. Q8_0 had four fewer word errors with Silero and seven more in the whole-clip control; BF16's word-error counts were unchanged. Small observed changes on this finite read-speech set do not establish identical accuracy on every recording. No human-reference lecture accuracy is claimed. Agreement between timing-corpus outputs is reported separately in the JSON and is not an accuracy score.

Normalization applies NFKC and lowercase, removes Arabic diacritics and tatweel, replaces punctuation/symbols/control characters with spaces, normalizes decimal digits, folds Alef variants to Alef and Alef Maqsura to Yaa, and preserves Taa Marbuta. CER excludes whitespace. Scores without Alef/Yaa folding are also included in the JSON.

## Repeating the checks

Build a CUDA binary with the toolkit available on PATH. For a nonstandard toolkit, set `CUDACXX` to its `nvcc` and `CUDAToolkit_ROOT` to its root. No native-library override is needed.

```sh
cargo build --release --locked --features cuda

# Run each binary against the same input folder and a separate empty output folder.
/usr/bin/time -f '%e seconds' ./target/release/tafrigh corpus/ \
  --transcriber cohere --device gpu --language ar \
  --format srt,txt --output-dir results-q8 \
  --vad silero --merge --max-chunk 30 --pad 0 \
  --vad-threshold 0.5 --min-speech-ms 500 --min-silence-ms 300 \
  --speech-pad-ms 250 --min-words 1 --on-chunk-error abort --quiet
```

The Q8 run deliberately omits model, batch and thread flags. For tuned BF16, add `--model bf16 --cohere-batch-size 32 --cohere-threads 8` and choose another output folder. For the whole-clip FLEURS control, replace `--vad silero --merge` with `--vad none --no-merge`. Run three fresh processes per timing setting, without another inference job on the GPU. Divide elapsed seconds by the total input duration in hours; do not divide by process count again. Download model weights before measuring.

The recorded runs also enabled `TRANSCRIBE_PERF_DEBUG=1` and `RUST_LOG=tafrigh_cohere=debug,transcribe_cpp=debug` and redirected process logs to files. Local build flags disabled system BLAS and CPU-specific native tuning and selected CUDA architecture 89. Those flags describe this build, not a requirement for another GPU.

## Validation and limits

Workspace formatting, Clippy with warnings denied, tests, Cohere CLI tests, generated CLI documentation, all six published package layouts, and a fresh CUDA installation were verified. The new queue tests cover preparation overlap, cancellation while waiting for capacity, and result ordering across concurrent files. The bundled source archive is checksum-verified before extraction, and its patches are tested during package preparation.

All 13 benchmark cases finished with no failed jobs, missing or empty outputs, invalid subtitle times, text/subtitle mismatches, or logged inference warnings. The bundled runtime also passed the opt-in real-model CPU smoke test with two concurrent recognition calls. Patched production runs peaked at 3,598 MiB of whole-device GPU use for Q8_0 and 6,266 MiB for BF16; these include desktop use.

Native-to-native tensor checks passed all 22 existing tolerances. The upstream March-model reference suite with the July Arabic model fails the same 14/22 thresholds in unmodified and patched native 0.4.1, and both have the same unsupported-timestamp result-clearing failure. These are not reported as clean upstream suite passes. tafrigh requests no native timestamps. The changed CUDA path was tested on the RTX 4070; this does not establish performance on every GPU or other acceleration backend.
