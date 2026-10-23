//! Pluggable activity visualizations (the right-hand panel).
//!
//! The Activity Grid is the only one today; others (heatmap, timeline, defrag sweep)
//! can implement [`Visualization`] and be swapped in without touching the watcher or
//! the tree.

pub mod grid;

use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use crate::change::Change;
use crate::ui::theme::Theme;

pub struct RenderCtx<'a> {
    pub theme: &'a Theme,
    pub now: f64,
    /// Live files in display order, for (re)seeding. Called lazily.
    pub files: &'a dyn Fn() -> Vec<PathBuf>,
}

pub trait Visualization {
    fn title(&self) -> &str;
    /// The tree was replaced wholesale (startup, root restored).
    fn reset(&mut self);
    fn apply(&mut self, change: &Change, now: f64);
    /// Housekeeping such as expiring deleted entries.
    fn tick(&mut self, now: f64);
    /// Whether anything is still fading at `now`.
    fn is_animating(&self, now: f64) -> bool;
    /// Draws into `area` (inside the panel border).
    fn render(&mut self, area: Rect, buf: &mut Buffer, ctx: &RenderCtx);
    /// A short status for the panel's title bar.
    fn summary(&self) -> String {
        String::new()
    }
}
