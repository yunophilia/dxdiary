//! Integration tests against real git repositories.
//!
//! These build actual repos with the git CLI rather than mocking, because the
//! thing under test is whether gix's view matches git's — a mock would only
//! assert that the code agrees with itself.

use std::path::{Path, PathBuf};
use std::process::Command;

use dxdiary_vcs::{ChangeKind, DiffBaseline, GitRepo, Vcs};

/// Run a git command in `dir`, failing loudly with its stderr.
fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("running git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

/// A repo on `main` with one commit, plus deterministic identity and settings
/// so the tests do not depend on the developer's global git config.
fn repo(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dxdiary-vcs-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    git(&dir, &["init", "--quiet"]);
    // `git init --initial-branch` needs 2.28; this works on every version and
    // is valid before the first commit exists.
    git(&dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    // `author.*` and `committer.*` are pinned as well as `user.*`: they take
    // precedence over `user.name` for their respective fields, so a developer
    // with those set globally would otherwise leak their own identity into
    // these fixtures.
    for (key, value) in [
        ("user.name", "Test"),
        ("user.email", "test@example.com"),
        ("author.name", "Test"),
        ("author.email", "test@example.com"),
        ("committer.name", "Test"),
        ("committer.email", "test@example.com"),
        ("commit.gpgsign", "false"),
    ] {
        git(&dir, &["config", key, value]);
    }

    write(&dir, "README.md", "hello\n");
    write(&dir, "src/main.rs", "fn main() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "initial"]);
    dir
}

fn paths(changes: &[dxdiary_vcs::FileChange]) -> Vec<String> {
    let mut v: Vec<_> = changes
        .iter()
        .map(|c| c.path.to_string_lossy().replace('\\', "/"))
        .collect();
    v.sort();
    v
}

#[test]
fn status_splits_staged_unstaged_and_untracked() {
    let dir = repo("status");

    write(&dir, "staged.txt", "s\n");
    git(&dir, &["add", "staged.txt"]);

    write(&dir, "README.md", "hello, edited\n"); // tracked, not staged
    write(&dir, "untracked.txt", "u\n");

    let vcs = GitRepo::open(&dir).unwrap();
    let status = vcs.status().unwrap();

    assert_eq!(paths(&status.staged), vec!["staged.txt"]);
    assert_eq!(paths(&status.unstaged), vec!["README.md"]);
    assert_eq!(paths(&status.untracked), vec!["untracked.txt"]);
    assert!(!status.is_clean());
}

#[test]
fn a_clean_repo_reports_clean() {
    let dir = repo("clean");
    let vcs = GitRepo::open(&dir).unwrap();
    assert!(vcs.status().unwrap().is_clean());
}

#[test]
fn deletions_and_additions_are_distinguished() {
    let dir = repo("kinds");
    std::fs::remove_file(dir.join("README.md")).unwrap();
    write(&dir, "added.rs", "//\n");
    git(&dir, &["add", "added.rs"]);

    let vcs = GitRepo::open(&dir).unwrap();
    let status = vcs.status().unwrap();

    assert_eq!(status.staged[0].kind, ChangeKind::Added);
    assert_eq!(
        status
            .unstaged
            .iter()
            .find(|c| c.path.ends_with("README.md"))
            .unwrap()
            .kind,
        ChangeKind::Deleted
    );
}

#[test]
fn badges_reach_the_file_tree() {
    let dir = repo("badges");
    write(&dir, "src/main.rs", "fn main() { /* edited */ }\n");
    write(&dir, "new.txt", "n\n");

    let vcs = GitRepo::open(&dir).unwrap();
    let badges = vcs.status().unwrap().badges();

    assert_eq!(badges[Path::new("src/main.rs")], 'M');
    assert_eq!(badges[Path::new("new.txt")], '?');
}

#[test]
fn fork_point_finds_the_trunk_a_branch_came_from() {
    let dir = repo("fork");

    // Advance main so the fork point is genuinely behind both tips -- if it
    // were just "main's tip", this test would pass for the wrong reason.
    write(&dir, "on-main.txt", "m\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "main moves"]);

    git(&dir, &["checkout", "--quiet", "-b", "feature"]);
    write(&dir, "feature-a.rs", "//a\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "feature a"]);

    write(&dir, "feature-b.rs", "//b\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "feature b"]);

    let vcs = GitRepo::open(&dir).unwrap();
    let fork = vcs.fork_point(None).unwrap();
    assert_eq!(fork.base, "main");

    // Must equal what git itself computes.
    let expected = Command::new("git")
        .current_dir(&dir)
        .args(["merge-base", "main", "HEAD"])
        .output()
        .unwrap();
    let expected = String::from_utf8_lossy(&expected.stdout).trim().to_string();
    assert_eq!(fork.oid, expected, "merge-base must match git");
}

#[test]
fn fork_point_diff_shows_only_this_branch_s_work() {
    let dir = repo("branch-diff");

    write(&dir, "on-main.txt", "m\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "main moves"]);

    git(&dir, &["checkout", "--quiet", "-b", "feature"]);
    write(&dir, "feature-a.rs", "//a\n");
    write(&dir, "README.md", "changed on the branch\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "feature work"]);

    let vcs = GitRepo::open(&dir).unwrap();
    let changes = vcs.changes(&DiffBaseline::ForkPoint).unwrap();

    // The branch touched exactly these two. `on-main.txt` predates the fork
    // point on this branch's ancestry, so it must not appear.
    assert_eq!(paths(&changes), vec!["README.md", "feature-a.rs"]);

    let kinds: Vec<_> = changes
        .iter()
        .map(|c| (c.path.to_string_lossy().to_string(), c.kind))
        .collect();
    assert!(kinds.contains(&("feature-a.rs".into(), ChangeKind::Added)));
    assert!(kinds.contains(&("README.md".into(), ChangeKind::Modified)));
}

#[test]
fn an_explicit_base_overrides_detection() {
    let dir = repo("explicit-base");
    git(&dir, &["tag", "v1"]);
    write(&dir, "later.txt", "l\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "after the tag"]);

    let vcs = GitRepo::open(&dir).unwrap();
    let fork = vcs.fork_point(Some("v1")).unwrap();
    assert_eq!(fork.base, "v1");
    assert_eq!(fork.source, dxdiary_vcs::BaseSource::Explicit);
}

#[test]
fn head_baseline_covers_every_uncommitted_change_once() {
    let dir = repo("head-baseline");
    write(&dir, "staged.txt", "s\n");
    git(&dir, &["add", "staged.txt"]);
    // Same file staged and then edited again: still one entry.
    write(&dir, "staged.txt", "s edited\n");
    write(&dir, "untracked.txt", "u\n");

    let vcs = GitRepo::open(&dir).unwrap();
    let changes = vcs.changes(&DiffBaseline::Head).unwrap();
    assert_eq!(paths(&changes), vec!["staged.txt", "untracked.txt"]);
}

#[test]
fn repo_info_reports_the_branch_and_commit_graph_state() {
    let dir = repo("info");
    let vcs = GitRepo::open(&dir).unwrap();

    let info = vcs.info().unwrap();
    assert_eq!(info.branch.as_deref(), Some("main"));
    assert_eq!(info.head_oid.len(), 40, "full sha: {}", info.head_oid);
    assert!(!info.has_commit_graph, "a fresh repo has none");

    vcs.write_commit_graph().unwrap();
    let after = GitRepo::open(&dir).unwrap().info().unwrap();
    assert!(after.has_commit_graph, "writing one must be detected");
}

#[test]
fn opening_a_subdirectory_discovers_the_repository_root() {
    let dir = repo("discover");
    let vcs = GitRepo::open(dir.join("src")).unwrap();
    assert!(vcs.status().is_ok());
}

#[test]
fn opening_somewhere_without_a_repository_fails_clearly() {
    let dir = std::env::temp_dir().join("dxdiary-vcs-not-a-repo");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let err = match GitRepo::open(&dir) {
        Ok(_) => panic!("expected an error outside a repository"),
        Err(e) => e.to_string(),
    };
    assert!(err.contains("no git repository"), "got: {err}");
}

// --------------------------------------------------------------- hunks ---

#[test]
fn file_diff_against_the_index_shows_unstaged_edits() {
    let dir = repo("hunk-worktree");
    write(
        &dir,
        "src/main.rs",
        "fn main() {\n    println!(\"hi\");\n}\n",
    );
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "body"]);

    write(
        &dir,
        "src/main.rs",
        "fn main() {\n    println!(\"bye\");\n}\n",
    );

    let vcs = GitRepo::open(&dir).unwrap();
    let d = vcs
        .file_diff(Path::new("src/main.rs"), &DiffBaseline::WorkingTree)
        .unwrap();

    assert_eq!(d.added, 1);
    assert_eq!(d.removed, 1);
    let texts: Vec<_> = d.hunks[0].lines.iter().map(|l| l.text.as_str()).collect();
    assert!(texts.iter().any(|t| t.contains("hi")), "{texts:?}");
    assert!(texts.iter().any(|t| t.contains("bye")), "{texts:?}");
}

#[test]
fn file_diff_accepts_an_absolute_path_too() {
    let dir = repo("hunk-abs");
    write(&dir, "README.md", "changed\n");

    let vcs = GitRepo::open(&dir).unwrap();
    let d = vcs
        .file_diff(&dir.join("README.md"), &DiffBaseline::WorkingTree)
        .unwrap();
    assert!(!d.is_empty(), "absolute paths must normalise");
}

#[test]
fn file_diff_against_the_fork_point_ignores_work_done_on_main() {
    let dir = repo("hunk-fork");

    write(&dir, "shared.txt", "v1\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "on main"]);

    git(&dir, &["checkout", "--quiet", "-b", "feature"]);
    write(&dir, "shared.txt", "v1\nadded on branch\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "branch work"]);

    // main advances further, after the fork.
    git(&dir, &["checkout", "--quiet", "main"]);
    write(&dir, "shared.txt", "v1\nadded on main\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "main advances"]);
    git(&dir, &["checkout", "--quiet", "feature"]);

    let vcs = GitRepo::open(&dir).unwrap();
    let d = vcs
        .file_diff(Path::new("shared.txt"), &DiffBaseline::ForkPoint)
        .unwrap();

    let added: Vec<_> = d
        .hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| l.kind == dxdiary_vcs::LineKind::Added)
        .map(|l| l.text.clone())
        .collect();
    assert_eq!(added, vec!["added on branch"], "only this branch's work");
}

#[test]
fn file_diff_of_a_staged_change_uses_the_index() {
    let dir = repo("hunk-staged");
    write(&dir, "README.md", "hello\nstaged line\n");
    git(&dir, &["add", "README.md"]);

    let vcs = GitRepo::open(&dir).unwrap();

    let staged = vcs
        .file_diff(Path::new("README.md"), &DiffBaseline::Index)
        .unwrap();
    assert_eq!(staged.added, 1, "HEAD -> index");

    // Nothing further was edited, so index -> worktree is clean.
    let unstaged = vcs
        .file_diff(Path::new("README.md"), &DiffBaseline::WorkingTree)
        .unwrap();
    assert!(unstaged.is_empty(), "{:?}", unstaged.hunks);
}

#[test]
fn a_new_file_diffs_as_all_additions() {
    let dir = repo("hunk-new");
    write(&dir, "fresh.txt", "one\ntwo\n");

    let vcs = GitRepo::open(&dir).unwrap();
    let d = vcs
        .file_diff(Path::new("fresh.txt"), &DiffBaseline::Head)
        .unwrap();
    assert_eq!(d.added, 2);
    assert_eq!(d.removed, 0);
}

#[test]
fn a_deleted_file_diffs_as_all_removals() {
    let dir = repo("hunk-deleted");
    std::fs::remove_file(dir.join("README.md")).unwrap();

    let vcs = GitRepo::open(&dir).unwrap();
    let d = vcs
        .file_diff(Path::new("README.md"), &DiffBaseline::Head)
        .unwrap();
    assert_eq!(d.removed, 1);
    assert_eq!(d.added, 0);
}

#[test]
fn a_binary_file_is_reported_rather_than_diffed() {
    let dir = repo("hunk-binary");
    std::fs::write(dir.join("blob.bin"), [0u8, 1, 2, 0, 3]).unwrap();

    let vcs = GitRepo::open(&dir).unwrap();
    let d = vcs
        .file_diff(Path::new("blob.bin"), &DiffBaseline::Head)
        .unwrap();
    assert!(d.binary);
    assert_eq!(d.summary(), "binary file");
}

#[test]
fn an_unchanged_file_has_no_hunks() {
    let dir = repo("hunk-clean");
    let vcs = GitRepo::open(&dir).unwrap();
    let d = vcs
        .file_diff(Path::new("README.md"), &DiffBaseline::Head)
        .unwrap();
    assert!(d.is_empty());
}

// --------------------------------------------------------------- blame ---

#[test]
fn blame_attributes_each_line_to_the_commit_that_introduced_it() {
    let dir = repo("blame-basic");
    write(&dir, "poem.txt", "first\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "one"]);

    write(&dir, "poem.txt", "first\nsecond\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "two"]);

    let vcs = GitRepo::open(&dir).unwrap();
    let blame = vcs.blame(Path::new("poem.txt")).unwrap();

    assert_eq!(blame.lines.len(), 2);
    assert_ne!(
        blame.get(0).unwrap().commit,
        blame.get(1).unwrap().commit,
        "the two lines came from different commits"
    );
    assert_eq!(blame.get(0).unwrap().summary, "one");
    assert_eq!(blame.get(1).unwrap().summary, "two");
}

#[test]
fn blame_records_the_author_and_a_relative_time() {
    let dir = repo("blame-author");
    let vcs = GitRepo::open(&dir).unwrap();
    let blame = vcs.blame(Path::new("README.md")).unwrap();

    let line = blame.get(0).unwrap();
    assert_eq!(line.author, "Test", "the fixture pins author.name");
    assert!(
        line.when.ends_with("ago") || line.when == "just now",
        "{:?}",
        line.when
    );
    assert_eq!(line.commit.len(), 8, "abbreviated: {:?}", line.commit);
}

#[test]
fn every_line_of_a_multi_line_hunk_is_covered() {
    let dir = repo("blame-hunk");
    write(&dir, "block.txt", "a\nb\nc\nd\ne\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "block"]);

    let vcs = GitRepo::open(&dir).unwrap();
    let blame = vcs.blame(Path::new("block.txt")).unwrap();

    assert_eq!(blame.lines.len(), 5, "no gaps: {:#?}", blame.lines);
    for (i, line) in blame.lines.iter().enumerate() {
        assert_eq!(line.line, i, "line numbers are their own index");
        assert!(!line.commit.is_empty());
    }
}

#[test]
fn blame_accepts_an_absolute_path() {
    let dir = repo("blame-abs");
    let vcs = GitRepo::open(&dir).unwrap();
    assert!(!vcs.blame(&dir.join("README.md")).unwrap().is_empty());
}

#[test]
fn blaming_a_file_that_is_not_tracked_fails_rather_than_lying() {
    let dir = repo("blame-untracked");
    write(&dir, "ghost.txt", "boo\n");

    let vcs = GitRepo::open(&dir).unwrap();
    assert!(
        vcs.blame(Path::new("ghost.txt")).is_err(),
        "an untracked file has no history to attribute"
    );
}

#[test]
fn blame_runs_off_thread_and_delivers_over_a_channel() {
    let dir = repo("blame-async");
    let rx = dxdiary_vcs::blame::spawn(dir.clone(), dir.join("README.md"));

    let blame = rx
        .recv_timeout(std::time::Duration::from_secs(30))
        .expect("worker delivered")
        .expect("blame succeeded");
    assert!(!blame.is_empty());
}
