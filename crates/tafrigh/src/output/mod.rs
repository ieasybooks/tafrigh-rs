//! Transcript formats and writing.

mod compact;
mod format;
mod layout;
mod sample;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use compact::compact;
pub use format::{BuiltinFormat, Format};
pub use layout::OutputMode;
pub(crate) use layout::{Layout, Target, unique_name};
pub(crate) use sample::{SampleReservoir, SampleRow, write_sample};

use crate::{Error, Result, Segment};

/// Writes transcripts for media items into a directory, one file per format.
#[derive(Debug, Clone)]
pub struct OutputWriter {
    dir: PathBuf,
    formats: Vec<Arc<dyn Format>>,
    min_words: usize,
    keep_uncompacted: bool,
}

/// Result of [`OutputWriter::write`].
#[derive(Debug, Clone, PartialEq)]
pub struct Written {
    /// Files written, in format order.
    pub files: Vec<PathBuf>,
    /// The segments after compaction, as written to the main files.
    pub segments: Vec<Segment>,
}

impl OutputWriter {
    /// All segment-level built-in formats ([`BuiltinFormat::SEGMENT_LEVEL`]), compaction to at
    /// least one word per segment.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            formats: BuiltinFormat::SEGMENT_LEVEL
                .into_iter()
                .map(Into::into)
                .collect(),
            min_words: 1,
            keep_uncompacted: false,
        }
    }

    /// Formats to write, replacing the defaults: [`BuiltinFormat`]s, or your own [`Format`]s
    /// as `Arc<dyn Format>`.
    #[must_use]
    pub fn formats<F: Into<Arc<dyn Format>>>(
        mut self,
        formats: impl IntoIterator<Item = F>,
    ) -> Self {
        self.formats = formats.into_iter().map(Into::into).collect();
        self
    }

    /// Segments with fewer words are merged into the following ones. `0` disables compaction.
    #[must_use]
    pub fn min_words(mut self, min_words: usize) -> Self {
        self.min_words = min_words;
        self
    }

    /// Also write `<id>.uncompacted.<ext>` files when compaction is enabled.
    #[must_use]
    pub fn keep_uncompacted(mut self, keep: bool) -> Self {
        self.keep_uncompacted = keep;
        self
    }

    /// Whether any configured format needs word timings.
    pub fn needs_words(&self) -> bool {
        self.formats.iter().any(|format| format.needs_words())
    }

    /// The output directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Use another directory with the same formats and compaction settings.
    #[must_use]
    pub fn in_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.dir = dir.into();
        self
    }

    /// Every file [`write`](Self::write) produces for `id`.
    pub fn outputs(&self, id: &str) -> Vec<PathBuf> {
        self.targets(id).map(|(path, _, _)| path).collect()
    }

    /// True when all outputs for `id` already exist.
    pub fn exists(&self, id: &str) -> bool {
        let outputs = self.outputs(id);
        !outputs.is_empty() && outputs.iter().all(|path| path.is_file())
    }

    /// Writes all formats atomically.
    pub fn write(&self, id: &str, segments: &[Segment]) -> Result<Written> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|err| Error::io(format!("creating {}", self.dir.display()), err))?;
        let compacted = compact(segments, self.min_words);
        let mut files = Vec::new();
        for (path, format, uncompacted) in self.targets(id) {
            let content = if uncompacted { segments } else { &compacted };
            write_atomic(&path, |out| format.write(content, out))?;
            files.push(path);
        }
        Ok(Written {
            files,
            segments: compacted,
        })
    }

    /// `(path, format, uncompacted)` for every file written for `id`.
    fn targets<'a>(
        &'a self,
        id: &'a str,
    ) -> impl Iterator<Item = (PathBuf, &'a Arc<dyn Format>, bool)> + 'a {
        let uncompacted = self.keep_uncompacted && self.min_words > 0;
        self.formats.iter().flat_map(move |format| {
            let main = (
                self.dir.join(format!("{id}.{}", format.extension())),
                format,
                false,
            );
            let extra = uncompacted.then(|| {
                (
                    self.dir
                        .join(format!("{id}.uncompacted.{}", format.extension())),
                    format,
                    true,
                )
            });
            std::iter::once(main).chain(extra)
        })
    }
}

/// The temporary file [`write_atomic`] writes before renaming it to `path`.
fn tmp_path(path: &Path) -> PathBuf {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    tmp.into()
}

/// Writes through a temporary file and renames it, so readers never see partial files.
fn write_atomic(
    path: &Path,
    write: impl FnOnce(&mut dyn Write) -> std::io::Result<()>,
) -> Result<()> {
    let tmp = tmp_path(path);
    let result = (|| {
        let file = std::fs::File::create(&tmp)?;
        let mut out = std::io::BufWriter::new(file);
        write(&mut out)?;
        out.into_inner()
            .map_err(std::io::IntoInnerError::into_error)?
            .sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    result.map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        Error::io(format!("writing {}", path.display()), err)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_every_target_atomically() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("out");
        let writer = OutputWriter::new(&dir)
            .formats([BuiltinFormat::Txt])
            .min_words(2)
            .keep_uncompacted(true);
        let segments = [
            Segment::new(0.0, 1.0, "one"),
            Segment::new(1.0, 2.0, "two words"),
        ];
        assert!(!writer.exists("x"));

        let written = writer.write("x", &segments).unwrap();
        assert_eq!(
            written.files,
            [dir.join("x.txt"), dir.join("x.uncompacted.txt")]
        );
        assert_eq!(written.segments, [Segment::new(0.0, 2.0, "one two words")]);
        assert_eq!(
            std::fs::read_to_string(dir.join("x.txt")).unwrap(),
            "one two words\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("x.uncompacted.txt")).unwrap(),
            "one\ntwo words\n"
        );
        assert!(writer.exists("x"));
        assert!(!dir.join("x.txt.tmp").exists());
    }
}
