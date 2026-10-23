//! The activity feed: newest event at the bottom, like a log tail.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use super::theme::{palette, Theme};
use crate::change::ChangeKind;
use crate::state::activity::Feed;

pub fn render(area: Rect, buf: &mut Buffer, feed: &Feed, theme: &Theme, now: f64) {
    let entries = feed.entries();
    if entries.is_empty() {
        Line::styled("Waiting for file activity…", theme.fg(palette::DIM)).render(area, buf);
        return;
    }
    let h = area.height as usize;
    let skip = entries.len().saturating_sub(h);
    for (i, e) in entries.iter().skip(skip).enumerate() {
        let age = now - e.at;
        let kind_style = theme.heat_style(e.kind, Theme::kind_rgb(e.kind).mix(palette::DIM, 0.55), age);
        let path_style = theme.heat_style(e.kind, palette::TEXT, age);
        let dir = if e.is_dir { "/" } else { "" };
        let mut spans = vec![
            Span::styled(e.wall.format("%H:%M:%S ").to_string(), theme.fg(palette::DIM)),
            Span::styled(format!("{} ", e.kind.symbol(theme.ascii)), kind_style),
        ];
        if let (ChangeKind::Rename, Some(from)) = (e.kind, &e.from) {
            spans.push(Span::styled(format!("{from}{dir}"), theme.fg(palette::DIM)));
            spans.push(Span::styled(format!(" {} ", theme.sym("→", "->")), kind_style));
        }
        spans.push(Span::styled(format!("{}{dir}", e.path), path_style));
        if e.count > 1 {
            spans.push(Span::styled(format!("  ×{}", e.count), theme.fg(palette::DIM)));
        }
        let y = area.y + i as u16;
        Line::from(spans).render(Rect { y, height: 1, ..area }, buf);
    }
}
