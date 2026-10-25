# User guide

See the [README](../README.md) for installation and basic usage, or the [CLI reference](cli.md) for every option.

## JavaScript runtime for YouTube

yt-dlp supports [Node.js 22 or newer](https://github.com/yt-dlp/yt-dlp/wiki/EJS#node). Install Node, then enable it through tafrigh's yt-dlp options:

```sh
tafrigh "https://www.youtube.com/watch?v=…" --transcriber youtube --language ar --yt-dlp-args="--no-js-runtimes --js-runtimes node"
```

`--no-js-runtimes` clears yt-dlp's default Deno selection so Node is used even when both are installed. To reuse this setting, put the runtime options in your [yt-dlp configuration file](https://github.com/yt-dlp/yt-dlp#configuration), or put the `--yt-dlp-args` option above in a tafrigh argument file.

## YouTube captions

Every transcription command requires `--transcriber wit` or `--transcriber youtube`. YouTube retrieves existing captions through yt-dlp and requires `--language` with one exact language code, such as `ar` or `en-US`. It accepts YouTube video, playlist and channel URLs. It does not require Wit tokens, ffmpeg or Silero, and does not download audio.

```sh
tafrigh "https://www.youtube.com/watch?v=…" --transcriber youtube --language ar -f txt,srt
tafrigh "https://www.youtube.com/playlist?list=…" --transcriber youtube --language ar -o transcripts --skip-existing
tafrigh "https://youtu.be/…" --transcriber youtube --language ar -f srt --words
```

Manual captions in the requested language take precedence over original automatic captions. Automatically translated tracks are not selected. A missing, empty, malformed or failed caption download fails that item, and the run continues with the remaining playlist items. There is no fallback to Wit.

Caption cues are converted into the same segments used by all six output formats. When JSON3 includes usable word start offsets, `--words` also writes word outputs. Each word ends at the next word's start, and the last word ends at the caption's end. Multiword fragments without individual offsets remain caption-level text. If the track has no usable word timings, tafrigh warns and writes empty word outputs, as it does with other transcribers.

Outputs use `<video id>.<format>`, such as `VIDEO_ID.txt` and `VIDEO_ID.words.srt`, with no backend or language suffix. Without `-o`, they go in `--download-dir` or the current directory. With `-o`, they go directly into that output directory. Raw JSON3 and intermediate metadata are temporary; custom yt-dlp output templates do not rename the final transcripts. Samples retain the YouTube link and leave the local media file column empty.

Pass cookies, JavaScript runtime settings and playlist selection through `--yt-dlp-args`. Caption retrieval enforces its download mode, selected track and temporary output paths after those arguments. The YouTube backend rejects explicit audio-processing and Wit options, `--recursive`, `--ffmpeg` and `--ffprobe`, including their negated forms where available.

## Cohere Transcribe Arabic

`--transcriber cohere` runs [Cohere Transcribe Arabic 07-2026](https://huggingface.co/CohereLabs/cohere-transcribe-arabic-07-2026) on your machine through [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp). It recognizes Modern Standard Arabic, the major dialects and Arabic–English code-switching, or English with `-l en`. It needs ffmpeg, and the network only to download the model.

```sh
tafrigh lecture.mp3 --transcriber cohere -f srt,txt
tafrigh lectures/ --transcriber cohere --model q4_k_m -r -o transcripts
```

### Models

`--model` picks a variant, downloaded on first use from [handy-computer/cohere-transcribe-arabic-07-2026-gguf](https://huggingface.co/handy-computer/cohere-transcribe-arabic-07-2026-gguf). tafrigh pins the repository revision and each file's SHA-256, and checks the hash after downloading.

| Variant | Size | WER on FLEURS Arabic |
|---|---|---|
| `bf16` | 4.11 GB | 11.02% |
| `f16` | 4.11 GB | 11.00% |
| `q8_0` (default) | 2.41 GB | 11.06% |
| `q6_k` | 1.97 GB | 11.07% |
| `q5_k_m` | 1.77 GB | 10.95% |
| `q4_k_m` | 1.56 GB | 11.18% |

The error rates come from transcribe.cpp's [model page](https://github.com/handy-computer/transcribe.cpp/blob/main/docs/models/cohere-transcribe-arabic-07-2026.md) (428 Egyptian Arabic clips). See [Speed and accuracy](#speed-and-accuracy) for tafrigh's own measurements.

Models are kept in `tafrigh/models` in the user cache folder: `$XDG_CACHE_HOME` or `~/.cache` on Linux, and `~/Library/Caches` on macOS. Use `--model-dir` to keep them elsewhere, or pass a local `.gguf` file to `--model`.

### GPUs

`--device auto`, the default, uses the GPU this build supports when it works, and the CPU otherwise. `--device cpu` forces the CPU, `--device gpu` fails without a GPU, and `--device gpu:N` picks one of several GPUs, counting from 0. A log line at the start shows where the model runs.

One process shares its loaded model across all input files. `--cohere-batch-size` controls the maximum GPU batch (1–64, default 8); larger batches and longer chunks use more memory. Audio for the next batch can be prepared while the current batch runs, with a bounded queue. CPU inference processes one chunk at a time. The CLI processes at most two files concurrently, so one-chunk files cannot fill batches larger than two; larger batches chiefly help recordings that produce many chunks.

`--threads` controls native inference threads. Omitted or 0 selects automatically: available CPU parallelism minus two on GPU (at least one), or up to eight threads on CPU, capped at the native limit of 512. Available parallelism uses Rust's estimate, including affinity and supported container CPU quotas; if it cannot be determined, the fallback is one. CPU automatic selection now respects that estimate instead of the native runtime's affinity-only count. Explicit CLI values cannot exceed available parallelism or 512. Single-chunk mel preprocessing and other pipeline work use their own threads, so this is not a process-wide thread cap. The startup log shows the actual device, maximum batch size and native inference thread count, including after an automatic fallback to CPU.

| Build | GPU |
|---|---|
| macOS arm64 archive | Metal |
| Linux x86_64 `-cuda12` archive | NVIDIA Turing (GTX 16 and RTX 20 series) or newer, with the NVIDIA driver and the CUDA 12 runtime and cuBLAS (`libcudart.so.12`, `libcublas.so.12`, `libcublasLt.so.12`) |
| Linux x86_64 `-cuda13` archive | NVIDIA Turing (GTX 16 and RTX 20 series) or newer, with driver 580 or newer and the CUDA 13 runtime and cuBLAS |
| Other archives | none; Cohere runs on the CPU |
| `cargo install tafrigh-cli --features cuda` | NVIDIA, built against your CUDA toolkit |

On the CPU, the Linux release archives need AVX2 (Intel since 2013, AMD Zen) on x86_64, or ARMv8.2 with dot-product instructions (such as AWS Graviton2 or Raspberry Pi 5) on arm64. Build from source with Cargo for older CPUs.

Both CUDA archives include native code for Turing, Ampere, Ada, Hopper and Blackwell GPUs; newer GPUs compile it on first use. For older GPUs, build from source with `--features cuda`. The CUDA archives don't include NVIDIA's libraries. Without them, tafrigh doesn't start and reports `error while loading shared libraries: libcublas.so.12` (or `.13`). Newer drivers run both archives, but the CUDA runtime must match the archive's major version.

### Chunks and limits

- Cohere merges consecutive speech into chunks of up to 30 s by default (`--merge --max-chunk 30`). In tafrigh's measurements, that was more accurate and faster than Wit's 15 s chunks; see [Speed and accuracy](#speed-and-accuracy). Use `--no-merge --max-chunk 15` for shorter subtitle cues.
- There are no word timings. Each chunk becomes one subtitle cue, so cues can be as long as the chunk. `--words` and the `words-*` formats are errors with Cohere, and `--resend-gaps` doesn't apply.
- When the model starts repeating itself or reaches its token limit, tafrigh keeps the chunk's text up to that point, with the repeats dropped, and prints a warning.
- Punctuation is always on.
- A chunk, padding included, must be at most 31 s: with the default `--pad 0.5` on each side, `--max-chunk` must be at most 30. Longer inputs can silently omit speech even though transcribe.cpp advertises a 400 s encoder capacity ([upstream issue](https://github.com/handy-computer/transcribe.cpp/issues/128)). tafrigh rejects settings that exceed this limit before loading a model; reduce `--max-chunk` or `--pad`. Long recordings still work: tafrigh splits them into supported chunks.
- The model can invent text for silence and noise. Speech detection removes them, so avoid `--vad none` on noisy recordings.

## Inputs and outputs

Create an app on [wit.ai](https://wit.ai) in the language you want to transcribe, and copy its **Client Access Token** from the app's settings. Then:

```sh
export TAFRIGH_WIT_TOKENS=TOKEN_A,TOKEN_B   # or pass -w TOKEN

tafrigh lecture.mp3 --transcriber wit                           # every format, beside the media file
tafrigh lectures/ --transcriber wit -o transcripts -f srt,txt    # mirror the input folder
tafrigh "https://www.youtube.com/playlist?list=…" --transcriber wit --skip-existing
tafrigh lecture.mp3 --transcriber wit -f srt --words            # also lecture.words.srt, one cue per word
tafrigh lectures/ --transcriber wit -r -o transcripts           # include nested folders
tafrigh lectures/ --transcriber wit -r -o transcripts --output-mode flat
tafrigh lectures/ --transcriber wit --merge                     # fewer, longer requests
```

Directories are scanned one level deep by default. Use `-r` / `--recursive` to include subdirectories. Recursive discovery skips directory symlinks and an output directory nested inside an input directory.

Outputs are named `<name>.<format>`. Choose their destination with `--output-mode`:

| Mode | Destination | `-o` |
|---|---|---|
| `side-by-side` | Beside each local or downloaded media file | Rejected; this is the default without `-o` |
| `tree-to-tree` | One mirrored tree with every format together | Required; this is the default with `-o` |
| `flat` | Every transcript directly in the output folder | Required |

For example, `tafrigh lectures/ --transcriber wit -r -o out` writes `lectures/course/01.mp3` as `out/course/01.srt`. With multiple directory inputs, each root gets its own folder: `tafrigh lectures/ interviews/ --transcriber wit -r -o out` writes under `out/lectures/` and `out/interviews/`. Roots with the same name get numeric suffixes. Individually supplied files and URL inputs go directly in `out` in tree mode.

Wit audio downloads go to `--download-dir` when given, otherwise to `-o` or the current directory. Side-by-side URL transcripts follow the actual downloaded file's directory and basename, including custom yt-dlp output templates. In tree and flat modes, URL transcripts use the video ID. YouTube caption placement is described [above](#youtube-captions).

Names gain `-2`, `-3`, and so on when their destination files collide, ignoring case. Inputs are resolved before transcription so mirrored directories take precedence over conflicting transcript names. Files in different destination folders keep their names. The same placement applies to word-level and uncompacted outputs. An unusable item destination is reported as a failure while the remaining items continue.

`--sample N` writes one sample CSV for the run, in `-o` or in the current directory for side-by-side mode. It uses `sample.csv`, or `sample-2.csv`, `sample-3.csv`, and so on when the path is needed by a transcript or directory.

To retain the flat layout from version 0.0.1, pass `--output-mode flat -o DIR` (use `-o .` for the current directory). Run `tafrigh --help` for every option.

## Resuming

`--skip-existing` skips an item only when every requested output file exists, including word-level and uncompacted variants. If any requested file is missing, the item is transcribed again and its requested outputs are rewritten. Without `--skip-existing`, existing files at the selected destinations are overwritten. Skipping checks filenames only; it does not verify the source or contents.

When resuming, keep the same inputs, order, and output settings. Changes to which colliding inputs are discovered or downloaded can change their numeric suffixes between runs.

Backend and language are not part of filenames. Use separate output directories when keeping multiple languages or comparing Wit and YouTube results; `--skip-existing` treats an existing file as complete regardless of which backend or language produced it.

## Help and shell completions

The CLI uses [Usage](https://usage.jdx.dev/rust/). Transcription is the default command: `tafrigh lecture.mp3 --transcriber wit` and `tafrigh transcribe lecture.mp3 --transcriber wit` do the same thing. Use `-h` for brief help, `--help` for detailed help with examples, or read the [complete CLI reference](cli.md). A [man page](tafrigh.1) is also included in release archives (`man ./docs/tafrigh.1`). Help, completions and specification export work without selecting a backend, Wit tokens or media tools.

Generate a completion script or install it into your shell's user directory:

```sh
tafrigh completions zsh --install --dry-run  # show the destination and setup instructions
tafrigh completions zsh --install           # install, then follow any printed instructions
tafrigh completions bash > tafrigh.bash     # alternatively, save the script yourself
```

Supported shells: `bash`, `elvish`, `fish`, `nu` (Nushell), `powershell`, and `zsh`. Completions suggest flags, format and mode values, and files or directories where appropriate. The scripts call `tafrigh` directly, so completion does not require the standalone `usage` program. Installation updates scripts previously generated by Usage and refuses to overwrite unrelated files; it never edits shell startup files.

## Reusable options and input lists

Use `@FILE` to read arguments from a UTF-8 file. For example, save this as `arabic.args`:

```text
--transcriber wit
--language ar
--format srt,txt
--merge
--output-dir transcripts
```

Then run a batch with optional overrides:

```sh
tafrigh @arabic.args lectures/ -r
tafrigh @arabic.args "lecture one.mp3" --output-dir another-folder
```

Files contain shell-style words: quotes preserve spaces, and `#` starts a comment. They can contain inputs as well as options, and include other argument files with `@relative/path.args`. Nested argument-file paths resolve beside the containing file; media and output paths resolve from the working directory. Cycles and nesting beyond 16 files are rejected. Escape a literal leading `@` with `@@`, for example `tafrigh @@recording.wav --transcriber wit`. These files are parsed as arguments; shell commands and environment-variable substitutions are not executed.

Repeated scalar options use the last value, so command-line flags after an argument file override its settings. Repeated list options such as `--format` and `--wit-token` accumulate. Boolean transcription flags have a `--no-…` counterpart, so `tafrigh @arabic.args lecture.mp3 --no-merge` turns off merging from the preset. Unknown flags and invalid option combinations are rejected before transcription starts. Every option supplied on the command line or in an argument file must be used by the selected transcriber; negated flags count too. For example, `--merge` and `--no-merge` both fail with `--transcriber youtube`.

You can also set `TAFRIGH_FFMPEG`, `TAFRIGH_FFPROBE`, and `TAFRIGH_YT_DLP`; explicit flags take precedence over these defaults. Keep tokens in `TAFRIGH_WIT_TOKENS` when using reusable option files. Environment defaults do not count as supplied options and are used only by backends that need them, so Wit tokens and audio-tool paths in your environment do not affect YouTube captions. Token environment values are hidden from help and specification output.

## CLI specification

`tafrigh spec` (also available through Usage's `__usage_spec__` endpoint) prints a machine-readable KDL definition of commands, flags, examples, completion hints and exit codes. It is generated from the same Rust declarations as the parser. With the optional [Usage CLI](https://usage.jdx.dev/cli/), you can generate other artifacts:

```sh
tafrigh spec > tafrigh.usage.kdl
usage generate markdown -f tafrigh.usage.kdl --html-encode --out-file reference.md
usage generate manpage -f tafrigh.usage.kdl > tafrigh.1
```

## Tips

- **More tokens, faster runs.** tafrigh starts one request per second per token by default; additional tokens can increase throughput. All tokens must belong to apps of the same language. Use `-l` / `--language` to set that language explicitly, or omit it to detect the language from the tokens.
- **`--merge` for speed with Wit.** It sends fewer, longer requests. In the benchmark below, it roughly halved runtime with a small accuracy tradeoff.
- **Quran recitation.** Silero scores some recitation, especially melodic (mujawwad) recitation, as silence, and audio it rejects never reaches the recognizer. The default `--vad-threshold` is therefore 0.15 rather than Silero's own 0.5. On 500 Arabic clips this lowered word errors from 24.4% to 22.3% with Cohere (Quran clips: 24.8% to 16.5%) and from 32.2% to 31.5–32.0% with Wit, with no text produced on 19 minutes of silence, noise and music at either setting. Even 0.15 still skips parts of melodic recitation.
- **Interrupted?** Rerun the same command with `--skip-existing`; finished items are kept.
- **A chunk that keeps failing** fails its file, so a rerun gives a complete transcript. `--on-chunk-error skip` leaves the chunk out instead.

Exit codes: `0` success, `1` error, `2` invalid arguments, `3` some items failed, `130` interrupted.

## Choosing a speech detector

`--vad silero` remains the default. Use `--vad firered` for the streaming [FireRedVAD](https://github.com/FireRedTeam/FireRedVAD) model, which retains more of the Quran recitation that Silero can miss. Both run locally on the CPU in Rust, with bundled weights. `--vad none` sends fixed windows of the entire recording, including silence and noise.

The default onset threshold is 0.15 for Silero and 0.4 for FireRed. `--vad-threshold`, `--min-speech-ms`, `--min-silence-ms`, `--speech-pad-ms`, and `--max-chunk` apply to the selected detector. Both use an exit threshold 0.15 below onset (at least 0.01), keep speech lasting at least 500 ms, wait for 300 ms silence, and keep 250 ms of source context on each side. `--pad` adds separate synthetic silence before recognition. Backend chunking defaults still apply: Cohere merges up to 30 s; Wit uses up to 15 s without merging.

In the 500-clip Arabic evaluation with Cohere `q8_0` and 30 s merged chunks, this Rust FireRed implementation gave 21.44% word error rate, versus 22.33% for Silero at its default 0.15 and 24.40% at 0.5. On four long recitations FireRed scored 15.99%, versus 41.57% and 57.41% for Silero at 0.15 and 0.5. All three settings produced zero chunks on nine non-speech controls totaling about 19 minutes. FireRed recovered missing words, but some clips acquired extra words; the advantage was concentrated in difficult recitation, with ordinary-speech accuracy close to Silero at 0.15. These results use Cohere recognition and do not establish Wit accuracy or performance on sung audio.

On an i5-8500, processing 510.4 s of audio took a median of 2.31 s with FireRed and 1.64 s with Silero at 0.5, excluding model loading and decoding. With a GTX 1660 SUPER running Cohere `q8_0` (GPU batch 8, four threads, 30 s merged chunks, `--pad 0.5`), complete CLI medians were 214.4 s for FireRed versus 198.9 s for Silero at 0.5 on 79.1 minutes of balanced audio, and 158.7 s versus 158.3 s on a 69.3-minute lecture. Silero at 0.15 took 7.9% longer than at 0.5 on the balanced audio. Each median uses three runs; complete CLI differences include the extra speech retained for recognition.

```sh
tafrigh recitation.mp3 --transcriber cohere --vad firered
tafrigh lecture.mp3 --transcriber wit --vad firered
```

## How it works

With `--transcriber wit`:

1. ffmpeg decodes the audio, and the selected detector ([Silero VAD](https://github.com/snakers4/silero-vad) by default) finds the speech in it. ffprobe, normally shipped with ffmpeg, adds progress percentages.
2. The speech is cut into chunks of up to 15 s by default, padded with silence, encoded as Opus at 24 kbps, and sent to Wit across all tokens. Opus requests are about a twelfth of the size of WAV, and need an ffmpeg built with libopus, as most builds are. `--wit-audio wav` sends WAV instead.
3. When Wit returns no words for a stretch of speech, that stretch is sent again on its own (`--resend-gaps`).
4. When requests time out or Wit returns HTTP 408 or a server error, fewer requests are sent at once, so an overloaded network or Wit gets time to recover. Successful requests raise the limit again, up to `--wit-concurrency` per token. Rate limits are handled separately, by waiting as Wit asks.
5. The next file starts while the current one waits for its last answers. Transcripts are still written in order.

## Speed and accuracy

48 Arabic lectures (1–78 min, 11.7 h in total) with 12 Wit tokens. Errors are measured against YouTube's automatic Arabic captions.

| | Time | Word errors | Character errors |
|---|---|---|---|
| Python tafrigh 1.7.8 | 34 min | 16.1% | 8.8% |
| tafrigh | 15 min | 15.6% | 8.4% |
| tafrigh `--merge` | 7 min | 16.0% | 8.9% |

Times depend on how busy Wit is. These runs sent WAV. With Opus, the default since then, 150 Arabic clips with human transcripts (from FLEURS, Common Voice, SADA, Casablanca and a Quran corpus) gave 26.9% word errors, against 27.7% with WAV, with word timings within 5 ms.

### Cohere

#### GTX 1660 SUPER review measurements

The [review of the performance stack at `9ffd7f7`](https://github.com/ieasybooks/tafrigh-rs/pull/44#issuecomment-6373249159) reports the following on a GTX 1660 SUPER (6 GiB), Core i5-8500 and CUDA 12.9. It uses the public [69-minute Arabic grammar lecture](https://www.youtube.com/watch?v=Cdle09QLPLI) and the 500-clip, five-source corpus described below, with the CLI defaults including `--pad 0.5`. These are reviewer measurements, not reruns of the revised commit. Lecture times are medians of three passes; clip accuracy is one pass per setting.

| Setting | Lecture, main → stack | WER, main → stack | CER, main → stack |
|---|---:|---:|---:|
| Q8_0, batch 8, automatic threads | 259.6 → 156.2 s | 24.43% → 24.40% | 12.60% → 12.60% |
| Q4_K_M, batch 8, automatic threads | 253.2 → 154.3 s | 24.64% → 24.59% | 12.70% → 12.60% |

Q8_0 took 286.8 → 194.1 seconds for the 500 clips, with lecture GPU use of 2,948 → 3,000 MiB. Paired 95% intervals for WER changes span zero: −0.17 to +0.09 percentage points for Q8_0 and −0.23 to +0.18 for Q4_K_M. The same 12 hard clips were empty in both builds. Batch 8 balanced speed and memory on this GPU; larger batches did not help. These results do not establish accuracy equivalence on every recording.

#### RTX 4070 measurements

On an RTX 4070 (12 GiB) and Core i7-14700F, 16 private recordings totaling 7.33 audio hours gave these times, including process startup, model loading, FFmpeg decoding, VAD, inference and output writing. All runs below use `--pad 0`, rather than the CLI default of 0.5 seconds on each side:

| Build and settings | GPU processes | Seconds per audio hour |
|---|---:|---:|
| Original 0.4.0, Q8_0, batch 8, automatic CPU threads | 1 | 58.00 |
| Current Q8_0, batch 8, automatic CPU threads | 1 | 13.52 |
| Original 0.4.0, BF16, batch 8 per process, automatic CPU threads | 2 | 47.02 |
| Current BF16, batch 32, 8 CPU threads | 1 | 12.12 |

These are medians of three passes except the original Q8_0 baseline, which is one pass. The BF16 comparison changes process count and tuning as well as the implementation. Downloads and application imports are excluded. See the [benchmark report](https://github.com/ieasybooks/tafrigh-rs/blob/main/docs/benchmarks/cohere-cuda-2026-10-23.md) for the settings, accuracy checks on 428 human-transcribed FLEURS clips, reproducible error-count calculations, output checks and limits. Benchmark artifacts are kept in the repository rather than the published CLI crate.

Q8_0, batch 8 and automatic threads remain the defaults. To try the measured BF16 tuning on a GPU with enough memory:

```sh
tafrigh recordings/ --transcriber cohere --device gpu --model bf16 \
  --cohere-batch-size 32 --threads 8
```

#### Earlier GTX 1660 SUPER benchmark

The following measurements compare tafrigh 0.4.0, before the CUDA fixes, with [cohere-transcribe](https://github.com/AliOsm/cohere-transcribe) 0.1.4, a Python package for the same model. They use the same lecture and 500 clips as the GTX 1660 SUPER review measurements above, but come from an earlier session. The [follow-up review](https://github.com/ieasybooks/tafrigh-rs/pull/44#issuecomment-6376403619) reports timing drift of about 3% on this machine; compare rows within each table. These runs used a GeForce GTX 1660 SUPER (6 GB) with a 6-core Intel Core i5-8500. The lecture is the 69-minute [Arabic grammar lecture](https://www.youtube.com/watch?v=Cdle09QLPLI) from cohere-transcribe's own benchmarks. Errors are measured on 500 Arabic clips (79 min), 100 each from FLEURS, Common Voice 18, SADA22, Casablanca and a Quran recitation corpus, with the text normalization from cohere-transcribe's benchmarks. Lecture times are medians of three fresh runs, including start-up and model loading; tafrigh's 500-clip times are single runs.

| | Lecture | 500 clips | Word errors | Character errors | GPU memory |
|---|---|---|---|---|---|
| cohere-transcribe, FP16, batch 17 | 471 s | 537 s | 36.1% | 25.5% | 5,706 MiB |
| tafrigh `q8_0` (default) | 251 s | 282 s | 24.4% | 12.6% | 2,948 MiB |
| tafrigh `q4_k_m` | 253 s | 270 s | 24.6% | 12.7% | 2,136 MiB |

tafrigh used its defaults, with speech merged into chunks of up to 30 s. With 15 s chunks and no merging, `q8_0` took 279 s on the lecture and made 27.4% word errors. The GTX 1660 has no BF16 support, so cohere-transcribe ran in FP16 with its largest batch that fits in 6 GB; its repetition guard left 81 of the 500 transcripts empty, which explains much of its error rate. On GPUs with BF16 and tensor cores, such as the RTX 3060 it was tuned on, cohere-transcribe is much faster than here.

## Using it as a library

```rust
use tafrigh::segment::silero::{SileroConfig, SileroSegmenter};
use tafrigh::{ChunkOptions, ChunkedTranscriber, NoProgress, Transcriber};
use tafrigh_wit::{WitConfig, WitRecognizer};

let recognizer = WitRecognizer::new(WitConfig::new(["TOKEN_A", "TOKEN_B"])).await?;
let segmenter = SileroSegmenter::new(SileroConfig::default())?;
let transcriber = ChunkedTranscriber::new(segmenter, recognizer, ChunkOptions::default())?;
let segments = transcriber.transcribe("lecture.mp3".as_ref(), &NoProgress).await?;
```

To process files, folders and URLs into transcript files, use `tafrigh::Tafrigh::builder`. The library writes flat output into the current directory by default; use `.output_mode(OutputMode::TreeToTree(dir))`, `OutputMode::Flat(dir)` or `OutputMode::SideBySide` to choose another layout, `.output(OutputWriter::new().formats([BuiltinFormat::Srt]))` to choose formats, and `.recursive(true)` to include subdirectories.

Custom chunk recognizers declare whether their results carry word timings through `ChunkRecognizer::word_timings()` (true by default). For a recognizer without word timings, set `ChunkOptions::resend_gaps` to `None`; `ChunkedTranscriber::new` rejects gap resends because it cannot distinguish recognized speech from missed speech without word timings. Untimed text becomes one segment covering its chunk.

The `tafrigh-cohere` library recognizes Arabic (`ar`, the default) or English (`en`) locally with Cohere Transcribe Arabic 07-2026. Select a variant or load a local `.gguf` file and pair it with the same core pipeline:

```rust
use tafrigh::segment::silero::{SileroConfig, SileroSegmenter};
use tafrigh::{ChunkOptions, ChunkedTranscriber, NoProgress, Transcriber};
use tafrigh_cohere::{CohereConfig, CohereRecognizer};

let recognizer = CohereRecognizer::load(CohereConfig::new("q8_0")).await?;
let segmenter = SileroSegmenter::new(SileroConfig::default())?;
let options = ChunkOptions { resend_gaps: None, ..ChunkOptions::default() };
let transcriber = ChunkedTranscriber::new(segmenter, recognizer, options)?;
let segments = transcriber.transcribe("lecture.mp3".as_ref(), &NoProgress).await?;
```

Cohere returns punctuated text with one segment per chunk and no word timings. `MAX_AUDIO_DURATION` and `ChunkRecognizer::max_audio_duration()` limit audio including padding to 31 seconds, or less if the native session reports a smaller capacity. Longer input can silently lose text even below the native encoder's capacity. `ChunkedTranscriber::new` rejects oversized chunk settings, and direct `recognize` calls reject oversized PCM before queueing inference. Chunks shorter than 10 ms, which the model can't accept, give no text. The normal 15-second library chunk policy still applies; 30-second chunks with the default 0.5-second padding on each side also fit. GPU builds use Metal on Apple or the crate's `cuda` feature on NVIDIA; CPU works elsewhere. `Device::Auto` attempts the available GPU and retries loading on CPU if needed, `Device::Cpu` selects CPU, and `Device::Gpu` requires a GPU. `Device::GpuIndex(N)` selects the zero-based Nth GPU in transcribe.cpp's GPU list and fails if unavailable. `backend()` and `device_name()` show the actual choice. Model or session load failures can trigger automatic CPU fallback; inference failures are reported.

CUDA builds need the NVIDIA CUDA toolkit and its runtime libraries. CMake locates them through its normal toolkit search, including `CUDAToolkit_ROOT` or `CUDA_PATH` for a nonstandard installation. The [`tafrigh-transcribe`](../crates/tafrigh-transcribe) package bundles the pinned native source, Rust bindings and local CUDA patches; ordinary builds need no separate transcribe.cpp installation. It also supplies the CUDA runtime, cuBLAS and driver link dependencies omitted by the upstream static-library manifest. Cross-compilation and custom static CUDA-runtime configurations are not validated.

Concurrent Cohere calls share one session and collect into batches up to `CohereConfig::batch_size` (8 by default); a partial batch flushes after `batch_wait` (10 ms). CPU runs one request at a time. A larger GPU batch needs more memory and can change numerical results. When the model repeats itself or reaches its token limit, the chunk keeps the text produced until then (transcribe.cpp drops the repeats) and a warning is logged. The small `cargo run -p tafrigh-cohere --release --example transcribe -- MODEL.gguf OUTPUT_DIR INPUT...` example writes text for files and folders using Silero and the core pipeline; add `--device gpu`, `--batch-size 8`, `--threads 4`, or `--chunk-seconds 30 --merge` to compare settings.

Model names are `bf16`, `f16`, `q8_0` (the default), `q6_k`, `q5_k_m` and `q4_k_m`. `CohereConfig::default()` selects Q8_0; `CohereConfig::new("/path/to/model.gguf")` uses an existing file directly. Downloaded variants come from the ungated `handy-computer/cohere-transcribe-arabic-07-2026-gguf` repository at the immutable `MODEL_REVISION`, with each file's SHA-256 and size pinned in code. Weights are Apache-2.0 and are not bundled in binaries.

On CPU and Metal, the native runtime can memory-map model weights. Keep the model file unchanged while tafrigh uses it; replace it through a new file and rename instead of overwriting it in place. The built-in downloader already writes a temporary file and renames it.

The default model directory is `$XDG_CACHE_HOME/tafrigh/models` on Linux (falling back to `~/.cache/tafrigh/models`) or `~/Library/Caches/tafrigh/models` on macOS. Set `config.model_dir = Some(path)` to override it; there is no tafrigh-specific cache environment variable. Files live under `cohere-transcribe-arabic-07-2026/`. Downloads stream to `<file>.part`, verify size and SHA-256, then rename into place. A per-file lock coordinates concurrent processes. Failed downloads remove the partial file; a partial file left by process cancellation is restarted on the next attempt. An existing final file with the right size skips downloading and rehashing. Remove that file to force a fresh verified download if it was modified.

`CohereRecognizer::load_with_progress(config, |progress| { /* progress.downloaded, progress.total */ }).await?` reports downloaded bytes for a CLI or application progress bar. `ModelSource::resolve` can also download weights without loading the model. Callbacks should return promptly and are not invoked for local or already cached files.

For captions, construct `tafrigh_youtube::YoutubeCaptions` and pass it to `Tafrigh::captions`, then configure the same output writer and yt-dlp options. Its `run` method accepts YouTube URLs and reports per-item failures through the same `RunReport`.

```rust
use tafrigh::source::Input;
use tafrigh::{NoProgress, Tafrigh};
use tafrigh_youtube::YoutubeCaptions;

let pipeline = Tafrigh::captions(YoutubeCaptions::new("ar")?).build();
let report = pipeline
    .run([Input::parse("https://youtu.be/VIDEO_ID")], &NoProgress)
    .await?;
```

YouTube validation, track selection, JSON3 parsing and word timings live in `tafrigh-youtube`. It implements the core `CaptionProvider` interface for URL resolution and fetching timed text. Core handles output layouts, progress and item failures, and supplies the configured yt-dlp client. Library users who only need captions can disable `tafrigh`'s default features; `tafrigh-youtube` does not enable Silero.

| Crate | What it is |
|---|---|
| [`tafrigh`](https://crates.io/crates/tafrigh) | the core: decoding, Silero and FireRed VAD, chunking, output formats |
| [`tafrigh-wit`](https://crates.io/crates/tafrigh-wit) | the Wit.ai recognizer |
| [`tafrigh-cohere`](../crates/tafrigh-cohere) | local Cohere Transcribe Arabic through transcribe.cpp |
| [`tafrigh-transcribe`](../crates/tafrigh-transcribe) | pinned native runtime and bindings with local CUDA fixes |
| [`tafrigh-youtube`](../crates/tafrigh-youtube) | existing YouTube captions through yt-dlp |
| [`tafrigh-cli`](https://crates.io/crates/tafrigh-cli) | the `tafrigh` command |

## Differences from the Python version

- Wit.ai speech recognition, Cohere Transcribe Arabic on your machine, and existing YouTube captions; Whisper isn't supported yet.
- Silero VAD instead of auditok, so long speech is split at pauses rather than at fixed lengths.
- Subtitle cues follow what Wit hears, so they're shorter.
- `--yt-dlp-args` takes yt-dlp command-line arguments instead of JSON.

## Development

The Rust version is pinned in `rust-toolchain.toml`. Run `mise install` from the repository root to install the configured tools, including ffmpeg, ffprobe, and yt-dlp. Wit tests use local mock servers and need no real credentials.

ffmpeg tracks the latest stable release, while yt-dlp tracks the latest [official nightly build](https://github.com/yt-dlp/yt-dlp#update-channels), including prereleases, through a mise backend alias. Run `mise upgrade ffmpeg yt-dlp` to update installed copies; the other tools keep their configured version requirements. CI installs the same media tools through mise.

The root package is `tafrigh-cli`, with its source in `src/`. The `tafrigh`, `tafrigh-wit`, `tafrigh-youtube`, `tafrigh-cohere` and `tafrigh-transcribe` libraries live under `crates/`. All six packages are workspace members and are selected by default when running Cargo from the root. Cohere builds require CMake and a C++ compiler; tests use a real model only when `TAFRIGH_TEST_COHERE_MODEL=/path/to/model.gguf` is set. See [AGENTS.md](../AGENTS.md) for verification commands and project conventions.

After changing the CLI, regenerate `docs/cli.md` and `docs/tafrigh.1` from the repository root:

```sh
mise install usage
sh scripts/cli-docs.sh
```

The script uses the official [Usage CLI generators](https://usage.jdx.dev/cli/) with the version pinned in `mise.toml`. CI installs the same pin and checks that the committed reference matches the current parser.

The Silero test fixtures are committed. To regenerate them, see [`generate.py`](../crates/tafrigh/tests/fixtures/generate.py).

FireRed feature, probability, and timestamp fixtures also come from the official Python code; see [fixture provenance and regeneration](../crates/tafrigh/tests/fixtures/firered/README.md). The `silero` and `firered` library features are both enabled by default and can be selected independently with `default-features = false`. Both use the same timestamp rules; FireRed carries its model cache across pushes and resets it for each recording.

For Conventional Commits, version bumps, and the release PR workflow, see [Releasing](../.github/RELEASING.md).
