//! End-to-end tests: real file operations against the real watcher.
//!
//! Most tests rebuild the tree purely from the delivered events and compare it with a
//! fresh scan of the disk, which catches lost, duplicated or misclassified events.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use filegarden::change::{Change, ChangeKind, WatchMsg};
use filegarden::watcher::filter::Filter;
use filegarden::watcher::scanner;
use filegarden::watcher::{self, WatchConfig, WatchHandle};

struct Harness {
    _dir: tempfile::TempDir,
    root: PathBuf,
    rx: Receiver<WatchMsg>,
    handle: WatchHandle,
    /// Tree rebuilt from snapshot + changes: path -> is_dir.
    replica: BTreeMap<PathBuf, bool>,
    changes: Vec<Change>,
    filter: Filter,
}

impl Harness {
    fn start(setup: impl FnOnce(&Path), configure: impl FnOnce(&mut WatchConfig)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        setup(&root);
        let mut cfg = WatchConfig::new(root.clone());
        cfg.debounce = Duration::from_millis(50);
        cfg.resync = None;
        cfg.filter = Filter::with_defaults(&["*.log".into()], true).unwrap();
        configure(&mut cfg);
        let filter = cfg.filter.clone();
        let (tx, rx) = mpsc::channel();
        let handle = watcher::spawn(cfg, move |m| tx.send(m).is_ok()).unwrap();
        let mut h =
            Harness { _dir: dir, root, rx, handle, replica: BTreeMap::new(), changes: Vec::new(), filter };
        match h.rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            WatchMsg::Snapshot { entries, .. } => {
                for e in entries {
                    h.replica.insert(e.path, e.is_dir);
                }
            }
            other => panic!("expected snapshot, got {other:?}"),
        }
        // Let the startup reconcile pass settle.
        h.settle(Duration::from_millis(250));
        h.changes.clear();
        h
    }

    fn p(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    fn write(&self, rel: &str, data: &str) {
        let p = self.p(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, data).unwrap();
    }

    fn handle_msg(&mut self, msg: WatchMsg) -> Option<WatchMsg> {
        match msg {
            WatchMsg::Changes(cs) => {
                for c in cs {
                    apply(&mut self.replica, &c);
                    self.changes.push(c);
                }
                None
            }
            other => Some(other),
        }
    }

    /// Collects messages until nothing arrives for `quiet`.
    fn settle(&mut self, quiet: Duration) -> Vec<WatchMsg> {
        let mut others = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            match self.rx.recv_timeout(quiet) {
                Ok(m) => others.extend(self.handle_msg(m)),
                Err(_) => break,
            }
        }
        others
    }

    /// Waits until `pred` holds for the collected changes.
    fn wait_for(&mut self, what: &str, pred: impl Fn(&[Change]) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !pred(&self.changes) {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "timed out waiting for {what}; got {:#?}", self.changes);
            if let Ok(m) = self.rx.recv_timeout(left) {
                self.handle_msg(m);
            }
        }
    }

    fn wait_msg(&mut self, what: &str, pred: impl Fn(&WatchMsg) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "timed out waiting for {what}");
            if let Ok(m) = self.rx.recv_timeout(left) {
                if let Some(other) = self.handle_msg(m) {
                    if pred(&other) {
                        return;
                    }
                }
            }
        }
    }

    /// The replica built from events must equal what is on disk.
    fn assert_in_sync(&self) {
        let disk: BTreeMap<PathBuf, bool> = scanner::scan(&self.root, &self.root, &self.filter)
            .entries
            .into_iter()
            .map(|e| (e.path, e.meta.is_dir))
            .collect();
        if disk != self.replica {
            let missing: Vec<_> = disk.keys().filter(|k| !self.replica.contains_key(*k)).collect();
            let extra: Vec<_> = self.replica.keys().filter(|k| !disk.contains_key(*k)).collect();
            panic!("replica out of sync\n missing: {missing:?}\n extra: {extra:?}");
        }
    }

    fn count(&self, kind: ChangeKind) -> usize {
        self.changes.iter().filter(|c| c.kind == kind).count()
    }
}

fn apply(replica: &mut BTreeMap<PathBuf, bool>, c: &Change) {
    match c.kind {
        ChangeKind::Create => {
            replica.insert(c.path.clone(), c.is_dir);
        }
        ChangeKind::Modify => {
            assert_eq!(replica.get(&c.path), Some(&false), "modify of unknown file {c:?}");
        }
        ChangeKind::Delete => {
            assert!(replica.contains_key(&c.path), "delete of unknown path {c:?}");
            replica.retain(|k, _| !k.starts_with(&c.path));
        }
        ChangeKind::Rename => {
            let from = c.from.clone().unwrap();
            assert!(replica.contains_key(&from), "rename of unknown path {c:?}");
            let moved: Vec<(PathBuf, bool)> =
                replica.iter().filter(|(k, _)| k.starts_with(&from)).map(|(k, v)| (k.clone(), *v)).collect();
            replica.retain(|k, _| !k.starts_with(&from) && !k.starts_with(&c.path));
            for (k, v) in moved {
                let rest = k.strip_prefix(&from).unwrap();
                let nk = if rest.as_os_str().is_empty() { c.path.clone() } else { c.path.join(rest) };
                replica.insert(nk, v);
            }
        }
    }
}

#[test]
fn create_modify_delete_rename() {
    let mut h = Harness::start(|r| fs::write(r.join("existing.txt"), "x").unwrap(), |_| {});

    h.write("new.rs", "fn main() {}");
    h.wait_for("create", |c| c.contains(&Change::create("new.rs", false)));

    fs::write(h.p("existing.txt"), "changed").unwrap();
    h.wait_for("modify", |c| c.contains(&Change::modify("existing.txt")));

    fs::rename(h.p("new.rs"), h.p("renamed.rs")).unwrap();
    h.wait_for("rename", |c| c.iter().any(|c| c.kind == ChangeKind::Rename));

    fs::remove_file(h.p("existing.txt")).unwrap();
    h.wait_for("delete", |c| c.contains(&Change::delete("existing.txt", false)));

    h.settle(Duration::from_millis(300));
    assert_eq!(h.count(ChangeKind::Create), 1, "{:#?}", h.changes);
    assert_eq!(h.count(ChangeKind::Delete), 1, "{:#?}", h.changes);
    assert!(
        h.changes.contains(&Change::rename("new.rs", "renamed.rs", false)),
        "rename should be detected as such: {:#?}",
        h.changes
    );
    h.assert_in_sync();
}

#[test]
fn repeated_saves_are_aggregated() {
    let mut h = Harness::start(|r| fs::write(r.join("a.txt"), "0").unwrap(), |_| {});
    for i in 0..30 {
        fs::write(h.p("a.txt"), format!("{i}")).unwrap();
    }
    h.wait_for("modify", |c| !c.is_empty());
    h.settle(Duration::from_millis(300));
    // 30 writes within a few ms collapse to (almost always exactly) one logical change.
    assert!(h.count(ChangeKind::Modify) <= 2, "{:#?}", h.changes);
    assert_eq!(h.changes.len(), h.count(ChangeKind::Modify));
}

#[test]
fn atomic_save_through_temp_file_is_a_modify() {
    let mut h = Harness::start(|r| fs::write(r.join("main.rs"), "old").unwrap(), |_| {});
    h.write(".main.rs.tmp.123", "new content");
    fs::rename(h.p(".main.rs.tmp.123"), h.p("main.rs")).unwrap();
    h.wait_for("modify", |c| !c.is_empty());
    h.settle(Duration::from_millis(300));
    assert_eq!(h.changes, vec![Change::modify("main.rs")]);
    h.assert_in_sync();
}

#[test]
fn burst_of_many_files() {
    let mut h = Harness::start(|_| {}, |_| {});
    for d in 0..20 {
        for f in 0..50 {
            h.write(&format!("pkg{d:02}/mod{f:02}.ts"), "export {}");
        }
    }
    for d in 0..20 {
        fs::write(h.p(&format!("pkg{d:02}/mod00.ts")), "changed").unwrap();
    }
    h.wait_for("all creates", |c| {
        c.iter().filter(|c| c.kind == ChangeKind::Create && !c.is_dir).count() >= 1000
    });
    h.settle(Duration::from_millis(400));
    h.assert_in_sync();
    assert_eq!(h.changes.iter().filter(|c| c.kind == ChangeKind::Create && !c.is_dir).count(), 1000);
}

#[test]
fn deep_directory_structure() {
    let mut h = Harness::start(|_| {}, |_| {});
    let deep: PathBuf = (0..40).map(|i| format!("d{i}")).collect();
    fs::create_dir_all(h.p(deep.to_str().unwrap())).unwrap();
    let leaf = deep.join("leaf.txt");
    fs::write(h.root.join(&leaf), "deep").unwrap();
    h.wait_for("deep leaf", |c| c.iter().any(|c| c.path == leaf));
    h.settle(Duration::from_millis(300));
    h.assert_in_sync();

    // Changes at the bottom keep being seen (the new directories are watched).
    fs::write(h.root.join(&leaf), "deeper").unwrap();
    h.wait_for("deep modify", |c| c.contains(&Change::modify(leaf.clone())));

    fs::remove_dir_all(h.p("d0")).unwrap();
    h.wait_for("deep delete", |c| c.contains(&Change::delete("d0", true)));
    h.settle(Duration::from_millis(300));
    h.assert_in_sync();
}

#[test]
fn directory_rename_moves_contents() {
    let mut h = Harness::start(
        |r| {
            fs::create_dir_all(r.join("old/sub")).unwrap();
            fs::write(r.join("old/sub/a.rs"), "a").unwrap();
        },
        |_| {},
    );
    fs::rename(h.p("old"), h.p("new")).unwrap();
    h.wait_for("rename", |c| !c.is_empty());
    h.settle(Duration::from_millis(300));
    h.assert_in_sync();
    // Contents of the moved directory keep producing events.
    fs::write(h.p("new/sub/a.rs"), "changed").unwrap();
    h.wait_for("modify after move", |c| c.contains(&Change::modify("new/sub/a.rs")));
}

#[test]
fn excluded_paths_are_silent() {
    let mut h = Harness::start(|r| fs::create_dir(r.join("node_modules")).unwrap(), |_| {});
    h.write("node_modules/react/index.js", "x");
    h.write(".git/objects/ab/cdef", "x");
    h.write("debug.log", "x");
    h.write("src/real.ts", "x");
    h.wait_for("real file", |c| c.contains(&Change::create("src/real.ts", false)));
    h.settle(Duration::from_millis(300));
    assert!(h.changes.iter().all(|c| c.path.starts_with("src")), "excluded paths leaked: {:#?}", h.changes);
}

#[test]
fn rescan_reports_in_sync() {
    let mut h = Harness::start(|_| {}, |_| {});
    h.write("a/b.txt", "x");
    h.wait_for("create", |c| c.contains(&Change::create("a/b.txt", false)));
    h.settle(Duration::from_millis(200));
    h.handle.rescan();
    h.wait_msg("rescanned", |m| matches!(m, WatchMsg::Rescanned { changes: 0, .. }));
    h.assert_in_sync();
}

#[test]
fn polling_backend() {
    let mut h = Harness::start(
        |r| fs::write(r.join("p.txt"), "1").unwrap(),
        |c| c.poll = Some(Duration::from_millis(100)),
    );
    h.write("dir/new.txt", "n");
    h.wait_for("poll create", |c| c.contains(&Change::create("dir/new.txt", false)));
    // Make sure the size changes so polling sees it even with coarse mtimes.
    fs::write(h.p("p.txt"), "longer content").unwrap();
    h.wait_for("poll modify", |c| c.contains(&Change::modify("p.txt")));
    fs::remove_file(h.p("p.txt")).unwrap();
    h.wait_for("poll delete", |c| c.contains(&Change::delete("p.txt", false)));
    h.settle(Duration::from_millis(400));
    h.assert_in_sync();
}

// Windows keeps a deleted directory around while a watch handle is open on it.
#[cfg_attr(windows, ignore)]
#[test]
fn root_removed_and_restored() {
    let mut h = Harness::start(|r| fs::write(r.join("a.txt"), "a").unwrap(), |_| {});
    fs::remove_dir_all(&h.root).unwrap();
    h.wait_msg("root missing", |m| matches!(m, WatchMsg::RootMissing));
    assert!(h.replica.is_empty());
    fs::create_dir(&h.root).unwrap();
    fs::write(h.p("b.txt"), "b").unwrap();
    h.wait_msg("root restored", |m| matches!(m, WatchMsg::RootRestored));
    h.wait_for("restored content", |c| c.contains(&Change::create("b.txt", false)));
    // Watching continues after restoration.
    h.write("c.txt", "c");
    h.wait_for("after restore", |c| c.contains(&Change::create("c.txt", false)));
    h.settle(Duration::from_millis(300));
    h.assert_in_sync();
}

#[cfg(unix)]
#[test]
fn unreadable_directory_does_not_break_watching() {
    use std::os::unix::fs::PermissionsExt;
    let mut h = Harness::start(
        |r| {
            fs::create_dir(r.join("secret")).unwrap();
            fs::write(r.join("secret/x"), "x").unwrap();
            fs::set_permissions(r.join("secret"), fs::Permissions::from_mode(0o000)).unwrap();
        },
        |_| {},
    );
    h.write("visible.txt", "v");
    h.wait_for("create", |c| c.contains(&Change::create("visible.txt", false)));
    fs::set_permissions(h.p("secret"), fs::Permissions::from_mode(0o755)).unwrap();
}
