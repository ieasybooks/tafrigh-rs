#[path = "../native/prepare.rs"]
mod prepare;

use std::path::Path;

fn metadata(native: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(native.join("upstream.json")).unwrap()).unwrap()
}

#[test]
fn bundled_source_is_verified_patched_and_reused() {
    let temporary = tempfile::tempdir().unwrap();
    let native = Path::new(env!("CARGO_MANIFEST_DIR")).join("native");
    assert_eq!(
        transcribe_cpp::compiled_version(),
        metadata(&native)["version"]
    );
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
    let metadata = metadata(&original);
    std::fs::write(
        native.join(metadata["archive"].as_str().unwrap()),
        b"corrupt archive",
    )
    .unwrap();
    let destination = temporary.path().join("source");
    let error = prepare::source(&native, &destination).unwrap_err();
    assert!(error.to_string().contains("SHA-256 mismatch"));
    assert!(!destination.exists());
}

#[test]
fn crlf_patches_prepare_the_same_native_source() {
    let temporary = tempfile::tempdir().unwrap();
    let original = Path::new(env!("CARGO_MANIFEST_DIR")).join("native");
    let metadata = metadata(&original);
    let native = temporary.path().join("native");
    std::fs::create_dir_all(native.join("patches")).unwrap();
    for name in ["upstream.json", metadata["archive"].as_str().unwrap()] {
        std::fs::copy(original.join(name), native.join(name)).unwrap();
    }
    for entry in metadata["patches"].as_array().unwrap() {
        let name = entry["patch"].as_str().unwrap();
        let patch = std::fs::read_to_string(original.join(name)).unwrap();
        std::fs::write(
            native.join(name),
            patch.replace("\r\n", "\n").replace('\n', "\r\n"),
        )
        .unwrap();
    }
    let expected = prepare::source(&original, &temporary.path().join("lf")).unwrap();
    let actual = prepare::source(&native, &temporary.path().join("crlf")).unwrap();
    for entry in metadata["patches"].as_array().unwrap() {
        let name = entry["path"].as_str().unwrap();
        assert_eq!(
            std::fs::read(actual.join(name)).unwrap(),
            std::fs::read(expected.join(name)).unwrap(),
            "{name}"
        );
    }
}
