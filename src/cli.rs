use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "filegarden",
    version,
    about = "Watch your code grow — a real-time file activity visualizer for the terminal",
    long_about = "Watch your code grow.\n\n\
        FileGarden shows files being created, modified, deleted and renamed in a directory \
        as they happen. Run it next to an AI coding agent, an editor, or a build and watch \
        the tree come alive. It only reads metadata and never modifies anything.",
    args_conflicts_with_subcommands = true,
    after_help = "Examples:\n  filegarden                      watch the current directory\n  \
        filegarden watch ~/projects/app\n  filegarden watch . --exclude '*.log' --exclude tmp/\n  \
        filegarden watch . --poll         for network drives, WSL /mnt/*, containers\n  \
        filegarden demo                 simulated AI agent session, no files touched"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[command(flatten)]
    pub watch: WatchArgs,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Watch a directory (the default command)
    Watch(WatchArgs),
    /// Play a simulated coding session without touching the file system
    Demo(DemoArgs),
}

#[derive(Debug, Args)]
pub struct WatchArgs {
    /// Directory to watch
    #[arg(default_value = ".")]
    pub path: PathBuf,

    /// Exclude paths matching a glob (repeatable). `name` matches anywhere, `dir/name` is
    /// anchored at the root
    #[arg(short = 'e', long = "exclude", value_name = "PATTERN")]
    pub exclude: Vec<String>,

    /// Do not exclude .git, node_modules, target, dist, build, ... by default
    #[arg(long)]
    pub no_default_excludes: bool,

    /// Use polling instead of native OS events (network drives, WSL /mnt, some containers)
    #[arg(long)]
    pub poll: bool,

    /// Polling interval in milliseconds
    #[arg(long, value_name = "MS", default_value_t = 1000, requires = "poll")]
    pub poll_interval: u64,

    /// Event aggregation window in milliseconds
    #[arg(long, value_name = "MS", default_value_t = 100)]
    pub debounce: u64,

    /// Full re-synchronisation interval in seconds (0 disables)
    #[arg(long, value_name = "SECS", default_value_t = 60)]
    pub resync: u64,

    #[command(flatten)]
    pub display: DisplayArgs,
}

#[derive(Debug, Args)]
pub struct DemoArgs {
    /// Random seed for a reproducible session
    #[arg(long)]
    pub seed: Option<u64>,

    /// Playback speed multiplier
    #[arg(long, default_value_t = 1.0)]
    pub speed: f64,

    #[command(flatten)]
    pub display: DisplayArgs,
}

#[derive(Debug, Args, Clone)]
pub struct DisplayArgs {
    /// Disable fades and glow (changes are highlighted for a few seconds instead)
    #[arg(long)]
    pub no_animation: bool,

    /// Disable colors (also honors the NO_COLOR environment variable)
    #[arg(long)]
    pub no_color: bool,

    /// Game mode: particles, combos, score and achievements (toggle with `x`)
    #[arg(long)]
    pub fx: bool,

    /// Use ASCII instead of Unicode symbols
    #[arg(long)]
    pub ascii: bool,

    /// Color depth
    #[arg(long, value_enum, default_value_t = ColorChoice::Auto)]
    pub colors: ColorChoice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorChoice {
    Auto,
    Truecolor,
    #[value(name = "256")]
    Ansi256,
    #[value(name = "16")]
    Ansi16,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn bare_path_and_subcommands() {
        let c = Cli::parse_from(["filegarden"]);
        assert!(c.command.is_none());
        assert_eq!(c.watch.path, PathBuf::from("."));

        let c =
            Cli::parse_from(["filegarden", "watch", "/tmp/x", "--exclude", "*.log", "-e", "tmp/", "--poll"]);
        match c.command {
            Some(Command::Watch(w)) => {
                assert_eq!(w.path, PathBuf::from("/tmp/x"));
                assert_eq!(w.exclude, ["*.log", "tmp/"]);
                assert!(w.poll);
            }
            other => panic!("{other:?}"),
        }

        let c = Cli::parse_from(["filegarden", "demo", "--seed", "3", "--no-animation"]);
        assert!(matches!(c.command, Some(Command::Demo(DemoArgs { seed: Some(3), .. }))));

        let c = Cli::parse_from(["filegarden", "src", "--no-color"]);
        assert_eq!(c.watch.path, PathBuf::from("src"));
        assert!(c.watch.display.no_color);
    }
}
