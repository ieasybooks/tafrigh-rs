# Changelog

Notable changes to tafrigh are documented in this file.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and releases follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Recursive directory processing with `-r` / `--recursive`, deterministic traversal, and exclusion of directory symlinks and nested output directories.
- `--output-mode side-by-side`, `tree-to-tree`, and `flat`, with one mirrored tree for all formats, separate folders for multiple input roots, and flat URL outputs in tree mode.

### Changed

- **Breaking:** CLI output defaults to beside each media file without `-o`, and a mirrored tree with `-o`. Use `--output-mode flat -o DIR` to retain the previous layout. Side-by-side rejects `-o`; tree-to-tree and flat require it.
- Filename suffixes resolve collisions at the actual destination, so matching names in separate folders remain unchanged. Word-level, uncompacted, and skip-existing outputs follow the selected layout; a run-wide `sample.csv` goes in `-o`, or the current directory in side-by-side mode.

## [0.0.1] - 2026-10-20

### Added

- Initial Rust release with the `tafrigh` core library, `tafrigh-wit` recognizer, and `tafrigh-cli` command-line application.
- Audio and video transcription through Wit.ai from local files, folders, and URLs, including videos, playlists, and channels supported by yt-dlp.
- Streaming audio decoding through ffmpeg and speech detection with Silero VAD v6, with configurable chunking and a fixed-window fallback.
- Concurrent recognition across multiple Wit tokens, with per-token rate limiting, automatic retries, and language detection from the tokens.
- TXT, SRT, VTT, CSV, TSV, and JSON output, with optional word-level timings for subtitle and structured formats.
- Optional speech-chunk merging with `--merge` to reduce recognition requests, and resending of speech gaps that Wit returned no words for.
- Configurable handling of failed chunks, with file failure by default and an option to skip failed chunks.
- Skipping completed items with `--skip-existing` when rerunning interrupted jobs.
- Transcript compaction, optional uncompacted output, and random transcript samples for spot-checking.
- Progress reporting and a Rust API with pluggable segmenters, recognizers, and output formats.
- Release builds for Linux and macOS on x86_64 and ARM64.

[Unreleased]: https://github.com/ieasybooks/tafrigh-rs/compare/v0.0.1...HEAD
[0.0.1]: https://github.com/ieasybooks/tafrigh-rs/releases/tag/v0.0.1
