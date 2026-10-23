# tafrigh-transcribe

The native runtime used by tafrigh's Cohere backend. This package combines the MIT-licensed transcribe.cpp 0.4.1 native source and Rust bindings, with a small set of Cohere CUDA patches maintained in tafrigh-rs. It is a downstream package, not an official transcribe.cpp release. The Rust library keeps the `transcribe_cpp` name so the existing recognizer uses the upstream API.

Normal Cargo builds verify the bundled source archive's SHA-256, extract it into `OUT_DIR`, apply the patches, and compile it through CMake. The build uses no network, does not modify Cargo's registry cache, and needs no `TRANSCRIBE_DIR` override or separate library installation. Source, bindings, licenses and patches are included in the published crate. CMake and a C++ compiler are required; CUDA builds also need the toolkit.

`native/upstream.json` records the source release, commit, registry checksums, binding adaptations and patch targets. The archive is the unchanged crates.io `transcribe-cpp-sys` package. The Rust files are copied from the corresponding official safe and raw binding packages; local adaptations combine their module paths and keep the native ABI version independent of tafrigh's package version. `native/build.rs` preserves the upstream build helper with an explicit source-directory argument.

The CUDA patches select explicit encoder attention because the bundled CUDA Flash Attention implementation does not support Cohere's 160-wide heads and per-head relative-position mask. They also make the GLU sigmoid input contiguous. Both changes prevent per-layer CPU fallback. Decoder Flash Attention and the upstream environment overrides remain available. Other model families keep their existing convolution policy.

To update the runtime, obtain matching native and binding crates from crates.io, verify their registry checksums, replace the archive and copied bindings, update `native/upstream.json` and the ABI version in `src/version.rs`, and rebase the patches. Run package verification, workspace checks, and real-model CPU/CUDA accuracy and throughput comparisons before changing the pin. Never download source or modify Cargo's registry cache at build time.

This crate follows tafrigh's release version. Its bundled native ABI version is currently 0.4.1. The first crates.io publication requires registering this package before enabling trusted publishing, as described in the repository's release guide.
