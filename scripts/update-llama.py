#!/usr/bin/env python3
"""Refresh the unchanged registry archives and notices bundled by tafrigh-llama."""

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

REPOSITORY = "https://github.com/utilityai/llama-cpp-rs"


def download(url):
    request = Request(url, headers={"User-Agent": "tafrigh-native-update"})
    with urlopen(request, timeout=60) as response:
        return response.read()


def verified_crate(name, version, work):
    records = [json.loads(line) for line in download(
        f"https://index.crates.io/{name[:2]}/{name[2:4]}/{name}"
    ).splitlines()]
    releases = [entry for entry in records if entry["vers"] == version and not entry.get("yanked")]
    if len(releases) != 1:
        raise ValueError(f"{name} {version} must be an available, non-yanked release")
    archive = f"{name}-{version}.crate"
    url = f"https://static.crates.io/crates/{name}/{archive}"
    data = download(url)
    checksum = hashlib.sha256(data).hexdigest()
    if checksum != releases[0]["cksum"]:
        raise ValueError(f"{archive}: registry SHA-256 mismatch")
    path = work / archive
    path.write_bytes(data)
    with tarfile.open(path, "r:gz") as bundle:
        for member in bundle.getmembers():
            parts = Path(member.name).parts
            if not parts or parts[0] != f"{name}-{version}" or ".." in parts:
                raise ValueError(f"unexpected archive member: {member.name}")
            if not (member.isfile() or member.isdir()):
                raise ValueError(f"unexpected archive member type: {member.name}")
        bundle.extractall(work, filter="data")
    root = work / f"{name}-{version}"
    manifest = tomllib.loads((root / "Cargo.toml").read_text())
    package = manifest["package"]
    if (package["name"], package["version"], package["repository"]) != (name, version, REPOSITORY):
        raise ValueError(f"unexpected package identity: {archive}")
    vcs = json.loads((root / ".cargo_vcs_info.json").read_text())["git"]
    if vcs.get("dirty"):
        raise ValueError(f"{archive} came from a dirty checkout")
    entry = dict(name=name, archive=archive, url=url, sha256=checksum,
                 commit=vcs["sha1"], license=package["license"])
    return root, manifest, data, entry


def generate(crate, version, work, previous):
    metadata = dict(previous, version=version, archives=[])
    files = {}
    roots = {}
    manifests = {}
    for name in ("llama-cpp-sys-2", "llama-cpp-2"):
        root, manifest, data, entry = verified_crate(name, version, work)
        roots[name], manifests[name] = root, manifest
        files["native/" + entry["archive"]] = data
        metadata["archives"].append(entry)
    if len({entry["commit"] for entry in metadata["archives"]}) != 1:
        raise ValueError("safe and raw archives came from different commits")
    requirement = manifests["llama-cpp-2"]["dependencies"]["llama-cpp-sys-2"]["version"]
    if requirement not in (version, "=" + version, "^" + version):
        raise ValueError("safe bindings require a different sys version")
    if version == previous["version"] and previous["archives"] and previous["archives"] != metadata["archives"]:
        raise ValueError("the checked-in pin disagrees with crates.io")
    for entry in metadata["patches"]:
        text = (crate / "native" / entry["patch"]).read_text()
        headers = [line for line in text.splitlines() if line.startswith(("--- ", "+++ "))]
        if headers != [f"--- a/{entry['path']}", f"+++ b/{entry['path']}"]:
            raise ValueError(f"patch target disagrees with upstream.json: {entry['patch']}")
        for flags in (["--check"], []):
            subprocess.run(["git", "apply", *flags, "--whitespace=nowarn", "-"], input=text,
                           encoding="utf-8", cwd=roots[entry["crate"]], check=True)
    raw = roots["llama-cpp-sys-2"]
    for name in ("LICENSE-MIT", "LICENSE-APACHE"):
        # The crates.io archives omit the workspace license files.
        commit = metadata["archives"][0]["commit"]
        url = f"https://raw.githubusercontent.com/utilityai/llama-cpp-rs/{commit}/{name}"
        data = download(url)
        files["native/licenses/" + name] = data
        metadata.setdefault("licenses", {})[name] = dict(url=url, sha256=hashlib.sha256(data).hexdigest())
    files["native/licenses/LICENSE.llama.cpp"] = (raw / "llama.cpp/LICENSE").read_bytes()
    files["native/upstream.json"] = (json.dumps(metadata, indent=2) + "\n").encode()
    return files


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("version", help="matching stable llama-cpp-2 and llama-cpp-sys-2 version")
    parser.add_argument("--check", action="store_true", help="compare without changing managed files")
    parser.add_argument("--crate-dir", type=Path, default=Path(__file__).resolve().parents[1] / "crates/tafrigh-llama")
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", args.version):
        parser.error("version must be an explicit stable MAJOR.MINOR.PATCH")
    crate = args.crate_dir.resolve(strict=True)
    previous = json.loads((crate / "native/upstream.json").read_text())
    with tempfile.TemporaryDirectory(prefix="tafrigh-llama-update-") as temporary:
        files = generate(crate, args.version, Path(temporary), previous)
    removed = {"native/" + entry["archive"] for entry in previous["archives"]} - files.keys()
    changed = [name for name, data in files.items() if not (crate / name).exists() or (crate / name).read_bytes() != data]
    for name in sorted(set(changed) | removed):
        print(name)
    if args.check:
        if changed or removed:
            raise SystemExit("regeneration differs; no files changed")
        print(f"Verified {len(files)} bundled files byte-for-byte.")
        return
    # Validate both downloads and patches before replacing any managed files.
    for name in removed:
        (crate / name).unlink()
    for name in changed:
        path = crate / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(files[name])
    print(f"Refreshed {args.version}; review dependencies and ggml compatibility before building.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(str(error)) from error
