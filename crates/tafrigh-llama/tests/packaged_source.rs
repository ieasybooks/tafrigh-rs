//! Verify the exact bundled inputs, patch application, reuse, and corrupt-archive handling.

#[path = "../native/prepare.rs"]
mod prepare;

use std::fs;
use std::path::Path;

#[test]
fn verifies_and_prepares_both_archives() {
    let temporary = tempfile::tempdir().unwrap();
    let native = Path::new(env!("CARGO_MANIFEST_DIR")).join("native");
    let destination = temporary.path().join("source");
    let sources = prepare::source(&native, &destination).unwrap();
    assert!(sources.raw.join("llama.cpp/include/llama.h").is_file());
    let mtmd = fs::read_to_string(sources.safe.join("src/mtmd.rs")).unwrap();
    assert!(mtmd.contains("pub flash_attn_type: i32"));
    assert!(mtmd.contains("pub warmup: bool"));
    assert!(mtmd.contains("crate::sys::mtmd_context_params_default"));
    let marker = destination.join("reuse-marker");
    fs::write(&marker, "reuse").unwrap();
    prepare::source(&native, &destination).unwrap();
    assert!(
        marker.exists(),
        "unchanged source should not be extracted again"
    );

    let local = temporary.path().join("native");
    fs::create_dir(&local).unwrap();
    let metadata = fs::read(native.join("upstream.json")).unwrap();
    fs::write(local.join("upstream.json"), &metadata).unwrap();
    let metadata: serde_json::Value = serde_json::from_slice(&metadata).unwrap();
    for entry in metadata["archives"].as_array().unwrap() {
        let archive = entry["archive"].as_str().unwrap();
        fs::copy(native.join(archive), local.join(archive)).unwrap();
    }
    // Verify every archive even when a successfully prepared tree already exists.
    for entry in metadata["archives"].as_array().unwrap() {
        let archive = entry["archive"].as_str().unwrap();
        fs::write(local.join(archive), b"corrupt").unwrap();
        let error = prepare::source(&local, &destination).err().unwrap();
        assert!(error.to_string().contains("SHA-256 mismatch"), "{error}");
        assert!(
            marker.exists(),
            "failed verification must preserve prepared source"
        );
        fs::copy(native.join(archive), local.join(archive)).unwrap();
    }
}
