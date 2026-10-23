//! File system watching on a background thread.
//!
//! The thread owns the `notify` backend and an [`Aggregator`]. Raw events are batched for
//! `debounce`, reconciled with the file system, and delivered as logical [`WatchMsg`]s.
//!
//! Polling mode uses no backend at all: it periodically reconciles the aggregator's model
//! with a metadata scan, which compares size and nanosecond mtimes.

pub mod aggregator;
pub mod events;
pub mod filter;
pub mod scanner;

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::change::{Change, WatchMsg};
use aggregator::{Aggregator, Flush};
use events::RawEvent;
use filter::Filter;

#[derive(Debug, Clone)]
pub struct WatchConfig {
    /// Canonical, absolute root directory.
    pub root: PathBuf,
    pub filter: Filter,
    /// `Some(interval)` polls instead of using native OS events.
    pub poll: Option<Duration>,
    /// Batch window for event aggregation.
    pub debounce: Duration,
    /// Periodic full re-synchronisation; `None` disables it.
    pub resync: Option<Duration>,
}

impl WatchConfig {
    pub fn new(root: PathBuf) -> Self {
        WatchConfig {
            root,
            filter: Filter::none(),
            poll: None,
            debounce: Duration::from_millis(100),
            resync: Some(Duration::from_secs(60)),
        }
    }
}

enum Input {
    Fs(notify::Result<notify::Event>),
    Rescan,
    Shutdown,
}

/// Handle to the watcher thread. Dropping it stops the thread.
pub struct WatchHandle {
    tx: Sender<Input>,
    thread: Option<JoinHandle<()>>,
}

impl WatchHandle {
    pub fn rescan(&self) {
        let _ = self.tx.send(Input::Rescan);
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Input::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Starts watching. `sink` receives messages and returns `false` once nobody listens.
pub fn spawn<F>(config: WatchConfig, sink: F) -> Result<WatchHandle>
where
    F: FnMut(WatchMsg) -> bool + Send + 'static,
{
    let meta = std::fs::metadata(&config.root)
        .with_context(|| format!("cannot access {}", config.root.display()))?;
    anyhow::ensure!(meta.is_dir(), "{} is not a directory", config.root.display());

    let (tx, rx) = mpsc::channel::<Input>();
    let backend = Backend::new(&config, tx.clone())?;
    let worker_tx = tx.clone();
    let thread = std::thread::Builder::new()
        .name("filegarden-watcher".into())
        .spawn(move || {
            let mut worker = Worker {
                agg: Aggregator::new(config.root.clone(), config.filter.clone(), config.debounce),
                backend,
                config,
                sink: Box::new(sink),
                tx: worker_tx,
                limit_warned: false,
            };
            worker.run(rx);
        })
        .context("failed to start watcher thread")?;
    Ok(WatchHandle { tx, thread: Some(thread) })
}

/// Whether directories are watched one by one (non-recursively).
///
/// inotify watches per directory anyway; doing it ourselves lets us skip excluded trees
/// such as `node_modules` instead of burning watches on them.
/// FSEvents and ReadDirectoryChangesW watch a whole tree with one handle.
const PER_DIR_WATCHES: bool = !cfg!(any(target_os = "macos", target_os = "windows"));

struct Backend {
    /// `None` in polling mode.
    watcher: Option<Box<dyn Watcher + Send>>,
    per_dir: bool,
}

impl Backend {
    fn new(config: &WatchConfig, tx: Sender<Input>) -> Result<Self> {
        let handler_tx = tx.clone();
        let handler = move |res: notify::Result<notify::Event>| {
            let _ = handler_tx.send(Input::Fs(res));
        };
        if config.poll.is_some() {
            return Ok(Backend { watcher: None, per_dir: false });
        }
        let watcher = RecommendedWatcher::new(handler, notify::Config::default())
            .context("failed to create file system watcher")?;
        Ok(Backend { watcher: Some(Box::new(watcher)), per_dir: PER_DIR_WATCHES })
    }

    fn watch_root(&mut self, root: &Path) -> notify::Result<()> {
        let mode = if self.per_dir { RecursiveMode::NonRecursive } else { RecursiveMode::Recursive };
        match &mut self.watcher {
            Some(w) => w.watch(root, mode),
            None => Ok(()),
        }
    }

    /// Returns true if the OS watch limit was hit.
    fn watch_dirs(&mut self, root: &Path, dirs: &[PathBuf]) -> bool {
        let Some(w) = self.watcher.as_mut().filter(|_| self.per_dir) else { return false };
        let mut limit = false;
        for d in dirs {
            if let Err(e) = w.watch(&root.join(d), RecursiveMode::NonRecursive) {
                limit |= matches!(e.kind, notify::ErrorKind::MaxFilesWatch);
            }
        }
        limit
    }

    fn unwatch_dirs(&mut self, root: &Path, dirs: &[PathBuf]) {
        let Some(w) = self.watcher.as_mut().filter(|_| self.per_dir) else { return };
        for d in dirs {
            let _ = w.unwatch(&root.join(d));
        }
    }
}

struct Worker {
    agg: Aggregator,
    backend: Backend,
    config: WatchConfig,
    sink: Box<dyn FnMut(WatchMsg) -> bool + Send>,
    /// Kept to re-create the backend when the root comes back.
    tx: Sender<Input>,
    limit_warned: bool,
}

const ROOT_CHECK: Duration = Duration::from_secs(2);

impl Worker {
    fn send(&mut self, msg: WatchMsg) -> bool {
        (self.sink)(msg)
    }

    fn send_changes(&mut self, changes: Vec<Change>) -> bool {
        changes.is_empty() || self.send(WatchMsg::Changes(changes))
    }

    fn run(&mut self, rx: mpsc::Receiver<Input>) {
        if !self.start() {
            return;
        }
        let mut root_missing = false;
        let mut last_resync = Instant::now();
        let mut last_root_check = Instant::now();
        let mut last_poll = Instant::now();

        loop {
            let now = Instant::now();
            let mut wake = now + ROOT_CHECK;
            if let Some(d) = self.agg.deadline() {
                wake = wake.min(d);
            }
            if let (Some(r), false) = (self.config.resync, root_missing) {
                wake = wake.min(last_resync + r);
            }
            if let (Some(p), false) = (self.config.poll, root_missing) {
                wake = wake.min(last_poll + p);
            }
            let input = rx.recv_timeout(wake.saturating_duration_since(now));
            match input {
                Ok(Input::Fs(Ok(ev))) if !root_missing => {
                    self.agg.record(RawEvent::from(ev), Instant::now());
                }
                Ok(Input::Fs(Ok(_))) => {}
                Ok(Input::Fs(Err(e))) => {
                    if !root_missing && !self.send(WatchMsg::Warning(format!("watch error: {e}"))) {
                        return;
                    }
                }
                Ok(Input::Rescan) if !root_missing => {
                    if !self.rescan() {
                        return;
                    }
                    last_resync = Instant::now();
                }
                Ok(Input::Rescan) => {}
                Ok(Input::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => {}
            }

            let now = Instant::now();
            if !root_missing && self.agg.is_due(now) {
                let flush = self.agg.flush();
                if flush.root_missing {
                    root_missing = true;
                    if !self.lose_root() {
                        return;
                    }
                    continue;
                }
                let rescan = flush.needs_rescan;
                if !self.deliver(flush) {
                    return;
                }
                if rescan {
                    if !self.rescan() {
                        return;
                    }
                    last_resync = Instant::now();
                }
            }

            if let (Some(p), false) = (self.config.poll, root_missing) {
                if now.duration_since(last_poll) >= p && self.agg.root_exists() {
                    last_poll = now;
                    let (flush, _) = self.agg.reconcile();
                    if !self.deliver(flush) {
                        return;
                    }
                }
            }

            if now.duration_since(last_root_check) >= ROOT_CHECK {
                last_root_check = now;
                let exists = self.agg.root_exists();
                if !root_missing && !exists {
                    root_missing = true;
                    if !self.lose_root() {
                        return;
                    }
                } else if root_missing && exists {
                    match self.restore_root() {
                        Some(true) => {
                            root_missing = false;
                            last_resync = Instant::now();
                        }
                        Some(false) => {}
                        None => return,
                    }
                }
            }

            if !root_missing && !self.agg.has_pending() {
                if let Some(r) = self.config.resync {
                    if now.duration_since(last_resync) >= r {
                        last_resync = now;
                        if !self.rescan() {
                            return;
                        }
                    }
                }
            }
        }
    }

    /// Registers watches and sends the initial snapshot.
    fn start(&mut self) -> bool {
        let root = self.config.root.clone();
        if let Err(e) = self.backend.watch_root(&root) {
            return self.send(WatchMsg::Warning(format!("cannot watch {}: {e}", root.display())));
        }
        let summary = self.agg.initial_scan();
        let limit = self.backend.watch_dirs(&root, &summary.dirs);
        let ok = self.send(WatchMsg::Snapshot {
            entries: summary.entries,
            elapsed: summary.elapsed,
            errors: summary.errors,
        });
        if !ok || !self.warn_limit(limit) {
            return false;
        }
        if self.backend.per_dir {
            // Catch anything created while per-directory watches were being registered.
            let (flush, _) = self.agg.reconcile();
            return self.deliver(flush);
        }
        true
    }

    fn warn_limit(&mut self, hit: bool) -> bool {
        if !hit || self.limit_warned {
            return true;
        }
        self.limit_warned = true;
        self.send(WatchMsg::Warning(
            "OS watch limit reached; some directories are not watched. \
             Raise fs.inotify.max_user_watches or use --poll."
                .into(),
        ))
    }

    /// Updates watches for added/removed directories and sends the changes.
    fn deliver(&mut self, mut flush: Flush) -> bool {
        let root = self.config.root.clone();
        self.backend.unwatch_dirs(&root, &flush.dirs_removed);
        let limit = self.backend.watch_dirs(&root, &flush.dirs_added);
        if self.backend.per_dir {
            for d in &flush.dirs_added {
                let extra = self.agg.reconcile_subtree(d);
                flush.changes.extend(extra.changes);
                self.backend.watch_dirs(&root, &extra.dirs_added);
            }
        }
        self.send_changes(flush.changes) && self.warn_limit(limit)
    }

    fn rescan(&mut self) -> bool {
        if self.agg.has_pending() {
            let flush = self.agg.flush();
            if !flush.root_missing && !self.deliver(flush) {
                return false;
            }
        }
        let (flush, summary) = self.agg.reconcile();
        let n = flush.changes.len();
        self.deliver(flush)
            && self.send(WatchMsg::Rescanned { elapsed: summary.elapsed, errors: summary.errors, changes: n })
    }

    fn lose_root(&mut self) -> bool {
        let deletes = self.agg.clear();
        self.send_changes(deletes) && self.send(WatchMsg::RootMissing)
    }

    /// `Some(true)` when watching resumed, `Some(false)` to retry later, `None` to stop.
    fn restore_root(&mut self) -> Option<bool> {
        let Ok(mut backend) = Backend::new(&self.config, self.tx.clone()) else { return Some(false) };
        if backend.watch_root(&self.config.root).is_err() {
            return Some(false);
        }
        self.backend = backend;
        let (flush, _) = self.agg.reconcile();
        let root = self.config.root.clone();
        let dirs: Vec<PathBuf> = flush.dirs_added.clone();
        self.backend.watch_dirs(&root, &dirs);
        let ok = self.send(WatchMsg::RootRestored) && self.send_changes(flush.changes);
        ok.then_some(true)
    }
}
