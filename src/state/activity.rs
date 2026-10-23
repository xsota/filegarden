//! Activity feed and statistics.

use std::collections::VecDeque;

use chrono::{DateTime, Local};

use crate::change::{display_path, Change, ChangeKind};

/// Consecutive modifications of one file within this window share a feed line.
const FEED_MERGE_WINDOW: f64 = 3.0;

#[derive(Debug, Clone)]
pub struct FeedEntry {
    pub wall: DateTime<Local>,
    pub at: f64,
    pub kind: ChangeKind,
    pub path: String,
    pub from: Option<String>,
    pub is_dir: bool,
    pub count: u32,
}

pub struct Feed {
    entries: VecDeque<FeedEntry>,
    cap: usize,
}

impl Feed {
    pub fn new(cap: usize) -> Self {
        Feed { entries: VecDeque::with_capacity(cap), cap }
    }

    pub fn push(&mut self, change: &Change, now: f64, wall: DateTime<Local>) {
        let path = display_path(&change.path);
        if let Some(last) = self.entries.back_mut() {
            if change.kind == ChangeKind::Modify
                && last.kind == ChangeKind::Modify
                && last.path == path
                && now - last.at < FEED_MERGE_WINDOW
            {
                last.count += 1;
                last.at = now;
                last.wall = wall;
                return;
            }
        }
        if self.entries.len() == self.cap {
            self.entries.pop_front();
        }
        self.entries.push_back(FeedEntry {
            wall,
            at: now,
            kind: change.kind,
            path,
            from: change.from.as_deref().map(display_path),
            is_dir: change.is_dir,
            count: 1,
        });
    }

    /// Oldest first.
    pub fn entries(&self) -> &VecDeque<FeedEntry> {
        &self.entries
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Counters since startup. Events that are part of the initial scan are not counted.
#[derive(Debug, Default)]
pub struct Stats {
    pub created: u64,
    pub modified: u64,
    pub deleted: u64,
    pub renamed: u64,
    recent: VecDeque<f64>,
    pub last_event_at: Option<f64>,
}

pub const RECENT_WINDOW: f64 = 60.0;

impl Stats {
    pub fn record(&mut self, kind: ChangeKind, now: f64) {
        match kind {
            ChangeKind::Create => self.created += 1,
            ChangeKind::Modify => self.modified += 1,
            ChangeKind::Delete => self.deleted += 1,
            ChangeKind::Rename => self.renamed += 1,
        }
        self.recent.push_back(now);
        self.last_event_at = Some(now);
    }

    pub fn total(&self) -> u64 {
        self.created + self.modified + self.deleted + self.renamed
    }

    pub fn prune(&mut self, now: f64) {
        while self.recent.front().is_some_and(|&t| now - t > RECENT_WINDOW) {
            self.recent.pop_front();
        }
    }

    /// Events in the last 60 seconds.
    pub fn recent_count(&self, now: f64) -> usize {
        self.recent.iter().rev().take_while(|&&t| now - t <= RECENT_WINDOW).count()
    }

    /// Event counts per bucket over the last 60 seconds, oldest first.
    pub fn histogram(&self, now: f64, buckets: usize) -> Vec<u64> {
        let buckets = buckets.max(1);
        let mut out = vec![0u64; buckets];
        let width = RECENT_WINDOW / buckets as f64;
        for &t in self.recent.iter().rev() {
            let age = now - t;
            if age > RECENT_WINDOW {
                break;
            }
            let b = ((age / width) as usize).min(buckets - 1);
            out[buckets - 1 - b] += 1;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_modifies_merge() {
        let mut f = Feed::new(3);
        let w = Local::now();
        f.push(&Change::modify("a"), 0.0, w);
        f.push(&Change::modify("a"), 1.0, w);
        f.push(&Change::modify("a"), 3.5, w);
        f.push(&Change::modify("b"), 3.6, w);
        let e: Vec<_> = f.entries().iter().map(|e| (e.path.clone(), e.count)).collect();
        assert_eq!(e, [("a".into(), 3), ("b".into(), 1)]);
    }

    #[test]
    fn feed_is_bounded() {
        let mut f = Feed::new(2);
        let w = Local::now();
        for p in ["a", "b", "c"] {
            f.push(&Change::create(p, false), 0.0, w);
        }
        let e: Vec<_> = f.entries().iter().map(|e| e.path.as_str()).collect();
        assert_eq!(e, ["b", "c"]);
    }

    #[test]
    fn recent_window_and_histogram() {
        let mut s = Stats::default();
        for t in [0.0, 30.0, 59.0, 59.5] {
            s.record(ChangeKind::Modify, t);
        }
        assert_eq!(s.recent_count(70.0), 3);
        s.prune(70.0);
        assert_eq!(s.histogram(60.0, 6), [0, 0, 1, 0, 0, 2]);
        assert_eq!(s.total(), 4);
    }
}
