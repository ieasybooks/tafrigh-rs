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

pub struct Sources {
    pub raw: PathBuf,
    pub safe: PathBuf,
}

/// Verify both unchanged registry archives, then extract and adapt them in OUT_DIR.
pub fn source(native: &Path, destination: &Path) -> Result<Sources> {
    let metadata_bytes = fs::read(native.join("upstream.json"))?;
    let metadata: serde_json::Value = serde_json::from_slice(&metadata_bytes)?;
    let version = metadata["version"].as_str().ok_or("missing version")?;
    let sources = Sources {
        raw: destination.join(format!("llama-cpp-sys-2-{version}")),
        safe: destination.join(format!("llama-cpp-2-{version}")),
    };
    let mut fingerprint = Sha256::new();
    fingerprint.update(&metadata_bytes);
    fingerprint.update(include_bytes!("prepare.rs"));
    let mut archives = Vec::new();
    for entry in metadata["archives"].as_array().ok_or("missing archives")? {
        let name = entry["archive"].as_str().ok_or("missing archive name")?;
        let archive = fs::read(native.join(name))?;
        let actual = hex(Sha256::digest(&archive));
        if Some(actual.as_str()) != entry["sha256"].as_str() {
            return Err(io::Error::other(format!("{name}: SHA-256 mismatch")).into());
        }
        fingerprint.update(actual.as_bytes());
        archives.push(archive);
    }
    let mut patches = Vec::new();
    for entry in metadata["patches"].as_array().ok_or("missing patches")? {
        let name = entry["crate"].as_str().ok_or("missing patch crate")?;
        let path = entry["path"].as_str().ok_or("missing patch target")?;
        let patch =
            fs::read_to_string(native.join(entry["patch"].as_str().ok_or("missing patch file")?))?
                .replace("\r\n", "\n");
        fingerprint.update(patch.as_bytes());
        patches.push((format!("{name}-{version}/{path}"), patch));
    }
    let fingerprint = hex(fingerprint.finalize());
    let stamp = destination.join("source.sha256");
    if sources.raw.is_dir()
        && sources.safe.is_dir()
        && fs::read_to_string(&stamp).ok().as_deref() == Some(&fingerprint)
    {
        return Ok(sources);
    }
    if destination.exists() {
        fs::remove_dir_all(destination)?;
    }
    fs::create_dir_all(destination)?;
    for archive in archives {
        tar::Archive::new(GzDecoder::new(archive.as_slice())).unpack(destination)?;
    }
    for (path, patch) in patches {
        let target = destination.join(path);
        let original = fs::read_to_string(&target)?;
        let patch = diffy::Patch::from_str(&patch)?;
        fs::write(&target, diffy::apply(&original, &patch)?)?;
    }
    adapt_bindings(&sources.safe.join("src"))?;
    let root = sources.safe.join("src/lib.rs");
    fs::write(&root, fs::read_to_string(&root)?.replace("//!", "//"))?;
    fs::write(stamp, fingerprint)?;
    Ok(sources)
}

fn adapt_bindings(directory: &Path) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.is_dir() {
            adapt_bindings(&path)?;
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            let text = fs::read_to_string(&path)?;
            fs::write(path, text.replace("llama_cpp_sys_2::", "crate::sys::"))?;
        }
    }
    Ok(())
}
