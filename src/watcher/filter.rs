//! Exclusion rules.
//!
//! A pattern without a `/` matches any path component (`node_modules`, `*.log`).
//! A pattern containing a `/` is anchored at the watched root (`docs/generated`).
//! Matching a directory excludes everything below it.

use std::path::Path;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

/// Directories excluded unless `--no-default-excludes` is given.
pub const DEFAULT_EXCLUDES: &[&str] =
    &[".git", "node_modules", "target", ".next", "dist", "build", "vendor", ".venv", "coverage"];

/// Editor swap/backup files. These are noise when watching an editor or agent save files.
pub const DEFAULT_TEMP_EXCLUDES: &[&str] =
    &["*.swp", "*.swo", "*.swx", "*~", ".#*", "*___jb_tmp___", "*___jb_old___", ".DS_Store"];

#[derive(Debug, Clone)]
pub struct Filter {
    set: GlobSet,
    patterns: Vec<String>,
}

impl Filter {
    pub fn new<I, S>(patterns: I) -> Result<Self, globset::Error>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut builder = GlobSetBuilder::new();
        let mut kept = Vec::new();
        for raw in patterns {
            let raw = raw.as_ref().trim().replace('\\', "/");
            let p = raw.strip_prefix("./").unwrap_or(&raw).trim_end_matches('/');
            if p.is_empty() {
                continue;
            }
            let anchored = p.contains('/');
            let p = p.trim_start_matches('/');
            let globs = if anchored {
                [p.to_string(), format!("{p}/**")]
            } else {
                [format!("**/{p}"), format!("**/{p}/**")]
            };
            for g in globs {
                builder.add(GlobBuilder::new(&g).literal_separator(true).build()?);
            }
            kept.push(raw.to_string());
        }
        Ok(Filter { set: builder.build()?, patterns: kept })
    }

    /// Builds the filter used by the CLI: optional defaults plus user patterns.
    pub fn with_defaults(extra: &[String], use_defaults: bool) -> Result<Self, globset::Error> {
        let mut all: Vec<String> = Vec::new();
        if use_defaults {
            all.extend(DEFAULT_EXCLUDES.iter().map(|s| s.to_string()));
            all.extend(DEFAULT_TEMP_EXCLUDES.iter().map(|s| s.to_string()));
        }
        all.extend(extra.iter().cloned());
        Filter::new(all)
    }

    pub fn none() -> Self {
        Filter::new(Vec::<String>::new()).expect("empty glob set")
    }

    /// `rel` is a path relative to the watched root.
    pub fn is_excluded(&self, rel: &Path) -> bool {
        !rel.as_os_str().is_empty() && self.set.is_match(rel)
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_patterns_match_anywhere() {
        let f = Filter::new(["node_modules", "*.log"]).unwrap();
        assert!(f.is_excluded(Path::new("node_modules")));
        assert!(f.is_excluded(Path::new("node_modules/react/index.js")));
        assert!(f.is_excluded(Path::new("packages/a/node_modules/x.js")));
        assert!(f.is_excluded(Path::new("debug.log")));
        assert!(f.is_excluded(Path::new("logs/today.log")));
        assert!(!f.is_excluded(Path::new("src/node_modules_helper.ts")));
        assert!(!f.is_excluded(Path::new("src/log.ts")));
        assert!(!f.is_excluded(Path::new("")));
    }

    #[test]
    fn anchored_patterns_match_from_root() {
        let f = Filter::new(["docs/generated/", "./tmp"]).unwrap();
        assert!(f.is_excluded(Path::new("docs/generated")));
        assert!(f.is_excluded(Path::new("docs/generated/a.md")));
        assert!(!f.is_excluded(Path::new("x/docs/generated/a.md")));
        assert!(f.is_excluded(Path::new("tmp/file")));
        assert!(f.is_excluded(Path::new("a/tmp/file")), "unanchored after ./ strip");
    }

    #[test]
    fn defaults_cover_common_dirs_and_temp_files() {
        let f = Filter::with_defaults(&[], true).unwrap();
        for p in [".git/HEAD", "target/debug/x", "web/.next/cache", "src/.main.rs.swp", "a.txt~"] {
            assert!(f.is_excluded(Path::new(p)), "{p}");
        }
        assert!(!f.is_excluded(Path::new("src/main.rs")));
        let none = Filter::with_defaults(&[], false).unwrap();
        assert!(!none.is_excluded(Path::new(".git/HEAD")));
    }
}
