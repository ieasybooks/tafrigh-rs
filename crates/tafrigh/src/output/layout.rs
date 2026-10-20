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

    pub fn write(&self, segments: &[crate::Segment]) -> Result<super::Written> {
        self.writer.write(&self.name, segments)
    }
}

/// Run-wide layout and reservations for output files and their parent directories.
pub(crate) struct Layout {
    mode: OutputMode,
    writer: OutputWriter,
    roots: Vec<Option<(PathBuf, PathBuf)>>,
    directories: HashSet<String>,
    reserved: HashSet<String>,
}

impl Layout {
    pub fn new(mode: OutputMode, writer: OutputWriter, inputs: &[Input]) -> Self {
        let separate_roots = inputs
            .iter()
            .filter(|input| matches!(input, Input::Path(path) if path.is_dir()))
            .count()
            > 1;
        let mut names = HashSet::new();
        // Allocate roots before discovery: a failed input still owns its directory name.
        let roots: Vec<_> = inputs
            .iter()
            .map(|input| {
                let Input::Path(path) = input else {
                    return None;
                };
                if mode != OutputMode::TreeToTree || !path.is_dir() {
                    return None;
                }
                let mut destination = writer.dir().to_owned();
                if separate_roots {
                    let root = path.canonicalize().unwrap_or_else(|_| path.clone());
                    let name = root
                        .file_name()
                        .unwrap_or_else(|| "root".as_ref())
                        .to_string_lossy();
                    destination.push(unique_name(&name, |name| names.insert(name.to_lowercase())));
                }
                Some((path.clone(), destination))
            })
            .collect();
        let mut directories = HashSet::new();
        for (_, destination) in roots.iter().flatten() {
            // Even empty or undiscoverable roots keep their directory paths. Unusable
            // destinations are diagnosed later, for each affected item.
            if let Ok(dir) = normalize_dir(destination) {
                directories.extend(dir.ancestors().map(path_key));
            }
        }
        Self {
            mode,
            writer,
            roots,
            directories,
            reserved: HashSet::new(),
        }
    }

    /// Remote side-by-side destinations are known only after downloading.
    pub fn directory(&self, item: &MediaItem, input: usize) -> Option<PathBuf> {
        Some(match (self.mode, &item.media) {
            (OutputMode::SideBySide, Media::Remote { .. }) => return None,
            (OutputMode::SideBySide, Media::File(path)) => parent(path).to_owned(),
            (OutputMode::TreeToTree, Media::File(path)) => match &self.roots[input] {
                Some((source, destination)) => {
                    let relative = path
                        .strip_prefix(source)
                        .expect("discovered file is inside its input root");
                    destination.join(parent(relative))
                }
                None => self.writer.dir().to_owned(),
            },
            _ => self.writer.dir().to_owned(),
        })
    }

    /// Register all known directories before any item claims a file name.
    pub fn require_directory(&mut self, dir: &Path) -> Result<PathBuf> {
        let dir = normalize_dir(dir)?;
        self.directories.extend(dir.ancestors().map(path_key));
        Ok(dir)
    }

    pub fn plan(&mut self, item: &MediaItem, dir: &Path) -> Result<Target> {
        self.reserve(dir, &item.id)
    }

    pub fn downloaded(&mut self, path: &Path) -> Result<Target> {
        let dir = self.require_directory(parent(path))?;
        let name = path
            .file_stem()
            .unwrap_or_else(|| "transcript".as_ref())
            .to_string_lossy();
        self.reserve(&dir, &name)
    }

    fn reserve(&mut self, dir: &Path, name: &str) -> Result<Target> {
        let writer = self.writer.clone().in_dir(normalize_dir(dir)?);
        let name = unique_name(name, |candidate| {
            let outputs = writer.outputs(candidate);
            // Atomic writes need temporary files as well as their final destinations.
            let mut paths = outputs.clone();
            paths.extend(outputs.iter().map(|path| {
                let mut tmp = path.as_os_str().to_owned();
                tmp.push(".tmp");
                PathBuf::from(tmp)
            }));
            if paths.iter().any(|path| {
                self.reserved.contains(&path_key(path))
                    || self.directories.contains(&path_key(path))
                    || path.is_dir()
            }) {
                return false;
            }
            self.reserved
                .extend(paths.iter().map(|path| path_key(path)));
            true
        });
        Ok(Target { writer, name })
    }

    /// The sample report is allocated last so it cannot replace a transcript or directory.
    pub fn sample_path(&self) -> Result<PathBuf> {
        let dir = normalize_dir(if self.mode == OutputMode::SideBySide {
            Path::new(".")
        } else {
            self.writer.dir()
        })?;
        let name = unique_name("sample", |name| {
            let path = dir.join(format!("{name}.csv"));
            !self.reserved.contains(&path_key(&path))
                && !self.directories.contains(&path_key(&path))
                && !path.is_dir()
        });
        Ok(dir.join(format!("{name}.csv")))
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
            Ok(path) => {
                if !path.is_dir() {
                    return Err(Error::io(
                        format!("resolving {}", path.display()),
                        std::io::ErrorKind::NotADirectory.into(),
                    ));
                }
                normalized = path;
            }
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
    fn directory_roots_are_reserved_before_any_discovery() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("a.txt");
        let second = temp.path().join("other");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let out = temp.path().join("out");
        let writer =
            OutputWriter::new(&out).formats([Arc::new(BuiltinFormat::Txt) as Arc<dyn Format>]);
        let mut layout = Layout::new(
            OutputMode::TreeToTree,
            writer,
            &[Input::Path(first), Input::Path(second)],
        );
        assert_eq!(layout.reserve(&out, "a").unwrap().name, "a-2");
    }

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
        let mut layout = Layout::new(OutputMode::Flat, writer, &[]);
        for (name, expected) in [
            ("foo", "foo"),
            ("foo", "foo-2"),
            ("foo-2", "foo-2-2"),
            ("foo", "foo-3"),
            ("foo.words", "foo.words-2"),
            ("foo.uncompacted", "foo.uncompacted-2"),
            ("FOO", "FOO-4"),
            ("sample", "sample"),
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
        let mut layout = Layout::new(OutputMode::Flat, OutputWriter::new(&real), &[]);
        assert_eq!(layout.reserve(&real.join("new"), "a").unwrap().name, "a");
        assert_eq!(layout.reserve(&alias.join("new"), "a").unwrap().name, "a-2");
    }

    #[test]
    fn side_by_side_sample_is_in_the_working_directory() {
        let layout = Layout::new(OutputMode::SideBySide, OutputWriter::new("ignored"), &[]);
        assert_eq!(
            layout.sample_path().unwrap(),
            std::env::current_dir()
                .unwrap()
                .canonicalize()
                .unwrap()
                .join("sample.csv")
        );
    }
}
