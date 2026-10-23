//! The interactive loop: receives watcher messages and key presses, paces redraws.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;

use crate::change::WatchMsg;
use crate::state::AppState;
use crate::ui::{self, theme::Theme};
use crate::viz::grid::ActivityGrid;
use crate::watcher::{self, WatchConfig, WatchHandle};

/// Frame interval while something is animating (~30 fps).
const FRAME: Duration = Duration::from_millis(33);
/// Redraw interval while idle (keeps the clock ticking).
const IDLE_FRAME: Duration = Duration::from_secs(1);
/// Upper bound for draining queued messages before drawing.
const DRAIN_BUDGET: Duration = Duration::from_millis(12);

/// Animation time. Stops while the display is paused so fades freeze too.
pub struct Clock {
    start: Instant,
    paused_at: Option<Instant>,
    paused_total: Duration,
}

impl Clock {
    pub fn new() -> Self {
        Clock { start: Instant::now(), paused_at: None, paused_total: Duration::ZERO }
    }

    pub fn now(&self) -> f64 {
        let real = self.paused_at.unwrap_or_else(Instant::now);
        (real.duration_since(self.start).saturating_sub(self.paused_total)).as_secs_f64()
    }

    pub fn pause(&mut self) {
        self.paused_at.get_or_insert_with(Instant::now);
    }

    pub fn resume(&mut self) {
        if let Some(p) = self.paused_at.take() {
            self.paused_total += p.elapsed();
        }
    }
}

impl Default for Clock {
    fn default() -> Self {
        Self::new()
    }
}

pub enum Source {
    Watch(WatchConfig),
    Demo { seed: u64, speed: f64 },
}

enum AppEvent {
    Input(Event),
    Watch(WatchMsg),
}

pub struct App {
    pub state: AppState,
    pub theme: Theme,
    pub view: ui::tree::TreeView,
    pub root_label: String,
    pub demo: bool,
    pub paused: bool,
    pub show_help: bool,
    pub active_only: bool,
    clock: Clock,
    started: Instant,
    queued: Vec<WatchMsg>,
    watch: Option<WatchHandle>,
    quit: bool,
}

impl App {
    pub fn new(theme: Theme, root_label: String, demo: bool) -> Self {
        App {
            state: AppState::new(Box::new(ActivityGrid::new())),
            theme,
            view: ui::tree::TreeView::default(),
            root_label,
            demo,
            paused: false,
            show_help: false,
            active_only: false,
            clock: Clock::new(),
            started: Instant::now(),
            queued: Vec::new(),
            watch: None,
            quit: false,
        }
    }

    pub fn now(&self) -> f64 {
        self.clock.now()
    }

    /// Wall time since start (keeps running while paused).
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    pub fn on_watch(&mut self, msg: WatchMsg) {
        if self.paused {
            self.queued.push(msg);
        } else {
            let now = self.now();
            self.state.apply(msg, now);
        }
    }

    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        if self.paused {
            self.clock.pause();
        } else {
            self.clock.resume();
            let now = self.now();
            for msg in std::mem::take(&mut self.queued) {
                self.state.apply(msg, now);
            }
        }
    }

    pub fn on_key(&mut self, key: KeyEvent) {
        if self.show_help {
            self.show_help = false;
            return;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let tree = &mut self.state.tree;
        let ao = self.active_only;
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Up | KeyCode::Char('k') => self.view.move_by(tree, ao, -1),
            KeyCode::Down | KeyCode::Char('j') => self.view.move_by(tree, ao, 1),
            KeyCode::PageUp => self.view.page(tree, ao, -1),
            KeyCode::PageDown => self.view.page(tree, ao, 1),
            KeyCode::Home | KeyCode::Char('g') => self.view.home_end(tree, ao, false),
            KeyCode::End | KeyCode::Char('G') => self.view.home_end(tree, ao, true),
            KeyCode::Left | KeyCode::Char('h') => self.view.collapse(tree, ao),
            KeyCode::Right | KeyCode::Char('l') => self.view.expand(tree, ao),
            KeyCode::Enter => self.view.toggle(tree, ao),
            KeyCode::Char('f') => {
                if self.view.follow {
                    self.view.follow = false;
                } else {
                    self.view.resume_follow();
                }
            }
            KeyCode::Char(' ') => self.toggle_pause(),
            KeyCode::Char('r') => {
                let now = self.now();
                match &self.watch {
                    Some(w) => {
                        w.rescan();
                        self.state.notify("rescanning…".into(), now);
                    }
                    None => self.state.notify("rescan is not available in demo mode".into(), now),
                }
            }
            KeyCode::Char('a') => {
                self.active_only = !self.active_only;
                self.view.offset = 0;
            }
            KeyCode::Char('x') => {
                let now = self.now();
                if !self.theme.animate {
                    self.state.notify("game mode needs animations (drop --no-animation)".into(), now);
                } else {
                    let fx = &mut self.state.fx;
                    fx.enabled = !fx.enabled;
                    let msg =
                        if fx.enabled { "game mode ON — make some changes!" } else { "game mode off" };
                    self.state.notify(msg.into(), now);
                }
            }
            KeyCode::Char('?') => self.show_help = true,
            _ => {}
        }
    }

    fn handle(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Watch(msg) => self.on_watch(msg),
            AppEvent::Input(Event::Key(k)) if k.kind != KeyEventKind::Release => self.on_key(k),
            AppEvent::Input(_) => {}
        }
    }

    fn animating(&self) -> bool {
        self.theme.animate && !self.paused && self.state.is_animating(self.now())
    }

    fn event_loop(&mut self, terminal: &mut DefaultTerminal, rx: mpsc::Receiver<AppEvent>) -> Result<()> {
        let mut last_draw: Option<Instant> = None;
        let mut dirty = true;
        loop {
            let interval = if dirty || self.animating() { FRAME } else { IDLE_FRAME };
            let due = last_draw.map_or_else(Instant::now, |t| t + interval);
            match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
                Ok(ev) => {
                    self.handle(ev);
                    dirty = true;
                    // Apply whatever else is queued, but never starve the screen.
                    let budget = Instant::now() + DRAIN_BUDGET;
                    while Instant::now() < budget {
                        match rx.try_recv() {
                            Ok(ev) => self.handle(ev),
                            Err(_) => break,
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if self.quit {
                break;
            }
            let now = Instant::now();
            if last_draw.is_none_or(|t| now >= t + FRAME) && (dirty || now >= due) {
                if !self.paused {
                    let t = self.now();
                    self.state.tick(t);
                }
                terminal.draw(|f| ui::draw(f, self))?;
                last_draw = Some(now);
                dirty = false;
            }
        }
        Ok(())
    }
}

/// Shortens the home directory to `~` for display.
pub fn root_label(root: &Path) -> String {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    if let Some(home) = home {
        if let Ok(rest) = root.strip_prefix(&home) {
            if rest.as_os_str().is_empty() {
                return "~".into();
            }
            return format!("~/{}", crate::change::display_path(rest));
        }
    }
    let s = root.display().to_string();
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
}

pub fn run(source: Source, theme: Theme, fx: bool) -> Result<()> {
    let (tx, rx) = mpsc::channel::<AppEvent>();
    let (label, demo) = match &source {
        Source::Watch(cfg) => (root_label(&cfg.root), false),
        Source::Demo { .. } => ("demo: simulated agent session".to_string(), true),
    };
    let mut app = App::new(theme, label, demo);
    app.state.fx.enabled = fx && theme.animate;

    // Start watching before the terminal switches modes, so errors print normally.
    let sink_tx = tx.clone();
    let sink = move |m: WatchMsg| sink_tx.send(AppEvent::Watch(m)).is_ok();
    match source {
        Source::Watch(cfg) => app.watch = Some(watcher::spawn(cfg, sink)?),
        Source::Demo { seed, speed } => {
            crate::demo::spawn(seed, speed, sink);
        }
    }

    let input_tx = tx;
    std::thread::Builder::new().name("filegarden-input".into()).spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if input_tx.send(AppEvent::Input(ev)).is_err() {
                break;
            }
        }
    })?;

    // `init` also installs a panic hook that restores the terminal.
    let mut terminal = ratatui::try_init()?;
    let result = app.event_loop(&mut terminal, rx);
    ratatui::restore();
    // Stops and joins the watcher thread.
    drop(app);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::{Change, Entry};
    use crate::ui::theme::ColorMode;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn app() -> App {
        let theme = Theme { colors: ColorMode::TrueColor, ascii: false, animate: true };
        let mut app = App::new(theme, "~/proj".into(), false);
        app.on_watch(WatchMsg::Snapshot {
            entries: vec![
                Entry { path: "src".into(), is_dir: true },
                Entry { path: "src/app.ts".into(), is_dir: false },
                Entry { path: "README.md".into(), is_dir: false },
            ],
            elapsed: Duration::ZERO,
            errors: 0,
        });
        app
    }

    fn screen(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| ui::draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_all_panels() {
        let mut a = app();
        a.on_watch(WatchMsg::Changes(vec![Change::create("src/Chat.tsx", false)]));
        let s = screen(&mut a, 100, 32);
        for needle in [
            "FILEGARDEN",
            "~/proj",
            "LIVE",
            "FILE TREE",
            "ACTIVITY GRID",
            "STATISTICS",
            "ACTIVITY",
            "+ Chat.tsx",
            "src/Chat.tsx",
            "elapsed",
        ] {
            assert!(s.contains(needle), "missing {needle:?} in\n{s}");
        }
    }

    #[test]
    fn small_terminals_degrade_gracefully() {
        let mut a = app();
        for (w, h) in [(40, 10), (24, 6), (10, 3), (63, 15), (200, 60)] {
            let s = screen(&mut a, w, h);
            assert!(!s.is_empty());
        }
        assert!(screen(&mut a, 40, 10).contains("FILE TREE"));
        assert!(screen(&mut a, 10, 3).contains("FileGard"));
    }

    #[test]
    fn pause_queues_until_resume() {
        let mut a = app();
        a.toggle_pause();
        a.on_watch(WatchMsg::Changes(vec![Change::create("x.rs", false)]));
        assert_eq!(a.state.stats.created, 0);
        a.toggle_pause();
        assert_eq!(a.state.stats.created, 1);
    }

    #[test]
    fn keys_navigate_and_fold() {
        let mut a = app();
        let key = |c| KeyEvent::new(c, KeyModifiers::NONE);
        a.on_key(key(KeyCode::Down));
        assert!(!a.view.follow);
        let src = a.state.tree.get(Path::new("src")).unwrap();
        assert_eq!(a.view.selected, Some(src));
        a.on_key(key(KeyCode::Left));
        assert!(!a.state.tree.node(src).unwrap().expanded);
        a.on_key(key(KeyCode::Right));
        assert!(a.state.tree.node(src).unwrap().expanded);
        a.on_key(key(KeyCode::Char('a')));
        assert!(a.active_only);
        a.on_key(key(KeyCode::Char('?')));
        assert!(a.show_help);
        a.on_key(key(KeyCode::Char('q')));
        assert!(!a.quit, "first key closes help");
        a.on_key(key(KeyCode::Char('q')));
        assert!(a.quit);
    }

    #[test]
    fn clock_freezes_while_paused() {
        let mut c = Clock::new();
        c.pause();
        let t = c.now();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(c.now(), t);
        c.resume();
        assert!(c.now() - t < 0.015);
    }
}
