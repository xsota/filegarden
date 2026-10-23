//! Turns noisy raw events into logical changes.
//!
//! Raw events mark paths as dirty. When a batch window closes, every dirty path is
//! re-examined (`symlink_metadata`) and compared with the aggregator's model of the tree:
//!
//! | before  | now     | result                                   |
//! |---------|---------|------------------------------------------|
//! | absent  | absent  | nothing (short-lived temp file)          |
//! | absent  | present | CREATE (directories are scanned)         |
//! | present | absent  | DELETE (whole subtree removed)           |
//! | present | present | MODIFY (files only)                      |
//!
//! This collapses bursts of events per file into one change and makes editor
//! "write temp + rename over original" saves show up as a single MODIFY.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::change::{Change, ChangeKind, Entry};

use super::events::{RawEvent, RawKind};
use super::filter::Filter;
use super::scanner::{self, EntryMeta};

#[derive(Debug, Default, Clone, Copy)]
struct Dirty {
    content: bool,
    rename: bool,
}

#[derive(Debug, Default)]
struct Pending {
    since: Option<Instant>,
    dirty: BTreeMap<PathBuf, Dirty>,
    /// First-seen order of dirty paths, so a batch is reported in the order it happened.
    order: HashMap<PathBuf, u64>,
    pairs: Vec<(PathBuf, PathBuf)>,
    from_by_tracker: HashMap<usize, PathBuf>,
    rescan: bool,
    root_touched: bool,
}

/// Result of closing a batch.
#[derive(Debug, Default)]
pub struct Flush {
    pub changes: Vec<Change>,
    /// Directories that appeared (relative); the watcher may need to watch them.
    pub dirs_added: Vec<PathBuf>,
    /// Directories that disappeared (relative).
    pub dirs_removed: Vec<PathBuf>,
    /// The backend asked for a full rescan.
    pub needs_rescan: bool,
    /// The watched root itself no longer exists.
    pub root_missing: bool,
}

#[derive(Debug)]
pub struct ScanSummary {
    pub entries: Vec<Entry>,
    pub dirs: Vec<PathBuf>,
    pub errors: usize,
    pub elapsed: Duration,
}

pub struct Aggregator {
    root: PathBuf,
    filter: Filter,
    window: Duration,
    known: BTreeMap<PathBuf, EntryMeta>,
    pending: Pending,
}

impl Aggregator {
    pub fn new(root: PathBuf, filter: Filter, window: Duration) -> Self {
        Aggregator { root, filter, window, known: BTreeMap::new(), pending: Pending::default() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn known_len(&self) -> usize {
        self.known.len()
    }

    pub fn is_known(&self, rel: &Path) -> bool {
        self.known.contains_key(rel)
    }

    pub fn root_exists(&self) -> bool {
        std::fs::metadata(&self.root).map(|m| m.is_dir()).unwrap_or(false)
    }

    /// Replaces the model with a fresh scan, emitting nothing.
    pub fn initial_scan(&mut self) -> ScanSummary {
        let started = Instant::now();
        let out = scanner::scan(&self.root, &self.root, &self.filter);
        self.known.clear();
        let mut entries = Vec::with_capacity(out.entries.len());
        let mut dirs = Vec::new();
        for e in out.entries {
            if e.meta.is_dir {
                dirs.push(e.path.clone());
            }
            entries.push(Entry { path: e.path.clone(), is_dir: e.meta.is_dir });
            self.known.insert(e.path, e.meta);
        }
        ScanSummary { entries, dirs, errors: out.errors, elapsed: started.elapsed() }
    }

    /// Records a raw event. Cheap: no file system access happens here.
    pub fn record(&mut self, ev: RawEvent, now: Instant) {
        if ev.kind == RawKind::Ignore {
            return;
        }
        if ev.kind == RawKind::Rescan {
            self.pending.rescan = true;
            self.pending.since.get_or_insert(now);
            return;
        }
        let rels: Vec<Option<PathBuf>> = ev.paths.iter().map(|p| self.relative(p)).collect();
        let content = !matches!(ev.kind, RawKind::Modify { content: false });
        let rename = matches!(
            ev.kind,
            RawKind::RenameFrom | RawKind::RenameTo | RawKind::RenameBoth | RawKind::RenameAny
        );
        match (ev.kind, ev.tracker) {
            (RawKind::RenameBoth, _) => {
                if let (Some(Some(from)), Some(Some(to))) = (rels.first(), rels.get(1)) {
                    self.pending.pairs.push((from.clone(), to.clone()));
                }
            }
            (RawKind::RenameFrom, Some(t)) => {
                if let Some(Some(from)) = rels.first() {
                    self.pending.from_by_tracker.insert(t, from.clone());
                }
            }
            (RawKind::RenameTo, Some(t)) => {
                if let (Some(from), Some(Some(to))) = (self.pending.from_by_tracker.remove(&t), rels.first())
                {
                    self.pending.pairs.push((from, to.clone()));
                }
            }
            _ => {}
        }
        let mut touched = self.pending.root_touched;
        for rel in rels.into_iter().flatten() {
            let seq = self.pending.order.len() as u64;
            self.pending.order.entry(rel.clone()).or_insert(seq);
            let d = self.pending.dirty.entry(rel).or_default();
            d.content |= content;
            d.rename |= rename;
            touched = true;
        }
        if touched {
            self.pending.since.get_or_insert(now);
        }
    }

    /// Maps an absolute backend path to a relative one; `None` if outside or excluded.
    fn relative(&mut self, abs: &Path) -> Option<PathBuf> {
        let rel = abs.strip_prefix(&self.root).ok()?;
        if rel.as_os_str().is_empty() {
            self.pending.root_touched = true;
            return None;
        }
        if self.filter.is_excluded(rel) {
            return None;
        }
        Some(rel.to_path_buf())
    }

    pub fn has_pending(&self) -> bool {
        self.pending.since.is_some()
    }

    /// When the current batch should be flushed.
    pub fn deadline(&self) -> Option<Instant> {
        self.pending.since.map(|s| s + self.window)
    }

    pub fn is_due(&self, now: Instant) -> bool {
        self.deadline().is_some_and(|d| now >= d)
    }

    /// Closes the current batch and reconciles every dirty path with the file system.
    pub fn flush(&mut self) -> Flush {
        let pending = std::mem::take(&mut self.pending);
        let mut flush = Flush { needs_rescan: pending.rescan, ..Flush::default() };
        if pending.root_touched && !self.root_exists() {
            flush.root_missing = true;
            return flush;
        }

        let mut out: Vec<Change> = Vec::new();
        // Subtrees fully handled in this batch (renamed or freshly scanned directories).
        let mut handled: Vec<PathBuf> = Vec::new();
        // Metadata of entries deleted in this batch, used for rename pairing.
        let mut removed_meta: HashMap<PathBuf, EntryMeta> = HashMap::new();
        let is_handled = |handled: &[PathBuf], p: &Path| handled.iter().any(|h| p.starts_with(h));

        // 1. Renames the backend paired for us.
        for (from, to) in &pending.pairs {
            if is_handled(&handled, from) || is_handled(&handled, to) {
                continue;
            }
            let Some(old) = self.known.get(from).copied() else { continue };
            if self.stat(from).is_some() {
                continue;
            }
            let Some(new) = self.stat(to) else { continue };
            if new.is_dir != old.is_dir {
                continue;
            }
            self.remove_subtree(to);
            if old.is_dir {
                flush.dirs_removed.extend(self.dirs_in_subtree(from));
            }
            self.move_subtree(from, to);
            self.known.insert(to.clone(), new);
            if old.is_dir {
                flush.dirs_added.extend(self.dirs_in_subtree(to));
            }
            out.push(Change::rename(from.clone(), to.clone(), old.is_dir));
            handled.push(from.clone());
            handled.push(to.clone());
        }

        // 2. Everything else, parents before children.
        for (path, dirty) in &pending.dirty {
            if is_handled(&handled, path) {
                continue;
            }
            let before = self.known.get(path).copied();
            let now = self.stat(path);
            match (before, now) {
                (None, None) => {}
                (None, Some(meta)) => {
                    self.ensure_ancestors(path, &mut out, &mut flush);
                    self.add_created(path, meta, &mut out, &mut flush, &mut handled);
                }
                (Some(old), None) => {
                    self.delete(path, old, &mut out, &mut flush, &mut removed_meta);
                }
                (Some(old), Some(meta)) if old.is_dir != meta.is_dir => {
                    self.delete(path, old, &mut out, &mut flush, &mut removed_meta);
                    self.add_created(path, meta, &mut out, &mut flush, &mut handled);
                }
                (Some(old), Some(meta)) => {
                    self.known.insert(path.clone(), meta);
                    if !meta.is_dir && (dirty.content || old != meta) {
                        out.push(Change::modify(path.clone()));
                    }
                }
            }
        }

        // 3. Pair up deletes and creates of rename-flagged paths the backend left unpaired.
        let flagged: HashSet<&PathBuf> =
            pending.dirty.iter().filter(|(_, d)| d.rename).map(|(p, _)| p).collect();
        if !flagged.is_empty() {
            self.pair_renames(&mut out, &flagged, &removed_meta);
        }

        // Report in event order. A path never sorts before any of its ancestors, and paths
        // discovered by scanning share their ancestor's key; the sort is stable, so
        // parents stay ahead of their contents.
        let key = |p: &Path| -> u64 {
            p.ancestors().filter_map(|a| pending.order.get(a).copied()).max().unwrap_or(u64::MAX)
        };
        out.sort_by_key(|c| key(&c.path));

        flush.changes = out;
        flush
    }

    fn stat(&self, rel: &Path) -> Option<EntryMeta> {
        scanner::stat(&self.root.join(rel))
    }

    fn ensure_ancestors(&mut self, path: &Path, out: &mut Vec<Change>, flush: &mut Flush) {
        let mut missing = Vec::new();
        let mut cur = path.parent();
        while let Some(p) = cur {
            if p.as_os_str().is_empty() || self.known.contains_key(p) {
                break;
            }
            missing.push(p.to_path_buf());
            cur = p.parent();
        }
        for p in missing.into_iter().rev() {
            if let Some(meta) = self.stat(&p).filter(|m| m.is_dir) {
                self.known.insert(p.clone(), meta);
                flush.dirs_added.push(p.clone());
                out.push(Change::create(p, true));
            }
        }
    }

    fn add_created(
        &mut self,
        path: &Path,
        meta: EntryMeta,
        out: &mut Vec<Change>,
        flush: &mut Flush,
        handled: &mut Vec<PathBuf>,
    ) {
        self.known.insert(path.to_path_buf(), meta);
        out.push(Change::create(path, meta.is_dir));
        if meta.is_dir {
            flush.dirs_added.push(path.to_path_buf());
            // Files may have been written before any watch on the new directory existed.
            self.scan_new(path, out, flush);
            handled.push(path.to_path_buf());
        }
    }

    /// Adds entries below `dir` that the model does not know yet.
    fn scan_new(&mut self, dir: &Path, out: &mut Vec<Change>, flush: &mut Flush) -> usize {
        let scanned = scanner::scan(&self.root, &self.root.join(dir), &self.filter);
        let mut n = 0;
        for e in scanned.entries {
            if self.known.contains_key(&e.path) {
                continue;
            }
            if e.meta.is_dir {
                flush.dirs_added.push(e.path.clone());
            }
            out.push(Change::create(e.path.clone(), e.meta.is_dir));
            self.known.insert(e.path, e.meta);
            n += 1;
        }
        n
    }

    fn delete(
        &mut self,
        path: &Path,
        old: EntryMeta,
        out: &mut Vec<Change>,
        flush: &mut Flush,
        removed_meta: &mut HashMap<PathBuf, EntryMeta>,
    ) {
        for (p, m) in self.remove_subtree(path) {
            if m.is_dir {
                flush.dirs_removed.push(p);
            }
        }
        removed_meta.insert(path.to_path_buf(), old);
        out.push(Change::delete(path, old.is_dir));
    }

    fn subtree_keys(&self, path: &Path) -> Vec<PathBuf> {
        // Path ordering is component-wise, so a subtree is a contiguous range.
        self.known
            .range(path.to_path_buf()..)
            .take_while(|(k, _)| k.starts_with(path))
            .map(|(k, _)| k.clone())
            .collect()
    }

    fn dirs_in_subtree(&self, path: &Path) -> Vec<PathBuf> {
        self.known
            .range(path.to_path_buf()..)
            .take_while(|(k, _)| k.starts_with(path))
            .filter(|(_, m)| m.is_dir)
            .map(|(k, _)| k.clone())
            .collect()
    }

    fn remove_subtree(&mut self, path: &Path) -> Vec<(PathBuf, EntryMeta)> {
        self.subtree_keys(path).into_iter().filter_map(|k| self.known.remove(&k).map(|m| (k, m))).collect()
    }

    fn move_subtree(&mut self, from: &Path, to: &Path) {
        for k in self.subtree_keys(from) {
            if let Some(m) = self.known.remove(&k) {
                let suffix = k.strip_prefix(from).unwrap_or(Path::new(""));
                let new_key = if suffix.as_os_str().is_empty() { to.to_path_buf() } else { to.join(suffix) };
                self.known.insert(new_key, m);
            }
        }
    }

    fn pair_renames(
        &self,
        out: &mut Vec<Change>,
        flagged: &HashSet<&PathBuf>,
        removed_meta: &HashMap<PathBuf, EntryMeta>,
    ) {
        let deletes: Vec<usize> = (0..out.len())
            .filter(|&i| out[i].kind == ChangeKind::Delete && flagged.contains(&out[i].path))
            .collect();
        let mut creates: Vec<usize> = (0..out.len())
            .filter(|&i| out[i].kind == ChangeKind::Create && flagged.contains(&out[i].path))
            .collect();
        let mut drop = vec![false; out.len()];
        let mut renamed_dirs: Vec<PathBuf> = Vec::new();

        for d in deletes {
            let from = out[d].path.clone();
            let is_dir = out[d].is_dir;
            let old_len = removed_meta.get(&from).map(|m| m.len);
            let score = |c: usize| -> Option<u8> {
                let to = &out[c].path;
                if out[c].is_dir != is_dir {
                    return None;
                }
                if !is_dir {
                    let new_len = self.known.get(to).map(|m| m.len);
                    if old_len != new_len {
                        return None;
                    }
                }
                Some(match (from.file_name() == to.file_name(), from.parent() == to.parent()) {
                    (true, _) => 2,
                    (false, true) => 1,
                    _ => 0,
                })
            };
            let best = creates
                .iter()
                .enumerate()
                .filter_map(|(slot, &c)| score(c).map(|s| (s, slot)))
                .max_by_key(|&(s, slot)| (s, std::cmp::Reverse(slot)));
            if let Some((_, slot)) = best {
                let c = creates.remove(slot);
                let to = out[c].path.clone();
                if is_dir {
                    renamed_dirs.push(to.clone());
                }
                out[d] = Change::rename(from, to, is_dir);
                drop[c] = true;
            }
        }
        // Creates for the contents of a renamed directory are part of the rename.
        for (i, c) in out.iter().enumerate() {
            if c.kind == ChangeKind::Create
                && renamed_dirs.iter().any(|d| c.path.starts_with(d) && c.path != *d)
            {
                drop[i] = true;
            }
        }
        let mut i = 0;
        out.retain(|_| {
            let keep = !drop[i];
            i += 1;
            keep
        });
    }

    /// Full rescan, diffed against the model. Used to recover from lost events.
    pub fn reconcile(&mut self) -> (Flush, ScanSummary) {
        let started = Instant::now();
        let scanned = scanner::scan(&self.root, &self.root, &self.filter);
        let mut fresh: BTreeMap<PathBuf, EntryMeta> = BTreeMap::new();
        for e in &scanned.entries {
            fresh.insert(e.path.clone(), e.meta);
        }
        let mut flush = Flush::default();
        let mut deleted_dir: Option<PathBuf> = None;
        for (path, old) in &self.known {
            if let Some(d) = &deleted_dir {
                if path.starts_with(d) {
                    if old.is_dir {
                        flush.dirs_removed.push(path.clone());
                    }
                    continue;
                }
            }
            let gone = match fresh.get(path) {
                None => true,
                Some(m) => m.is_dir != old.is_dir,
            };
            if gone {
                flush.changes.push(Change::delete(path.clone(), old.is_dir));
                if old.is_dir {
                    flush.dirs_removed.push(path.clone());
                    deleted_dir = Some(path.clone());
                }
            }
        }
        for (path, meta) in &fresh {
            match self.known.get(path) {
                Some(old) if old.is_dir == meta.is_dir => {
                    if !meta.is_dir && old != meta {
                        flush.changes.push(Change::modify(path.clone()));
                    }
                }
                _ => {
                    if meta.is_dir {
                        flush.dirs_added.push(path.clone());
                    }
                    flush.changes.push(Change::create(path.clone(), meta.is_dir));
                }
            }
        }
        self.known = fresh;
        let summary = ScanSummary {
            entries: Vec::new(),
            dirs: Vec::new(),
            errors: scanned.errors,
            elapsed: started.elapsed(),
        };
        (flush, summary)
    }

    /// Picks up entries created below `dir` that no event reported (watch registration race).
    pub fn reconcile_subtree(&mut self, dir: &Path) -> Flush {
        let mut flush = Flush::default();
        let mut out = Vec::new();
        if self.known.get(dir).is_some_and(|m| m.is_dir) {
            self.scan_new(dir, &mut out, &mut flush);
        }
        flush.changes = out;
        flush
    }

    /// Forgets everything (the root vanished). Returns deletes for the top-level entries.
    pub fn clear(&mut self) -> Vec<Change> {
        let top: Vec<Change> = self
            .known
            .iter()
            .filter(|(p, _)| p.components().count() == 1)
            .map(|(p, m)| Change::delete(p.clone(), m.is_dir))
            .collect();
        self.known.clear();
        self.pending = Pending::default();
        top
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Fixture {
        dir: tempfile::TempDir,
        agg: Aggregator,
    }

    impl Fixture {
        fn new(setup: impl FnOnce(&Path)) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().canonicalize().unwrap();
            setup(&root);
            let mut agg = Aggregator::new(root, Filter::new(["ignored"]).unwrap(), Duration::ZERO);
            agg.initial_scan();
            Fixture { dir, agg }
        }

        fn root(&self) -> PathBuf {
            self.dir.path().canonicalize().unwrap()
        }

        fn ev(&mut self, kind: RawKind, paths: &[&str]) {
            let root = self.root();
            let ev = RawEvent::new(kind, paths.iter().map(|p| root.join(p)).collect());
            self.agg.record(ev, Instant::now());
        }

        fn flush(&mut self) -> Vec<Change> {
            self.agg.flush().changes
        }
    }

    fn write(root: &Path, p: &str, s: &str) {
        let path = root.join(p);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, s).unwrap();
    }

    #[test]
    fn burst_of_events_on_one_file_is_one_modify() {
        let mut f = Fixture::new(|r| write(r, "a.txt", "1"));
        let root = f.root();
        for i in 0..50 {
            fs::write(root.join("a.txt"), format!("{i}")).unwrap();
            f.ev(RawKind::Modify { content: true }, &["a.txt"]);
        }
        assert_eq!(f.flush(), vec![Change::modify("a.txt")]);
    }

    #[test]
    fn create_then_modify_in_same_window_is_one_create() {
        let mut f = Fixture::new(|_| {});
        write(&f.root(), "new.rs", "x");
        f.ev(RawKind::Create, &["new.rs"]);
        f.ev(RawKind::Modify { content: true }, &["new.rs"]);
        assert_eq!(f.flush(), vec![Change::create("new.rs", false)]);
    }

    #[test]
    fn transient_file_is_ignored() {
        let mut f = Fixture::new(|_| {});
        write(&f.root(), "tmp123", "x");
        fs::remove_file(f.root().join("tmp123")).unwrap();
        f.ev(RawKind::Create, &["tmp123"]);
        f.ev(RawKind::Remove, &["tmp123"]);
        assert!(f.flush().is_empty());
    }

    #[test]
    fn atomic_save_via_temp_rename_is_a_modify() {
        let mut f = Fixture::new(|r| write(r, "main.rs", "old"));
        let root = f.root();
        write(&root, ".main.rs.tmp", "new contents");
        fs::rename(root.join(".main.rs.tmp"), root.join("main.rs")).unwrap();
        f.ev(RawKind::Create, &[".main.rs.tmp"]);
        f.ev(RawKind::RenameBoth, &[".main.rs.tmp", "main.rs"]);
        assert_eq!(f.flush(), vec![Change::modify("main.rs")]);
    }

    #[test]
    fn delete_of_directory_is_one_event_and_forgets_children() {
        let mut f = Fixture::new(|r| {
            write(r, "lib/a.rs", "a");
            write(r, "lib/sub/b.rs", "b");
        });
        fs::remove_dir_all(f.root().join("lib")).unwrap();
        f.ev(RawKind::Remove, &["lib/sub/b.rs"]);
        f.ev(RawKind::Remove, &["lib/a.rs"]);
        f.ev(RawKind::Remove, &["lib/sub"]);
        f.ev(RawKind::Remove, &["lib"]);
        assert_eq!(f.flush(), vec![Change::delete("lib", true)]);
        assert_eq!(f.agg.known_len(), 0);
    }

    #[test]
    fn new_directory_contents_are_discovered() {
        let mut f = Fixture::new(|_| {});
        write(&f.root(), "feat/x/a.ts", "a");
        write(&f.root(), "feat/b.ts", "b");
        f.ev(RawKind::Create, &["feat"]);
        f.ev(RawKind::Create, &["feat/b.ts"]);
        let changes = f.flush();
        assert_eq!(
            changes,
            vec![
                Change::create("feat", true),
                Change::create("feat/x", true),
                Change::create("feat/x/a.ts", false),
                Change::create("feat/b.ts", false),
            ]
        );
    }

    #[test]
    fn paired_rename_moves_subtree() {
        let mut f = Fixture::new(|r| write(r, "old/a.rs", "a"));
        fs::rename(f.root().join("old"), f.root().join("new")).unwrap();
        f.ev(RawKind::RenameBoth, &["old", "new"]);
        assert_eq!(f.flush(), vec![Change::rename("old", "new", true)]);
        assert!(f.agg.is_known(Path::new("new/a.rs")));
        assert!(!f.agg.is_known(Path::new("old/a.rs")));
    }

    #[test]
    fn tracker_paired_rename() {
        let mut f = Fixture::new(|r| write(r, "a.rs", "a"));
        fs::rename(f.root().join("a.rs"), f.root().join("b.rs")).unwrap();
        let root = f.root();
        let mut from = RawEvent::new(RawKind::RenameFrom, vec![root.join("a.rs")]);
        from.tracker = Some(7);
        let mut to = RawEvent::new(RawKind::RenameTo, vec![root.join("b.rs")]);
        to.tracker = Some(7);
        f.agg.record(from, Instant::now());
        f.agg.record(to, Instant::now());
        assert_eq!(f.flush(), vec![Change::rename("a.rs", "b.rs", false)]);
    }

    #[test]
    fn unpaired_rename_is_inferred() {
        let mut f = Fixture::new(|r| {
            write(r, "src/client.ts", "abc");
            write(r, "src/other.ts", "abcdef");
        });
        fs::create_dir(f.root().join("lib")).unwrap();
        fs::rename(f.root().join("src/client.ts"), f.root().join("lib/client.ts")).unwrap();
        f.ev(RawKind::RenameAny, &["src/client.ts"]);
        f.ev(RawKind::Create, &["lib"]);
        f.ev(RawKind::RenameAny, &["lib/client.ts"]);
        let changes = f.flush();
        assert_eq!(
            changes,
            vec![Change::create("lib", true), Change::rename("src/client.ts", "lib/client.ts", false)]
        );
    }

    #[test]
    fn excluded_paths_produce_nothing() {
        let mut f = Fixture::new(|_| {});
        write(&f.root(), "ignored/x.txt", "x");
        f.ev(RawKind::Create, &["ignored"]);
        f.ev(RawKind::Create, &["ignored/x.txt"]);
        assert!(!f.agg.has_pending());
        assert!(f.flush().is_empty());
    }

    #[test]
    fn reconcile_recovers_missed_changes() {
        let mut f = Fixture::new(|r| {
            write(r, "keep.rs", "1");
            write(r, "gone/a.rs", "1");
            write(r, "edit.rs", "1");
        });
        let root = f.root();
        fs::remove_dir_all(root.join("gone")).unwrap();
        write(&root, "fresh/n.rs", "n");
        fs::write(root.join("edit.rs"), "changed!").unwrap();
        let (flush, _) = f.agg.reconcile();
        assert_eq!(
            flush.changes,
            vec![
                Change::delete("gone", true),
                Change::modify("edit.rs"),
                Change::create("fresh", true),
                Change::create("fresh/n.rs", false),
            ]
        );
        let (again, _) = f.agg.reconcile();
        assert!(again.changes.is_empty());
    }

    #[test]
    fn batch_is_reported_in_event_order() {
        let mut f = Fixture::new(|r| {
            write(r, "z.rs", "1");
            write(r, "a.rs", "1");
        });
        let root = f.root();
        fs::write(root.join("z.rs"), "22").unwrap();
        write(&root, "m/new.rs", "n");
        fs::remove_file(root.join("a.rs")).unwrap();
        f.ev(RawKind::Modify { content: true }, &["z.rs"]);
        f.ev(RawKind::Create, &["m"]);
        f.ev(RawKind::Remove, &["a.rs"]);
        assert_eq!(
            f.flush(),
            vec![
                Change::modify("z.rs"),
                Change::create("m", true),
                Change::create("m/new.rs", false),
                Change::delete("a.rs", false),
            ]
        );
    }

    #[test]
    fn metadata_only_event_without_change_is_ignored() {
        let mut f = Fixture::new(|r| write(r, "a.rs", "1"));
        f.ev(RawKind::Modify { content: false }, &["a.rs"]);
        assert!(f.flush().is_empty());
    }

    #[test]
    fn deleted_root_is_reported() {
        let mut f = Fixture::new(|r| write(r, "a.rs", "1"));
        let root = f.root();
        fs::remove_dir_all(&root).unwrap();
        f.agg.record(RawEvent::new(RawKind::Remove, vec![root.clone()]), Instant::now());
        assert!(f.agg.flush().root_missing);
        assert_eq!(f.agg.clear(), vec![Change::delete("a.rs", false)]);
    }
}
