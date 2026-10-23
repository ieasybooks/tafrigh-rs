use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn hex(bytes: impl AsRef<[u8]>) -> String {
    let mut value = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        write!(&mut value, "{byte:02x}").unwrap();
    }
    value
}

/// Extract verified upstream source into the build directory and apply the local patches.
/// Neither the Cargo registry nor the checkout is modified, and no network is used.
pub fn source(native: &Path, destination: &Path) -> Result<PathBuf> {
    let metadata_bytes = fs::read(native.join("upstream.json"))?;
    let metadata: serde_json::Value = serde_json::from_slice(&metadata_bytes)?;
    let field = |key: &str| -> Result<&str> {
        metadata[key]
            .as_str()
            .ok_or_else(|| io::Error::other(format!("missing upstream {key}")).into())
    };
    let archive = fs::read(native.join(field("archive")?))?;
    let actual = hex(Sha256::digest(&archive));
    if actual != field("sha256")? {
        return Err(io::Error::other("bundled transcribe.cpp archive SHA-256 mismatch").into());
    }
    let mut fingerprint = Sha256::new();
    fingerprint.update(&metadata_bytes);
    fingerprint.update(actual.as_bytes());
    let mut patches = Vec::new();
    for entry in metadata["patches"].as_array().ok_or("missing patch list")? {
        let path = entry["path"].as_str().ok_or("missing patch target")?;
        let patch =
            fs::read_to_string(native.join(entry["patch"].as_str().ok_or("missing patch file")?))?
                .replace("\r\n", "\n");
        fingerprint.update(patch.as_bytes());
        patches.push((path, patch));
    }
    let fingerprint = hex(fingerprint.finalize());
    let stamp = destination.join("source.sha256");
    let source = destination.join(format!("transcribe-cpp-sys-{}", field("version")?));
    if source.is_dir() && fs::read_to_string(&stamp).ok().as_deref() == Some(&fingerprint) {
        return Ok(source);
    }
    if destination.exists() {
        fs::remove_dir_all(destination)?;
    }
    fs::create_dir_all(destination)?;
    tar::Archive::new(GzDecoder::new(archive.as_slice())).unpack(destination)?;
    for (path, patch) in patches {
        let target = source.join(path);
        let original = fs::read_to_string(&target)?;
        let patch = diffy::Patch::from_str(&patch)?;
        let patched = diffy::apply(&original, &patch).map_err(|error| {
            io::Error::other(format!("native patch for {path} failed: {error}"))
        })?;
        fs::write(target, patched)?;
    }
    fs::write(stamp, fingerprint)?;
    Ok(source)
}
