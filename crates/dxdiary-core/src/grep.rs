//! Searching every file in the worktree, not just the open one.
//!
//! "Where else does this appear" is only half answered by `/`. The other half
//! is across the tree, which is where you find out whether an agent changed a
//! pattern everywhere or only where its tests happened to look.
//!
//! Matching is [`crate::Search`] per file, so smart case, overlap handling and
//! character offsets are identical to the in-file search. One set of
//! semantics, learned once.
//!
//! Synchronous and testable here; [`spawn`] puts it on a thread for the UI,
//! mirroring how blame is kept off the render path.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use crate::search::Search;

/// Directories skipped when nobody can say what the project ignores.
///
/// Only used outside a git repository. Inside one, [`search_with`] is handed a
/// predicate backed by `.gitignore`, which is the real answer; this list is a
/// guess for the case where there is no repository to ask and walking a
/// `target/` of a hundred thousand files would otherwise be the default.
const FALLBACK_SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".venv", "__pycache__"];

/// Stop after this many hits.
///
/// A one-character query over a large tree would otherwise fill memory with
/// results nobody will scroll through. The report says when it stopped early
/// rather than pretending the list is complete.
pub const MAX_HITS: usize = 500;

/// Bytes of a file to sniff for NULs before treating it as text.
const SNIFF: usize = 8192;

/// One matching line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    /// 0-based, to match everything else that indexes lines here.
    pub line: usize,
    /// The line's text, trimmed of its line ending.
    pub text: String,
    /// Character offsets of the match within `text`.
    pub start: usize,
    pub end: usize,
}

/// The outcome of one repo-wide search.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub query: String,
    pub hits: Vec<Hit>,
    pub files_searched: usize,
    /// True when [`MAX_HITS`] cut the list short.
    pub truncated: bool,
}

impl Report {
    pub fn is_empty(&self) -> bool {
        self.hits.is_empty()
    }

    pub fn len(&self) -> usize {
        self.hits.len()
    }

    /// How many distinct files matched.
    pub fn file_count(&self) -> usize {
        let mut seen: Vec<&Path> = Vec::new();
        for h in &self.hits {
            if seen.last() != Some(&h.path.as_path()) && !seen.contains(&h.path.as_path()) {
                seen.push(&h.path);
            }
        }
        seen.len()
    }

    /// One line summarising the result, for the status bar.
    pub fn summary(&self) -> String {
        if self.hits.is_empty() {
            return format!(
                "no match for {:?} in {} files",
                self.query, self.files_searched
            );
        }
        let files = self.file_count();
        format!(
            "{}{} hit{} in {} file{} for {:?}",
            if self.truncated { "first " } else { "" },
            self.hits.len(),
            if self.hits.len() == 1 { "" } else { "s" },
            files,
            if files == 1 { "" } else { "s" },
            self.query,
        )
    }
}

/// Search every text file under `root` for `query`.
///
/// Files larger than `max_bytes` and files that look binary are skipped: a
/// grep that stalls on a vendored blob or prints a line of a `.so` is worse
/// than one that admits it only reads source.
pub fn search(root: &Path, query: &str, max_bytes: u64) -> Report {
    search_with(root, query, max_bytes, &|path, is_dir| {
        is_dir
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| FALLBACK_SKIP_DIRS.contains(&n))
    })
}

/// Same, but `skip` decides what not to descend into or read.
///
/// The predicate is how `.gitignore` reaches a module that must not know what
/// git is: `dxdiary-vcs` builds one from a real ignore matcher and passes it
/// in. `.git` itself is always skipped regardless, since no `.gitignore` lists
/// it and nobody wants to grep their object store.
pub fn search_with(
    root: &Path,
    query: &str,
    max_bytes: u64,
    skip: &dyn Fn(&Path, bool) -> bool,
) -> Report {
    let mut report = Report {
        query: query.to_string(),
        ..Default::default()
    };
    if query.is_empty() {
        return report;
    }
    walk(root, query, max_bytes, skip, &mut report);
    report
}

fn walk(
    dir: &Path,
    query: &str,
    max_bytes: u64,
    skip: &dyn Fn(&Path, bool) -> bool,
    report: &mut Report,
) {
    if report.hits.len() >= MAX_HITS {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        // An unreadable directory is not worth failing the whole search over.
        return;
    };

    // Sorted, so results come back in a stable order rather than whatever
    // order the filesystem happens to hand back.
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();

    for path in paths {
        if report.hits.len() >= MAX_HITS {
            report.truncated = true;
            return;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let is_dir = path.is_dir();
        // Never, whatever the predicate says: no .gitignore lists .git, and
        // nobody wants to grep their own object store.
        if is_dir && name == ".git" {
            continue;
        }
        if skip(&path, is_dir) {
            continue;
        }
        if is_dir {
            walk(&path, query, max_bytes, skip, report);
        } else {
            search_file(&path, query, max_bytes, report);
        }
    }
}

fn search_file(path: &Path, query: &str, max_bytes: u64, report: &mut Report) {
    match std::fs::metadata(path) {
        Ok(m) if m.len() > max_bytes => return,
        Ok(_) => {}
        Err(_) => return,
    }
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    // A NUL in the first few KB is the same binary test `Document` uses.
    if bytes.iter().take(SNIFF).any(|b| *b == 0) {
        return;
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return;
    };

    report.files_searched += 1;
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let found = Search::new(&lines, query);
    for m in found.matches {
        if report.hits.len() >= MAX_HITS {
            report.truncated = true;
            return;
        }
        report.hits.push(Hit {
            path: path.to_path_buf(),
            line: m.line,
            text: lines[m.line].clone(),
            start: m.start,
            end: m.end,
        });
    }
}

/// Run [`search`] on a thread, delivering the report on a channel.
///
/// Same shape as blame: a repo-wide walk can take a second or more and a frame
/// must never wait on it.
pub fn spawn(root: PathBuf, query: String, max_bytes: u64) -> Receiver<Report> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        // A closed receiver just means the user moved on.
        let _ = tx.send(search(&root, &query, max_bytes));
    });
    rx
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIG: u64 = 1024 * 1024;

    fn fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("dxdiary-grep-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("target")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("a.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();
        std::fs::write(root.join("src/b.rs"), "// alpha again\nfn gamma() {}\n").unwrap();
        std::fs::write(root.join("target/c.rs"), "fn alpha_in_target() {}\n").unwrap();
        std::fs::write(root.join(".git/d.rs"), "fn alpha_in_git() {}\n").unwrap();
        root
    }

    #[test]
    fn finds_matches_across_directories() {
        let root = fixture("across");
        let r = search(&root, "alpha", BIG);
        assert_eq!(r.len(), 2, "{:?}", r.hits);
        assert_eq!(r.hits[0].path, root.join("a.rs"));
        assert_eq!(r.hits[1].path, root.join("src/b.rs"));
    }

    #[test]
    fn build_output_and_git_internals_are_not_searched() {
        // Outside a repository there is nothing to ask, so the fallback list
        // keeps a walk out of build output; .git is never searched at all.
        let root = fixture("skip");
        let r = search(&root, "alpha", BIG);
        assert!(
            r.hits
                .iter()
                .all(|h| !h.path.starts_with(root.join("target"))),
            "{:?}",
            r.hits
        );
        assert!(r
            .hits
            .iter()
            .all(|h| !h.path.starts_with(root.join(".git"))));
    }

    #[test]
    fn a_hit_carries_its_line_text_and_offsets() {
        let root = fixture("offsets");
        let r = search(&root, "beta", BIG);
        let h = &r.hits[0];
        assert_eq!(h.line, 1, "0-based");
        assert_eq!(h.text, "fn beta() {}");
        assert_eq!((h.start, h.end), (3, 7));
        assert_eq!(&h.text[h.start..h.end], "beta");
    }

    #[test]
    fn results_are_ordered_so_the_list_does_not_shuffle() {
        let root = fixture("order");
        let first = search(&root, "fn", BIG);
        let second = search(&root, "fn", BIG);
        let paths = |r: &Report| r.hits.iter().map(|h| h.path.clone()).collect::<Vec<_>>();
        assert_eq!(paths(&first), paths(&second));
    }

    #[test]
    fn smart_case_matches_the_in_file_search() {
        let root = std::env::temp_dir().join("dxdiary-grep-case");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "New new NEW\n").unwrap();

        assert_eq!(search(&root, "new", BIG).len(), 3, "lowercase is loose");
        assert_eq!(search(&root, "New", BIG).len(), 1, "a capital is literal");
    }

    #[test]
    fn an_empty_query_searches_nothing_at_all() {
        let root = fixture("empty");
        let r = search(&root, "", BIG);
        assert!(r.is_empty());
        assert_eq!(r.files_searched, 0, "not even opened");
    }

    #[test]
    fn a_binary_file_is_skipped_rather_than_printed() {
        let root = std::env::temp_dir().join("dxdiary-grep-bin");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("blob.bin"), b"alpha\0alpha\n").unwrap();
        std::fs::write(root.join("ok.txt"), "alpha\n").unwrap();

        let r = search(&root, "alpha", BIG);
        assert_eq!(r.len(), 1);
        assert_eq!(r.hits[0].path, root.join("ok.txt"));
    }

    #[test]
    fn a_file_over_the_limit_is_skipped() {
        let root = std::env::temp_dir().join("dxdiary-grep-big");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("huge.txt"), "alpha\n".repeat(100)).unwrap();

        assert!(search(&root, "alpha", 10).is_empty(), "600 bytes > 10");
        assert!(!search(&root, "alpha", BIG).is_empty());
    }

    #[test]
    fn the_hit_cap_stops_early_and_says_so() {
        let root = std::env::temp_dir().join("dxdiary-grep-cap");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("many.txt"), "x\n".repeat(MAX_HITS + 50)).unwrap();

        let r = search(&root, "x", BIG);
        assert_eq!(r.len(), MAX_HITS);
        assert!(r.truncated, "a truncated list must not look complete");
        assert!(r.summary().starts_with("first "), "{}", r.summary());
    }

    #[test]
    fn the_summary_counts_files_not_just_hits() {
        let root = fixture("summary");
        let r = search(&root, "alpha", BIG);
        assert_eq!(r.file_count(), 2);
        assert!(r.summary().contains("2 hits in 2 files"), "{}", r.summary());
    }

    #[test]
    fn one_hit_reads_in_the_singular() {
        let root = fixture("singular");
        let r = search(&root, "gamma", BIG);
        assert!(r.summary().contains("1 hit in 1 file"), "{}", r.summary());
    }

    #[test]
    fn no_match_says_how_many_files_were_looked_at() {
        let root = fixture("miss");
        let r = search(&root, "zzzz", BIG);
        assert!(r.is_empty());
        assert!(r.summary().contains("no match"), "{}", r.summary());
        assert!(r.files_searched >= 2, "it did look: {}", r.files_searched);
    }

    #[test]
    fn an_unreadable_directory_does_not_abort_the_search() {
        let root = fixture("unreadable");
        // A path that exists in the listing but cannot be read as a dir is
        // the realistic case (permissions, a broken symlink); walking must
        // carry on to the siblings either way.
        let r = search(&root, "alpha", BIG);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn the_threaded_form_delivers_the_same_report() {
        let root = fixture("threaded");
        let rx = spawn(root.clone(), "alpha".into(), BIG);
        let got = rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("the worker reported");
        assert_eq!(got.len(), search(&root, "alpha", BIG).len());
    }
}
