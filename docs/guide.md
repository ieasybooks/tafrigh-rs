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
- **`--merge` for speed.** It sends fewer, longer requests. In the benchmark below, it roughly halved runtime with a small accuracy tradeoff.
- **Interrupted?** Rerun the same command with `--skip-existing`; finished items are kept.
- **A chunk that keeps failing** fails its file, so a rerun gives a complete transcript. `--on-chunk-error skip` leaves the chunk out instead.

Exit codes: `0` success, `1` error, `2` invalid arguments, `3` some items failed, `130` interrupted.

## How it works

With `--transcriber wit`:

1. ffmpeg decodes the audio, and [Silero VAD](https://github.com/snakers4/silero-vad) finds the speech in it. ffprobe, normally shipped with ffmpeg, adds progress percentages.
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

Cohere returns punctuated text with one segment per chunk and no word timings. Audio including padding must be shorter than 400 seconds: transcribe.cpp advertises 400 but rejects exactly that, so the limit is one sample less. Chunks shorter than 10 ms, which the model can't accept, give no text. The normal 15-second chunk policy still applies. GPU builds use Metal on Apple or the crate's `cuda` feature on NVIDIA; CPU works elsewhere. `Device::Auto` attempts the available GPU and retries loading on CPU if needed, `Device::Cpu` selects CPU, and `Device::Gpu` requires a GPU. `Device::GpuIndex(N)` selects the zero-based Nth GPU in transcribe.cpp's GPU list and fails if unavailable. `backend()` and `device_name()` show the actual choice. Model or session load failures can trigger automatic CPU fallback; inference failures are reported.

CUDA builds need the NVIDIA CUDA toolkit and its runtime libraries. CMake locates them through its normal toolkit search, including `CUDAToolkit_ROOT` or `CUDA_PATH` for a nonstandard installation. The crate supplies the CUDA runtime, cuBLAS and driver link dependencies omitted by transcribe-cpp-sys 0.4.0's static-library manifest. Cross-compilation and custom static CUDA-runtime configurations are not validated.

Concurrent Cohere calls share one session and collect into batches up to `CohereConfig::batch_size` (8 by default); a partial batch flushes after `batch_wait` (10 ms). CPU runs one request at a time. A larger GPU batch needs more memory and can change numerical results. When the model repeats itself or reaches its token limit, the chunk keeps the text produced until then (transcribe.cpp drops the repeats) and a warning is logged. The small `cargo run -p tafrigh-cohere --release --example transcribe -- MODEL.gguf OUTPUT_DIR INPUT...` example writes text for files and folders using Silero and the core pipeline; add `--device gpu`, `--batch-size 8`, `--threads 4`, or `--chunk-seconds 30 --merge` to compare settings.

Model names are `bf16`, `f16`, `q8_0` (the default), `q6_k`, `q5_k_m` and `q4_k_m`. `CohereConfig::default()` selects Q8_0; `CohereConfig::new("/path/to/model.gguf")` uses an existing file directly. Downloaded variants come from the ungated `handy-computer/cohere-transcribe-arabic-07-2026-gguf` repository at the immutable `MODEL_REVISION`, with each file's SHA-256 and size pinned in code. Weights are Apache-2.0 and are not bundled in binaries.

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
| [`tafrigh`](https://crates.io/crates/tafrigh) | the core: decoding, Silero VAD, chunking, output formats |
| [`tafrigh-wit`](https://crates.io/crates/tafrigh-wit) | the Wit.ai recognizer |
| [`tafrigh-cohere`](../crates/tafrigh-cohere) | local Cohere Transcribe Arabic through transcribe.cpp |
| [`tafrigh-youtube`](../crates/tafrigh-youtube) | existing YouTube captions through yt-dlp |
| [`tafrigh-cli`](https://crates.io/crates/tafrigh-cli) | the `tafrigh` command |

## Differences from the Python version

- Wit.ai speech recognition and existing YouTube captions; Whisper isn't supported yet.
- Silero VAD instead of auditok, so long speech is split at pauses rather than at fixed lengths.
- Subtitle cues follow what Wit hears, so they're shorter.
- `--yt-dlp-args` takes yt-dlp command-line arguments instead of JSON.

## Development

The Rust version is pinned in `rust-toolchain.toml`. Run `mise install` from the repository root to install the configured tools, including ffmpeg, ffprobe, and yt-dlp. Wit tests use local mock servers and need no real credentials.

ffmpeg tracks the latest stable release, while yt-dlp tracks the latest [official nightly build](https://github.com/yt-dlp/yt-dlp#update-channels), including prereleases, through a mise backend alias. Run `mise upgrade ffmpeg yt-dlp` to update installed copies; the other tools keep their configured version requirements. CI installs the same media tools through mise.

The root package is `tafrigh-cli`, with its source in `src/`. The `tafrigh`, `tafrigh-wit`, `tafrigh-youtube` and `tafrigh-cohere` libraries live under `crates/`. All five packages are workspace members and are selected by default when running Cargo from the root. Cohere builds require CMake and a C++ compiler; tests use a real model only when `TAFRIGH_TEST_COHERE_MODEL=/path/to/model.gguf` is set. See [AGENTS.md](../AGENTS.md) for verification commands and project conventions.

After changing the CLI, regenerate `docs/cli.md` and `docs/tafrigh.1` from the repository root:

```sh
mise install usage
sh scripts/cli-docs.sh
```

The script uses the official [Usage CLI generators](https://usage.jdx.dev/cli/) with the version pinned in `mise.toml`. CI installs the same pin and checks that the committed reference matches the current parser.

The Silero test fixtures are committed. To regenerate them, see [`generate.py`](../crates/tafrigh/tests/fixtures/generate.py).

For Conventional Commits, version bumps, and the release PR workflow, see [Releasing](../.github/RELEASING.md).
