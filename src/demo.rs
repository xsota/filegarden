//! Demo mode: a simulated AI coding session, fed to the UI as if it came from the
//! watcher. Nothing touches the file system. Useful for development and screenshots.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

use crate::change::{Change, Entry, WatchMsg};

/// Small deterministic PRNG (xorshift64*), so demo sessions are reproducible.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }

    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.next_u64() % (hi - lo).max(1)
    }

    pub fn chance(&mut self, p: f64) -> bool {
        (self.next_u64() % 10_000) as f64 / 10_000.0 < p
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

const FEATURES: &[&str] = &[
    "auth",
    "billing",
    "chat",
    "search",
    "settings",
    "upload",
    "profile",
    "notifications",
    "editor",
    "analytics",
    "onboarding",
    "teams",
];
const PARTS: &[&str] = &["index", "api", "store", "types", "utils", "View", "Panel", "hooks", "schema"];
const EXTS: &[&str] = &["ts", "tsx"];

const INITIAL: &[&str] = &[
    "README.md",
    "package.json",
    "tsconfig.json",
    ".gitignore",
    "docs/architecture.md",
    "docs/setup.md",
    "public/favicon.svg",
    "public/robots.txt",
    "src/app.ts",
    "src/main.tsx",
    "src/router.ts",
    "src/components/Button.tsx",
    "src/components/Header.tsx",
    "src/components/Sidebar.tsx",
    "src/components/Modal.tsx",
    "src/components/Avatar.tsx",
    "src/components/Table.tsx",
    "src/hooks/useAuth.ts",
    "src/hooks/useFetch.ts",
    "src/hooks/useTheme.ts",
    "src/lib/client.ts",
    "src/lib/format.ts",
    "src/lib/logger.ts",
    "src/legacy/oldClient.ts",
    "src/legacy/compat.ts",
    "src/styles/globals.css",
    "src/styles/theme.css",
    "tests/app.test.ts",
    "tests/router.test.ts",
    "tests/client.test.ts",
    "tests/format.test.ts",
    "tests/hooks.test.ts",
    "docs/api.md",
    "docs/contributing.md",
    "public/icons/check.svg",
    "public/icons/close.svg",
    "public/icons/menu.svg",
    "public/icons/search.svg",
    "public/icons/user.svg",
    "public/locales/en.json",
    "public/locales/ja.json",
    "public/locales/de.json",
    "src/api/auth.ts",
    "src/api/client.ts",
    "src/api/messages.ts",
    "src/api/users.ts",
    "src/api/uploads.ts",
    "src/components/Badge.tsx",
    "src/components/Card.tsx",
    "src/components/Dropdown.tsx",
    "src/components/Footer.tsx",
    "src/components/Input.tsx",
    "src/components/Spinner.tsx",
    "src/components/Tabs.tsx",
    "src/components/Toast.tsx",
    "src/components/Tooltip.tsx",
    "src/hooks/useDebounce.ts",
    "src/hooks/useLocalStorage.ts",
    "src/hooks/useMediaQuery.ts",
    "src/lib/date.ts",
    "src/lib/errors.ts",
    "src/lib/validate.ts",
    "src/pages/Home.tsx",
    "src/pages/Login.tsx",
    "src/pages/Settings.tsx",
    "src/pages/Profile.tsx",
    "src/pages/NotFound.tsx",
    "src/store/index.ts",
    "src/store/session.ts",
    "src/store/ui.ts",
    "src/styles/components.css",
    "src/styles/reset.css",
    "src/types/api.ts",
    "src/types/models.ts",
    "src/legacy/helpers.ts",
    "scripts/build.ts",
    "scripts/release.ts",
    "vite.config.ts",
    ".eslintrc.json",
];

/// One step of the script: wait, then deliver these changes.
pub type Step = (Duration, Vec<Change>);

/// Generates a plausible stream of agent activity. Keeps its own model of the tree so
/// every change is consistent (no modifying files that do not exist).
pub struct DemoGen {
    rng: Rng,
    files: BTreeSet<PathBuf>,
    dirs: BTreeSet<PathBuf>,
    queue: std::collections::VecDeque<Step>,
    used_features: Vec<&'static str>,
}

impl DemoGen {
    pub fn new(seed: u64) -> Self {
        let mut g = DemoGen {
            rng: Rng::new(seed),
            files: BTreeSet::new(),
            dirs: BTreeSet::new(),
            queue: Default::default(),
            used_features: Vec::new(),
        };
        for f in INITIAL {
            let p = PathBuf::from(f);
            g.add_parents(&p);
            g.files.insert(p);
        }
        g
    }

    fn add_parents(&mut self, p: &std::path::Path) -> Vec<PathBuf> {
        let mut created = Vec::new();
        let mut cur = p.parent();
        while let Some(d) = cur {
            if d.as_os_str().is_empty() {
                break;
            }
            if self.dirs.insert(d.to_path_buf()) {
                created.push(d.to_path_buf());
            }
            cur = d.parent();
        }
        created.reverse();
        created
    }

    pub fn snapshot(&self) -> Vec<Entry> {
        let mut all: Vec<Entry> = self
            .dirs
            .iter()
            .map(|d| Entry { path: d.clone(), is_dir: true })
            .chain(self.files.iter().map(|f| Entry { path: f.clone(), is_dir: false }))
            .collect();
        all.sort_by(|a, b| a.path.cmp(&b.path));
        all
    }

    pub fn files(&self) -> &BTreeSet<PathBuf> {
        &self.files
    }

    pub fn dirs(&self) -> &BTreeSet<PathBuf> {
        &self.dirs
    }

    fn ms(&mut self, lo: u64, hi: u64) -> Duration {
        Duration::from_millis(self.rng.range(lo, hi))
    }

    fn push(&mut self, delay: Duration, changes: Vec<Change>) {
        self.queue.push_back((delay, changes));
    }

    fn create(&mut self, delay: Duration, p: PathBuf) {
        let mut changes: Vec<Change> =
            self.add_parents(&p).into_iter().map(|d| Change::create(d, true)).collect();
        if self.files.insert(p.clone()) {
            changes.push(Change::create(p, false));
        } else {
            changes.push(Change::modify(p));
        }
        self.push(delay, changes);
    }

    fn modify(&mut self, delay: Duration, p: PathBuf) {
        if self.files.contains(&p) {
            self.push(delay, vec![Change::modify(p)]);
        }
    }

    fn random_file(&mut self, prefix: &str) -> Option<PathBuf> {
        let c: Vec<&PathBuf> = self.files.iter().filter(|f| f.starts_with(prefix)).collect();
        if c.is_empty() {
            None
        } else {
            Some(c[self.rng.below(c.len())].clone())
        }
    }

    /// Plans the next "task" of the simulated agent.
    fn plan(&mut self) {
        // A calm moment between tasks: the screen should settle.
        let pause = self.ms(1200, 4200);
        match self.rng.below(10) {
            0..=2 => self.task_feature(pause),
            3 | 4 => self.task_edit_loop(pause),
            5 => self.task_refactor(pause),
            6 => self.task_cleanup(pause),
            7 => self.task_tests(pause),
            8 => self.task_format(pause),
            _ => self.task_docs(pause),
        }
    }

    fn task_feature(&mut self, pause: Duration) {
        let available: Vec<&'static str> =
            FEATURES.iter().copied().filter(|f| !self.used_features.contains(f)).collect();
        let feature =
            if available.is_empty() { *self.rng.pick(FEATURES) } else { *self.rng.pick(&available) };
        self.used_features.push(feature);
        let base = PathBuf::from("src/features").join(feature);
        let n = self.rng.range(3, 7) as usize;
        let mut created = Vec::new();
        for i in 0..n {
            let part = PARTS[(i + self.rng.below(PARTS.len())) % PARTS.len()];
            let ext = if part.chars().next().is_some_and(char::is_uppercase) {
                "tsx"
            } else {
                *self.rng.pick(EXTS)
            };
            let name = if part.chars().next().is_some_and(char::is_uppercase) {
                let mut cap = feature.to_string();
                cap[..1].make_ascii_uppercase();
                format!("{cap}{part}.{ext}")
            } else {
                format!("{part}.{ext}")
            };
            let p = base.join(name);
            let d = if i == 0 { pause } else { self.ms(180, 650) };
            self.create(d, p.clone());
            created.push(p);
        }
        for _ in 0..self.rng.range(2, 6) {
            let p = created[self.rng.below(created.len())].clone();
            let d = self.ms(150, 500);
            self.modify(d, p);
        }
        let d = self.ms(300, 700);
        self.modify(d, PathBuf::from("src/router.ts"));
        let d = self.ms(200, 600);
        self.create(d, PathBuf::from(format!("tests/{feature}.test.ts")));
    }

    fn task_edit_loop(&mut self, pause: Duration) {
        let Some(target) = self.random_file("src") else { return };
        let saves = self.rng.range(3, 9);
        for i in 0..saves {
            let d = if i == 0 { pause } else { self.ms(120, 420) };
            self.modify(d, target.clone());
        }
        if let Some(other) = self.random_file("src") {
            let d = self.ms(200, 500);
            self.modify(d, other);
        }
    }

    fn task_refactor(&mut self, pause: Duration) {
        let Some(from) = self.random_file("src") else { return };
        let dest_dir = *self.rng.pick(&["src/lib", "src/core", "src/shared", "src/components"]);
        let to = PathBuf::from(dest_dir).join(from.file_name().expect("file name"));
        if to == from || self.files.contains(&to) {
            return self.task_edit_loop(pause);
        }
        let mut changes: Vec<Change> =
            self.add_parents(&to).into_iter().map(|d| Change::create(d, true)).collect();
        self.files.remove(&from);
        self.files.insert(to.clone());
        changes.push(Change::rename(from, to.clone(), false));
        self.push(pause, changes);
        for _ in 0..self.rng.range(2, 5) {
            if let Some(f) = self.random_file("src") {
                let d = self.ms(150, 450);
                self.modify(d, f);
            }
        }
    }

    fn task_cleanup(&mut self, pause: Duration) {
        let victim = self.random_file("src/legacy").or_else(|| self.random_file("src/features"));
        let Some(victim) = victim else { return self.task_tests(pause) };
        self.files.remove(&victim);
        self.push(pause, vec![Change::delete(victim.clone(), false)]);
        // Remove the directory too once it is empty.
        if let Some(parent) = victim.parent() {
            if !self.files.iter().any(|f| f.starts_with(parent)) && self.dirs.contains(parent) {
                let parent = parent.to_path_buf();
                let sub: Vec<PathBuf> =
                    self.dirs.iter().filter(|d| d.starts_with(&parent)).cloned().collect();
                for d in sub {
                    self.dirs.remove(&d);
                }
                let d = self.ms(300, 800);
                self.push(d, vec![Change::delete(parent, true)]);
            }
        }
        if let Some(f) = self.random_file("src") {
            let d = self.ms(250, 600);
            self.modify(d, f);
        }
    }

    fn task_tests(&mut self, pause: Duration) {
        let tests: Vec<PathBuf> = self.files.iter().filter(|f| f.starts_with("tests")).cloned().collect();
        let mut first = true;
        for t in tests.iter().take(6) {
            let d = if first { pause } else { self.ms(90, 260) };
            first = false;
            self.modify(d, t.clone());
        }
        // A short-lived snapshot directory appears and disappears.
        let d = self.ms(200, 400);
        self.create(d, PathBuf::from("tests/__snapshots__/app.test.ts.snap"));
        if self.rng.chance(0.5) {
            let d = self.ms(800, 1600);
            self.files.retain(|f| !f.starts_with("tests/__snapshots__"));
            self.dirs.remove(&PathBuf::from("tests/__snapshots__"));
            self.push(d, vec![Change::delete("tests/__snapshots__", true)]);
        }
    }

    fn task_format(&mut self, pause: Duration) {
        // `prettier --write .`: many files in a quick burst.
        let all: Vec<PathBuf> = self.files.iter().cloned().collect();
        let mut first = true;
        for f in &all {
            if !self.rng.chance(0.7) {
                continue;
            }
            let d = if first { pause } else { self.ms(15, 60) };
            first = false;
            self.modify(d, f.clone());
        }
    }

    fn task_docs(&mut self, pause: Duration) {
        self.modify(pause, PathBuf::from("README.md"));
        let d = self.ms(400, 900);
        if self.rng.chance(0.5) {
            let name = format!("docs/{}.md", self.rng.pick(FEATURES));
            self.create(d, PathBuf::from(name));
        } else {
            self.modify(d, PathBuf::from("docs/architecture.md"));
        }
        let d = self.ms(300, 800);
        self.modify(d, PathBuf::from("package.json"));
    }
}

impl Iterator for DemoGen {
    type Item = Step;

    fn next(&mut self) -> Option<Step> {
        while self.queue.is_empty() {
            self.plan();
        }
        self.queue.pop_front()
    }
}

/// Runs the demo on a background thread until `sink` returns false.
pub fn spawn<F>(seed: u64, speed: f64, mut sink: F) -> std::thread::JoinHandle<()>
where
    F: FnMut(WatchMsg) -> bool + Send + 'static,
{
    std::thread::spawn(move || {
        let mut gen = DemoGen::new(seed);
        let ok = sink(WatchMsg::Snapshot { entries: gen.snapshot(), elapsed: Duration::ZERO, errors: 0 });
        if !ok {
            return;
        }
        let speed = if speed.is_finite() && speed > 0.0 { speed } else { 1.0 };
        std::thread::sleep(Duration::from_millis(800));
        for (delay, changes) in &mut gen {
            std::thread::sleep(delay.div_f64(speed));
            if !sink(WatchMsg::Changes(changes)) {
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::ChangeKind;
    use std::collections::BTreeSet;

    /// The demo must be a valid change stream: every change consistent with the tree.
    #[test]
    fn demo_stream_is_consistent() {
        for seed in 1..20 {
            let mut gen = DemoGen::new(seed);
            let mut files: BTreeSet<PathBuf> =
                gen.snapshot().into_iter().filter(|e| !e.is_dir).map(|e| e.path).collect();
            let mut dirs: BTreeSet<PathBuf> =
                gen.snapshot().into_iter().filter(|e| e.is_dir).map(|e| e.path).collect();
            let mut steps = 0;
            while steps < 400 || !gen.queue.is_empty() {
                steps += 1;
                let (_, changes) = gen.next().unwrap();
                for c in changes {
                    match (c.kind, c.is_dir) {
                        (ChangeKind::Create, true) => assert!(dirs.insert(c.path.clone()), "{c:?}"),
                        (ChangeKind::Create, false) => assert!(files.insert(c.path.clone()), "{c:?}"),
                        (ChangeKind::Modify, _) => assert!(files.contains(&c.path), "{c:?}"),
                        (ChangeKind::Delete, false) => assert!(files.remove(&c.path), "{c:?}"),
                        (ChangeKind::Delete, true) => {
                            assert!(dirs.remove(&c.path), "{c:?}");
                            files.retain(|f| !f.starts_with(&c.path));
                            dirs.retain(|d| !d.starts_with(&c.path));
                        }
                        (ChangeKind::Rename, _) => {
                            assert!(files.remove(c.from.as_ref().unwrap()), "{c:?}");
                            assert!(files.insert(c.path.clone()), "{c:?}");
                        }
                    }
                    if let Some(parent) = c.path.parent().filter(|p| !p.as_os_str().is_empty()) {
                        if c.kind != ChangeKind::Delete {
                            assert!(dirs.contains(parent), "parent missing for {c:?}");
                        }
                    }
                }
            }
            assert_eq!(&files, gen.files());
            assert_eq!(&dirs, gen.dirs());
        }
    }
}
