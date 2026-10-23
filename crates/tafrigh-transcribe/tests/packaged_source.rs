#[path = "../native/prepare.rs"]
mod prepare;

use std::path::Path;

#[test]
fn bundled_source_is_verified_patched_and_reused() {
    let temporary = tempfile::tempdir().unwrap();
    let native = Path::new(env!("CARGO_MANIFEST_DIR")).join("native");
    let destination = temporary.path().join("source");
    let source = prepare::source(&native, &destination).unwrap();
    let model = std::fs::read_to_string(source.join("src/arch/cohere/model.cpp")).unwrap();
    assert!(model.contains("cc->encoder_use_flash = !is_metal && !is_cuda;"));
    let encoder = std::fs::read_to_string(source.join("src/arch/cohere/encoder.cpp")).unwrap();
    assert!(encoder.contains("policy.contiguous_glu"));
    let convolution = std::fs::read_to_string(source.join("src/conformer/conformer.cpp")).unwrap();
    assert!(convolution.contains("value = ggml_cont(ctx, value);"));
    let marker = source.join("cache-marker");
    std::fs::write(&marker, "retain an unchanged source cache").unwrap();
    assert_eq!(prepare::source(&native, &destination).unwrap(), source);
    assert!(marker.exists());
}

#[test]
fn altered_archive_is_rejected_before_extraction() {
    let temporary = tempfile::tempdir().unwrap();
    let original = Path::new(env!("CARGO_MANIFEST_DIR")).join("native");
    let native = temporary.path().join("native");
    std::fs::create_dir(&native).unwrap();
    std::fs::copy(original.join("upstream.json"), native.join("upstream.json")).unwrap();
    std::fs::write(
        native.join("transcribe-cpp-sys-0.4.1.crate"),
        b"corrupt archive",
    )
    .unwrap();
    let destination = temporary.path().join("source");
    let error = prepare::source(&native, &destination).unwrap_err();
    assert!(error.to_string().contains("SHA-256 mismatch"));
    assert!(!destination.exists());
}
