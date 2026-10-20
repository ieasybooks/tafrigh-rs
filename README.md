# tafrigh

Transcribe audio and video into subtitles and text with [Wit.ai](https://wit.ai). A Rust rewrite of [tafrigh](https://github.com/ieasybooks/tafrigh): as accurate as the Python version, and 2.5–5× faster.

- Works on files, folders, and anything [yt-dlp](https://github.com/yt-dlp/yt-dlp) can download (videos, playlists, channels).
- Writes txt, srt, vtt, csv, tsv and json, with optional word-level timings.
- Uses as many Wit tokens as you give it, and resumes interrupted runs with `--skip-existing`.

## Install

Download the archive for your platform from the [latest release](https://github.com/ieasybooks/tafrigh-rs/releases/latest), extract it, and put `tafrigh` on your `PATH`. Or build it with Cargo (Rust 1.99 or newer):

```sh
cargo install tafrigh-cli
```

tafrigh also needs these programs:

| Program | Needed for |
|---|---|
| [ffmpeg](https://ffmpeg.org/download.html) | everything (`ffprobe`, shipped with it, adds progress percentages) |
| [yt-dlp](https://github.com/yt-dlp/yt-dlp/wiki/Installation) | URLs; keep it updated with `yt-dlp -U` |
| [Deno](https://deno.com) | YouTube URLs (yt-dlp needs a JavaScript runtime) |

## Usage

Create an app on [wit.ai](https://wit.ai) in the language you want to transcribe, and copy its **Client Access Token** from the app's settings. Then:

```sh
export TAFRIGH_WIT_TOKENS=TOKEN_A,TOKEN_B   # or pass -w TOKEN

tafrigh lecture.mp3                           # every format, beside the media file
tafrigh lectures/ -o transcripts -f srt,txt    # mirror the input folder
tafrigh "https://www.youtube.com/playlist?list=…" --skip-existing
tafrigh lecture.mp3 -f srt --words            # also lecture.words.srt, one cue per word
tafrigh lectures/ -r -o transcripts           # include nested folders
tafrigh lectures/ -r -o transcripts --output-mode flat
tafrigh lectures/ --merge                     # about 2× faster, see below
```

Directories are scanned one level deep by default. Use `-r` / `--recursive` to include subdirectories. Recursive discovery skips directory symlinks and an output directory nested inside an input directory.

Outputs are named `<name>.<format>`. Choose their destination with `--output-mode`:

| Mode | Destination | `-o` |
|---|---|---|
| `side-by-side` | Beside each local or downloaded media file | Rejected; this is the default without `-o` |
| `tree-to-tree` | One mirrored tree with every format together | Required; this is the default with `-o` |
| `flat` | Every transcript directly in the output folder | Required |

For example, `tafrigh lectures/ -r -o out` writes `lectures/course/01.mp3` as `out/course/01.srt`. With multiple directory inputs, each root gets its own folder: `tafrigh lectures/ interviews/ -r -o out` writes under `out/lectures/` and `out/interviews/`. Roots with the same name get numeric suffixes. Individually supplied files and URL inputs go directly in `out` in tree mode.

Downloads go to `--download-dir` when given, otherwise to `-o` or the current directory. Side-by-side URL transcripts follow the actual downloaded file's directory and basename, including custom yt-dlp output templates. In tree and flat modes, URL transcripts use the video ID.

Names gain `-2`, `-3`, and so on only when their destination files collide, ignoring case. Files in different destination folders keep their names. The same placement applies to word-level and uncompacted outputs. `--skip-existing` skips an item only when every requested output exists; keep the same inputs and order when resuming, since numeric suffixes are assigned in discovery order. One `sample.csv` is written for the run, in `-o` or in the current directory for side-by-side mode.

To retain the flat layout from version 0.0.1, pass `--output-mode flat -o DIR` (use `-o .` for the current directory). Run `tafrigh --help` for every option.

### Tips

- **More tokens, faster runs.** Wit allows each token about one request per second, so speed grows with the number of tokens. All tokens must belong to apps of the same language.
- **`--merge` for speed.** It sends fewer, longer requests: about twice as fast, with the same accuracy as the Python version, but slightly below the default.
- **Interrupted?** Rerun the same command with `--skip-existing`; finished items are kept.
- **A chunk that keeps failing** fails its file, so a rerun gives a complete transcript. `--on-chunk-error skip` leaves the chunk out instead.

Exit codes: `0` success, `1` error, `2` invalid arguments, `3` some items failed, `130` interrupted.

## How it works

1. ffmpeg decodes the audio, and [Silero VAD](https://github.com/snakers4/silero-vad) finds the speech in it.
2. The speech is cut into chunks of up to 15 s, padded with silence, and sent to Wit across all tokens.
3. When Wit returns no words for a stretch of speech, that stretch is sent again on its own (`--resend-gaps`).
4. The next file starts while the current one waits for its last answers. Transcripts are still written in order.

## Speed and accuracy

48 Arabic lectures (1–78 min, 11.7 h in total) with 12 Wit tokens. Errors are measured against YouTube's automatic Arabic captions.

| | Time | Word errors | Character errors |
|---|---|---|---|
| Python tafrigh 1.7.8 | 34–49 min | 16.1% | 8.8% |
| tafrigh | 15 min | 15.6% | 8.4% |
| tafrigh `--merge` | 7 min | 16.0% | 8.9% |

Times depend on how busy Wit is; the Python times are from two runs on different days.

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

To process files, folders and URLs into transcript files, use `tafrigh::Tafrigh::builder`. The library retains flat output by default; use `.output_mode(tafrigh::output::OutputMode::TreeToTree)` or `SideBySide` to choose another layout, and `.recursive(true)` to include subdirectories.

| Crate | What it is |
|---|---|
| [`tafrigh`](https://crates.io/crates/tafrigh) | the core: decoding, Silero VAD, chunking, output formats |
| [`tafrigh-wit`](https://crates.io/crates/tafrigh-wit) | the Wit.ai recognizer |
| [`tafrigh-cli`](https://crates.io/crates/tafrigh-cli) | the `tafrigh` command |

## Differences from the Python version

- Wit.ai only; Whisper isn't supported yet.
- Silero VAD instead of auditok, so long speech is split at pauses rather than at fixed lengths.
- Subtitle cues follow what Wit hears, so they're shorter.
- `--yt-dlp-args` takes yt-dlp command-line arguments instead of JSON.

## Development

The Rust version is pinned in `rust-toolchain.toml`; rustup (or `mise install`) fetches it. The tests need ffmpeg.

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

The Silero test fixtures are committed. To regenerate them, see `crates/tafrigh/tests/fixtures/generate.py`.

## License

MIT. The bundled Silero model is also MIT; see `crates/tafrigh/assets/`.
