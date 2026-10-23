//! The Activity Grid: one block per file, lit up when it changes — like a defrag map.
//!
//! Positions are sticky: a file keeps its cell for as long as it is on the grid. When the
//! grid is full, a file that becomes active takes over the cell that has been quiet the
//! longest, so recent activity always has a place.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::{RenderCtx, Visualization};
use crate::change::{Change, ChangeKind};
use crate::state::tree::GHOST_TTL;
use crate::ui::theme::{palette, Theme, GLOW};

/// Each cell is a glyph plus a gap.
pub const CELL_WIDTH: u16 = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct Slot {
    pub path: PathBuf,
    pub kind: ChangeKind,
    /// Last activity; `NEG_INFINITY` for files that have not changed since startup.
    pub at: f64,
    pub hits: u32,
}

impl Slot {
    fn idle(path: PathBuf) -> Self {
        Slot { path, kind: ChangeKind::Modify, at: f64::NEG_INFINITY, hits: 0 }
    }

    fn is_dead(&self, now: f64) -> bool {
        self.kind == ChangeKind::Delete && now - self.at >= GHOST_TTL
    }
}

#[derive(Default)]
pub struct ActivityGrid {
    slots: Vec<Option<Slot>>,
    index: HashMap<PathBuf, usize>,
    /// Re-seed from the tree on the next render.
    needs_seed: bool,
    last_activity: Option<f64>,
}

impl ActivityGrid {
    pub fn new() -> Self {
        ActivityGrid { needs_seed: true, ..Default::default() }
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    pub fn slots(&self) -> &[Option<Slot>] {
        &self.slots
    }

    pub fn position(&self, path: &Path) -> Option<usize> {
        self.index.get(path).copied()
    }

    /// Changes the number of cells, keeping the most recently active files, then fills
    /// free cells with quiet files from `files`.
    pub fn resize(&mut self, cap: usize, files: impl IntoIterator<Item = PathBuf>) {
        let mut occupied: Vec<Slot> = self.slots.drain(..).flatten().collect();
        if occupied.len() > cap {
            let mut order: Vec<usize> = (0..occupied.len()).collect();
            order.sort_by(|&a, &b| occupied[b].at.total_cmp(&occupied[a].at).then(a.cmp(&b)));
            let mut keep = vec![false; occupied.len()];
            for &i in order.iter().take(cap) {
                keep[i] = true;
            }
            let mut i = 0;
            occupied.retain(|_| {
                let k = keep[i];
                i += 1;
                k
            });
        }
        self.slots = occupied.into_iter().map(Some).collect();
        self.slots.resize(cap, None);
        self.reindex();
        let mut free = self.slots.iter().filter(|s| s.is_none()).count();
        if free > 0 {
            for f in files {
                if free == 0 {
                    break;
                }
                if !self.index.contains_key(&f) {
                    let i = self.free_slot().expect("free slot counted");
                    self.index.insert(f.clone(), i);
                    self.slots[i] = Some(Slot::idle(f));
                    free -= 1;
                }
            }
        }
    }

    fn reindex(&mut self) {
        self.index = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_ref().map(|s| (s.path.clone(), i)))
            .collect();
    }

    fn free_slot(&self) -> Option<usize> {
        self.slots.iter().position(|s| s.is_none())
    }

    /// The cell a newly active file should take: a free one, else the quietest.
    fn claim(&mut self) -> Option<usize> {
        if let Some(i) = self.free_slot() {
            return Some(i);
        }
        let victim = self
            .slots
            .iter()
            .enumerate()
            .filter_map(|(i, s)| s.as_ref().map(|s| (i, s.at)))
            .min_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)))?
            .0;
        if let Some(old) = self.slots[victim].take() {
            self.index.remove(&old.path);
        }
        Some(victim)
    }

    fn touch(&mut self, path: &Path, kind: ChangeKind, now: f64) {
        if let Some(&i) = self.index.get(path) {
            if let Some(s) = self.slots[i].as_mut() {
                s.kind = kind;
                s.at = now;
                s.hits += 1;
            }
            return;
        }
        if kind == ChangeKind::Delete {
            return;
        }
        if let Some(i) = self.claim() {
            self.index.insert(path.to_path_buf(), i);
            self.slots[i] = Some(Slot { path: path.to_path_buf(), kind, at: now, hits: 1 });
        }
    }

    fn for_each_under(&mut self, dir: &Path, mut f: impl FnMut(&mut Slot)) {
        for s in self.slots.iter_mut().flatten() {
            if s.path.starts_with(dir) {
                f(s);
            }
        }
    }
}

/// Glyph ramp from quiet to blazing.
fn glyph(theme: &Theme, level: usize) -> &'static str {
    const UNI: [&str; 5] = ["·", "░", "▒", "▓", "█"];
    const ASC: [&str; 5] = [".", ":", "+", "*", "#"];
    if theme.ascii {
        ASC[level]
    } else {
        UNI[level]
    }
}

impl Visualization for ActivityGrid {
    fn title(&self) -> &str {
        "ACTIVITY GRID"
    }

    fn reset(&mut self) {
        self.slots.clear();
        self.index.clear();
        self.needs_seed = true;
    }

    fn apply(&mut self, change: &Change, now: f64) {
        self.last_activity = Some(now);
        match change.kind {
            ChangeKind::Rename => {
                let from = change.from.clone().unwrap_or_default();
                if change.is_dir {
                    let to = change.path.clone();
                    self.for_each_under(&from, |s| {
                        let rest = s.path.strip_prefix(&from).map(Path::to_path_buf).unwrap_or_default();
                        s.path = to.join(rest);
                        s.kind = ChangeKind::Rename;
                        s.at = now;
                        s.hits += 1;
                    });
                    self.reindex();
                } else if let Some(i) = self.index.remove(&from) {
                    self.index.insert(change.path.clone(), i);
                    if let Some(s) = self.slots[i].as_mut() {
                        s.path = change.path.clone();
                    }
                    self.touch(&change.path, ChangeKind::Rename, now);
                } else {
                    self.touch(&change.path, ChangeKind::Rename, now);
                }
            }
            ChangeKind::Delete if change.is_dir => self.for_each_under(&change.path, |s| {
                s.kind = ChangeKind::Delete;
                s.at = now;
            }),
            _ if change.is_dir => {}
            kind => self.touch(&change.path, kind, now),
        }
    }

    fn tick(&mut self, now: f64) {
        let mut dead = Vec::new();
        for (i, s) in self.slots.iter().enumerate() {
            if s.as_ref().is_some_and(|s| s.is_dead(now)) {
                dead.push(i);
            }
        }
        for i in dead {
            if let Some(s) = self.slots[i].take() {
                self.index.remove(&s.path);
            }
        }
    }

    fn is_animating(&self, now: f64) -> bool {
        self.last_activity.is_some_and(|t| now - t < GLOW.max(GHOST_TTL) + 0.1)
    }

    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &RenderCtx) {
        let cols = (area.width / CELL_WIDTH) as usize;
        let cap = cols * area.height as usize;
        if cap == 0 {
            return;
        }
        if self.needs_seed || cap != self.slots.len() {
            self.needs_seed = false;
            self.resize(cap, (ctx.files)());
        }
        let theme = ctx.theme;
        // Center the grid horizontally.
        let x0 = area.x + (area.width - cols as u16 * CELL_WIDTH) / 2;
        for (i, slot) in self.slots.iter().enumerate() {
            let x = x0 + (i % cols) as u16 * CELL_WIDTH;
            let y = area.y + (i / cols) as u16;
            let (sym, style) = match slot {
                None => (glyph(theme, 0), theme.fg(palette::FAINT)),
                Some(s) => {
                    let age = ctx.now - s.at;
                    let g = theme.glow(age);
                    // Files that change often keep a faint warmth: a quiet heat map.
                    let warm = (s.hits.min(8) as f64 / 8.0) * 0.6;
                    let base = palette::CELL_WARM.mix(palette::CELL_IDLE, warm);
                    let level = if s.kind == ChangeKind::Delete && g == 0.0 {
                        0
                    } else if g > 0.55 {
                        4
                    } else if g > 0.25 {
                        3
                    } else if g > 0.0 || s.hits >= 3 {
                        2
                    } else {
                        1
                    };
                    (glyph(theme, level), theme.heat_style(s.kind, base, age))
                }
            };
            buf.set_string(x, y, sym, style);
        }
    }

    fn summary(&self) -> String {
        format!("{} cells", self.slots.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(n: usize) -> Vec<PathBuf> {
        (0..n).map(|i| PathBuf::from(format!("f{i:02}"))).collect()
    }

    #[test]
    fn seeds_in_order_and_keeps_positions() {
        let mut g = ActivityGrid::new();
        g.resize(4, files(3));
        assert_eq!(g.position(Path::new("f01")), Some(1));
        g.apply(&Change::modify("f01"), 1.0);
        g.apply(&Change::create("new", false), 2.0);
        assert_eq!(g.position(Path::new("f01")), Some(1));
        assert_eq!(g.position(Path::new("new")), Some(3));
    }

    #[test]
    fn full_grid_evicts_quietest() {
        let mut g = ActivityGrid::new();
        g.resize(3, files(3));
        g.apply(&Change::modify("f00"), 1.0);
        g.apply(&Change::create("x", false), 2.0);
        // f01 was the first idle cell after f00 became active.
        assert_eq!(g.position(Path::new("x")), Some(1));
        assert_eq!(g.position(Path::new("f01")), None);
        assert_eq!(g.position(Path::new("f00")), Some(0));
    }

    #[test]
    fn rename_keeps_cell_and_delete_frees_it_later() {
        let mut g = ActivityGrid::new();
        g.resize(4, files(2));
        g.apply(&Change::rename("f01", "moved", false), 1.0);
        assert_eq!(g.position(Path::new("moved")), Some(1));
        g.apply(&Change::delete("moved", false), 2.0);
        g.tick(2.0 + GHOST_TTL / 2.0);
        assert_eq!(g.position(Path::new("moved")), Some(1), "still glowing red");
        g.tick(2.0 + GHOST_TTL);
        assert_eq!(g.position(Path::new("moved")), None);
    }

    #[test]
    fn directory_rename_and_delete_affect_children() {
        let mut g = ActivityGrid::new();
        g.resize(4, ["a/x", "a/y", "b"].map(PathBuf::from));
        g.apply(&Change::rename("a", "c", true), 1.0);
        assert_eq!(g.position(Path::new("c/y")), Some(1));
        g.apply(&Change::delete("c", true), 2.0);
        g.tick(10.0);
        assert_eq!(g.position(Path::new("c/x")), None);
        assert_eq!(g.position(Path::new("b")), Some(2));
    }

    #[test]
    fn shrinking_keeps_recent_activity() {
        let mut g = ActivityGrid::new();
        g.resize(6, files(6));
        g.apply(&Change::modify("f04"), 1.0);
        g.apply(&Change::modify("f05"), 2.0);
        g.resize(2, Vec::new());
        assert!(g.position(Path::new("f04")).is_some());
        assert!(g.position(Path::new("f05")).is_some());
        assert_eq!(g.capacity(), 2);
    }
}
