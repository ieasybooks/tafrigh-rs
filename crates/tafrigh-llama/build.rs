#[path = "native/prepare.rs"]
mod prepare;

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=native");
    // These upstream branches are intentionally unavailable: this package builds
    // only llama + mtmd with the backends supplied by tafrigh-transcribe.
    println!("cargo:rustc-check-cfg=cfg(feature, values(\"common\", \"llguidance\", \"dynamic-backends\"))");
    let package = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let sources = prepare::source(&package.join("native"), &output.join("source"))
        .expect("verify and prepare the bundled llama.cpp sources");
    let lib =
        PathBuf::from(env::var_os("DEP_TRANSCRIBE_LIB_DIR").expect("transcribe library metadata"));
    let include = PathBuf::from(
        env::var_os("DEP_TRANSCRIBE_INCLUDE_DIR").expect("transcribe include metadata"),
    );
    let ggml = lib.join("cmake/ggml");
    assert!(
        ggml.join("ggml-config.cmake").is_file(),
        "missing ggml CMake package at {}",
        ggml.display()
    );
    let llama = sources.raw.join("llama.cpp");
    let mut config = cmake::Config::new(&llama);
    config
        .profile("Release")
        .define("CMAKE_INSTALL_LIBDIR", "lib")
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("LLAMA_USE_SYSTEM_GGML", "ON")
        .define("ggml_DIR", &ggml)
        .define("LLAMA_BUILD_MTMD", "ON")
        .define("LLAMA_BUILD_COMMON", "OFF")
        .define("LLAMA_BUILD_TESTS", "OFF")
        .define("LLAMA_BUILD_TOOLS", "OFF")
        .define("LLAMA_BUILD_EXAMPLES", "OFF")
        .define("LLAMA_BUILD_SERVER", "OFF")
        .define("LLAMA_BUILD_APP", "OFF")
        .define("LLAMA_OPENSSL", "OFF")
        .define("LLAMA_SUBPROCESS", "OFF")
        .define("MTMD_VIDEO", "OFF");
    let installed = config.build();
    println!(
        "cargo:rustc-link-search=native={}",
        installed.join("lib").display()
    );
    println!("cargo:rustc-link-lib=static=mtmd");
    println!("cargo:rustc-link-lib=static=llama");
    // mtmd's small hashing helper is built but not installed by upstream.
    // Multi-configuration generators (MSVC/Xcode) add the profile directory.
    let hash = installed.join("build/vendor/hash");
    for directory in [&hash, &hash.join("Release")] {
        println!("cargo:rustc-link-search=native={}", directory.display());
    }
    println!("cargo:rustc-link-lib=static=vendor-hash");

    // Use the installed ggml headers for the exact runtime we link. Do not parse
    // wrapper.h: it includes the unused ggml copy from llama-cpp-sys-2's archive.
    bindgen::Builder::default()
        .header(llama.join("include/llama.h").to_string_lossy())
        .header(include.join("gguf.h").to_string_lossy())
        .header(llama.join("tools/mtmd/mtmd.h").to_string_lossy())
        .header(llama.join("tools/mtmd/mtmd-helper.h").to_string_lossy())
        .clang_arg(format!("-I{}", include.display()))
        .clang_arg(format!("-I{}", llama.join("include").display()))
        .derive_partialeq(true)
        .allowlist_item("ggml_.*")
        .allowlist_item("gguf_.*")
        .allowlist_item("llama_.*")
        .allowlist_item("mtmd_.*")
        .allowlist_recursively(false)
        .raw_line("type FILE = ::std::os::raw::c_void;")
        .prepend_enum_name(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new().rerun_on_header_files(false)))
        .generate()
        .expect("generate llama/mtmd bindings; install libclang and its builtin headers, or set BINDGEN_EXTRA_CLANG_ARGS (see README)")
        .write_to_file(output.join("bindings.rs"))
        .expect("write raw bindings");
    println!(
        "cargo:rustc-env=TAFRIGH_LLAMA_SOURCE={}",
        sources.safe.display()
    );
    println!(
        "cargo:rustc-env=TAFRIGH_LLAMA_BINDINGS={}",
        sources.safe.join("src/lib.rs").display()
    );
    println!("cargo:include_dir={}", installed.join("include").display());
    println!("cargo:lib_dir={}", installed.join("lib").display());
}
