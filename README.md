# tafrigh

Transcribe audio and video into subtitles and text with [Wit.ai](https://wit.ai). A Rust rewrite of [tafrigh](https://github.com/ieasybooks/tafrigh).

Works with files, folders, and URLs supported by yt-dlp. Writes txt, srt, vtt, csv, tsv, and json, with optional word-level timings.

## Install

Download the archive for your platform from the [latest release](https://github.com/ieasybooks/tafrigh-rs/releases/latest), extract it, and put `tafrigh` on your `PATH`. Or install with Cargo (Rust 1.99 or newer):

```sh
cargo install tafrigh-cli --locked
```

Also install:

- [ffmpeg](https://ffmpeg.org/download.html) for all transcription.
- [yt-dlp](https://github.com/yt-dlp/yt-dlp/wiki/Installation) for URLs.
- [Deno](https://deno.com) or [Node.js 22+](https://nodejs.org) for YouTube ([Node setup](https://github.com/ieasybooks/tafrigh-rs/blob/main/docs/guide.md#javascript-runtime-for-youtube)).

From this checkout, `mise install ffmpeg yt-dlp` installs the latest stable ffmpeg and latest nightly yt-dlp. Update them with `mise upgrade ffmpeg yt-dlp`.

## Usage

Create an app on [wit.ai](https://wit.ai) in the language you want to transcribe, then copy its **Client Access Token** from the app's settings. You can supply multiple tokens from apps using the same language to increase throughput.

```sh
export TAFRIGH_WIT_TOKENS=TOKEN_A,TOKEN_B

tafrigh lecture.mp3
tafrigh lectures/ -r -o transcripts -f srt,txt
tafrigh "https://www.youtube.com/playlist?list=…" --skip-existing
```

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

Install the configured tools with mise, then run the checks from the repository root:

```sh
mise install
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

See [AGENTS.md](https://github.com/ieasybooks/tafrigh-rs/blob/main/AGENTS.md) for project conventions and the [development notes](https://github.com/ieasybooks/tafrigh-rs/blob/main/docs/guide.md#development) for CLI documentation, fixtures, and releases.

## License

MIT. The bundled Silero model is also MIT.
