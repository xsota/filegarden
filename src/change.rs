//! Logical change events shared by the watcher, the demo generator and the UI state.

use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    Create,
    Modify,
    Delete,
    Rename,
}

impl ChangeKind {
    pub fn symbol(self, ascii: bool) -> &'static str {
        match self {
            ChangeKind::Create => "+",
            ChangeKind::Modify => "~",
            ChangeKind::Delete => "-",
            ChangeKind::Rename if ascii => ">",
            ChangeKind::Rename => "→",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ChangeKind::Create => "create",
            ChangeKind::Modify => "modify",
            ChangeKind::Delete => "delete",
            ChangeKind::Rename => "rename",
        }
    }
}

/// A de-duplicated, logical file system change. Paths are relative to the watched root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub kind: ChangeKind,
    /// The affected path. For renames this is the new location.
    pub path: PathBuf,
    /// The previous location for renames.
    pub from: Option<PathBuf>,
    pub is_dir: bool,
}

impl Change {
    pub fn create(path: impl Into<PathBuf>, is_dir: bool) -> Self {
        Change { kind: ChangeKind::Create, path: path.into(), from: None, is_dir }
    }

    pub fn modify(path: impl Into<PathBuf>) -> Self {
        Change { kind: ChangeKind::Modify, path: path.into(), from: None, is_dir: false }
    }

    pub fn delete(path: impl Into<PathBuf>, is_dir: bool) -> Self {
        Change { kind: ChangeKind::Delete, path: path.into(), from: None, is_dir }
    }

    pub fn rename(from: impl Into<PathBuf>, to: impl Into<PathBuf>, is_dir: bool) -> Self {
        Change { kind: ChangeKind::Rename, path: to.into(), from: Some(from.into()), is_dir }
    }
}

/// An entry of a directory snapshot (relative path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Messages sent from a change source (real watcher or demo) to the UI.
#[derive(Debug, Clone)]
pub enum WatchMsg {
    /// Full state of the tree. Replaces whatever the UI knew, silently.
    Snapshot {
        entries: Vec<Entry>,
        elapsed: Duration,
        errors: usize,
    },
    /// A batch of logical changes.
    Changes(Vec<Change>),
    /// A rescan finished; any differences were already delivered as `Changes`.
    Rescanned {
        elapsed: Duration,
        errors: usize,
        changes: usize,
    },
    RootMissing,
    RootRestored,
    Warning(String),
}

/// Renders a relative path with `/` separators on every platform.
pub fn display_path(path: &Path) -> String {
    let mut out = String::new();
    for (i, c) in path.components().enumerate() {
        if i > 0 {
            out.push('/');
        }
        out.push_str(&c.as_os_str().to_string_lossy());
    }
    out
}
