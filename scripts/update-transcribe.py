#!/usr/bin/env python3
"""Refresh the bundled transcribe.cpp crates; run through update-transcribe.sh.

Only the Python standard library and the existing Git installation are used.
Network access belongs to this maintenance command, never the Cargo build.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import tomllib
from urllib.request import Request, urlopen


REPOSITORY = "https://github.com/handy-computer/transcribe.cpp"
ABI_DOCUMENTATION = """/// Taken from this crate's own `Cargo.toml` (`CARGO_PKG_VERSION`), not the
/// generated FFI macros: a version-only bump must not churn the committed
/// bindings or the abihash (notes/releasing.md §8 P0 #1). The generators no
/// longer emit `TRANSCRIBE_VERSION_*`, so this is also the only source left.
"""
BUILD_ENTRY = """fn main() {
    // CARGO_MANIFEST_DIR for this crate is the repo root (the sys crate's
    // manifest lives there so the tarball can carry the whole C++ tree).
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

"""
PREBUILT_DOCUMENTATION = """//! Prebuilt path: setting TRANSCRIBE_DIR (OPENSSL_DIR-style) to an install
//! prefix produced by `cmake --install` of a TRANSCRIBE_INSTALL=ON build
//! skips the source build entirely and links against that prefix's manifest
//! instead. Cargo features are inert there: the prebuilt already decided its
//! configuration (static/shared, backends), and the manifest records it.
//!
"""
GENERATED_SOURCE_WATCHES_AND_OVERRIDE = """    for p in [
        "CMakeLists.txt",
        "CMakePresets.json",
        "include",
        "src",
        "ggml",
        "cmake",
        "bindings/rust/sys/build.rs",
        "bindings/rust/sys/src",
    ] {
        println!("cargo:rerun-if-changed={}", root.join(p).display());
    }

    // Prebuilt path: TRANSCRIBE_DIR points at an existing install prefix (a
    // `cmake --install` tree from a TRANSCRIBE_INSTALL=ON configure). Skip the
    // source build and emit the link line from that prefix's manifest; the
    // manifest records the install's own posture (static/shared, backends), so
    // the Cargo features below never apply here.
    println!("cargo:rerun-if-env-changed=TRANSCRIBE_DIR");
    if let Some(dir) = env::var_os("TRANSCRIBE_DIR") {
        let prefix = PathBuf::from(dir);
        let manifest = find_manifest(&prefix).unwrap_or_else(|| {
            panic!(
                "TRANSCRIBE_DIR is set but no lib/transcribe-link.json (or lib64/) exists \\
                 under {}. It must point at an install prefix of this library, produced by \\
                 `cmake -B build -DTRANSCRIBE_INSTALL=ON && cmake --build build && \\
                 cmake --install build --prefix <dir>`. Unset TRANSCRIBE_DIR to build \\
                 the vendored sources instead.",
                prefix.display()
            )
        });
        println!("cargo:rerun-if-changed={}", manifest.display());
        emit_link_lines(&prefix, &manifest);
        return;
    }

"""


def replace_once(text, old, new, description):
    if text.count(old) != 1:
        raise ValueError(f"upstream changed: review the {description} adaptation")
    return text.replace(old, new, 1)


def download(url):
    request = Request(url, headers={"User-Agent": "tafrigh-native-update"})
    # urlopen uses normal HTTPS certificate and hostname verification.
    with urlopen(request, timeout=60) as response:
        return response.read()


def verified_crate(name, version, work):
    index_url = f"https://index.crates.io/{name[:2]}/{name[2:4]}/{name}"
    records = [json.loads(line) for line in download(index_url).splitlines()]
    matches = [entry for entry in records if entry["vers"] == version]
    if len(matches) != 1 or matches[0].get("yanked"):
        raise ValueError(f"{name} {version} must be an available, non-yanked release")
    checksum = matches[0]["cksum"]
    archive_name = f"{name}-{version}.crate"
    archive_url = f"https://static.crates.io/crates/{name}/{archive_name}"
    archive = download(archive_url)
    if hashlib.sha256(archive).hexdigest() != checksum:
        raise ValueError(f"{archive_name}: crates.io index SHA-256 mismatch")
    archive_path = work / archive_name
    archive_path.write_bytes(archive)
    prefix = f"{name}-{version}"
    with tarfile.open(archive_path, "r:gz") as bundle:
        # Crates are ordinary files in one named directory. Reject links and
        # unexpected roots before extraction so maintenance stays in staging.
        for member in bundle.getmembers():
            parts = Path(member.name).parts
            if not parts or parts[0] != prefix or ".." in parts:
                raise ValueError(f"unexpected archive member: {member.name}")
            if not (member.isfile() or member.isdir()):
                raise ValueError(f"unexpected archive member type: {member.name}")
        bundle.extractall(work, filter="data")
    root = work / prefix
    manifest = tomllib.loads((root / "Cargo.toml").read_text(encoding="utf-8"))
    package = manifest["package"]
    if (package["name"], package["version"], package["repository"]) != (
        name, version, REPOSITORY
    ):
        raise ValueError(f"unexpected package identity in {archive_name}")
    vcs = json.loads((root / ".cargo_vcs_info.json").read_text(encoding="utf-8"))
    if vcs["git"].get("dirty"):
        raise ValueError(f"{archive_name} was packaged from a dirty checkout")
    return root, manifest, vcs["git"]["sha1"], archive, checksum, archive_url


def copied_bindings(safe, raw):
    files = {}
    for path in sorted((safe / "src").rglob("*")):
        if path.is_file():
            text = path.read_text(encoding="utf-8")
            text = re.sub(r"(?m)^use transcribe_cpp_sys as sys;$", "use crate::sys;", text)
            files["src/" + path.relative_to(safe / "src").as_posix()] = text
    files["src/lib.rs"] = replace_once(
        files["src/lib.rs"],
        "//! The raw FFI lives in the [`transcribe-cpp-sys`](transcribe_cpp_sys) crate",
        "//! The raw FFI lives in the bundled [`sys`] module",
        "raw-module documentation",
    )
    files["src/lib.rs"] = replace_once(
        files["src/lib.rs"], "pub use transcribe_cpp_sys as sys;",
        '#[path = "sys/lib.rs"]\npub mod sys;', "raw-module import",
    )
    files["src/lib.rs"] = replace_once(
        files["src/lib.rs"], 'https://docs.rs/transcribe-cpp',
        'https://docs.rs/tafrigh-transcribe', "rustdoc crate URL",
    )
    files["src/version.rs"] = replace_once(
        files["src/version.rs"], ABI_DOCUMENTATION,
        "/// This is the bundled upstream version, independent of the tafrigh package version.\n",
        "native-version documentation",
    )
    files["src/version.rs"] = replace_once(
        files["src/version.rs"], 'base(env!("CARGO_PKG_VERSION")).to_string()',
        'env!("TAFRIGH_TRANSCRIBE_VERSION").to_string()', "native ABI version",
    )
    for path in sorted((raw / "bindings/rust/sys/src").rglob("*")):
        if path.is_file():
            files["src/sys/" + path.relative_to(raw / "bindings/rust/sys/src").as_posix()] = path.read_text(encoding="utf-8")
    files["src/sys/lib.rs"] = replace_once(
        files["src/sys/lib.rs"], 'assert_eq!(version, env!("CARGO_PKG_VERSION"));',
        'assert_eq!(version, crate::compiled_version());', "native-version smoke test",
    )
    helper = (raw / "bindings/rust/sys/build.rs").read_text(encoding="utf-8")
    helper = replace_once(helper, BUILD_ENTRY, "pub(super) fn build(root: &Path) {\n", "native build entry")
    helper = replace_once(helper, "cmake::Config::new(&root)", "cmake::Config::new(root)", "native source path")
    helper = replace_once(helper, PREBUILT_DOCUMENTATION,
                          "//! tafrigh always builds its patched source; the outer build script rejects\n"
                          "//! TRANSCRIBE_DIR and watches the bundled inputs instead of this extracted tree.\n//!\n",
                          "prebuilt documentation")
    helper = replace_once(helper, GENERATED_SOURCE_WATCHES_AND_OVERRIDE, "", "generated-source watches and prebuilt override")
    files["native/build.rs"] = helper
    return {name: text.encode() for name, text in files.items()}


def validate_patches(crate, metadata, raw):
    for entry in metadata["patches"]:
        patch = crate / "native" / entry["patch"]
        text = patch.read_text(encoding="utf-8")
        headers = [line for line in text.splitlines() if line.startswith(("--- ", "+++ "))]
        if headers != [f"--- a/{entry['path']}", f"+++ b/{entry['path']}"]:
            raise ValueError(f"patch target disagrees with upstream.json: {patch.name}")
        # The bundled archive stays byte-for-byte intact; only temporary source
        # is patched here. Cargo applies these same patches through diffy.
        subprocess.run(["git", "apply", "--check", "--whitespace=nowarn", "-"],
                       input=text, encoding="utf-8", cwd=raw, check=True)
        subprocess.run(["git", "apply", "--whitespace=nowarn", "-"],
                       input=text, encoding="utf-8", cwd=raw, check=True)


def generate(crate, version, work):
    metadata = json.loads((crate / "native/upstream.json").read_text(encoding="utf-8"))
    safe, safe_manifest, safe_commit, _, safe_hash, _ = verified_crate("transcribe-cpp", version, work)
    raw, _, raw_commit, raw_archive, raw_hash, raw_url = verified_crate("transcribe-cpp-sys", version, work)
    if safe_commit != raw_commit:
        raise ValueError("safe and sys releases came from different upstream commits")
    requirement = safe_manifest["dependencies"]["transcribe-cpp-sys"]["version"]
    if requirement not in (version, "=" + version, "^" + version):
        raise ValueError("the safe release does not declare the matching sys version; inspect before upgrading")
    if version == metadata["version"]:
        if (raw_hash, safe_hash, raw_commit) != (
            metadata["sha256"], metadata["bindings_archive_sha256"], metadata["commit"]
        ):
            raise ValueError("the checked-in pin disagrees with crates.io; inspect before regenerating")
    files = copied_bindings(safe, raw)
    validate_patches(crate, metadata, raw)
    metadata.update(version=version, commit=raw_commit, archive=f"transcribe-cpp-sys-{version}.crate",
                    url=raw_url, sha256=raw_hash, bindings_archive_sha256=safe_hash)
    files["native/upstream.json"] = (json.dumps(metadata, ensure_ascii=False, indent=2) + "\n").encode()
    files["native/" + metadata["archive"]] = raw_archive
    local_copyright = [line for line in (crate.parents[1] / "LICENSE").read_text(encoding="utf-8").splitlines()
                       if line.startswith("Copyright ")]
    if not local_copyright:
        raise ValueError("the workspace LICENSE has no copyright notice")
    license_text = (raw / "LICENSE").read_text(encoding="utf-8")
    upstream_copyright = [line for line in license_text.splitlines() if line.startswith("Copyright ")]
    if not upstream_copyright:
        raise ValueError("the upstream LICENSE has no copyright notice")
    copyright_block = "\n".join(upstream_copyright)
    license_text = replace_once(license_text, copyright_block + "\n",
                                copyright_block + "\n" + "\n".join(local_copyright) + "\n",
                                "combined MIT copyright")
    files["LICENSE"] = license_text.encode()
    # The downstream notice adds one statement locating archive-relative paths.
    # Do not silently discard other local license changes on an upstream bump.
    notice = (raw / "THIRD-PARTY-LICENSES.md").read_text(encoding="utf-8")
    notice = replace_once(notice, "# Third-party licenses\n",
                          "# Third-party licenses\n\nThis notice is reproduced from the pinned upstream release. Relative native-source paths refer to the source archive in `native/`, rather than this crate's root. Links to upstream tooling and documentation omitted from that archive point to the pinned upstream commit.\n",
                          "third-party notice location")
    for path in ("ggml/UPSTREAM", "scripts/lib/phonon-LICENSE-Apache-2.0.txt", "docs/models/phonon-2.md"):
        notice = replace_once(notice, f"]({path})", f"]({REPOSITORY}/blob/{raw_commit}/{path})", "omitted upstream notice link")
    files["THIRD-PARTY-LICENSES.md"] = notice.encode()
    return files


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="matching stable transcribe-cpp and transcribe-cpp-sys version")
    parser.add_argument("--check", action="store_true", help="regenerate and compare without changing the checkout")
    parser.add_argument("--crate-dir", type=Path, default=Path(__file__).resolve().parents[1] / "crates/tafrigh-transcribe")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", args.version):
        parser.error("version must be an explicit stable MAJOR.MINOR.PATCH")
    crate = args.crate_dir.resolve(strict=True)
    old = json.loads((crate / "native/upstream.json").read_text(encoding="utf-8"))
    with tempfile.TemporaryDirectory(prefix="tafrigh-transcribe-update-") as temporary:
        files = generate(crate, args.version, Path(temporary))
    removed = {p.relative_to(crate).as_posix() for p in (crate / "src").rglob("*") if p.is_file()} - files.keys()
    if old["archive"] != f"transcribe-cpp-sys-{args.version}.crate":
        removed.add("native/" + old["archive"])
    changed = [name for name, data in files.items() if not (crate / name).exists() or (crate / name).read_bytes() != data]
    for name in sorted(set(changed) | removed):
        print(name)
    if args.check:
        if changed or removed:
            raise SystemExit("regeneration differs; no files changed")
        print(f"Verified {len(files)} generated files byte-for-byte, including the unchanged native archive.")
        return
    # All downloads, identities, transformations, and native patches have passed
    # before any managed file in the checkout is changed.
    for name in sorted(removed):
        (crate / name).unlink()
    for name in changed:
        path = crate / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(files[name])
    print(f"Refreshed native {args.version}; review dependencies and patches, then run the documented checks.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from error
