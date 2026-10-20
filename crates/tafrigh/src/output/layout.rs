use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use super::OutputWriter;
use crate::source::{Input, Media, MediaItem};
use crate::{Error, Result};

/// How a run places transcripts relative to its inputs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputMode {
    /// Beside each local or downloaded media file.
    SideBySide,
    /// Mirror input directories under the output directory, with all formats together.
    TreeToTree,
    /// Put every transcript directly in the output directory (the library default).
    #[default]
    Flat,
}

/// One item's destination, independent of its progress ID and download ID.
pub(crate) struct Target {
    pub writer: OutputWriter,
    pub name: String,
}

impl Target {
    pub fn exists(&self) -> bool {
        self.writer.exists(&self.name)
    }
}

/// Run-wide layout and reservations for every selected output format.
pub(crate) struct Layout {
    mode: OutputMode,
    writer: OutputWriter,
    separate_roots: bool,
    roots: HashSet<String>,
    reserved: HashSet<String>,
    pub sample_path: PathBuf,
}

impl Layout {
    pub fn new(
        mode: OutputMode,
        writer: OutputWriter,
        inputs: &[Input],
        sample: bool,
    ) -> Result<Self> {
        let sample_dir = if mode == OutputMode::SideBySide {
            Path::new(".")
        } else {
            writer.dir()
        };
        let sample_path = normalize_dir(sample_dir)?.join("sample.csv");
        let mut reserved = HashSet::new();
        if sample {
            reserved.insert(path_key(&sample_path));
        }
        Ok(Self {
            mode,
            writer,
            separate_roots: inputs
                .iter()
                .filter(|input| matches!(input, Input::Path(path) if path.is_dir()))
                .count()
                > 1,
            roots: HashSet::new(),
            reserved,
            sample_path,
        })
    }

    /// Source and destination roots for a directory input in tree mode.
    pub fn input_root(&mut self, input: &Input) -> Option<(PathBuf, PathBuf)> {
        let Input::Path(path) = input else {
            return None;
        };
        if self.mode != OutputMode::TreeToTree || !path.is_dir() {
            return None;
        }
        let mut destination = self.writer.dir().to_owned();
        if self.separate_roots {
            // Canonicalization also gives "." and ".." their directory names.
            let root = path.canonicalize().unwrap_or_else(|_| path.clone());
            let name = root
                .file_name()
                .unwrap_or_else(|| "root".as_ref())
                .to_string_lossy();
            destination.push(unique_name(&name, |name| {
                self.roots.insert(name.to_lowercase())
            }));
        }
        Some((path.clone(), destination))
    }

    /// Remote side-by-side destinations are known only after downloading.
    pub fn plan(
        &mut self,
        item: &MediaItem,
        root: Option<&(PathBuf, PathBuf)>,
    ) -> Result<Option<Target>> {
        let dir = match (self.mode, &item.media) {
            (OutputMode::SideBySide, Media::Remote { .. }) => return Ok(None),
            (OutputMode::SideBySide, Media::File(path)) => parent(path).to_owned(),
            (OutputMode::TreeToTree, Media::File(path)) => match root {
                Some((source, destination)) => {
                    let relative = path
                        .strip_prefix(source)
                        .expect("discovered file is inside its input root");
                    destination.join(parent(relative))
                }
                None => self.writer.dir().to_owned(),
            },
            _ => self.writer.dir().to_owned(),
        };
        self.reserve(&dir, &item.id).map(Some)
    }

    pub fn downloaded(&mut self, path: &Path) -> Result<Target> {
        let name = path
            .file_stem()
            .unwrap_or_else(|| "transcript".as_ref())
            .to_string_lossy();
        self.reserve(parent(path), &name)
    }

    fn reserve(&mut self, dir: &Path, name: &str) -> Result<Target> {
        let writer = self.writer.clone().in_dir(normalize_dir(dir)?);
        let name = unique_name(name, |name| {
            let paths: Vec<_> = writer
                .outputs(name)
                .iter()
                .map(|path| path_key(path))
                .collect();
            if paths.iter().any(|path| self.reserved.contains(path)) {
                return false;
            }
            self.reserved.extend(paths);
            true
        });
        Ok(Target { writer, name })
    }
}

/// Claim the original name, or the first available numeric suffix.
pub(crate) fn unique_name(name: &str, mut claim: impl FnMut(&str) -> bool) -> String {
    let mut candidate = name.to_owned();
    let mut suffix = 2;
    while !claim(&candidate) {
        candidate = format!("{name}-{suffix}");
        suffix += 1;
    }
    candidate
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Resolve existing symlinks and normalize relative paths, including directories not created yet.
fn normalize_dir(path: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(path)
        .map_err(|err| Error::io(format!("resolving {}", path.display()), err))?;
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            _ => normalized.push(component),
        }
        match normalized.canonicalize() {
            Ok(path) => normalized = path,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => {
                return Err(Error::io(
                    format!("resolving {}", normalized.display()),
                    err,
                ));
            }
        }
    }
    Ok(normalized)
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::output::{BuiltinFormat, Format};

    #[test]
    fn reserves_every_format_and_variant_only_in_its_destination() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path();
        let writer = OutputWriter::new(dir)
            .formats([
                Arc::new(BuiltinFormat::Srt) as Arc<dyn Format>,
                Arc::new(BuiltinFormat::WordsSrt),
                Arc::new(BuiltinFormat::Csv),
            ])
            .keep_uncompacted(true);
        let mut layout = Layout::new(OutputMode::Flat, writer, &[], true).unwrap();
        for (name, expected) in [
            ("foo", "foo"),
            ("foo", "foo-2"),
            ("foo-2", "foo-2-2"),
            ("foo", "foo-3"),
            ("foo.words", "foo.words-2"),
            ("foo.uncompacted", "foo.uncompacted-2"),
            ("FOO", "FOO-4"),
            ("sample", "sample-2"),
        ] {
            assert_eq!(layout.reserve(dir, name).unwrap().name, expected);
        }
        assert_eq!(
            layout.reserve(&dir.join("other"), "foo").unwrap().name,
            "foo"
        );
        assert_eq!(
            layout.reserve(&dir.join("other/../."), "foo").unwrap().name,
            "foo-5"
        );
    }

    #[cfg(unix)]
    #[test]
    fn aliases_share_reservations() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        let alias = temp.path().join("alias");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let mut layout =
            Layout::new(OutputMode::Flat, OutputWriter::new(&real), &[], false).unwrap();
        assert_eq!(layout.reserve(&real.join("new"), "a").unwrap().name, "a");
        assert_eq!(layout.reserve(&alias.join("new"), "a").unwrap().name, "a-2");
    }

    #[test]
    fn side_by_side_sample_is_in_the_working_directory() {
        let layout = Layout::new(
            OutputMode::SideBySide,
            OutputWriter::new("ignored"),
            &[],
            true,
        )
        .unwrap();
        assert_eq!(
            layout.sample_path,
            std::env::current_dir()
                .unwrap()
                .canonicalize()
                .unwrap()
                .join("sample.csv")
        );
    }
}
