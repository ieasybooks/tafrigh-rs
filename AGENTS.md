# AGENTS.md

Read `README.md` for setup and project context. Use the toolchain pinned in `rust-toolchain.toml`. Install configured development tools with `mise install`.

Building the workspace includes the patched transcribe.cpp runtime through `tafrigh-transcribe` and requires CMake (pinned in mise) and a C++ compiler. `tafrigh-cohere` uses that runtime. Model-free tests need no Cohere weights; set `TAFRIGH_TEST_COHERE_MODEL` to a local GGUF to enable the real-model test.

## Workflow and communication

- Check relevant code and documentation before asking factual questions. Ask the user about intent and preferences when those remain unclear.
- Do not invent requirements or silently choose unclear behavior. Ask before deciding output names, defaults, fallback behavior, supported options, or compatibility changes when the user has not settled them. Pause work that depends on the answer while continuing independent, authorized work.
- Ask focused questions early. Give concise choices and explain their consequences. Clearly distinguish recommendations from approved decisions.
- Carry the user's answers forward as constraints. Do not reopen settled decisions or treat silence as approval.
- Use simple English in explanations, comments, and discussions. Keep progress updates focused on what you learned, what needs a decision, and what remains to verify. Avoid narrating every command.
- Before declaring completion, challenge the approach, review the final diff for bugs and unrelated changes, and verify the requested behavior with realistic inputs where appropriate. Report what changed, what was tested, and any remaining uncertainty.
- If a repeated mistake suggests a useful rule for `AGENTS.md`, propose the specific improvement and wait for user approval before adding it.

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

- The root package is `tafrigh-cli`, producing the `tafrigh` binary. Core logic lives in `crates/tafrigh`; Wit integration lives in `crates/tafrigh-wit`, and YouTube captions live in `crates/tafrigh-youtube`.
- Reuse workspace dependencies and lint settings from `Cargo.toml`.
- CLI declarations live in `src/cli.rs`. After CLI changes, regenerate `docs/cli.md` and `docs/tafrigh.1` with `sh scripts/cli-docs.sh` and include the generated changes. Install the pinned generator with `mise install usage`.
- Silero golden fixtures come from `crates/tafrigh/tests/fixtures/generate.py`. Regenerate them when changing the reference model, reference version, or test cases.
- Use Conventional Commits for commits and PR titles. Release-please manages crate versions and changelogs; follow `.github/RELEASING.md`.
