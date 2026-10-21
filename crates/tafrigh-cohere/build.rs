//! Supply the CUDA link dependencies missing from transcribe-cpp-sys 0.4.0's manifest.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=native/CMakeLists.txt");
    for variable in ["CMAKE", "CUDAToolkit_ROOT", "CUDA_PATH", "CUDACXX"] {
        println!("cargo:rerun-if-env-changed={variable}");
    }
    if env::var_os("CARGO_FEATURE_CUDA").is_none() {
        return;
    }
    let directory = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("cuda-link");
    let status = Command::new(env::var_os("CMAKE").unwrap_or_else(|| "cmake".into()))
        // Re-discover libraries when switching toolkits in the same Cargo target directory.
        .args(["-UCUDA_*", "-UCUDAToolkit_*"])
        .args(["-S", "native", "-B"])
        .arg(&directory)
        .status()
        .expect("CMake is required to locate the CUDA toolkit libraries");
    assert!(status.success(), "could not locate CUDA toolkit libraries");
    let libraries = std::fs::read_to_string(directory.join("cuda-libraries.txt")).unwrap();
    for line in libraries.lines() {
        println!("cargo:rustc-link-search=native={line}");
    }
    // The Windows SDK names its driver import library cuda.lib (the DLL is nvcuda.dll).
    for library in ["cudart", "cublas", "cublasLt", "cuda"] {
        println!("cargo:rustc-link-lib=dylib={library}");
    }
}
