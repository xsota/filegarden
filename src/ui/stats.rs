//! Statistics panel: counters plus a one-minute activity sparkline.

use std::time::Duration;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Sparkline, Widget};

use super::theme::{palette, Rgb, Theme};
use crate::state::AppState;

pub fn format_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// Height (inside borders) the panel would like.
pub const PREFERRED_HEIGHT: u16 = 7;

pub fn render(area: Rect, buf: &mut Buffer, state: &AppState, theme: &Theme, now: f64, elapsed: Duration) {
    let label = theme.fg(palette::DIM);
    let value = |v: String, c: Rgb| Span::styled(v, theme.fg(c).add_modifier(Modifier::BOLD));
    let s = &state.stats;
    let recent = s.recent_count(now);
    let two_cols = area.width >= 30;

    let pair = |l1: &str, v1: Span<'static>, l2: &str, v2: Span<'static>| -> Vec<Line<'static>> {
        let w = if two_cols { (area.width / 2) as usize } else { area.width as usize };
        let cell = |l: &str, v: Span<'static>| {
            let pad = w.saturating_sub(l.len() + v.content.chars().count() + 1);
            vec![Span::styled(l.to_string(), label), Span::raw(" ".repeat(pad)), v, Span::raw(" ")]
        };
        if two_cols {
            let mut a = cell(l1, v1);
            a.extend(cell(l2, v2));
            vec![Line::from(a)]
        } else {
            vec![Line::from(cell(l1, v1)), Line::from(cell(l2, v2))]
        }
    };

    let mut lines: Vec<Line> = Vec::new();
    lines.extend(pair(
        "Files",
        value(state.tree.file_count().to_string(), palette::TEXT),
        "Dirs",
        value(state.tree.dir_count().to_string(), palette::TEXT),
    ));
    lines.extend(pair(
        "Created",
        value(s.created.to_string(), palette::CREATE),
        "Modified",
        value(s.modified.to_string(), palette::MODIFY),
    ));
    lines.extend(pair(
        "Deleted",
        value(s.deleted.to_string(), palette::DELETE),
        "Renamed",
        value(s.renamed.to_string(), palette::RENAME),
    ));
    lines.extend(pair(
        "Last 60s",
        value(recent.to_string(), palette::ACCENT),
        "Elapsed",
        value(format_elapsed(elapsed), palette::TEXT),
    ));

    let text_h = (lines.len() as u16).min(area.height);
    for (i, l) in lines.into_iter().take(text_h as usize).enumerate() {
        l.render(Rect { y: area.y + i as u16, height: 1, ..area }, buf);
    }

    let spark_h = area.height.saturating_sub(text_h);
    if spark_h > 0 && area.width > 4 {
        let data = s.histogram(now, area.width as usize);
        let bar_set = if theme.ascii {
            symbols::bar::Set {
                full: "#",
                seven_eighths: "#",
                three_quarters: "=",
                five_eighths: "=",
                half: "-",
                three_eighths: "-",
                one_quarter: ".",
                one_eighth: ".",
                empty: " ",
            }
        } else {
            symbols::bar::NINE_LEVELS
        };
        Sparkline::default()
            .data(&data)
            .bar_set(bar_set)
            .style(theme.fg(palette::ACCENT))
            .render(Rect { y: area.y + text_h, height: spark_h, ..area }, buf);
        // A faint baseline so a quiet minute still reads as a chart.
        let y = area.y + area.height - 1;
        for x in area.x..area.x + area.width {
            let cell = &mut buf[(x, y)];
            if cell.symbol() == " " {
                cell.set_symbol(theme.sym("▁", "_")).set_style(theme.fg(palette::FAINT));
            }
        }
    }
}
