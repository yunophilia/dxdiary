//! File tree model.
//!
//! Kept as a flat list of visible rows rather than a recursive structure.
//! Rendering, scrolling, and hit testing all want "the Nth visible row", and a
//! flat vector answers that in constant time. Expanding or collapsing rebuilds
//! the list, which is cheap next to the directory read it implies.
//!
//! Phase 1 reads directories eagerly on expand. Git status decoration and
//! .gitignore filtering arrive in Phase 2, when there is a `Vcs` to ask.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Directories never shown at all.
///
/// Only git's own bookkeeping. Build output and dependency directories used to
/// be hidden here by name, which was wrong twice over: it hid `target/` in a
/// repository that tracks it, and showed `build/` in one that does not. What a
/// project ignores is a question only its `.gitignore` can answer, so the tree
/// now shows everything and the view greys out whatever git would ignore --
/// the same thing VS Code does, and for the same reason: you still need to see
/// that the directory is there.
const SKIP_DIRS: &[&str] = &[".git"];

#[derive(Debug, Clone)]
pub struct Row {
    pub path: PathBuf,
    /// Nesting level; the root's children are 0.
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    /// Cached for rendering, so the widget never touches the filesystem.
    pub name: String,
}

#[derive(Debug)]
pub struct FileTree {
    pub root: PathBuf,
    expanded: BTreeSet<PathBuf>,
    rows: Vec<Row>,
    /// Directories that could not be read, so the UI can say so instead of
    /// silently showing nothing.
    unreadable: BTreeSet<PathBuf>,
}

impl FileTree {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let mut tree = Self {
            root,
            expanded: BTreeSet::new(),
            rows: Vec::new(),
            unreadable: BTreeSet::new(),
        };
        tree.rebuild();
        tree
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn is_unreadable(&self, path: &Path) -> bool {
        self.unreadable.contains(path)
    }

    /// Expand or collapse the directory at `index`. Returns false if the row is
    /// not a directory, so the caller can fall through to "open this file".
    pub fn toggle(&mut self, index: usize) -> bool {
        let Some(row) = self.rows.get(index) else {
            return false;
        };
        if !row.is_dir {
            return false;
        }
        let path = row.path.clone();
        if !self.expanded.remove(&path) {
            self.expanded.insert(path);
        }
        self.rebuild();
        true
    }

    /// Expand every ancestor of `path` so it becomes visible, then return its
    /// row index. Used to reveal a file the user reached some other way.
    pub fn reveal(&mut self, path: &Path) -> Option<usize> {
        let mut cursor = path.parent();
        while let Some(dir) = cursor {
            if !dir.starts_with(&self.root) && dir != self.root {
                break;
            }
            self.expanded.insert(dir.to_path_buf());
            if dir == self.root {
                break;
            }
            cursor = dir.parent();
        }
        self.rebuild();
        self.index_of(path)
    }

    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.rows.iter().position(|r| r.path == path)
    }

    /// Re-read the filesystem, preserving which directories are expanded.
    pub fn refresh(&mut self) {
        self.unreadable.clear();
        self.rebuild();
    }

    fn rebuild(&mut self) {
        let mut rows = Vec::new();
        let root = self.root.clone();
        self.push_dir(&root, 0, &mut rows);
        self.rows = rows;
    }

    fn push_dir(&mut self, dir: &Path, depth: usize, out: &mut Vec<Row>) {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => {
                self.unreadable.insert(dir.to_path_buf());
                return;
            }
        };

        // Collect first so directories can be sorted ahead of files. read_dir
        // order is filesystem-defined and differs across platforms, which
        // matters for a tool that has to look the same on Windows and Linux.
        let mut dirs = Vec::new();
        let mut files = Vec::new();

        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue; // non-UTF-8 name; skip rather than render mojibake
            };
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);

            if is_dir && SKIP_DIRS.contains(&name) {
                continue;
            }

            let row = Row {
                depth,
                is_dir,
                expanded: is_dir && self.expanded.contains(&path),
                name: name.to_string(),
                path,
            };
            if is_dir {
                dirs.push(row);
            } else {
                files.push(row);
            }
        }

        let sort_key = |r: &Row| r.name.to_lowercase();
        dirs.sort_by_key(sort_key);
        files.sort_by_key(sort_key);

        for row in dirs {
            let expanded = row.expanded;
            let path = row.path.clone();
            out.push(row);
            if expanded {
                self.push_dir(&path, depth + 1, out);
            }
        }
        out.extend(files);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a small tree under a unique temp directory.
    fn fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("dxdiary-tree-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::write(root.join("README.md"), "x").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        root
    }

    #[test]
    fn hides_git_internals_and_sorts_dirs_first() {
        let root = fixture("basic");
        let tree = FileTree::new(&root);
        let names: Vec<_> = tree.rows().iter().map(|r| r.name.as_str()).collect();

        assert_eq!(names, vec!["src", "target", "README.md"]);
        assert!(
            !names.contains(&".git"),
            "git's own bookkeeping stays hidden"
        );
    }

    #[test]
    fn a_build_directory_is_listed_like_any_other() {
        // It used to be hidden by name, which was wrong in both directions:
        // it hid `target/` in a repository that tracks it, and left `build/`
        // visible in one that ignores it. Whether something is ignored is a
        // question for `.gitignore`, and the view greys the answer rather
        // than hiding it -- you still need to see the directory is there.
        let root = fixture("build-dir");
        let tree = FileTree::new(&root);
        assert!(tree.rows().iter().any(|r| r.name == "target"));
    }

    #[test]
    fn collapsed_directories_hide_their_children() {
        let root = fixture("collapsed");
        let tree = FileTree::new(&root);
        assert!(tree.rows().iter().all(|r| r.name != "main.rs"));
    }

    #[test]
    fn toggle_expands_then_collapses() {
        let root = fixture("toggle");
        let mut tree = FileTree::new(&root);

        assert!(tree.toggle(0), "row 0 is the src directory");
        let names: Vec<_> = tree.rows().iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["src", "main.rs", "target", "README.md"]);
        assert_eq!(tree.rows()[1].depth, 1);

        assert!(tree.toggle(0));
        assert_eq!(tree.len(), 3);
    }

    #[test]
    fn toggling_a_file_reports_false() {
        let root = fixture("file-toggle");
        let mut tree = FileTree::new(&root);
        let idx = tree.index_of(&root.join("README.md")).unwrap();
        assert!(!tree.toggle(idx));
    }

    #[test]
    fn reveal_expands_ancestors_and_finds_the_row() {
        let root = fixture("reveal");
        let mut tree = FileTree::new(&root);
        let target = root.join("src/main.rs");

        let idx = tree.reveal(&target).expect("revealed");
        assert_eq!(tree.rows()[idx].path, target);
    }
}
