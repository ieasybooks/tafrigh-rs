use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use tokio::process::Command;

use super::{Media, MediaItem, is_media_path};
use crate::{Error, Result};

/// Resolves and downloads URLs with the `yt-dlp` executable.
///
/// Playlists and channels are resolved flat (fast, no per-video requests); each item is then
/// downloaded right before it is transcribed, and reused if already present.
///
/// yt-dlp's warnings (e.g. that no JS runtime was found) are logged through `tracing`, each
/// distinct one once.
#[derive(Debug, Clone)]
pub struct YtDlp {
    program: PathBuf,
    args: Vec<String>,
    download_dir: PathBuf,
    save_metadata: bool,
    /// Messages already logged; shared between clones.
    logged: Arc<Mutex<HashSet<String>>>,
}

impl YtDlp {
    /// Downloads into `download_dir`, using `yt-dlp` from `PATH`.
    pub fn new(download_dir: impl Into<PathBuf>) -> Self {
        Self {
            program: "yt-dlp".into(),
            args: Vec::new(),
            download_dir: download_dir.into(),
            save_metadata: false,
            logged: Arc::default(),
        }
    }

    /// Path to the yt-dlp executable.
    #[must_use]
    pub fn program(mut self, program: impl Into<PathBuf>) -> Self {
        self.program = program.into();
        self
    }

    /// Extra yt-dlp arguments. Audio downloads allow overrides; backends may enforce
    /// their required arguments after these.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Writes yt-dlp's JSON for each resolved URL to `<download dir>/<id>.info.json`.
    #[must_use]
    pub fn save_metadata(mut self, save: bool) -> Self {
        self.save_metadata = save;
        self
    }

    /// Where downloads go.
    pub fn download_dir(&self) -> &Path {
        &self.download_dir
    }

    /// Checks that yt-dlp can be run, failing with [`Error::ToolUnavailable`] otherwise.
    pub async fn check(&self) -> Result<()> {
        let status = Command::new(&self.program)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status()
            .await;
        Error::check_tool("yt-dlp", &self.program, "--version", status)
    }

    /// Lists the downloadable items behind `url`.
    pub async fn resolve(&self, url: &str) -> Result<Vec<MediaItem>> {
        let command = self.with_defaults(
            &["--flat-playlist", "--dump-single-json", "--ignore-errors"],
            url,
        );
        let output = self.output(command, url, Failure::UnlessOutput).await?;
        self.items(url, &output).await
    }

    /// Runs yt-dlp with the configured extra arguments followed by `args`, and returns its
    /// standard output. `args` come last, so backends can enforce the options they rely on;
    /// include `--` and the URL when the run needs one. `url` names the item in errors.
    pub async fn run<I, S>(&self, args: I, url: &str) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.output(self.enforcing(args), url, Failure::Always)
            .await
    }

    /// Lists the items behind `url` from a run with `args`, which must include
    /// `--dump-single-json`. Like [`Self::resolve`], it skips unavailable entries, saves the
    /// metadata when enabled, and accepts a failed run that still printed JSON, such as a
    /// playlist with unavailable entries.
    pub async fn list<I, S>(&self, args: I, url: &str) -> Result<Vec<MediaItem>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self
            .output(self.enforcing(args), url, Failure::UnlessOutput)
            .await?;
        self.items(url, &output).await
    }

    /// Parses yt-dlp JSON metadata into items and saves it when metadata output is enabled.
    async fn items(&self, url: &str, output: &[u8]) -> Result<Vec<MediaItem>> {
        let info: Value = serde_json::from_slice(output)
            .map_err(|err| Error::parse(format!("yt-dlp returned invalid JSON for {url}"), err))?;

        if self.save_metadata {
            let id = info
                .get("id")
                .and_then(Value::as_str)
                .map_or_else(|| "metadata".to_owned(), sanitize);
            tokio::fs::create_dir_all(&self.download_dir)
                .await
                .map_err(|err| {
                    Error::io(format!("creating {}", self.download_dir.display()), err)
                })?;
            let path = self.download_dir.join(format!("{id}.info.json"));
            let pretty = serde_json::to_vec_pretty(&info).expect("JSON value serializes");
            tokio::fs::write(&path, pretty)
                .await
                .map_err(|err| Error::io(format!("writing {}", path.display()), err))?;
        }

        let mut entries = Vec::new();
        flatten(&info, &mut entries);
        let mut seen = HashSet::new();
        let mut items = Vec::new();
        for entry in entries {
            let Some(id) = entry.get("id").and_then(Value::as_str) else {
                continue;
            };
            let title = entry.get("title").and_then(Value::as_str);
            if let Some(reason) = unavailable(entry) {
                tracing::info!("skipping {id} ({}): {reason}", title.unwrap_or("untitled"));
                continue;
            }
            let Some(link) = ["webpage_url", "url", "original_url"]
                .iter()
                .find_map(|key| entry.get(*key).and_then(Value::as_str))
            else {
                continue;
            };
            let id = sanitize(id);
            if !seen.insert(id.clone()) {
                continue;
            }
            items.push(MediaItem {
                id: Arc::from(id.as_str()),
                title: title.map(Arc::from),
                media: Media::Remote {
                    url: link.to_owned(),
                },
                link: Some(link.to_owned()),
            });
        }
        Ok(items)
    }

    /// Downloads the audio of `item` (or reuses an earlier download) and returns its path.
    pub async fn download(&self, item: &MediaItem) -> Result<PathBuf> {
        let url = match &item.media {
            Media::File(path) => return Ok(path.clone()),
            Media::Remote { url } => url,
        };
        if let Some(existing) = self.existing_download(&item.id) {
            return Ok(existing);
        }
        tokio::fs::create_dir_all(&self.download_dir)
            .await
            .map_err(|err| Error::io(format!("creating {}", self.download_dir.display()), err))?;
        let template = self.download_dir.join("%(id)s.%(ext)s");
        let template = template.to_string_lossy();
        let command = self.with_defaults(
            &[
                "-f",
                "bestaudio/best",
                "--no-playlist",
                "--no-simulate",
                "--no-progress",
                "--print",
                "after_move:filepath",
                "-o",
                template.as_ref(),
            ],
            url,
        );
        let output = self.output(command, url, Failure::UnlessOutput).await?;
        let stdout = String::from_utf8_lossy(&output);
        let path = stdout
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_file())
            .ok_or_else(|| Error::Source(format!("yt-dlp did not produce a file for {url}")))?;
        Ok(path)
    }

    fn existing_download(&self, id: &str) -> Option<PathBuf> {
        let entries = std::fs::read_dir(&self.download_dir).ok()?;
        let mut found: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .filter(|path| path.file_stem().is_some_and(|stem| stem == id) && is_media_path(path))
            .collect();
        found.sort();
        found.into_iter().next()
    }

    /// yt-dlp on `url` with `defaults` that the configured extra arguments may override.
    fn with_defaults(&self, defaults: &[&str], url: &str) -> Command {
        let mut command = Command::new(&self.program);
        command.args(defaults).args(&self.args).arg("--").arg(url);
        command
    }

    /// yt-dlp with the configured extra arguments, then `args`, which take precedence.
    fn enforcing<S: AsRef<OsStr>>(&self, args: impl IntoIterator<Item = S>) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args).args(args);
        command
    }

    /// Runs `command` and returns its stdout, logging yt-dlp's warnings. A failed run becomes
    /// an error carrying yt-dlp's last error line, unless `failure` accepts its output.
    async fn output(&self, mut command: Command, url: &str, failure: Failure) -> Result<Vec<u8>> {
        let output = command
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|err| Error::tool("yt-dlp", &self.program, err))?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        let diagnostics = Diagnostics::parse(&stderr);
        diagnostics.warnings.iter().for_each(|w| self.log_once(w));
        let accepted = failure == Failure::UnlessOutput && !output.stdout.is_empty();
        if !output.status.success() && !accepted {
            let message = diagnostics
                .errors
                .last()
                .copied()
                .or(diagnostics.last_line)
                .unwrap_or("yt-dlp failed");
            return Err(Error::Source(format!("{url}: {message}")));
        }
        // Partial failures, e.g. unavailable entries of a playlist resolved with
        // `--ignore-errors`.
        diagnostics.errors.iter().for_each(|e| self.log_once(e));
        Ok(output.stdout)
    }

    fn log_once(&self, message: &str) {
        let first = self
            .logged
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(message.to_owned());
        if first {
            tracing::warn!("yt-dlp: {message}");
        }
    }
}

/// When a yt-dlp run that exited with an error fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    Always,
    /// Unless it printed something, such as the available entries of a playlist with
    /// unavailable ones (`--ignore-errors`).
    UnlessOutput,
}

/// The parts of yt-dlp's stderr worth reporting.
#[derive(Debug, Default, PartialEq)]
struct Diagnostics<'a> {
    /// `WARNING:` lines, without the prefix.
    warnings: Vec<&'a str>,
    /// `ERROR:` lines, without the prefix.
    errors: Vec<&'a str>,
    /// The last non-empty line, for failures that print no `ERROR:` line.
    last_line: Option<&'a str>,
}

impl<'a> Diagnostics<'a> {
    fn parse(stderr: &'a str) -> Self {
        let mut diagnostics = Self::default();
        for line in stderr
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
        {
            if let Some(warning) = line.strip_prefix("WARNING:") {
                diagnostics.warnings.push(warning.trim_start());
            } else if let Some(error) = line.strip_prefix("ERROR:") {
                diagnostics.errors.push(error.trim_start());
            }
            diagnostics.last_line = Some(line);
        }
        diagnostics
    }
}

fn flatten<'a>(value: &'a Value, out: &mut Vec<&'a Value>) {
    match value.get("entries").and_then(Value::as_array) {
        Some(entries) => {
            for entry in entries.iter().filter(|e| e.is_object()) {
                flatten(entry, out);
            }
        }
        None => out.push(value),
    }
}

fn unavailable(entry: &Value) -> Option<&'static str> {
    let field = |key: &str| entry.get(key).and_then(Value::as_str);
    match field("title") {
        Some("[Private video]") => return Some("private"),
        Some("[Deleted video]") => return Some("deleted"),
        _ => {}
    }
    match field("availability") {
        Some("private") => return Some("private"),
        Some("premium_only") => return Some("premium only"),
        Some("subscriber_only") => return Some("subscribers only"),
        Some("needs_auth") => return Some("needs authentication"),
        _ => {}
    }
    match field("live_status") {
        Some("is_upcoming") => Some("upcoming"),
        Some("is_live") => Some("live"),
        _ => None,
    }
}

/// Makes an id safe to use as a file stem.
fn sanitize(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == '.' || c == ' ');
    if trimmed.is_empty() {
        "item".to_owned()
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flattens_nested_playlists_and_skips_unavailable() {
        let info = json!({
            "_type": "playlist", "id": "PL1",
            "entries": [
                {"id": "a", "title": "A", "url": "https://www.youtube.com/watch?v=a"},
                {"_type": "playlist", "id": "PL2", "entries": [
                    {"id": "b", "title": "B", "url": "https://www.youtube.com/watch?v=b"},
                    {"id": "c", "title": "[Private video]", "url": "https://www.youtube.com/watch?v=c"},
                    null
                ]},
                {"id": "d", "title": "D", "live_status": "is_upcoming", "url": "x"},
            ]
        });
        let mut entries = Vec::new();
        flatten(&info, &mut entries);
        let ids: Vec<_> = entries
            .iter()
            .filter(|e| unavailable(e).is_none())
            .map(|e| e["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["a", "b"]);
    }

    #[test]
    fn splits_stderr_into_warnings_and_errors() {
        let stderr = "\
WARNING: [youtube] No supported JavaScript runtime could be found.
[youtube] x: Downloading webpage

ERROR: [youtube] a: Private video
ERROR: [youtube] b: Video unavailable
";
        assert_eq!(
            Diagnostics::parse(stderr),
            Diagnostics {
                warnings: vec!["[youtube] No supported JavaScript runtime could be found."],
                errors: vec![
                    "[youtube] a: Private video",
                    "[youtube] b: Video unavailable"
                ],
                last_line: Some("ERROR: [youtube] b: Video unavailable"),
            }
        );
        assert_eq!(Diagnostics::parse(" \n"), Diagnostics::default());
    }

    #[tokio::test]
    async fn check_reports_missing_program() {
        let missing = YtDlp::new(".").program("/nonexistent/yt-dlp");
        let err = missing.check().await.unwrap_err();
        assert!(
            matches!(&err, Error::ToolUnavailable { tool: "yt-dlp", source, .. }
                if source.kind() == std::io::ErrorKind::NotFound),
            "{err:?}"
        );
        let resolved = missing.resolve("https://example.com/x").await.unwrap_err();
        assert!(
            matches!(resolved, Error::ToolUnavailable { .. }),
            "{resolved:?}"
        );
    }

    #[test]
    fn sanitizes_ids() {
        assert_eq!(sanitize("a/b:c"), "a_b_c");
        assert_eq!(sanitize(".."), "item");
    }
}
