# tafrigh

Transcribe audio and video with [Wit.ai](https://wit.ai) or, on your own machine, with [Cohere Transcribe Arabic](https://huggingface.co/CohereLabs/cohere-transcribe-arabic-07-2026) or [Audar-ASR V1](https://github.com/AudarAI/Audar-ASR-V1). Or retrieve existing YouTube captions. A Rust rewrite of [tafrigh](https://github.com/ieasybooks/tafrigh).

Works with files, folders, and URLs supported by yt-dlp. Writes txt, srt, vtt, csv, tsv, and json, with optional word-level timings.

## Install

Download the archive for your platform from the [latest release](https://github.com/ieasybooks/tafrigh-rs/releases/latest), extract it, and put `tafrigh` on your `PATH`. Archives include Cohere. On Linux with an NVIDIA GPU, pick the `-cuda12` or `-cuda13` archive that matches your CUDA version ([requirements](docs/guide.md#gpus)).

Or install with Cargo (Rust 1.99 or newer). The `cohere` feature adds Cohere and needs CMake and a C++ compiler; `cuda` also needs the CUDA toolkit:

```sh
cargo install tafrigh-cli --locked
cargo install tafrigh-cli --locked --features cohere
```

Also install:

- [ffmpeg](https://ffmpeg.org/download.html) for Wit, Cohere and Audar transcription. Wit's default Opus uploads need an ffmpeg with libopus (most builds have it).
- [yt-dlp](https://github.com/yt-dlp/yt-dlp/wiki/Installation) for URLs.
- [Deno](https://deno.com) or [Node.js 22+](https://nodejs.org) for YouTube ([Node setup](https://github.com/ieasybooks/tafrigh-rs/blob/main/docs/guide.md#javascript-runtime-for-youtube)).

From this checkout, `mise install ffmpeg yt-dlp` installs the latest stable ffmpeg and latest nightly yt-dlp. Update them with `mise upgrade ffmpeg yt-dlp`.

## Usage

Choose `--transcriber wit`, `cohere`, `audar` or `youtube` explicitly for each run. Backends never switch automatically.

For Wit, create an app on [wit.ai](https://wit.ai) in the language you want to transcribe, then copy its **Client Access Token** from the app's settings. You can supply multiple tokens from apps using the same language to increase throughput.

```sh
export TAFRIGH_WIT_TOKENS=TOKEN_A,TOKEN_B

tafrigh lecture.mp3 --transcriber wit
tafrigh lectures/ --transcriber wit -r -o transcripts -f srt,txt
tafrigh "https://www.youtube.com/playlist?list=…" --transcriber wit --skip-existing
```

Cohere Transcribe Arabic runs on your machine, on its GPU when there is one. The first run downloads the model: 2.4 GB for the default `q8_0`, or 1.6 GB for the smallest, `q4_k_m`.

```sh
tafrigh lecture.mp3 --transcriber cohere
tafrigh lectures/ --transcriber cohere --model q4_k_m -r -o transcripts -f srt,txt
```

Cohere merges speech into chunks of up to 30 s and gives one subtitle cue per chunk, without word timings. See the [Cohere guide](docs/guide.md#cohere-transcribe-arabic) for variants, devices and speed.

Audar-ASR V1 is an optional Arabic backend with Turbo and Flash models. Build it with `cargo install tafrigh-cli --locked --features audar` (CMake, a C++ compiler and libclang required), or `--features audar,cuda` for NVIDIA GPUs. The default Turbo Q8_0 downloads a 2.17 GB decoder and its required 0.64 GB BF16 audio encoder.

```sh
tafrigh lecture.mp3 --transcriber audar
tafrigh lecture.mp3 --transcriber audar --model flash-q8_0
```

Audar merges up to 29 seconds of speech per cue and has no word timings. See the [Audar guide](docs/guide.md#audar-asr-v1) for accuracy, speed, local model pairs and model license limits.

For existing YouTube captions, select one subtitle language. This requires yt-dlp, with no Wit tokens or ffmpeg:

```sh
tafrigh "https://www.youtube.com/watch?v=…" --transcriber youtube --language ar -f txt,srt
```

Manual captions take precedence over original automatic captions. Missing captions fail that item; playlists continue. See the [YouTube captions guide](docs/guide.md#youtube-captions) for word timings and output placement.

Transcripts go beside the input by default. Use `-o` for a mirrored output folder and `-r` to include subfolders. `--skip-existing` skips an item only when all requested outputs exist; existing outputs are otherwise overwritten.

## Documentation

- [User guide](https://github.com/ieasybooks/tafrigh-rs/blob/main/docs/guide.md): output layouts, reusable options, benchmarks, and library examples.
- [CLI reference](docs/cli.md): every option and exit code. Also available through `tafrigh --help`.
- [Man page](docs/tafrigh.1): included in release archives.

Install shell completions, then follow any printed setup instructions (replace `zsh` with your shell):

```sh
tafrigh completions zsh --install
```

## Development

Workspace builds include the Cohere and Audar recognizers and the bundled llama.cpp runtime. They share transcribe.cpp's ggml and need CMake (pinned in mise), a C++ compiler, and libclang with its builtin headers ([native build notes](crates/tafrigh-llama/README.md)). Ordinary tests do not need model weights.

Install the configured tools with mise, then run the checks from the repository root:

```sh
mise install
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

See [AGENTS.md](https://github.com/ieasybooks/tafrigh-rs/blob/main/AGENTS.md) for project conventions and the [development notes](https://github.com/ieasybooks/tafrigh-rs/blob/main/docs/guide.md#development) for CLI documentation, fixtures, and releases.

## License

MIT. The bundled Silero model is also MIT; the bundled FireRedVAD model and normalization statistics are Apache-2.0 ([sources and licenses](crates/tafrigh/assets/NOTICE.firered-vad.md)). Cohere builds include a pinned transcribe.cpp source bundle with local CUDA fixes and ggml, both MIT ([third-party notices](crates/tafrigh-cohere/THIRD-PARTY-LICENSES.md)). The bundled llama.cpp runtime is MIT and its Rust bindings are MIT OR Apache-2.0 ([notices](crates/tafrigh-llama/THIRD-PARTY-LICENSES.md)). The Cohere weights are Apache-2.0 and are downloaded, not bundled.

Audar model weights are downloaded separately: Turbo uses the AudarAI Community License v1.0 and Flash the AudarAI Open License v1.0. See the [Audar license notes](crates/tafrigh-audar/THIRD-PARTY-LICENSES.md) and [native runtime notices](crates/tafrigh-llama/THIRD-PARTY-LICENSES.md).
