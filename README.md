# tafrigh

Transcribe audio and video with [Wit.ai](https://wit.ai) or, on your own machine, with [Cohere Transcribe Arabic](https://huggingface.co/CohereLabs/cohere-transcribe-arabic-07-2026). Or retrieve existing YouTube captions. A Rust rewrite of [tafrigh](https://github.com/ieasybooks/tafrigh).

Works with files, folders, and URLs supported by yt-dlp. Writes txt, srt, vtt, csv, tsv, and json, with optional word-level timings.

## Install

Download the archive for your platform from the [latest release](https://github.com/ieasybooks/tafrigh-rs/releases/latest), extract it, and put `tafrigh` on your `PATH`. Archives include Cohere. On Linux with an NVIDIA GPU, pick the `-cuda12` or `-cuda13` archive that matches your CUDA version ([requirements](docs/guide.md#gpus)).

Or install with Cargo (Rust 1.99 or newer). The `cohere` feature adds Cohere and needs CMake and a C++ compiler; `cuda` also needs the CUDA toolkit:

```sh
cargo install tafrigh-cli --locked
cargo install tafrigh-cli --locked --features cohere
```

Also install:

- [ffmpeg](https://ffmpeg.org/download.html) for Wit and Cohere transcription. Wit's default Opus uploads need an ffmpeg with libopus (most builds have it).
- [yt-dlp](https://github.com/yt-dlp/yt-dlp/wiki/Installation) for URLs.
- [Deno](https://deno.com) or [Node.js 22+](https://nodejs.org) for YouTube ([Node setup](https://github.com/ieasybooks/tafrigh-rs/blob/main/docs/guide.md#javascript-runtime-for-youtube)).

From this checkout, `mise install ffmpeg yt-dlp` installs the latest stable ffmpeg and latest nightly yt-dlp. Update them with `mise upgrade ffmpeg yt-dlp`.

## Usage

Choose `--transcriber wit`, `cohere` or `youtube` explicitly for each run. Backends never switch automatically.

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

Workspace builds include the Cohere recognizer, which compiles transcribe.cpp and needs CMake (pinned in mise) and a C++ compiler. Ordinary tests don't need the Cohere weights.

Install the configured tools with mise, then run the checks from the repository root:

```sh
mise install
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

See [AGENTS.md](https://github.com/ieasybooks/tafrigh-rs/blob/main/AGENTS.md) for project conventions and the [development notes](https://github.com/ieasybooks/tafrigh-rs/blob/main/docs/guide.md#development) for CLI documentation, fixtures, and releases.

## License

MIT. The bundled Silero model is also MIT; the bundled FireRedVAD model and normalization statistics are Apache-2.0 ([sources and licenses](crates/tafrigh/assets/NOTICE.firered-vad.md)). Cohere builds include a pinned transcribe.cpp source bundle with local CUDA fixes and ggml, both MIT ([third-party notices](crates/tafrigh-cohere/THIRD-PARTY-LICENSES.md)). The Cohere weights are Apache-2.0 and are downloaded, not bundled.
