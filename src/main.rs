use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::Parser;

use filegarden::app::{self, Source};
use filegarden::cli::{Cli, ColorChoice, Command, DisplayArgs, WatchArgs};
use filegarden::ui::theme::{ColorMode, Theme};
use filegarden::watcher::filter::Filter;
use filegarden::watcher::WatchConfig;

fn theme(d: &DisplayArgs) -> Theme {
    let forced = match d.colors {
        ColorChoice::Auto => None,
        ColorChoice::Truecolor => Some(ColorMode::TrueColor),
        ColorChoice::Ansi256 => Some(ColorMode::Ansi256),
        ColorChoice::Ansi16 => Some(ColorMode::Ansi16),
    };
    Theme::detect(d.no_color, d.ascii, !d.no_animation, forced)
}

fn watch_config(w: &WatchArgs) -> Result<WatchConfig> {
    let root = w.path.canonicalize().with_context(|| format!("cannot open {}", w.path.display()))?;
    anyhow::ensure!(root.is_dir(), "{} is not a directory", w.path.display());
    let filter =
        Filter::with_defaults(&w.exclude, !w.no_default_excludes).context("invalid --exclude pattern")?;
    Ok(WatchConfig {
        root,
        filter,
        poll: w.poll.then(|| Duration::from_millis(w.poll_interval.max(50))),
        debounce: Duration::from_millis(w.debounce),
        resync: (w.resync > 0).then(|| Duration::from_secs(w.resync)),
    })
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let (source, display) = match cli.command {
        Some(Command::Demo(d)) => {
            let seed = d.seed.unwrap_or_else(|| {
                SystemTime::now().duration_since(UNIX_EPOCH).map(|t| t.as_nanos() as u64).unwrap_or(42)
            });
            (Source::Demo { seed, speed: d.speed }, d.display)
        }
        Some(Command::Watch(w)) => (Source::Watch(watch_config(&w)?), w.display),
        None => (Source::Watch(watch_config(&cli.watch)?), cli.watch.display),
    };
    app::run(source, theme(&display))
}
