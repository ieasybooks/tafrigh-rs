use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;
use tokio::process::Command;

use super::{Media, MediaItem, captions, is_media_path};
use crate::{Error, Result, Segment};

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

    /// Extra yt-dlp arguments. Audio downloads allow overrides; caption retrieval enforces
    /// simulation/download mode, track selection and temporary output paths after these args.
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
        let output = self
            .run(
                ["--flat-playlist", "--dump-single-json", "--ignore-errors"],
                url,
            )
            .await?;
        self.items_from_info(url, &output).await
    }

    /// Resolves [YouTube](https://www.youtube.com) videos, playlists and channels without requiring audio formats.
    pub async fn resolve_youtube(&self, url: &str) -> Result<Vec<MediaItem>> {
        if !captions::is_youtube_url(url) {
            return Err(Error::Source(
                "the youtube transcriber requires YouTube URLs".into(),
            ));
        }
        let mut command = self.caption_command();
        command.args([
            "--flat-playlist",
            "--dump-single-json",
            "--ignore-errors",
            "--simulate",
            "--",
            url,
        ]);
        let output = self.execute(command, url, true).await?;
        self.items_from_info(url, &output).await
    }

    async fn items_from_info(&self, url: &str, output: &[u8]) -> Result<Vec<MediaItem>> {
        let info: Value = serde_json::from_slice(output).map_err(|err| {
            Error::Source(format!("yt-dlp returned invalid JSON for {url}: {err}"))
        })?;

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

    /// Fetches existing captions in exactly `language`, preferring manual captions to
    /// original automatic captions. Missing or translated-only tracks fail the item.
    /// Audio is never downloaded. Temporary caption files are removed after parsing.
    pub async fn captions(&self, item: &MediaItem, language: &str) -> Result<Vec<Segment>> {
        if !captions::is_subtitle_language(language) {
            return Err(Error::Source(
                "expected one subtitle language code, such as ar or en-US".into(),
            ));
        }
        let Media::Remote { url } = &item.media else {
            return Err(Error::Source(
                "the youtube transcriber requires YouTube URLs".into(),
            ));
        };
        if !captions::is_youtube_url(url) {
            return Err(Error::Source(
                "the youtube transcriber requires YouTube URLs".into(),
            ));
        }
        let mut command = self.caption_command();
        command.args([
            "--no-playlist",
            "--dump-single-json",
            "--simulate",
            "--",
            url,
        ]);
        let output = self.execute(command, url, false).await?;
        let mut info: Value = serde_json::from_slice(&output).map_err(|err| {
            Error::Source(format!("yt-dlp returned invalid JSON for {url}: {err}"))
        })?;
        captions::select_track(&mut info, language)?;

        let temporary =
            tempfile::tempdir().map_err(|err| Error::io("creating caption directory", err))?;
        let metadata = temporary.path().join("selected.info.json");
        tokio::fs::write(
            &metadata,
            serde_json::to_vec(&info).expect("JSON value serializes"),
        )
        .await
        .map_err(|err| Error::io("writing selected caption metadata", err))?;
        // Escape literal percent signs in TMPDIR before adding yt-dlp's extension template.
        let template = format!(
            "{}/captions.%(ext)s",
            temporary.path().to_string_lossy().replace('%', "%%")
        );
        let mut command = self.caption_command();
        command
            .args([
                "--no-playlist",
                "--no-simulate",
                "--write-subs",
                "--no-write-auto-subs",
                "--sub-langs",
                language,
                "--sub-format",
                "json3",
                "--convert-subs",
                "none",
                "--no-embed-subs",
                "--no-write-info-json",
                "--no-write-thumbnail",
                "--output",
                &template,
                "--output",
                &format!("subtitle:{template}"),
                "--load-info-json",
            ])
            .arg(&metadata);
        self.execute(command, url, false).await?;
        let path = temporary.path().join(format!("captions.{language}.json3"));
        let bytes = tokio::fs::read(&path).await.map_err(|err| {
            Error::io(
                format!("reading downloaded {language} captions for {url}"),
                err,
            )
        })?;
        captions::parse_json3(&bytes)
    }

    fn caption_command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args).args([
            "--skip-download",
            "--ignore-no-formats-error",
            "--no-progress",
            "--quiet",
        ]);
        command
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
        let output = self
            .run(
                [
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
            )
            .await?;
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

    /// Runs yt-dlp on `url` and returns its stdout. Warnings are logged; a run that failed
    /// without output becomes an error carrying yt-dlp's last error line.
    async fn run<'a>(&self, own: impl IntoIterator<Item = &'a str>, url: &str) -> Result<Vec<u8>> {
        let mut command = Command::new(&self.program);
        command.args(own).args(&self.args).arg("--").arg(url);
        self.execute(command, url, true).await
    }

    async fn execute(
        &self,
        mut command: Command,
        url: &str,
        allow_partial: bool,
    ) -> Result<Vec<u8>> {
        let output = command
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|err| Error::tool("yt-dlp", &self.program, err))?;

        let stderr = String::from_utf8_lossy(&output.stderr);
        let diagnostics = Diagnostics::parse(&stderr);
        diagnostics.warnings.iter().for_each(|w| self.log_once(w));
        if !output.status.success() && (!allow_partial || output.stdout.is_empty()) {
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
