# AGENTS.md

Read `README.md` for setup and project context. Use the toolchain pinned in `rust-toolchain.toml`. Install configured development tools with `mise install`.

## Verification

Run focused tests while iterating. Before handing off code changes, run these checks from the repository root:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Tests require `ffmpeg` on PATH. Wit tests use local mock servers and need no real credentials. For documentation-only changes, run the relevant checks. Report any checks you could not run.

## Working style

- Prefer simple, straightforward code that is easy to read and maintain. Use clear names and direct control flow. Add abstractions and dependencies only when they simplify current requirements.
- Reproduce bugs before fixing them, address the root cause, and add a regression test when practical. Test observable behavior through the relevant interface.
- Do not hard-wrap prose in documentation or Markdown files. Keep each paragraph on one source line; preserve structural newlines and code-block formatting. Keep generated documentation in the generator's format.
- Keep `README.md` concise: installation, basic usage, and links. Put detailed explanations and advanced examples in `docs/`.
- When adding or upgrading dependencies and tools, verify and use the latest stable releases from official sources, except for explicitly configured release channels such as yt-dlp's official nightly builds. Update the relevant pins and lockfiles. Use committed version requirements for routine development and documentation matching the installed versions.

## Project conventions

- The root package is `tafrigh-cli`, producing the `tafrigh` binary. Core logic lives in `crates/tafrigh`; Wit integration lives in `crates/tafrigh-wit`.
- Reuse workspace dependencies and lint settings from `Cargo.toml`.
- CLI declarations live in `src/cli.rs`. After CLI changes, regenerate `docs/cli.md` and `docs/tafrigh.1` with `sh scripts/cli-docs.sh` and include the generated changes. Install the pinned generator with `mise install usage`.
- Silero golden fixtures come from `crates/tafrigh/tests/fixtures/generate.py`. Regenerate them when changing the reference model, reference version, or test cases.
- Use Conventional Commits for commits and PR titles. Release-please manages crate versions and changelogs; follow `.github/RELEASING.md`.
