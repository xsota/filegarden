//! Directory scanning. Only metadata is read, never file contents.

use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use walkdir::WalkDir;

use super::filter::Filter;

/// What the watcher remembers about an entry; enough to detect missed modifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryMeta {
    pub is_dir: bool,
    pub len: u64,
    pub mtime: Option<SystemTime>,
}

impl EntryMeta {
    /// `meta` must come from `symlink_metadata` so symlinks are never treated as directories.
    pub fn from_metadata(meta: &Metadata) -> Self {
        let is_dir = meta.is_dir();
        EntryMeta { is_dir, len: if is_dir { 0 } else { meta.len() }, mtime: meta.modified().ok() }
    }
}

pub fn stat(path: &Path) -> Option<EntryMeta> {
    std::fs::symlink_metadata(path).ok().map(|m| EntryMeta::from_metadata(&m))
}

#[derive(Debug, Clone)]
pub struct ScanEntry {
    /// Relative to the watched root.
    pub path: PathBuf,
    pub meta: EntryMeta,
}

#[derive(Debug, Default)]
pub struct ScanOutput {
    pub entries: Vec<ScanEntry>,
    /// Entries that could not be read (permissions, races).
    pub errors: usize,
}

/// Walks `start` (which must be `root` or a directory below it), excluding `start` itself.
/// Symlinks are not followed, so link cycles cannot cause infinite recursion.
/// Entries are yielded parents-first in name order.
pub fn scan(root: &Path, start: &Path, filter: &Filter) -> ScanOutput {
    let mut out = ScanOutput::default();
    let walker =
        WalkDir::new(start).follow_links(false).min_depth(1).sort_by_file_name().into_iter().filter_entry(
            |e| match e.path().strip_prefix(root) {
                Ok(rel) => !filter.is_excluded(rel),
                Err(_) => false,
            },
        );
    for item in walker {
        let entry = match item {
            Ok(e) => e,
            Err(_) => {
                out.errors += 1;
                continue;
            }
        };
        let Ok(rel) = entry.path().strip_prefix(root) else { continue };
        match entry.metadata() {
            Ok(meta) => {
                out.entries.push(ScanEntry { path: rel.to_path_buf(), meta: EntryMeta::from_metadata(&meta) })
            }
            Err(_) => out.errors += 1,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn scans_recursively_and_respects_filter() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src/deep/er")).unwrap();
        fs::create_dir_all(root.join("node_modules/x")).unwrap();
        fs::write(root.join("src/deep/er/a.rs"), "a").unwrap();
        fs::write(root.join("node_modules/x/i.js"), "x").unwrap();
        fs::write(root.join("README.md"), "hello").unwrap();

        let out = scan(root, root, &Filter::new(["node_modules"]).unwrap());
        let paths: Vec<_> = out.entries.iter().map(|e| e.path.clone()).collect();
        assert_eq!(
            paths,
            ["README.md", "src", "src/deep", "src/deep/er", "src/deep/er/a.rs"]
                .iter()
                .map(PathBuf::from)
                .collect::<Vec<_>>()
        );
        assert_eq!(out.entries[0].meta.len, 5);
        assert!(out.entries[1].meta.is_dir);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_loops_do_not_recurse() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir(root.join("a")).unwrap();
        std::os::unix::fs::symlink(root, root.join("a/loop")).unwrap();
        let out = scan(root, root, &Filter::none());
        assert_eq!(out.entries.len(), 2);
        assert!(!out.entries[1].meta.is_dir, "symlink is listed as a leaf");
    }
}
