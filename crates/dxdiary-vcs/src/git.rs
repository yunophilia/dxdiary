//! gitoxide backend.
//!
//! The only module in the workspace that imports `gix`. Everything else goes
//! through [`Vcs`], so gix's API churn stays contained here.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gix::bstr::ByteSlice;

use crate::types::*;

pub struct GitRepo {
    repo: gix::Repository,
}

impl GitRepo {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let mut repo = gix::discover(path)
            .with_context(|| format!("no git repository at {}", path.display()))?;

        // Recommended by gix for repeated commit lookups, which merge-base and
        // blame both do heavily.
        repo.object_cache_size_if_unset(4 * 1024 * 1024);
        Ok(Self { repo })
    }

    pub fn workdir(&self) -> Option<&Path> {
        self.repo.workdir()
    }

    /// Raw handle, for the sibling modules in this crate. Nothing outside
    /// `dxdiary-vcs` may reach gix.
    pub(crate) fn repo(&self) -> &gix::Repository {
        &self.repo
    }

    /// Repository-relative form of `path`. See [`GitRepo::relative`].
    pub(crate) fn relative_path(&self, path: &Path) -> PathBuf {
        self.relative(path)
    }

    fn tree_of(&self, rev: &str) -> Result<gix::Tree<'_>> {
        let id = self
            .repo
            .rev_parse_single(rev)
            .with_context(|| format!("cannot resolve {rev:?}"))?;
        Ok(self.repo.find_object(id)?.peel_to_tree()?)
    }

    fn head_tree(&self) -> Result<gix::Tree<'_>> {
        Ok(self.repo.head_tree()?)
    }

    /// Diff two trees into our own change type.
    fn diff_trees(&self, old: &gix::Tree<'_>, new: &gix::Tree<'_>) -> Result<Vec<FileChange>> {
        let changes = self.repo.diff_tree_to_tree(Some(old), Some(new), None)?;
        Ok(changes.iter().map(tree_change_to_file_change).collect())
    }
}

impl Vcs for GitRepo {
    fn info(&self) -> Result<RepoInfo> {
        let head = self.repo.head()?;
        let branch = head
            .referent_name()
            .map(|n| n.shorten().to_str_lossy().into_owned());

        let head_oid = self
            .repo
            .head_id()
            .map(|id| id.to_string())
            .unwrap_or_default();

        let root = self
            .repo
            .workdir()
            .unwrap_or_else(|| self.repo.git_dir())
            .to_path_buf();

        Ok(RepoInfo {
            branch,
            head_oid,
            has_commit_graph: self.has_commit_graph(),
            root,
        })
    }

    fn status(&self) -> Result<Status> {
        let mut out = Status::default();

        let platform = self
            .repo
            .status(gix::progress::Discard)?
            .index_worktree_rewrites(None);

        for item in platform.into_iter(None)? {
            match item? {
                // HEAD -> index: staged.
                gix::status::Item::TreeIndex(change) => {
                    out.staged.push(index_change_to_file_change(&change));
                }
                // index -> worktree: unstaged, plus untracked from the walk.
                gix::status::Item::IndexWorktree(item) => match item {
                    gix::status::index_worktree::Item::Modification {
                        rela_path, status, ..
                    } => {
                        let kind = worktree_status_kind(&status);
                        out.unstaged
                            .push(FileChange::new(bstr_path(&rela_path), kind));
                    }
                    gix::status::index_worktree::Item::DirectoryContents { entry, .. } => {
                        out.untracked.push(FileChange::new(
                            bstr_path(&entry.rela_path),
                            ChangeKind::Untracked,
                        ));
                    }
                    gix::status::index_worktree::Item::Rewrite {
                        source,
                        dirwalk_entry,
                        ..
                    } => {
                        let mut c = FileChange::new(
                            bstr_path(&dirwalk_entry.rela_path),
                            ChangeKind::Renamed,
                        );
                        c.from = Some(bstr_path(source.rela_path()));
                        out.unstaged.push(c);
                    }
                },
            }
        }

        out.staged.sort_by(|a, b| a.path.cmp(&b.path));
        out.unstaged.sort_by(|a, b| a.path.cmp(&b.path));
        out.untracked.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn changes(&self, baseline: &DiffBaseline) -> Result<Vec<FileChange>> {
        match baseline {
            DiffBaseline::WorkingTree => Ok(self.status()?.unstaged),
            DiffBaseline::Index => Ok(self.status()?.staged),
            DiffBaseline::Head => {
                let s = self.status()?;
                // Everything uncommitted, deduplicated: a file can be both
                // staged and further modified, and it is still one file.
                let mut all = s.staged;
                all.extend(s.unstaged);
                all.extend(s.untracked);
                all.sort_by(|a, b| a.path.cmp(&b.path));
                all.dedup_by(|a, b| a.path == b.path);
                Ok(all)
            }
            DiffBaseline::ForkPoint => {
                let fork = self.fork_point(None)?;
                let old = self.tree_of(&fork.oid)?;
                let new = self.head_tree()?;
                self.diff_trees(&old, &new)
            }
            DiffBaseline::Rev(rev) => {
                let old = self.tree_of(rev)?;
                let new = self.head_tree()?;
                self.diff_trees(&old, &new)
            }
        }
    }

    fn fork_point(&self, base: Option<&str>) -> Result<ForkPoint> {
        let head = self.repo.head_id().context("HEAD has no commit")?;

        let (base_ref, source) = match base {
            Some(b) => (b.to_string(), BaseSource::Explicit),
            None => self.detect_base()?,
        };

        let base_id = self
            .repo
            .rev_parse_single(base_ref.as_str())
            .with_context(|| format!("cannot resolve base {base_ref:?}"))?;

        let merge_base = self
            .repo
            .merge_base(base_id, head)
            .with_context(|| format!("no common ancestor between HEAD and {base_ref}"))?;

        Ok(ForkPoint {
            base: base_ref,
            oid: merge_base.to_string(),
            source,
        })
    }

    fn file_diff(&self, path: &Path, baseline: &DiffBaseline) -> Result<crate::diff::FileDiff> {
        let rela = self.relative(path);
        let (old_side, new_side) = sides_for(self, baseline)?;

        let old = self.content(&old_side, &rela)?;
        let new = self.content(&new_side, &rela)?;

        // A file present on neither side is not an error — it may simply not
        // exist at that baseline.
        let (old_text, new_text) = match (old, new) {
            (None, None) => (String::new(), String::new()),
            (a, b) => (a.unwrap_or_default(), b.unwrap_or_default()),
        };

        if is_binary(old_text.as_bytes()) || is_binary(new_text.as_bytes()) {
            return Ok(crate::diff::FileDiff {
                path: rela,
                binary: true,
                ..Default::default()
            });
        }

        Ok(crate::diff::file_diff(
            rela,
            &old_text,
            &new_text,
            crate::diff::CONTEXT,
        ))
    }

    fn write_commit_graph(&self) -> Result<()> {
        // gix cannot write commit-graphs yet, so shell out. This is the one
        // place the backend calls git itself, and it is a maintenance
        // operation rather than something on a render path.
        let dir = self.repo.git_dir();
        let status = std::process::Command::new("git")
            .arg("--git-dir")
            .arg(dir)
            .args(["commit-graph", "write", "--reachable"])
            .status()
            .context("running `git commit-graph write` (is git installed?)")?;
        anyhow::ensure!(status.success(), "git commit-graph write failed");
        Ok(())
    }
}

/// One end of a diff.
#[derive(Debug, Clone)]
enum Side {
    Head,
    Index,
    Worktree,
    Rev(String),
}

/// Which two sides a baseline compares.
///
/// This is the whole semantic of `DiffBaseline` in one place — everything else
/// just fetches bytes.
fn sides_for(repo: &GitRepo, baseline: &DiffBaseline) -> Result<(Side, Side)> {
    Ok(match baseline {
        DiffBaseline::WorkingTree => (Side::Index, Side::Worktree),
        DiffBaseline::Index => (Side::Head, Side::Index),
        DiffBaseline::Head => (Side::Head, Side::Worktree),
        DiffBaseline::ForkPoint => {
            let fork = repo.fork_point(None)?;
            (Side::Rev(fork.oid), Side::Head)
        }
        DiffBaseline::Rev(rev) => (Side::Rev(rev.clone()), Side::Head),
    })
}

/// Same NUL-byte heuristic git uses.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8192).any(|&b| b == 0)
}

impl GitRepo {
    /// Normalise to a repository-relative path.
    ///
    /// git speaks repo-relative; the UI holds absolute paths. Every comparison
    /// against git output has to convert, and forgetting to is a silent
    /// mismatch rather than an error.
    fn relative(&self, path: &Path) -> PathBuf {
        match self.repo.workdir() {
            Some(root) => path.strip_prefix(root).unwrap_or(path).to_path_buf(),
            None => path.to_path_buf(),
        }
    }

    /// Read one side's content for a path, or None if absent there.
    fn content(&self, side: &Side, rela: &Path) -> Result<Option<String>> {
        let as_bytes =
            |s: &Path| -> Vec<u8> { s.to_string_lossy().replace('\\', "/").into_bytes() };

        match side {
            Side::Worktree => {
                let Some(root) = self.repo.workdir() else {
                    return Ok(None);
                };
                match std::fs::read(root.join(rela)) {
                    Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
                    Err(_) => Ok(None), // deleted, or never existed
                }
            }
            Side::Index => {
                let index = self.repo.index_or_empty()?;
                let bytes = as_bytes(rela);
                let Some(entry) = index.entry_by_path(bytes.as_slice().into()) else {
                    return Ok(None);
                };
                let obj = self.repo.find_object(entry.id)?;
                Ok(Some(String::from_utf8_lossy(&obj.data).into_owned()))
            }
            Side::Head => self.blob_in_tree("HEAD", rela),
            Side::Rev(rev) => self.blob_in_tree(rev, rela),
        }
    }

    fn blob_in_tree(&self, rev: &str, rela: &Path) -> Result<Option<String>> {
        let Ok(tree) = self.tree_of(rev) else {
            return Ok(None); // unborn HEAD, or an unresolvable rev
        };
        let components: Vec<String> = rela
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();

        let Some(entry) = tree.lookup_entry(components.iter().map(|s| s.as_bytes()))? else {
            return Ok(None);
        };
        let obj = entry.object()?;
        Ok(Some(String::from_utf8_lossy(&obj.data).into_owned()))
    }

    fn has_commit_graph(&self) -> bool {
        let info = self.repo.git_dir().join("objects").join("info");
        info.join("commit-graph").exists() || info.join("commit-graphs").is_dir()
    }

    /// Pick a base ref, most trustworthy signal first (DESIGN.md §5).
    fn detect_base(&self) -> Result<(String, BaseSource)> {
        // 1. The branch's own upstream is the user's stated intent.
        if let Ok(Some(r)) = self.repo.head_ref() {
            if let Some(Ok(upstream)) = r.remote_tracking_ref_name(gix::remote::Direction::Fetch) {
                let name = upstream.shorten().to_str_lossy().into_owned();
                if self.repo.rev_parse_single(name.as_str()).is_ok() {
                    return Ok((name, BaseSource::Upstream));
                }
            }
        }

        // 2. A conventional trunk, local or on origin.
        let current = self
            .repo
            .head_ref()
            .ok()
            .flatten()
            .map(|r| r.name().shorten().to_string());

        for name in TRUNK_CANDIDATES {
            if current.as_deref() == Some(*name) {
                continue; // a branch is not its own fork point
            }
            for candidate in [name.to_string(), format!("origin/{name}")] {
                if self.repo.rev_parse_single(candidate.as_str()).is_ok() {
                    return Ok((candidate, BaseSource::Trunk));
                }
            }
        }

        // 3. Fall back to the nearest tag.
        if let Some(tag) = self.nearest_tag() {
            return Ok((tag, BaseSource::Tag));
        }

        anyhow::bail!("no upstream, trunk branch, or tag to compare against")
    }

    /// Nearest tag by commit distance from HEAD.
    fn nearest_tag(&self) -> Option<String> {
        let head = self.repo.head_id().ok()?;
        let mut best: Option<(usize, String)> = None;

        for reference in self.repo.references().ok()?.tags().ok()?.flatten() {
            let name = reference.name().shorten().to_string();
            let Ok(id) = self.repo.rev_parse_single(name.as_str()) else {
                continue;
            };
            let Ok(base) = self.repo.merge_base(id, head) else {
                continue;
            };
            // Distance stands in for recency; an exact count would mean walking
            // the whole graph for every tag.
            let distance = self
                .repo
                .rev_walk([base.detach()])
                .all()
                .ok()
                .map(|w| w.take(1000).count())
                .unwrap_or(usize::MAX);

            if best.as_ref().is_none_or(|(d, _)| distance > *d) {
                best = Some((distance, name));
            }
        }
        best.map(|(_, n)| n)
    }
}

// ---------------------------------------------------------------- mapping ---

/// Git paths are bytes, not necessarily UTF-8. Lossy conversion keeps an
/// oddly-encoded filename visible rather than dropping the entry.
fn bstr_path(b: impl AsRef<[u8]>) -> PathBuf {
    PathBuf::from(String::from_utf8_lossy(b.as_ref()).into_owned())
}

/// Same, for the index-diff types, whose locations are `Cow<BStr>`.
/// `AsRef<[u8]>` is not implemented for `Cow`, so this cannot share the
/// function above; callers rely on deref coercion from `&Cow<BStr>`.
fn cow_path(b: &gix::bstr::BStr) -> PathBuf {
    bstr_path(b.as_bytes())
}

fn tree_change_to_file_change(change: &gix::object::tree::diff::ChangeDetached) -> FileChange {
    use gix::object::tree::diff::ChangeDetached as C;
    match change {
        C::Addition { location, .. } => FileChange::new(bstr_path(location), ChangeKind::Added),
        C::Deletion { location, .. } => FileChange::new(bstr_path(location), ChangeKind::Deleted),
        C::Modification { location, .. } => {
            FileChange::new(bstr_path(location), ChangeKind::Modified)
        }
        C::Rewrite {
            location,
            source_location,
            ..
        } => {
            let mut c = FileChange::new(bstr_path(location), ChangeKind::Renamed);
            c.from = Some(bstr_path(source_location));
            c
        }
    }
}

fn index_change_to_file_change(change: &gix::diff::index::Change) -> FileChange {
    use gix::diff::index::Change as C;
    match change {
        C::Addition { location, .. } => FileChange::new(cow_path(location), ChangeKind::Added),
        C::Deletion { location, .. } => FileChange::new(cow_path(location), ChangeKind::Deleted),
        C::Modification { location, .. } => {
            FileChange::new(cow_path(location), ChangeKind::Modified)
        }
        C::Rewrite {
            location,
            source_location,
            ..
        } => {
            let mut c = FileChange::new(cow_path(location), ChangeKind::Renamed);
            c.from = Some(cow_path(source_location));
            c
        }
    }
}

/// Generic over `EntryStatus`'s two payload parameters, which vary with how the
/// status platform was configured and are not interesting here.
fn worktree_status_kind<T, U>(
    status: &gix::status::plumbing::index_as_worktree::EntryStatus<T, U>,
) -> ChangeKind {
    use gix::status::plumbing::index_as_worktree::{Change, EntryStatus};
    match status {
        EntryStatus::Conflict { .. } => ChangeKind::Conflicted,
        EntryStatus::Change(Change::Removed) => ChangeKind::Deleted,
        EntryStatus::Change(Change::Type { .. }) => ChangeKind::TypeChange,
        // Content changes, submodule changes, and NeedsUpdate all read as
        // "modified" to a user looking at a file tree.
        _ => ChangeKind::Modified,
    }
}
