//! The displayed file tree. Nodes live in an arena; deleted nodes linger as "ghosts"
//! for a moment so the deletion can be seen before they disappear.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::change::{Change, ChangeKind, Entry};

pub type NodeId = usize;

/// How long a deleted entry stays visible (seconds).
pub const GHOST_TTL: f64 = 2.4;
/// Trees larger than this start with directories collapsed.
const EXPAND_ALL_LIMIT: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Activity {
    pub kind: ChangeKind,
    pub at: f64,
    pub count: u32,
}

#[derive(Debug, Clone)]
pub struct Node {
    pub name: String,
    pub is_dir: bool,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub expanded: bool,
    /// The user folded/unfolded this directory; automatic expansion leaves it alone.
    pub pinned: bool,
    pub activity: Option<Activity>,
    /// For directories: last time something below changed.
    pub touched: Option<(ChangeKind, f64)>,
    pub deleted_at: Option<f64>,
}

impl Node {
    fn new(name: String, is_dir: bool, parent: Option<NodeId>, expanded: bool) -> Self {
        Node {
            name,
            is_dir,
            parent,
            children: Vec::new(),
            expanded,
            pinned: false,
            activity: None,
            touched: None,
            deleted_at: None,
        }
    }

    pub fn is_ghost(&self) -> bool {
        self.deleted_at.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub id: NodeId,
    pub depth: u16,
}

pub struct FileTree {
    nodes: Vec<Option<Node>>,
    free: Vec<NodeId>,
    index: HashMap<PathBuf, NodeId>,
    root: NodeId,
    ghosts: Vec<(NodeId, f64)>,
    files: usize,
    dirs: usize,
    expand_new: bool,
    version: u64,
    rows: Vec<Row>,
    rows_key: Option<(u64, bool)>,
}

impl Default for FileTree {
    fn default() -> Self {
        Self::new()
    }
}

impl FileTree {
    pub fn new() -> Self {
        FileTree {
            nodes: vec![Some(Node::new(String::new(), true, None, true))],
            free: Vec::new(),
            index: HashMap::new(),
            root: 0,
            ghosts: Vec::new(),
            files: 0,
            dirs: 0,
            expand_new: true,
            version: 0,
            rows: Vec::new(),
            rows_key: None,
        }
    }

    /// Replaces the whole tree with a snapshot, without any activity.
    pub fn load(&mut self, entries: &[Entry]) {
        *self = FileTree::new();
        let big = entries.len() > EXPAND_ALL_LIMIT;
        self.expand_new = !big;
        for e in entries {
            self.insert(&e.path, e.is_dir);
        }
        // New directories appearing later start expanded so growth is visible.
        self.expand_new = true;
    }

    pub fn file_count(&self) -> usize {
        self.files
    }

    pub fn dir_count(&self) -> usize {
        self.dirs
    }

    pub fn version(&self) -> u64 {
        self.version
    }

    pub fn root(&self) -> NodeId {
        self.root
    }

    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id).and_then(|n| n.as_ref())
    }

    fn node_mut(&mut self, id: NodeId) -> &mut Node {
        self.nodes[id].as_mut().expect("live node")
    }

    pub fn get(&self, path: &Path) -> Option<NodeId> {
        self.index.get(path).copied()
    }

    pub fn path_of(&self, id: NodeId) -> PathBuf {
        let mut parts = Vec::new();
        let mut cur = Some(id);
        while let Some(c) = cur {
            if c == self.root {
                break;
            }
            let n = self.node(c).expect("live node");
            parts.push(n.name.as_str());
            cur = n.parent;
        }
        parts.iter().rev().collect()
    }

    pub fn depth_of(&self, id: NodeId) -> usize {
        let mut d = 0;
        let mut cur = self.node(id).and_then(|n| n.parent);
        while let Some(c) = cur {
            if c == self.root {
                break;
            }
            d += 1;
            cur = self.node(c).and_then(|n| n.parent);
        }
        d
    }

    fn alloc(&mut self, node: Node) -> NodeId {
        if let Some(id) = self.free.pop() {
            self.nodes[id] = Some(node);
            id
        } else {
            self.nodes.push(Some(node));
            self.nodes.len() - 1
        }
    }

    fn attach(&mut self, parent: NodeId, child: NodeId) {
        let key = {
            let c = self.node(child).expect("live node");
            (!c.is_dir, c.name.to_lowercase(), c.name.clone())
        };
        let pos = {
            let nodes = &self.nodes;
            let p = nodes[parent].as_ref().expect("live node");
            p.children
                .binary_search_by(|&o| {
                    let n = nodes[o].as_ref().expect("live node");
                    (!n.is_dir, n.name.to_lowercase(), n.name.clone()).cmp(&key)
                })
                .unwrap_or_else(|e| e)
        };
        self.node_mut(parent).children.insert(pos, child);
        self.node_mut(child).parent = Some(parent);
    }

    fn detach(&mut self, child: NodeId) {
        if let Some(p) = self.node(child).and_then(|n| n.parent) {
            self.node_mut(p).children.retain(|&c| c != child);
        }
    }

    /// Returns the directory node for `path`, creating missing (or reviving ghost) ancestors.
    fn ensure_dir(&mut self, path: &Path) -> NodeId {
        if path.as_os_str().is_empty() {
            return self.root;
        }
        match self.get(path) {
            Some(id) if self.node(id).is_some_and(|n| n.is_dir && !n.is_ghost()) => id,
            _ => self.insert(path, true),
        }
    }

    /// Inserts `path` (no-op if it exists with the same kind). Ghosts are replaced.
    pub fn insert(&mut self, path: &Path, is_dir: bool) -> NodeId {
        if let Some(id) = self.get(path) {
            let n = self.node(id).expect("indexed node");
            if !n.is_ghost() && n.is_dir == is_dir {
                return id;
            }
            self.purge(id);
        }
        let parent_path = path.parent().unwrap_or(Path::new(""));
        let parent = self.ensure_dir(parent_path);
        let name = path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let id = self.alloc(Node::new(name, is_dir, Some(parent), is_dir && self.expand_new));
        self.attach(parent, id);
        self.index.insert(path.to_path_buf(), id);
        if is_dir {
            self.dirs += 1;
        } else {
            self.files += 1;
        }
        self.version += 1;
        id
    }

    /// Removes a subtree immediately (no ghost).
    fn purge(&mut self, id: NodeId) {
        let path = self.path_of(id);
        self.detach(id);
        let mut stack = vec![(id, path)];
        while let Some((nid, p)) = stack.pop() {
            let Some(node) = self.nodes[nid].take() else { continue };
            if !node.is_ghost() {
                if node.is_dir {
                    self.dirs -= 1;
                } else {
                    self.files -= 1;
                }
            }
            if self.index.get(&p) == Some(&nid) {
                self.index.remove(&p);
            }
            for c in node.children {
                if let Some(cn) = self.node(c) {
                    let cp = p.join(&cn.name);
                    stack.push((c, cp));
                }
            }
            self.free.push(nid);
        }
        self.version += 1;
    }

    fn mark(&mut self, id: NodeId, kind: ChangeKind, now: f64) {
        let n = self.node_mut(id);
        let count = n.activity.map_or(0, |a| a.count) + 1;
        n.activity = Some(Activity { kind, at: now, count });
    }

    /// Flags ancestors as touched and expands them so the change is visible.
    fn touch_ancestors(&mut self, id: NodeId, kind: ChangeKind, now: f64) {
        let mut cur = self.node(id).and_then(|n| n.parent);
        while let Some(c) = cur {
            if c == self.root {
                break;
            }
            let n = self.node_mut(c);
            n.touched = Some((kind, now));
            if !n.pinned && !n.expanded {
                n.expanded = true;
                self.version += 1;
            }
            cur = self.node(c).and_then(|n| n.parent);
        }
    }

    /// Applies a logical change. Returns the node that should draw the eye.
    pub fn apply(&mut self, change: &Change, now: f64) -> Option<NodeId> {
        let id = match change.kind {
            ChangeKind::Create => Some(self.insert(&change.path, change.is_dir)),
            ChangeKind::Modify => {
                let live = self.get(&change.path).filter(|&id| !self.node(id).expect("node").is_ghost());
                Some(live.unwrap_or_else(|| self.insert(&change.path, false)))
            }
            ChangeKind::Delete => self.delete(&change.path, now),
            ChangeKind::Rename => Some(self.rename(change.from.as_deref(), &change.path, change.is_dir, now)),
        }?;
        self.mark(id, change.kind, now);
        self.touch_ancestors(id, change.kind, now);
        Some(id)
    }

    fn delete(&mut self, path: &Path, now: f64) -> Option<NodeId> {
        let id = self.get(path)?;
        if self.node(id)?.is_ghost() {
            return None;
        }
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            let node = self.node_mut(n);
            if node.deleted_at.is_none() {
                if node.is_dir {
                    self.dirs -= 1;
                } else {
                    self.files -= 1;
                }
            }
            let node = self.node_mut(n);
            node.deleted_at = Some(now);
            stack.extend(node.children.iter().copied());
        }
        self.ghosts.push((id, now));
        self.version += 1;
        Some(id)
    }

    fn rename(&mut self, from: Option<&Path>, to: &Path, is_dir: bool, now: f64) -> NodeId {
        let src = from.and_then(|f| self.get(f)).filter(|&id| self.node(id).is_some_and(|n| !n.is_ghost()));
        let Some(src) = src else {
            return self.insert(to, is_dir);
        };
        let from = from.expect("source exists");
        if let Some(existing) = self.get(to) {
            if existing != src {
                self.purge(existing);
            }
        }
        // Re-index the subtree under its new path.
        let mut stack = vec![(src, from.to_path_buf(), to.to_path_buf())];
        while let Some((nid, old, new)) = stack.pop() {
            if self.index.get(&old) == Some(&nid) {
                self.index.remove(&old);
            }
            self.index.insert(new.clone(), nid);
            for &c in &self.node(nid).expect("node").children {
                let name = &self.node(c).expect("node").name;
                stack.push((c, old.join(name), new.join(name)));
            }
        }
        self.detach(src);
        let parent = self.ensure_dir(to.parent().unwrap_or(Path::new("")));
        let name = to.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        self.node_mut(src).name = name;
        self.attach(parent, src);
        // The old parent saw something leave.
        if let Some(old_parent) = from.parent().and_then(|p| self.get(p)) {
            self.node_mut(old_parent).touched = Some((ChangeKind::Rename, now));
        }
        self.version += 1;
        src
    }

    /// Drops ghosts whose time is up. Returns true if anything changed.
    pub fn tick(&mut self, now: f64) -> bool {
        let mut changed = false;
        let mut i = 0;
        while i < self.ghosts.len() {
            let (id, at) = self.ghosts[i];
            if now - at < GHOST_TTL {
                i += 1;
                continue;
            }
            self.ghosts.swap_remove(i);
            if self.node(id).is_some_and(|n| n.deleted_at == Some(at)) {
                self.purge(id);
                changed = true;
            }
        }
        changed
    }

    pub fn has_ghosts(&self) -> bool {
        !self.ghosts.is_empty()
    }

    pub fn set_expanded(&mut self, id: NodeId, expanded: bool) {
        if let Some(Some(n)) = self.nodes.get_mut(id) {
            if n.is_dir {
                n.pinned = true;
                if n.expanded != expanded {
                    n.expanded = expanded;
                    self.version += 1;
                }
            }
        }
    }

    /// Visible rows in display order. `active_only` keeps only entries that changed
    /// since startup, plus their ancestors.
    pub fn rows(&mut self, active_only: bool) -> &[Row] {
        let key = (self.version, active_only);
        if self.rows_key != Some(key) {
            let keep = active_only.then(|| self.active_mask());
            let mut rows = Vec::new();
            let mut stack: Vec<(NodeId, u16)> = Vec::new();
            let root = self.node(self.root).expect("root");
            for &c in root.children.iter().rev() {
                stack.push((c, 0));
            }
            while let Some((id, depth)) = stack.pop() {
                if keep.as_ref().is_some_and(|k| !k[id]) {
                    continue;
                }
                rows.push(Row { id, depth });
                let n = self.node(id).expect("node");
                if n.is_dir && n.expanded {
                    for &c in n.children.iter().rev() {
                        stack.push((c, depth + 1));
                    }
                }
            }
            self.rows = rows;
            self.rows_key = Some(key);
        }
        &self.rows
    }

    /// For each node: did it or anything below it change?
    fn active_mask(&self) -> Vec<bool> {
        let mut mask = vec![false; self.nodes.len()];
        for (id, n) in self.nodes.iter().enumerate() {
            let Some(n) = n else { continue };
            if n.activity.is_some() {
                let mut cur = Some(id);
                while let Some(c) = cur {
                    if mask[c] {
                        break;
                    }
                    mask[c] = true;
                    cur = self.node(c).and_then(|x| x.parent);
                }
            }
        }
        mask
    }

    /// Live file paths in display (depth-first, sorted) order.
    pub fn file_paths(&self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![(self.root, PathBuf::new())];
        while let Some((id, p)) = stack.pop() {
            let n = self.node(id).expect("node");
            if n.is_ghost() {
                continue;
            }
            if !n.is_dir {
                out.push(p);
                continue;
            }
            for &c in n.children.iter().rev() {
                let cn = self.node(c).expect("node");
                stack.push((c, p.join(&cn.name)));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(list: &[(&str, bool)]) -> Vec<Entry> {
        list.iter().map(|(p, d)| Entry { path: PathBuf::from(p), is_dir: *d }).collect()
    }

    fn names(tree: &mut FileTree) -> Vec<String> {
        let rows = tree.rows(false).to_vec();
        rows.iter()
            .map(|r| format!("{}{}", "  ".repeat(r.depth as usize), tree.node(r.id).unwrap().name))
            .collect()
    }

    #[test]
    fn load_sorts_dirs_first() {
        let mut t = FileTree::new();
        t.load(&entries(&[("b.txt", false), ("a", true), ("a/z.rs", false), ("A.md", false)]));
        assert_eq!(names(&mut t), ["a", "  z.rs", "A.md", "b.txt"]);
        assert_eq!((t.file_count(), t.dir_count()), (3, 1));
    }

    #[test]
    fn delete_leaves_ghost_then_purges() {
        let mut t = FileTree::new();
        t.load(&entries(&[("src", true), ("src/a.rs", false), ("src/b.rs", false)]));
        t.apply(&Change::delete("src", true), 10.0);
        assert_eq!((t.file_count(), t.dir_count()), (0, 0));
        assert_eq!(names(&mut t).len(), 3, "ghosts still shown");
        assert!(!t.tick(10.0 + GHOST_TTL / 2.0));
        assert!(t.tick(10.0 + GHOST_TTL));
        assert!(names(&mut t).is_empty());
        assert!(t.get(Path::new("src/a.rs")).is_none());
    }

    #[test]
    fn recreate_replaces_ghost() {
        let mut t = FileTree::new();
        t.load(&entries(&[("a.rs", false)]));
        t.apply(&Change::delete("a.rs", false), 1.0);
        t.apply(&Change::create("a.rs", false), 1.5);
        assert_eq!(t.file_count(), 1);
        assert!(!t.tick(100.0), "old ghost record is stale");
        assert_eq!(names(&mut t), ["a.rs"]);
    }

    #[test]
    fn rename_moves_subtree_and_reindexes() {
        let mut t = FileTree::new();
        t.load(&entries(&[("old", true), ("old/x", true), ("old/x/f.rs", false)]));
        t.apply(&Change::rename("old", "lib/new", true), 1.0);
        assert!(t.get(Path::new("lib/new/x/f.rs")).is_some());
        assert!(t.get(Path::new("old/x/f.rs")).is_none());
        assert_eq!(names(&mut t), ["lib", "  new", "    x", "      f.rs"]);
        assert_eq!((t.file_count(), t.dir_count()), (1, 3));
        let id = t.get(Path::new("lib/new")).unwrap();
        assert_eq!(t.path_of(id), PathBuf::from("lib/new"));
    }

    #[test]
    fn rename_over_existing_target() {
        let mut t = FileTree::new();
        t.load(&entries(&[("a", false), ("b", false)]));
        t.apply(&Change::rename("a", "b", false), 1.0);
        assert_eq!(names(&mut t), ["b"]);
        assert_eq!(t.file_count(), 1);
    }

    #[test]
    fn active_only_shows_changed_paths_with_ancestors() {
        let mut t = FileTree::new();
        t.load(&entries(&[("src", true), ("src/a.rs", false), ("src/b.rs", false), ("c", false)]));
        t.apply(&Change::modify("src/b.rs"), 1.0);
        let rows = t.rows(true).to_vec();
        let shown: Vec<_> = rows.iter().map(|r| t.node(r.id).unwrap().name.clone()).collect();
        assert_eq!(shown, ["src", "b.rs"]);
    }

    #[test]
    fn changes_expand_collapsed_ancestors_unless_pinned() {
        let big: Vec<Entry> = (0..500)
            .map(|i| Entry { path: PathBuf::from(format!("d/f{i}")), is_dir: false })
            .chain([Entry { path: "e".into(), is_dir: true }])
            .collect();
        let mut t = FileTree::new();
        t.load(&big);
        assert_eq!(t.rows(false).len(), 2, "large trees start collapsed");
        t.apply(&Change::modify("d/f3"), 1.0);
        assert_eq!(t.rows(false).len(), 502);
        let e = t.get(Path::new("e")).unwrap();
        t.set_expanded(e, false);
        t.apply(&Change::create("e/new", false), 2.0);
        assert!(!t.node(e).unwrap().expanded);
    }

    #[test]
    fn file_paths_in_display_order_without_ghosts() {
        let mut t = FileTree::new();
        t.load(&entries(&[("z.rs", false), ("a", true), ("a/b.rs", false), ("m.rs", false)]));
        t.apply(&Change::delete("m.rs", false), 0.0);
        assert_eq!(t.file_paths(), [PathBuf::from("a/b.rs"), PathBuf::from("z.rs")]);
    }
}
