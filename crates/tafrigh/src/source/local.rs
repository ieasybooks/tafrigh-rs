use std::path::Path;
use std::sync::Arc;

use super::{Media, MediaItem};
use crate::{Error, Result};

/// File extensions treated as media when expanding folders.
const MEDIA_EXTENSIONS: &[&str] = &[
    "3gp", "aac", "ac3", "aif", "aiff", "amr", "ape", "au", "avi", "caf", "flac", "flv", "m2ts",
    "m4a", "m4b", "m4v", "mka", "mkv", "mov", "mp2", "mp3", "mp4", "mpeg", "mpg", "mts", "oga",
    "ogg", "ogv", "opus", "ra", "spx", "ts", "vob", "wav", "weba", "webm", "wma", "wmv",
];

/// True when the file extension is one of [`MEDIA_EXTENSIONS`] (case-insensitive).
pub(super) fn is_media_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| MEDIA_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// A file becomes one item; a folder becomes its media files (with `recursive`, also those in
/// nested folders other than `excluded`), sorted by path.
pub(crate) fn discover_local(
    path: &Path,
    recursive: bool,
    excluded: Option<&Path>,
) -> Result<Vec<MediaItem>> {
    let meta = std::fs::metadata(path)
        .map_err(|err| Error::io(format!("reading {}", path.display()), err))?;
    if meta.is_file() {
        return Ok(vec![item(path)]);
    }
    // A directory that does not exist yet cannot appear in this traversal.
    let excluded = excluded.and_then(|dir| dir.canonicalize().ok());
    let mut files = Vec::new();
    let mut directories = vec![path.to_owned()];
    while let Some(dir) = directories.pop() {
        let source_error = |err| Error::io(format!("reading {}", dir.display()), err);
        for entry in std::fs::read_dir(&dir).map_err(source_error)? {
            let entry = entry.map_err(source_error)?;
            let kind = entry.file_type().map_err(source_error)?;
            let path = entry.path();
            if recursive && kind.is_dir() {
                if let Some(excluded) = &excluded
                    && path.canonicalize().map_err(source_error)? == *excluded
                {
                    continue;
                }
                directories.push(path);
            } else if is_media_path(&path) && path.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files.iter().map(|file| item(file)).collect())
}

fn item(path: &Path) -> MediaItem {
    let stem = path.file_stem().map_or_else(
        || "transcript".to_owned(),
        |stem| stem.to_string_lossy().into_owned(),
    );
    MediaItem {
        id: Arc::from(stem.as_str()),
        title: path
            .file_name()
            .map(|name| Arc::from(name.to_string_lossy().as_ref())),
        link: None,
        media: Media::File(path.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for name in [
            "z.mp3",
            "a/02.MP4",
            "a/deeper/01.wav",
            "notes.txt",
            "out/old.mp3",
        ] {
            let path = dir.path().join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"").unwrap();
        }
        dir
    }

    fn paths(items: Vec<MediaItem>, root: &Path) -> Vec<std::path::PathBuf> {
        items
            .into_iter()
            .map(|item| match item.media {
                Media::File(path) => path.strip_prefix(root).unwrap().to_owned(),
                Media::Remote { .. } => panic!("local discovery returned a URL"),
            })
            .collect()
    }

    #[test]
    fn shallow_by_default_and_recursive_in_path_order() {
        let dir = fixture();
        let root = dir.path();
        assert_eq!(
            paths(discover_local(root, false, None).unwrap(), root),
            [Path::new("z.mp3")]
        );
        assert_eq!(
            paths(discover_local(root, true, None).unwrap(), root),
            ["a/02.MP4", "a/deeper/01.wav", "out/old.mp3", "z.mp3"].map(std::path::PathBuf::from),
        );
        // An explicitly supplied file still works, regardless of its extension.
        assert_eq!(
            discover_local(&root.join("notes.txt"), false, None)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn excludes_nested_output_but_not_the_input_root() {
        let dir = fixture();
        let root = dir.path();
        let items = discover_local(root, true, Some(&root.join("a/../out"))).unwrap();
        assert_eq!(
            paths(items, root),
            ["a/02.MP4", "a/deeper/01.wav", "z.mp3"].map(std::path::PathBuf::from)
        );
        assert_eq!(discover_local(root, true, Some(root)).unwrap().len(), 4);
        assert_eq!(
            discover_local(root, true, Some(&root.join("new-output")))
                .unwrap()
                .len(),
            4
        );
    }

    #[cfg(unix)]
    #[test]
    fn does_not_follow_directory_symlinks() {
        let dir = fixture();
        let root = dir.path();
        std::os::unix::fs::symlink(root, root.join("a/loop")).unwrap();
        std::os::unix::fs::symlink(root.join("z.mp3"), root.join("linked.mp3")).unwrap();
        let items = discover_local(root, true, None).unwrap();
        assert_eq!(items.len(), 5);
        assert!(items.iter().any(|item| &*item.id == "linked"));
    }

    #[test]
    fn missing_inputs_report_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let error = discover_local(&missing, true, None).unwrap_err();
        assert!(error.to_string().contains(&missing.display().to_string()));
    }
}
