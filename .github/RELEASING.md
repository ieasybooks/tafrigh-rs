# Releasing

All three crates share one version and one `vX.Y.Z` tag. Use [Conventional Commits](https://github.com/googleapis/release-please): `fix:` bumps the patch version, `feat:` bumps the minor version, and `feat!:` or a `BREAKING CHANGE:` footer marks incompatible changes. Before 1.0, breaking changes also bump the minor version. The current feature stack therefore proposes 0.1.0 from 0.0.1. Keep the same convention in squash-merge titles and preserve breaking-change footers.

After changes reach `main`, release-please opens or updates one release PR. It generates `CHANGELOG.md` and GitHub release notes from the Conventional Commits, including breaking-change descriptions. Write useful commit messages instead of maintaining an `Unreleased` section by hand. Review the generated version, lockfile, changelog, and checks before merging.

Merging the release PR creates the GitHub release and dispatches the existing binary-build and crates.io workflows on the new tag. Ordinary feature PRs do not publish packages. The existing `release` environment and crates.io trusted publisher configuration still apply. Manual runs of the build and publish workflows remain checks only by default; to retry publishing, select the release tag and enable the `publish` input.

The workflow uses the repository's `GITHUB_TOKEN`; no additional personal token is needed. GitHub's **Settings → Actions → General → Allow GitHub Actions to create and approve pull requests** setting must be enabled. If GitHub requests approval for checks on a bot-created release PR, approve those workflow runs before merging.

The root `Cargo.toml` defines `tafrigh-cli` and lists the two library members explicitly. Each package and internal path dependency declares its version directly. release-please's `rust` strategy updates all three package versions, their internal dependency requirements, and `Cargo.lock` together in the release PR. `.release-please-manifest.json` records the release baseline. CI checks that the three package versions match.

The workflow explicitly dispatches publishing because a release created with `GITHUB_TOKEN` does not trigger release-event workflows. See [GitHub's workflow trigger rules](https://docs.github.com/en/actions/how-tos/write-workflows/choose-when-workflows-run/trigger-a-workflow) and [release-please's Rust strategy](https://github.com/googleapis/release-please/blob/main/src/strategies/rust.ts).
