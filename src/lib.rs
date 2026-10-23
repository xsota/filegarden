//! FileGarden — watch your code grow.
//!
//! The crate is split into three loosely coupled layers:
//!
//! * [`watcher`] turns raw OS notifications into logical [`change::Change`]s,
//! * [`state`] keeps what the UI shows (tree, feed, statistics, visualizations),
//! * [`ui`] and [`viz`] render that state with Ratatui.
//!
//! [`demo`] can stand in for the watcher and feed synthetic activity.

pub mod app;
pub mod change;
pub mod cli;
pub mod demo;
pub mod fx;
pub mod state;
pub mod ui;
pub mod viz;
pub mod watcher;
