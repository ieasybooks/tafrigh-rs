use std::path::Path;
use std::sync::Arc;

use super::{Media, MediaItem};
use crate::{Error, Result};

/// File extensions treated as media when expanding folders.
pub const MEDIA_EXTENSIONS: &[&str] = &[
    "3gp", "aac", "ac3", "aif", "aiff", "amr", "ape", "au", "avi", "caf", "flac", "flv", "m2ts",
    "m4a", "m4b", "m4v", "mka", "mkv", "mov", "mp2", "mp3", "mp4", "mpeg", "mpg", "mts", "oga",
    "ogg", "ogv", "opus", "ra", "spx", "ts", "vob", "wav", "weba", "webm", "wma", "wmv",
];

/// True when the file extension is one of [`MEDIA_EXTENSIONS`] (case-insensitive).
pub fn is_media_path(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| MEDIA_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
}

/// A file becomes one item; a folder becomes its media files, sorted by name.
pub fn expand_local(path: &Path) -> Result<Vec<MediaItem>> {
    let meta = std::fs::metadata(path)
        .map_err(|err| Error::Source(format!("{}: {err}", path.display())))?;
    if meta.is_file() {
        return Ok(vec![item(path)]);
    }
    let mut files = Vec::new();
    let entries = std::fs::read_dir(path)
        .map_err(|err| Error::Source(format!("{}: {err}", path.display())))?;
    for entry in entries {
        let entry = entry.map_err(|err| Error::Source(format!("{}: {err}", path.display())))?;
        let entry_path = entry.path();
        if entry_path.is_file() && is_media_path(&entry_path) {
            files.push(entry_path);
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
