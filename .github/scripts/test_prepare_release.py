"""Checks the release changelog without creating a tag or publishing anything."""

import unittest

from prepare_release import release_changelog


CHANGELOG = """# Changelog

## [Unreleased]

### Added

- A new feature with a long paragraph that must stay on one line.

### Changed

- A breaking change.

## [0.0.1] - 2026-10-20

### Added

- Initial release.

[Unreleased]: https://github.com/ieasybooks/tafrigh-rs/compare/v0.0.1...HEAD
[0.0.1]: https://github.com/ieasybooks/tafrigh-rs/releases/tag/v0.0.1
"""


class ReleaseChangelogTests(unittest.TestCase):
    def test_promotes_notes_preserving_history_and_lines(self):
        result = release_changelog(CHANGELOG, "0.1.0", "2026-10-21")
        self.assertIn("## [Unreleased]\n\n## [0.1.0] - 2026-10-21\n\n### Added", result)
        self.assertIn("[Unreleased]: https://github.com/ieasybooks/tafrigh-rs/compare/v0.1.0...HEAD", result)
        self.assertIn("[0.1.0]: https://github.com/ieasybooks/tafrigh-rs/compare/v0.0.1...v0.1.0", result)
        for line in CHANGELOG.splitlines():
            if not line.startswith("[Unreleased]:"):
                self.assertIn(line, result)
        self.assertEqual(result, release_changelog(CHANGELOG, "0.1.0", "2026-10-21"))

    def test_rejects_missing_notes_links_or_duplicate_version(self):
        for content, version in [
            (CHANGELOG.replace("## [Unreleased]", "## Upcoming"), "0.1.0"),
            (CHANGELOG.replace("[Unreleased]:", "[Upcoming]:"), "0.1.0"),
            (CHANGELOG, "0.0.1"),
            ("## [Unreleased]\n\n## [0.0.1]\n", "0.1.0"),
        ]:
            with self.subTest(content=content, version=version):
                with self.assertRaises(ValueError):
                    release_changelog(content, version, "2026-10-21")
