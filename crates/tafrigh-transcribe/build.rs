#[path = "native/prepare.rs"]
mod prepare;
#[path = "native/build.rs"]
mod upstream;

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=TRANSCRIBE_DIR");
    assert!(
        env::var_os("TRANSCRIBE_DIR").is_none(),
        "tafrigh-transcribe builds its patched source; unset TRANSCRIBE_DIR"
    );
    let package = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    println!("cargo:rerun-if-changed=native");
    let metadata: serde_json::Value = serde_json::from_str(include_str!("native/upstream.json"))
        .expect("parse bundled runtime metadata");
    let version = metadata["version"].as_str().expect("native ABI version");
    println!("cargo:rustc-env=TAFRIGH_TRANSCRIBE_VERSION={version}");
    let source = prepare::source(&package.join("native"), &output.join("source"))
        .expect("verify and prepare the bundled transcribe.cpp source");
    upstream::build(&source);
    cuda_libraries(&output);
}

// The upstream 0.4.1 static link manifest omits CUDA's toolkit dependencies.
// Keep discovery with the crate that owns the native link, including driver stubs
// on build hosts without a GPU. Stub paths are never added to runtime search paths.
fn cuda_libraries(output: &std::path::Path) {
    for variable in ["CMAKE", "CUDAToolkit_ROOT", "CUDA_PATH", "CUDACXX"] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    if env::var_os("CARGO_FEATURE_CUDA").is_none() {
        return;
    }
    let directory = output.join("cuda-link");
    let status = Command::new(env::var_os("CMAKE").unwrap_or_else(|| "cmake".into()))
        .args(["-UCUDA_*", "-UCUDAToolkit_*"])
        .args(["-S", "native/cuda", "-B"])
        .arg(&directory)
        .status()
        .expect("CMake is required to locate the CUDA toolkit libraries");
    assert!(status.success(), "could not locate CUDA toolkit libraries");
    let libraries = std::fs::read_to_string(directory.join("cuda-libraries.txt")).unwrap();
    for line in libraries.lines() {
        println!("cargo:rustc-link-search=native={line}");
    }
    for library in ["cudart", "cublas", "cublasLt", "cuda"] {
        println!("cargo:rustc-link-lib=dylib={library}");
    }
}
