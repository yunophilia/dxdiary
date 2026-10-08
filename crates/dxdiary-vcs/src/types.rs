//! Version-control types, independent of any backend.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What happened to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    /// File became a symlink, or vice versa.
    TypeChange,
    Untracked,
    Conflicted,
}

impl ChangeKind {
    /// Single-character badge, matching the letters git and VS Code use.
    pub fn badge(self) -> char {
        match self {
            ChangeKind::Added => 'A',
            ChangeKind::Modified => 'M',
            ChangeKind::Deleted => 'D',
            ChangeKind::Renamed => 'R',
            ChangeKind::TypeChange => 'T',
            ChangeKind::Untracked => '?',
            ChangeKind::Conflicted => 'U',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// Repository-relative path.
    pub path: PathBuf,
    pub kind: ChangeKind,
    /// Previous path, for renames.
    pub from: Option<PathBuf>,
}

impl FileChange {
    pub fn new(path: impl Into<PathBuf>, kind: ChangeKind) -> Self {
        Self {
            path: path.into(),
            kind,
            from: None,
        }
    }
}

/// The three buckets VS Code's SCM panel shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// HEAD → index.
    pub staged: Vec<FileChange>,
    /// index → working tree.
    pub unstaged: Vec<FileChange>,
    pub untracked: Vec<FileChange>,
}

impl Status {
    pub fn is_clean(&self) -> bool {
        self.staged.is_empty() && self.unstaged.is_empty() && self.untracked.is_empty()
    }

    pub fn len(&self) -> usize {
        self.staged.len() + self.unstaged.len() + self.untracked.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Path → badge, for decorating the file tree.
    ///
    /// A file can be in more than one bucket — staged edits plus further
    /// unstaged edits, say. Unstaged wins, because it is what the user has yet
    /// to deal with.
    pub fn badges(&self) -> BTreeMap<PathBuf, char> {
        let mut out = BTreeMap::new();
        for c in &self.staged {
            out.insert(c.path.clone(), c.kind.badge());
        }
        for c in self.unstaged.iter().chain(&self.untracked) {
            out.insert(c.path.clone(), c.kind.badge());
        }
        out
    }
}

/// What the current view is being compared against.
///
/// The whole diff pane is "current versus *some* baseline"; this is the choice.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DiffBaseline {
    /// Unstaged work: index → working tree.
    #[default]
    WorkingTree,
    /// Staged work: HEAD → index.
    Index,
    /// Everything not yet committed.
    Head,
    /// This branch versus where it forked. The reason this project exists.
    ForkPoint,
    /// An explicit ref, tag, or commit.
    Rev(String),
}

impl DiffBaseline {
    pub fn label(&self) -> String {
        match self {
            DiffBaseline::WorkingTree => "unstaged".into(),
            DiffBaseline::Index => "staged".into(),
            DiffBaseline::Head => "vs HEAD".into(),
            DiffBaseline::ForkPoint => "vs fork point".into(),
            DiffBaseline::Rev(r) => format!("vs {r}"),
        }
    }

    /// Cycle order for the `b` key.
    pub fn next(&self) -> DiffBaseline {
        match self {
            DiffBaseline::WorkingTree => DiffBaseline::Index,
            DiffBaseline::Index => DiffBaseline::Head,
            DiffBaseline::Head => DiffBaseline::ForkPoint,
            DiffBaseline::ForkPoint => DiffBaseline::WorkingTree,
            // An explicit revision is a deliberate choice; cycling returns to
            // the default rather than silently discarding it mid-rotation.
            DiffBaseline::Rev(_) => DiffBaseline::WorkingTree,
        }
    }
}

/// Where this branch diverged from its parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkPoint {
    /// The ref the fork point was computed against.
    pub base: String,
    /// Merge-base commit id.
    pub oid: String,
    /// How the base was chosen, for display when it was guessed.
    pub source: BaseSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaseSource {
    /// The branch's configured upstream.
    Upstream,
    /// Matched one of the configured trunk names.
    Trunk,
    /// Nearest reachable tag.
    Tag,
    /// Given explicitly by the caller.
    Explicit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    /// Short branch name, or None when HEAD is detached.
    pub branch: Option<String>,
    pub head_oid: String,
    /// A commit-graph makes merge-base dramatically faster on deep history:
    /// measured 6.7× on an 80k-commit repo (spike 0.3).
    pub has_commit_graph: bool,
    /// Worktree root.
    pub root: PathBuf,
}

/// Backend-independent access to a repository.
///
/// Everything above the model layer talks to this rather than to gix, so
/// swapping backends stays a one-crate change.
///
/// `Send` but not `Sync`: `gix::Repository` holds `RefCell` caches internally,
/// so it can move to another thread but not be shared across several. That is
/// enough for Phase 5, where blame moves off the render thread — it needs its
/// own handle anyway.
pub trait Vcs: Send {
    fn info(&self) -> anyhow::Result<RepoInfo>;

    /// Working-tree state, in the three buckets a user thinks in.
    fn status(&self) -> anyhow::Result<Status>;

    /// Files that differ between the current state and `baseline`.
    fn changes(&self, baseline: &DiffBaseline) -> anyhow::Result<Vec<FileChange>>;

    /// Line-level diff for one file against `baseline`.
    ///
    /// `path` may be absolute or repository-relative; implementations normalise
    /// it. Mixing the two silently is the mistake this signature exists to
    /// absorb.
    fn file_diff(
        &self,
        path: &Path,
        baseline: &DiffBaseline,
    ) -> anyhow::Result<crate::diff::FileDiff>;

    /// Resolve where this branch forked. `base` overrides auto-detection.
    fn fork_point(&self, base: Option<&str>) -> anyhow::Result<ForkPoint>;

    /// Write a commit-graph. Cheap, and worth a lot — see [`RepoInfo`].
    fn write_commit_graph(&self) -> anyhow::Result<()>;

    /// Which of these paths git would ignore.
    ///
    /// Batched, and takes the directory flag the caller already knows, because
    /// the matcher is a stateful stack that walks down through directories:
    /// asking it one path at a time in arbitrary order makes it rebuild that
    /// stack for every question. A directory pattern like `target/` also only
    /// matches when the matcher is told the path is a directory.
    ///
    /// Returns an empty set rather than an error when the ignore machinery
    /// cannot be built: not knowing what is ignored is a cosmetic loss, and no
    /// reason to refuse to draw a tree.
    fn ignored(&self, paths: &[(PathBuf, bool)]) -> std::collections::HashSet<PathBuf>;
}

/// Trunk names tried in order when a branch has no upstream.
pub const TRUNK_CANDIDATES: &[&str] = &["main", "master", "develop", "trunk"];

/// True if `path` is inside `dir`, for rolling file badges up onto directories.
pub fn is_under(path: &Path, dir: &Path) -> bool {
    path.starts_with(dir) && path != dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badges_use_the_letters_git_users_expect() {
        assert_eq!(ChangeKind::Added.badge(), 'A');
        assert_eq!(ChangeKind::Modified.badge(), 'M');
        assert_eq!(ChangeKind::Deleted.badge(), 'D');
        assert_eq!(ChangeKind::Untracked.badge(), '?');
    }

    #[test]
    fn unstaged_changes_win_over_staged_in_the_tree() {
        // A file staged and then edited again shows the edit still pending,
        // which is the state the user has to act on.
        let status = Status {
            staged: vec![FileChange::new("a.rs", ChangeKind::Added)],
            unstaged: vec![FileChange::new("a.rs", ChangeKind::Modified)],
            untracked: vec![],
        };
        assert_eq!(status.badges()[Path::new("a.rs")], 'M');
    }

    #[test]
    fn badges_cover_every_bucket() {
        let status = Status {
            staged: vec![FileChange::new("s.rs", ChangeKind::Added)],
            unstaged: vec![FileChange::new("u.rs", ChangeKind::Modified)],
            untracked: vec![FileChange::new("n.rs", ChangeKind::Untracked)],
        };
        let b = status.badges();
        assert_eq!(b[Path::new("s.rs")], 'A');
        assert_eq!(b[Path::new("u.rs")], 'M');
        assert_eq!(b[Path::new("n.rs")], '?');
    }

    #[test]
    fn a_clean_status_is_clean() {
        assert!(Status::default().is_clean());
    }

    #[test]
    fn baseline_cycles_back_to_the_start() {
        let mut b = DiffBaseline::WorkingTree;
        for _ in 0..4 {
            b = b.next();
        }
        assert_eq!(b, DiffBaseline::WorkingTree, "four steps is a full circle");
    }

    #[test]
    fn cycling_from_an_explicit_revision_returns_to_the_default() {
        assert_eq!(
            DiffBaseline::Rev("v1.0".into()).next(),
            DiffBaseline::WorkingTree
        );
    }

    #[test]
    fn is_under_excludes_the_directory_itself() {
        assert!(is_under(Path::new("src/main.rs"), Path::new("src")));
        assert!(!is_under(Path::new("src"), Path::new("src")));
        assert!(!is_under(Path::new("other/x.rs"), Path::new("src")));
    }
}
