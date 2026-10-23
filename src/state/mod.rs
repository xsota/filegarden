//! Everything the UI shows, updated from [`WatchMsg`]s. Independent of rendering.

pub mod activity;
pub mod tree;

use std::time::Duration;

use chrono::Local;

use crate::change::WatchMsg;
use crate::viz::Visualization;
use activity::{Feed, Stats};
use tree::{FileTree, NodeId};

pub const FEED_CAPACITY: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceStatus {
    Scanning,
    Live,
    RootMissing,
}

pub struct AppState {
    pub tree: FileTree,
    pub feed: Feed,
    pub stats: Stats,
    pub viz: Box<dyn Visualization>,
    pub status: SourceStatus,
    /// The most recent change worth scrolling to.
    pub focus: Option<NodeId>,
    pub notice: Option<(String, f64)>,
    pub scan_time: Option<Duration>,
    pub scan_errors: usize,
}

impl AppState {
    pub fn new(viz: Box<dyn Visualization>) -> Self {
        AppState {
            tree: FileTree::new(),
            feed: Feed::new(FEED_CAPACITY),
            stats: Stats::default(),
            viz,
            status: SourceStatus::Scanning,
            focus: None,
            notice: None,
            scan_time: None,
            scan_errors: 0,
        }
    }

    pub fn apply(&mut self, msg: WatchMsg, now: f64) {
        match msg {
            WatchMsg::Snapshot { entries, elapsed, errors } => {
                self.tree.load(&entries);
                self.viz.reset();
                self.focus = None;
                self.status = SourceStatus::Live;
                self.scan_time = Some(elapsed);
                self.scan_errors = errors;
                if errors > 0 {
                    self.notify(format!("{errors} entries could not be read"), now);
                }
            }
            WatchMsg::Changes(changes) => {
                let wall = Local::now();
                for c in &changes {
                    if let Some(id) = self.tree.apply(c, now) {
                        self.focus = Some(id);
                    }
                    self.viz.apply(c, now);
                    self.feed.push(c, now, wall);
                    self.stats.record(c.kind, now);
                }
            }
            WatchMsg::Rescanned { elapsed, errors, changes } => {
                self.scan_time = Some(elapsed);
                self.scan_errors = errors;
                let ms = elapsed.as_millis();
                let msg = match changes {
                    0 => format!("rescan: in sync ({ms} ms)"),
                    n => format!("rescan: recovered {n} change(s) ({ms} ms)"),
                };
                self.notify(msg, now);
            }
            WatchMsg::RootMissing => {
                self.status = SourceStatus::RootMissing;
                self.notify("watched directory was removed; waiting for it to come back".into(), now);
            }
            WatchMsg::RootRestored => {
                self.status = SourceStatus::Live;
                self.notify("watched directory is back".into(), now);
            }
            WatchMsg::Warning(w) => self.notify(w, now),
        }
    }

    pub fn notify(&mut self, msg: String, now: f64) {
        self.notice = Some((msg, now));
    }

    /// Expires ghosts and old statistics.
    pub fn tick(&mut self, now: f64) {
        self.tree.tick(now);
        self.viz.tick(now);
        self.stats.prune(now);
        if self.notice.as_ref().is_some_and(|(_, at)| now - at > 8.0) {
            self.notice = None;
        }
    }

    pub fn is_animating(&self, now: f64) -> bool {
        let recent = self.stats.last_event_at.is_some_and(|t| now - t < crate::ui::theme::GLOW + 0.1);
        recent || self.tree.has_ghosts() || self.viz.is_animating(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::{Change, Entry};
    use crate::viz::grid::ActivityGrid;

    #[test]
    fn snapshot_is_silent_and_changes_are_counted() {
        let mut s = AppState::new(Box::new(ActivityGrid::new()));
        s.apply(
            WatchMsg::Snapshot {
                entries: vec![Entry { path: "a.rs".into(), is_dir: false }],
                elapsed: Duration::ZERO,
                errors: 0,
            },
            0.0,
        );
        assert_eq!(s.stats.total(), 0);
        assert_eq!(s.tree.file_count(), 1);
        s.apply(
            WatchMsg::Changes(vec![
                Change::create("b.rs", false),
                Change::modify("a.rs"),
                Change::rename("b.rs", "c.rs", false),
                Change::delete("a.rs", false),
            ]),
            1.0,
        );
        assert_eq!((s.stats.created, s.stats.modified, s.stats.renamed, s.stats.deleted), (1, 1, 1, 1));
        assert_eq!(s.tree.file_count(), 1);
        assert_eq!(s.feed.entries().len(), 4);
        assert!(s.is_animating(1.5));
        s.tick(10.0);
        assert!(!s.is_animating(10.0));
    }
}
