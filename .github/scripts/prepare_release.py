"""Move the curated Unreleased notes into a dated Keep a Changelog release."""

from datetime import datetime, timezone
from pathlib import Path
import re
import subprocess
import tomllib


def release_changelog(content: str, version: str, date: str) -> str:
    marker = "## [Unreleased]\n"
    if content.count(marker) != 1:
        raise ValueError("CHANGELOG.md must contain one Unreleased heading")
    if f"## [{version}]" in content:
        raise ValueError(f"Version {version} is already in CHANGELOG.md")
    notes = content.split(marker, 1)[1].split("\n## [", 1)[0].strip()
    if not notes:
        raise ValueError("Add release notes under Unreleased in CHANGELOG.md")
    link = re.search(r"(?m)^\[Unreleased\]: (.+)/compare/(.+)\.\.\.HEAD$", content)
    if not link:
        raise ValueError("CHANGELOG.md needs an Unreleased comparison link")
    repository, previous = link.groups()
    content = content.replace(marker, f"{marker}\n## [{version}] - {date}\n", 1)
    return content.replace(
        link.group(0),
        f"[Unreleased]: {repository}/compare/v{version}...HEAD\n"
        f"[{version}]: {repository}/compare/{previous}...v{version}",
        1,
    )


if __name__ == "__main__":
    manifest = tomllib.loads(Path("Cargo.toml").read_text())
    version = manifest["workspace"]["package"]["version"]
    # Start from main, so reruns also pick up note edits that did not change the release PR.
    content = subprocess.check_output(
        ["git", "show", "origin/main:CHANGELOG.md"], text=True
    )
    date = datetime.now(timezone.utc).date().isoformat()
    Path("CHANGELOG.md").write_text(release_changelog(content, version, date))
