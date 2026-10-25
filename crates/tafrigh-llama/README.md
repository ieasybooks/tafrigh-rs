# tafrigh-llama

Bundled llama.cpp and its multimodal library (mtmd), using the ggml runtime built by `tafrigh-transcribe`. The Rust library is named `llama_cpp_2` and exposes the upstream `llama-cpp-2` 0.1.160 API with mtmd encoder `flash_attn_type` and `warmup` parameters. This is a downstream package for tafrigh's upcoming Audar backend; it does not add a recognizer or CLI option.

A normal Cargo build verifies the two unchanged crates.io archives, extracts them into `OUT_DIR`, applies the binding patches, native logger patch and path adaptations, generates raw bindings, and builds only llama and mtmd. The direct `tafrigh-transcribe` dependency supplies `DEP_TRANSCRIBE_LIB_DIR` and `DEP_TRANSCRIBE_INCLUDE_DIR`; CMake uses its installed ggml package and headers automatically. There is no separate prefix setup, network download in the build script, registry-cache modification, or second ggml build. Both runtimes link the same ggml and backend libraries. CUDA's toolkit link dependencies belong to `tafrigh-transcribe`, so this crate does not add static cuBLAS.

The one native change removes `ggml_log_set` from `llama_log_set`: shared ggml logging belongs to `tafrigh-transcribe`, so loading llama while Cohere runs cannot replace ggml's callback and user-data pair. `send_logs_to_tracing` initializes that shared sink first, then installs only llama and mtmd's tracing loggers. The first call to transcribe's `init_logging` or `disable_logging` wins for the process; subsequent calls, including before each model load, do nothing. Native inference is unchanged.

Use the package's actual name in Cargo dependencies; Rust code imports `llama_cpp_2`:

```toml
[dependencies]
tafrigh-llama = "0.4.0"
```

Default features are `mtmd` (the safe multimodal API) and `metal` (forwarded to `tafrigh-transcribe`, effective on Apple targets). `cuda` forwards to `tafrigh-transcribe/cuda`. `--no-default-features` disables Metal and the safe mtmd module; both native libraries are still built. `sampler` retains the upstream sampler feature. Upstream `common`, llguidance, dynamic linking and separate ggml/backend features are not exposed. Do not combine this crate with `llama-cpp-sys-2`; both declare `links = "llama"`.

Build requirements are the pinned Rust toolchain, CMake, a C++ compiler, and libclang with its builtin C headers. CUDA additionally needs the toolkit; Apple builds use the SDK and Metal setup supported by `tafrigh-transcribe`. Set `LIBCLANG_PATH` if bindgen cannot locate libclang. If a libclang installation lacks builtin headers (`stddef.h` or similar), install the matching Clang resource headers or supply an appropriate include directory through `BINDGEN_EXTRA_CLANG_ARGS`. For a native Linux GCC toolchain, `export BINDGEN_EXTRA_CLANG_ARGS="-isystem $(gcc -print-file-name=include)"` can supply those headers. For cross compilation use the target SDK/sysroot instead. No machine-specific path is embedded in the crate. Backend architecture settings still go through `TRANSCRIBE_CMAKE_ARGS`, for example `-DCMAKE_CUDA_ARCHITECTURES=75`.

Run `cargo test -p tafrigh-llama --locked` for archive, patch, upstream binding and combined native link checks. To exercise a real Audar Flash Q8_0 model with the bundled Arabic speech fixture, also install ffmpeg and run:

```sh
TAFRIGH_TEST_AUDAR_MODEL=/path/to/Audar-ASR-V1-Flash-Q8_0.gguf \
TAFRIGH_TEST_AUDAR_MMPROJ=/path/to/mmproj-Audar-ASR-V1-Flash.gguf \
cargo test -p tafrigh-llama --locked --test real_model -- --nocapture
```

Add `--features cuda` to test the shared CUDA backend. The test uses three CPU threads, a 2,048-position context, greedy decoding, the Audar Arabic prefill, and a 128-token bound for the short fixture. Models are never downloaded by tests. Their licenses are separate from this source package; no weights are included.

`native/upstream.json` records archive URLs, versions, registry checksums, source commits, license sources, patches and binding adaptations. Run `mise exec -- sh scripts/update-llama.sh 0.1.160 --check` from the repository root to reproduce the bundle without changing it; omit `--check` to refresh or supply a new matching release version. The updater checks both registry identities, checksums and the patch before replacing managed files. On an upstream update, review the dependency versions, ggml ABI compatibility with the pinned transcribe runtime, and licenses, then rerun CPU/CUDA model checks and package verification. An incompatible patch stops the updater before it changes files.

The package includes all native source, binding source, patches, licenses and test fixtures. `cargo package -p tafrigh-llama --locked --offline` can verify it after Cargo dependencies are cached and the matching `tafrigh-transcribe` version is available from the registry (or a local package-verification source). Native compilation itself is offline. Publish `tafrigh-transcribe` first, then this crate. Its first publication needs the token/bootstrap step in the repository's release guide before trusted publishing can be configured. This crate follows tafrigh's release version independently of the bundled upstream version.

See `THIRD-PARTY-LICENSES.md` for the MIT llama.cpp runtime, MIT OR Apache-2.0 Rust bindings, bundled third-party notices and the speech fixture.

`MtmdContext::init_from_file_with_device` selects an encoder device by its ggml index from `list_llama_ggml_backend_devices`, checking the index before initialization. This exposes the existing native device field without changing inference settings; the original initializer keeps automatic selection.
