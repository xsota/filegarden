//! Backend-neutral raw events, converted from `notify` events.
//!
//! Raw events are only hints about which paths to look at. The aggregator decides what
//! actually happened by comparing the file system with its own model, which makes the
//! result independent of how a particular OS backend reports things.

use std::path::PathBuf;

use notify::event::{EventKind, ModifyKind, RenameMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RawKind {
    Create,
    /// `content: false` for pure metadata changes (permissions, timestamps).
    Modify {
        content: bool,
    },
    Remove,
    /// Rename source; paired with a `RenameTo` through the tracker id when available.
    RenameFrom,
    RenameTo,
    /// `paths[0]` was renamed to `paths[1]`.
    RenameBoth,
    /// A path involved in a rename, direction unknown (macOS FSEvents).
    RenameAny,
    /// The backend lost events; everything must be re-checked.
    Rescan,
    Ignore,
}

#[derive(Debug, Clone)]
pub struct RawEvent {
    pub kind: RawKind,
    /// Absolute paths, as reported by the backend.
    pub paths: Vec<PathBuf>,
    pub tracker: Option<usize>,
}

impl RawEvent {
    pub fn new(kind: RawKind, paths: Vec<PathBuf>) -> Self {
        RawEvent { kind, paths, tracker: None }
    }
}

impl From<notify::Event> for RawEvent {
    fn from(ev: notify::Event) -> Self {
        let kind = if ev.need_rescan() {
            RawKind::Rescan
        } else {
            match ev.kind {
                EventKind::Access(_) => RawKind::Ignore,
                EventKind::Create(_) => RawKind::Create,
                EventKind::Remove(_) => RawKind::Remove,
                EventKind::Modify(ModifyKind::Metadata(_)) => RawKind::Modify { content: false },
                EventKind::Modify(ModifyKind::Name(mode)) => match mode {
                    RenameMode::From => RawKind::RenameFrom,
                    RenameMode::To => RawKind::RenameTo,
                    RenameMode::Both if ev.paths.len() == 2 => RawKind::RenameBoth,
                    _ => RawKind::RenameAny,
                },
                EventKind::Modify(_) | EventKind::Any | EventKind::Other => RawKind::Modify { content: true },
            }
        };
        RawEvent { kind, tracker: ev.tracker(), paths: ev.paths }
    }
}
