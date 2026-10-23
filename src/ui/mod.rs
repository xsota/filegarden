//! Rendering. Reads [`App`] state; never touches the file system.

pub mod feed;
pub mod stats;
pub mod theme;
pub mod tree;

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};
use ratatui::Frame;

use crate::app::App;
use crate::state::SourceStatus;
use crate::viz::RenderCtx;
use theme::{palette, Theme};

/// Below this size only a notice is shown.
const MIN_WIDTH: u16 = 24;
const MIN_HEIGHT: u16 = 6;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    let buf = frame.buffer_mut();
    let theme = app.theme;
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        Paragraph::new("FileGarden: terminal too small")
            .style(theme.fg(palette::DIM))
            .wrap(Wrap { trim: true })
            .render(area, buf);
        return;
    }

    // Vertical budget: header, body, feed, footer. The feed goes first when space is short.
    let feed_h = match area.height {
        h if h >= 30 => 9,
        h if h >= 22 => 7,
        h if h >= 16 => 5,
        _ => 0,
    };
    let [header, body, feed, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(feed_h),
        Constraint::Length(1),
    ])
    .areas(area);

    render_header(header, buf, app);

    // Horizontal: tree | right column (grid + stats). Narrow terminals show the tree only.
    let side = body.width >= 64;
    let (tree_area, right) = if side {
        let right_w = (body.width * 45 / 100).clamp(26, 72);
        let [l, r] = Layout::horizontal([Constraint::Min(20), Constraint::Length(right_w)]).areas(body);
        (l, Some(r))
    } else {
        (body, None)
    };

    render_tree_panel(tree_area, buf, app);

    let mut stats_shown = false;
    if let Some(right) = right {
        let stats_h = stats::PREFERRED_HEIGHT + 2;
        let show_stats = right.height >= stats_h + 6;
        let [grid_area, stats_area] =
            Layout::vertical([Constraint::Min(3), Constraint::Length(if show_stats { stats_h } else { 0 })])
                .areas(right);
        render_grid_panel(grid_area, buf, app);
        if show_stats {
            stats_shown = true;
            let block = panel(&theme, " STATISTICS ", false);
            let inner = block.inner(stats_area);
            block.render(stats_area, buf);
            stats::render(inner, buf, &app.state, &theme, app.now(), app.elapsed());
        }
    }

    if feed_h > 0 {
        let block = panel(&theme, " ACTIVITY ", false);
        let inner = block.inner(feed);
        block.render(feed, buf);
        feed::render(inner, buf, &app.state.feed, &theme, app.now());
    }

    render_footer(footer, buf, app, !stats_shown);

    if app.show_help {
        render_help(area, buf, &theme);
    }
}

fn panel<'a>(theme: &Theme, title: &'a str, focused: bool) -> Block<'a> {
    let border = if focused { palette::DIM } else { palette::BORDER };
    Block::default()
        .borders(Borders::ALL)
        .border_set(theme.border_set())
        .border_style(theme.fg(border))
        .title(Span::styled(title, theme.fg(palette::TEXT).add_modifier(Modifier::BOLD)))
}

fn render_header(area: Rect, buf: &mut Buffer, app: &App) {
    let t = &app.theme;
    let now = app.now();
    let dot = t.sym("●", "*");
    let (label, color) = if app.paused {
        (format!("{} PAUSED", t.sym("❚❚", "||")), palette::MODIFY)
    } else {
        match app.state.status {
            SourceStatus::Scanning => (format!("{} SCANNING", t.sym("◌", "o")), palette::DIM),
            SourceStatus::RootMissing => (format!("{} ROOT MISSING", t.sym("✖", "x")), palette::DELETE),
            SourceStatus::Live if app.demo => (format!("{dot} DEMO"), palette::RENAME),
            SourceStatus::Live => (format!("{dot} LIVE"), palette::ACCENT),
        }
    };
    // The dot brightens with recent activity.
    let since = app.state.stats.last_event_at.map_or(f64::INFINITY, |t0| now - t0);
    let color = if app.paused || app.state.status != SourceStatus::Live {
        color
    } else {
        palette::FLASH.mix(color, t.glow(since) * 0.6)
    };
    let label = format!("{label} ");
    let right_w = label.chars().count();
    Line::from(Span::styled(label, t.fg(color).add_modifier(Modifier::BOLD)))
        .alignment(Alignment::Right)
        .render(area, buf);

    let title = format!(" {}FILEGARDEN", t.sym("✿ ", "* "));
    let room = (area.width as usize).saturating_sub(title.chars().count() + right_w + 4);
    let left = Line::from(vec![
        Span::styled(title, t.fg(palette::ACCENT).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {}", truncate_start(&app.root_label, room, t)), t.fg(palette::DIM)),
    ]);
    left.render(Rect { width: area.width.saturating_sub(right_w as u16 + 1), ..area }, buf);
}

/// Keeps the end of `s` (the most specific part of a path) within `max` columns.
fn truncate_start(s: &str, max: usize, t: &Theme) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let ell = t.sym("…", "...");
    let keep = max.saturating_sub(ell.chars().count());
    let tail: String = s.chars().skip(n - keep).collect();
    format!("{ell}{tail}")
}

fn render_tree_panel(area: Rect, buf: &mut Buffer, app: &mut App) {
    let theme = app.theme;
    let mode = if app.active_only { "active" } else { "all" };
    let follow = if app.view.follow { theme.sym(" · follow", " - follow") } else { "" };
    let title = format!(" FILE TREE {} {mode}{follow} ", theme.sym("·", "-"));
    let rows = app.state.tree.rows(app.active_only).len();
    let block = panel(&theme, "", !app.view.follow)
        .title(Span::styled(title, theme.fg(palette::TEXT).add_modifier(Modifier::BOLD)));
    let block = block.title(
        Line::from(Span::styled(format!(" {rows} rows "), theme.fg(palette::DIM)))
            .alignment(Alignment::Right),
    );
    let inner = block.inner(area);
    block.render(area, buf);
    let now = app.now();
    tree::render(inner, buf, &mut app.state, &mut app.view, app.active_only, &theme, now);
}

fn render_grid_panel(area: Rect, buf: &mut Buffer, app: &mut App) {
    let theme = app.theme;
    let title = format!(" {} ", app.state.viz.title());
    let legend = [
        (crate::change::ChangeKind::Create, "new"),
        (crate::change::ChangeKind::Modify, "mod"),
        (crate::change::ChangeKind::Delete, "del"),
        (crate::change::ChangeKind::Rename, "mv"),
    ];
    let mut legend_spans = vec![Span::raw(" ")];
    for (k, name) in legend {
        legend_spans.push(Span::styled(theme.sym("■", "#"), theme.fg(Theme::kind_rgb(k))));
        legend_spans.push(Span::styled(format!(" {name}  "), theme.fg(palette::DIM)));
    }
    let block = panel(&theme, "", false)
        .title(Span::styled(title, theme.fg(palette::TEXT).add_modifier(Modifier::BOLD)))
        .title(
            Line::from(Span::styled(format!(" {} ", app.state.viz.summary()), theme.fg(palette::DIM)))
                .alignment(Alignment::Right),
        );
    let block = if area.width >= 30 { block.title_bottom(Line::from(legend_spans)) } else { block };
    let inner = block.inner(area);
    block.render(area, buf);
    let now = app.now();
    let tree = &app.state.tree;
    let files = || tree.file_paths();
    let ctx = RenderCtx { theme: &theme, now, files: &files };
    app.state.viz.render(inner, buf, &ctx);
}

fn render_footer(area: Rect, buf: &mut Buffer, app: &App, compact_stats: bool) {
    let t = &app.theme;
    let sep = Span::styled(format!(" {} ", t.sym("│", "|")), t.fg(palette::FAINT));
    let now = app.now();
    let mut spans = Vec::new();

    if let Some((msg, _)) = &app.state.notice {
        spans.push(Span::styled(format!(" {msg}"), t.fg(palette::MODIFY)));
    } else {
        let what = if app.paused {
            "Paused"
        } else {
            match app.state.status {
                SourceStatus::Scanning => "Scanning",
                SourceStatus::Live if app.demo => "Demo",
                SourceStatus::Live => "Watching",
                SourceStatus::RootMissing => "Waiting",
            }
        };
        spans.push(Span::styled(format!(" {what}"), t.fg(palette::TEXT)));
    }
    spans.push(sep.clone());
    spans.push(Span::styled(format!("{} elapsed", stats::format_elapsed(app.elapsed())), t.fg(palette::DIM)));
    if compact_stats {
        let s = &app.state.stats;
        spans.push(sep.clone());
        spans.push(Span::styled(format!("{} files ", app.state.tree.file_count()), t.fg(palette::DIM)));
        spans.push(Span::styled(format!("+{} ", s.created), t.fg(palette::CREATE)));
        spans.push(Span::styled(format!("~{} ", s.modified), t.fg(palette::MODIFY)));
        spans.push(Span::styled(format!("-{} ", s.deleted), t.fg(palette::DELETE)));
        spans.push(Span::styled(format!("{}{}", t.sym("→", ">"), s.renamed), t.fg(palette::RENAME)));
    } else {
        spans.push(sep.clone());
        spans.push(Span::styled(format!("{} ev/min", app.state.stats.recent_count(now)), t.fg(palette::DIM)));
    }
    spans.push(sep);
    spans.push(Span::styled("?", t.fg(palette::TEXT).add_modifier(Modifier::BOLD)));
    spans.push(Span::styled(" help  ", t.fg(palette::DIM)));
    spans.push(Span::styled("q", t.fg(palette::TEXT).add_modifier(Modifier::BOLD)));
    spans.push(Span::styled(" quit", t.fg(palette::DIM)));
    Line::from(spans).render(area, buf);
}

pub const KEYS: &[(&str, &str)] = &[
    ("q / Esc", "quit"),
    ("↑ ↓  PgUp PgDn", "move selection"),
    ("Home / End", "first / last entry"),
    ("← →", "collapse / expand directory"),
    ("Enter", "toggle directory"),
    ("f", "follow latest change"),
    ("Space", "pause / resume display"),
    ("r", "rescan directory"),
    ("a", "all files / active files only"),
    ("?", "this help"),
];

fn render_help(area: Rect, buf: &mut Buffer, theme: &Theme) {
    let w = 56.min(area.width);
    let h = (KEYS.len() as u16 + 6).min(area.height);
    let popup =
        Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    Clear.render(popup, buf);
    let block = panel(theme, " HELP ", true).border_style(theme.fg(palette::ACCENT));
    let inner = block.inner(popup);
    block.render(popup, buf);
    let mut lines = vec![
        Line::styled("Watch your code grow.", theme.fg(palette::ACCENT).add_modifier(Modifier::BOLD)),
        Line::raw(""),
    ];
    for (k, d) in KEYS {
        let k = if theme.ascii {
            k.replace('↑', "Up").replace('↓', "Dn").replace('←', "Lt").replace('→', "Rt")
        } else {
            k.to_string()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("  {k:<18}"), theme.fg(palette::TEXT).add_modifier(Modifier::BOLD)),
            Span::styled(*d, theme.fg(palette::DIM)),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::styled("  press any key to close", theme.fg(palette::FAINT)));
    Paragraph::new(lines).render(inner, buf);
}
