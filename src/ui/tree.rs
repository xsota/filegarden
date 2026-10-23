//! The file tree panel and its scroll/selection state.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget, Widget};

use super::theme::{palette, Theme, MARKER_TTL};
use crate::change::ChangeKind;
use crate::state::tree::{FileTree, NodeId, Row};
use crate::state::AppState;

/// Follow mode scrolls at most this often, so bursts don't make the view jump around.
const FOLLOW_SCROLL_INTERVAL: f64 = 0.35;

pub struct TreeView {
    pub selected: Option<NodeId>,
    pub offset: usize,
    /// Keep the latest change in view. Turned off by manual navigation.
    pub follow: bool,
    last_scroll: f64,
    height: usize,
}

impl Default for TreeView {
    fn default() -> Self {
        TreeView { selected: None, offset: 0, follow: true, last_scroll: f64::NEG_INFINITY, height: 10 }
    }
}

impl TreeView {
    fn selected_index(&self, rows: &[Row]) -> Option<usize> {
        self.selected.and_then(|s| rows.iter().position(|r| r.id == s))
    }

    /// Moves the selection by `delta` rows; leaves follow mode.
    pub fn move_by(&mut self, tree: &mut FileTree, active_only: bool, delta: isize) {
        let rows = tree.rows(active_only);
        if rows.is_empty() {
            return;
        }
        let cur = match (self.follow, self.selected_index(rows)) {
            (false, Some(i)) => i as isize + delta,
            // First key press picks up from the top visible row.
            _ => self.offset as isize,
        };
        let i = cur.clamp(0, rows.len() as isize - 1) as usize;
        self.selected = Some(rows[i].id);
        self.follow = false;
        self.ensure_visible(i, rows.len());
    }

    pub fn page(&mut self, tree: &mut FileTree, active_only: bool, dir: isize) {
        let step = self.height.saturating_sub(1).max(1) as isize;
        self.move_by(tree, active_only, dir * step);
    }

    pub fn home_end(&mut self, tree: &mut FileTree, active_only: bool, end: bool) {
        let len = tree.rows(active_only).len() as isize;
        self.move_by(tree, active_only, if end { len } else { -len });
    }

    /// `←`: fold the selected directory, or jump to its parent.
    pub fn collapse(&mut self, tree: &mut FileTree, active_only: bool) {
        let Some(id) = self.current(tree, active_only) else { return };
        let node = tree.node(id).expect("row node");
        if node.is_dir && node.expanded {
            tree.set_expanded(id, false);
        } else if let Some(p) = node.parent.filter(|&p| p != tree.root()) {
            self.selected = Some(p);
            self.reveal(tree, active_only);
        }
    }

    /// `→`: unfold the selected directory, or step into it.
    pub fn expand(&mut self, tree: &mut FileTree, active_only: bool) {
        let Some(id) = self.current(tree, active_only) else { return };
        let node = tree.node(id).expect("row node");
        if !node.is_dir {
            return;
        }
        if !node.expanded {
            tree.set_expanded(id, true);
        } else if let Some(&c) = node.children.first() {
            self.selected = Some(c);
            self.reveal(tree, active_only);
        }
    }

    pub fn toggle(&mut self, tree: &mut FileTree, active_only: bool) {
        let Some(id) = self.current(tree, active_only) else { return };
        if let Some(n) = tree.node(id).filter(|n| n.is_dir) {
            let e = n.expanded;
            tree.set_expanded(id, !e);
        }
    }

    /// The selected row, selecting the top visible row if nothing is selected yet.
    fn current(&mut self, tree: &mut FileTree, active_only: bool) -> Option<NodeId> {
        self.follow = false;
        let rows = tree.rows(active_only);
        if self.selected_index(rows).is_none() {
            self.selected = rows.get(self.offset.min(rows.len().saturating_sub(1))).map(|r| r.id);
        }
        self.selected
    }

    fn reveal(&mut self, tree: &mut FileTree, active_only: bool) {
        let rows = tree.rows(active_only);
        if let Some(i) = self.selected_index(rows) {
            self.ensure_visible(i, rows.len());
        }
    }

    fn ensure_visible(&mut self, i: usize, len: usize) {
        let h = self.height.max(1);
        if i < self.offset {
            self.offset = i;
        } else if i >= self.offset + h {
            self.offset = i + 1 - h;
        }
        self.offset = self.offset.min(len.saturating_sub(h));
    }

    pub fn resume_follow(&mut self) {
        self.follow = true;
        self.selected = None;
        self.last_scroll = f64::NEG_INFINITY;
    }

    /// Before drawing: in follow mode, scroll so the latest change is visible.
    fn update_scroll(&mut self, rows: &[Row], focus: Option<NodeId>, now: f64) {
        let h = self.height.max(1);
        if self.follow {
            if let Some(i) = focus.and_then(|f| rows.iter().position(|r| r.id == f)) {
                let visible = i >= self.offset && i < self.offset + h;
                if !visible && now - self.last_scroll >= FOLLOW_SCROLL_INTERVAL {
                    // Leave some context above the change.
                    self.offset = i.saturating_sub(h / 3);
                    self.last_scroll = now;
                }
            }
        } else if let Some(i) = self.selected_index(rows) {
            self.ensure_visible(i, rows.len());
        }
        self.offset = self.offset.min(rows.len().saturating_sub(h));
    }
}

pub fn render(
    area: Rect,
    buf: &mut Buffer,
    state: &mut AppState,
    view: &mut TreeView,
    active_only: bool,
    theme: &Theme,
    now: f64,
) {
    let focus = state.focus;
    let tree = &mut state.tree;
    view.height = area.height as usize;
    let rows = tree.rows(active_only).to_vec();
    view.update_scroll(&rows, focus, now);

    if rows.is_empty() {
        let msg =
            if active_only { "No activity yet — waiting for changes…" } else { "(empty directory)" };
        let msg = if theme.ascii { msg.replace('—', "-").replace('…', "...") } else { msg.to_string() };
        Line::styled(msg, theme.fg(palette::DIM)).render(area, buf);
        return;
    }

    let guide = theme.sym("│ ", "| ");
    for (line_no, row) in rows.iter().skip(view.offset).take(area.height as usize).enumerate() {
        let node = tree.node(row.id).expect("row node");
        let mut spans: Vec<Span> = Vec::with_capacity(6);
        for _ in 0..row.depth {
            spans.push(Span::styled(guide, theme.fg(palette::FAINT)));
        }
        let ghost_age = node.deleted_at.map(|t| now - t);
        let activity = node.activity;
        let age = activity.map_or(f64::INFINITY, |a| now - a.at);

        // Marker / fold arrow column.
        if node.is_dir {
            let arrow = match (node.expanded, theme.ascii) {
                (true, false) => "▾ ",
                (false, false) => "▸ ",
                (true, true) => "v ",
                (false, true) => "> ",
            };
            spans.push(Span::styled(arrow, theme.fg(palette::DIM)));
        } else {
            match activity.filter(|_| age < MARKER_TTL) {
                Some(a) => spans.push(Span::styled(
                    format!("{} ", a.kind.symbol(theme.ascii)),
                    theme.heat_style(a.kind, palette::DIM, age),
                )),
                None => spans.push(Span::raw("  ")),
            }
        }

        // Name.
        let mut name = node.name.clone();
        if node.is_dir {
            name.push('/');
        }
        let style = if let Some(gage) = ghost_age {
            theme
                .fg(theme.heat(ChangeKind::Delete, palette::FAINT, gage * 0.75))
                .add_modifier(Modifier::CROSSED_OUT)
        } else if node.is_dir {
            let base = palette::DIR;
            let mut s = match (activity, node.touched) {
                (Some(a), _) if age < 2.0 => theme.heat_style(a.kind, base, age),
                // Directories glow softly when something inside changes.
                (_, Some((k, t))) => theme.fg(Theme::kind_rgb(k).mix(base, theme.glow(now - t) * 0.45)),
                _ => theme.fg(base),
            };
            s = s.add_modifier(Modifier::BOLD);
            s
        } else {
            match activity {
                Some(a) => theme.heat_style(a.kind, palette::FILE, age),
                None => theme.fg(palette::FILE),
            }
        };
        spans.push(Span::styled(name, style));
        if let Some(a) = activity.filter(|a| a.count > 1 && !node.is_dir && age < MARKER_TTL) {
            spans.push(Span::styled(format!(" ×{}", a.count), theme.fg(palette::DIM)));
        }

        let mut line = Line::from(spans);
        if !view.follow && view.selected == Some(row.id) {
            line = line.patch_style(Style::default().add_modifier(Modifier::REVERSED));
        }
        let y = area.y + line_no as u16;
        line.render(Rect { x: area.x, y, width: area.width, height: 1 }, buf);
    }

    if rows.len() > area.height as usize && area.width > 2 {
        let mut state =
            ScrollbarState::new(rows.len().saturating_sub(area.height as usize)).position(view.offset);
        let (thumb, track) = if theme.ascii { ("#", "|") } else { ("┃", "│") };
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .thumb_symbol(thumb)
            .track_symbol(Some(track))
            .track_style(theme.fg(palette::FAINT))
            .thumb_style(theme.fg(palette::DIM))
            .render(Rect { width: area.width + 1, ..area }, buf, &mut state);
    }
}
