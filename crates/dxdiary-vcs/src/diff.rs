//! Line diffing and hunk assembly.
//!
//! Pure: text in, hunks out. The git plumbing that fetches the two sides lives
//! in `git.rs`, so everything here is testable without a repository.
//!
//! Uses the slider heuristics gix ships, which nudge hunk boundaries to the
//! places git would choose. Without them a diff is still *correct* but often
//! lands on a blank line or a closing brace instead of the change itself.

use std::path::PathBuf;

use gix::diff::blob::{diff_with_slider_heuristics, Algorithm, InternedInput};

/// Lines of unchanged context shown around each change.
pub const CONTEXT: usize = 3;

/// imara-diff's line tokens keep their terminator. Strip it, so rendering does
/// not have to and line text compares as the user sees it.
fn trim_eol(s: &str) -> &str {
    s.strip_suffix('\n')
        .map(|s| s.strip_suffix('\r').unwrap_or(s))
        .unwrap_or(s)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

impl LineKind {
    /// The leading character in a unified diff.
    pub fn sigil(self) -> char {
        match self {
            LineKind::Context => ' ',
            LineKind::Added => '+',
            LineKind::Removed => '-',
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// 1-based line number on the old side, absent for additions.
    pub old_no: Option<u32>,
    /// 1-based line number on the new side, absent for removals.
    pub new_no: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    pub lines: Vec<DiffLine>,
}

impl Hunk {
    /// The `@@ -a,b +c,d @@` header git prints.
    pub fn header(&self) -> String {
        format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_count, self.new_start, self.new_count
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FileDiff {
    pub path: PathBuf,
    pub hunks: Vec<Hunk>,
    pub added: usize,
    pub removed: usize,
    /// One side was binary, so no line diff was attempted.
    pub binary: bool,
    /// Both sides in full.
    ///
    /// Kept so the renderer can syntax-highlight each side *as a whole file*
    /// and look results up by line number. Highlighting diff lines in isolation
    /// gets multi-line strings and block comments wrong, which is exactly the
    /// code a reviewer is squinting at.
    pub old_text: String,
    pub new_text: String,
}

impl FileDiff {
    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty()
    }

    /// Total rendered rows, headers included — the scroll bound.
    pub fn display_rows(&self) -> usize {
        self.hunks.iter().map(|h| h.lines.len() + 1).sum()
    }

    pub fn summary(&self) -> String {
        if self.binary {
            return "binary file".into();
        }
        if self.hunks.is_empty() {
            return "no changes".into();
        }
        format!(
            "{} hunk(s), +{} -{}",
            self.hunks.len(),
            self.added,
            self.removed
        )
    }
}

/// Diff two texts into hunks with `context` lines of surrounding context.
pub fn diff_text(old: &str, new: &str, context: usize) -> Vec<Hunk> {
    let input = InternedInput::new(old, new);
    let diff = diff_with_slider_heuristics(Algorithm::Histogram, &input);

    let old_lines: Vec<&str> = input.before.iter().map(|&t| input.interner[t]).collect();
    let new_lines: Vec<&str> = input.after.iter().map(|&t| input.interner[t]).collect();

    let raw: Vec<(std::ops::Range<u32>, std::ops::Range<u32>)> = diff
        .hunks()
        .map(|h| (h.before.clone(), h.after.clone()))
        .collect();

    assemble(&raw, &old_lines, &new_lines, context)
}

/// Turn raw change ranges into displayable hunks.
///
/// Two changes closer together than twice the context would print overlapping
/// context, so they are merged into one hunk — the same rule `git diff` uses.
fn assemble(
    raw: &[(std::ops::Range<u32>, std::ops::Range<u32>)],
    old_lines: &[&str],
    new_lines: &[&str],
    context: usize,
) -> Vec<Hunk> {
    if raw.is_empty() {
        return Vec::new();
    }
    let ctx = context as u32;
    let mut out = Vec::new();
    let mut group_start = 0usize;

    for i in 0..raw.len() {
        let is_last = i + 1 == raw.len();
        let merge_with_next = !is_last && {
            let gap = raw[i + 1].0.start.saturating_sub(raw[i].0.end);
            gap <= ctx * 2
        };
        if merge_with_next {
            continue;
        }

        let group = &raw[group_start..=i];
        out.push(build_hunk(group, old_lines, new_lines, ctx));
        group_start = i + 1;
    }
    out
}

fn build_hunk(
    group: &[(std::ops::Range<u32>, std::ops::Range<u32>)],
    old_lines: &[&str],
    new_lines: &[&str],
    ctx: u32,
) -> Hunk {
    let first = &group[0];
    let last = &group[group.len() - 1];

    let old_from = first.0.start.saturating_sub(ctx);
    let old_to = (last.0.end + ctx).min(old_lines.len() as u32);
    let new_from = first.1.start.saturating_sub(ctx);
    let new_to = (last.1.end + ctx).min(new_lines.len() as u32);

    let mut lines = Vec::new();
    let mut old_cursor = old_from;
    let mut new_cursor = new_from;

    for (before, after) in group {
        // Context before this change: the two sides advance together.
        while old_cursor < before.start {
            lines.push(DiffLine {
                kind: LineKind::Context,
                old_no: Some(old_cursor + 1),
                new_no: Some(new_cursor + 1),
                text: trim_eol(old_lines[old_cursor as usize]).to_string(),
            });
            old_cursor += 1;
            new_cursor += 1;
        }
        // Removals first, then additions — the order git prints.
        while old_cursor < before.end {
            lines.push(DiffLine {
                kind: LineKind::Removed,
                old_no: Some(old_cursor + 1),
                new_no: None,
                text: trim_eol(old_lines[old_cursor as usize]).to_string(),
            });
            old_cursor += 1;
        }
        while new_cursor < after.end {
            lines.push(DiffLine {
                kind: LineKind::Added,
                old_no: None,
                new_no: Some(new_cursor + 1),
                text: trim_eol(new_lines[new_cursor as usize]).to_string(),
            });
            new_cursor += 1;
        }
    }

    // Trailing context.
    while old_cursor < old_to && new_cursor < new_to {
        lines.push(DiffLine {
            kind: LineKind::Context,
            old_no: Some(old_cursor + 1),
            new_no: Some(new_cursor + 1),
            text: trim_eol(old_lines[old_cursor as usize]).to_string(),
        });
        old_cursor += 1;
        new_cursor += 1;
    }

    Hunk {
        // Headers are 1-based; an empty side is reported as starting at 0,
        // matching git.
        old_start: if old_cursor > old_from {
            old_from + 1
        } else {
            0
        },
        old_count: old_cursor - old_from,
        new_start: if new_cursor > new_from {
            new_from + 1
        } else {
            0
        },
        new_count: new_cursor - new_from,
        lines,
    }
}

/// Build a [`FileDiff`], counting additions and removals.
pub fn file_diff(path: impl Into<PathBuf>, old: &str, new: &str, context: usize) -> FileDiff {
    let hunks = diff_text(old, new, context);
    let added = hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| l.kind == LineKind::Added)
        .count();
    let removed = hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| l.kind == LineKind::Removed)
        .count();

    FileDiff {
        path: path.into(),
        hunks,
        added,
        removed,
        binary: false,
        old_text: old.to_string(),
        new_text: new.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(h: &Hunk) -> String {
        h.lines.iter().map(|l| l.kind.sigil()).collect()
    }

    #[test]
    fn identical_text_produces_no_hunks() {
        assert!(diff_text("a\nb\nc\n", "a\nb\nc\n", CONTEXT).is_empty());
    }

    #[test]
    fn a_single_replacement_shows_removal_then_addition() {
        let hunks = diff_text("a\nb\nc\n", "a\nB\nc\n", CONTEXT);
        assert_eq!(hunks.len(), 1);
        assert_eq!(kinds(&hunks[0]), " -+ ");

        let removed = &hunks[0].lines[1];
        assert_eq!(removed.text, "b");
        assert_eq!(removed.old_no, Some(2));
        assert_eq!(removed.new_no, None);

        let added = &hunks[0].lines[2];
        assert_eq!(added.text, "B");
        assert_eq!(added.old_no, None);
        assert_eq!(added.new_no, Some(2));
    }

    #[test]
    fn context_is_limited_to_the_requested_number_of_lines() {
        let old = (1..=20).map(|n| format!("line{n}\n")).collect::<String>();
        let new = old.replace("line10", "CHANGED");

        let hunks = diff_text(&old, &new, 2);
        assert_eq!(hunks.len(), 1);

        let ctx = hunks[0]
            .lines
            .iter()
            .filter(|l| l.kind == LineKind::Context)
            .count();
        assert_eq!(ctx, 4, "two lines either side");
    }

    #[test]
    fn nearby_changes_merge_into_one_hunk() {
        let old = (1..=20).map(|n| format!("line{n}\n")).collect::<String>();
        let new = old.replace("line10", "A").replace("line12", "B");

        // Two lines apart, well inside 2*context.
        let hunks = diff_text(&old, &new, 3);
        assert_eq!(hunks.len(), 1, "merged: {hunks:#?}");
    }

    #[test]
    fn distant_changes_stay_separate() {
        let old = (1..=60).map(|n| format!("line{n}\n")).collect::<String>();
        let new = old.replace("line5", "A").replace("line50", "B");

        let hunks = diff_text(&old, &new, 3);
        assert_eq!(hunks.len(), 2, "not merged: {hunks:#?}");
    }

    #[test]
    fn pure_addition_at_the_end() {
        let hunks = diff_text("a\n", "a\nb\n", CONTEXT);
        assert_eq!(hunks.len(), 1);
        assert!(hunks[0].lines.iter().any(|l| l.kind == LineKind::Added));
        assert!(!hunks[0].lines.iter().any(|l| l.kind == LineKind::Removed));
    }

    #[test]
    fn diffing_against_empty_reports_every_line() {
        let hunks = diff_text("", "a\nb\n", CONTEXT);
        let added: Vec<_> = hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == LineKind::Added)
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(added, vec!["a", "b"]);
    }

    #[test]
    fn deleting_everything_reports_every_line_removed() {
        let hunks = diff_text("a\nb\n", "", CONTEXT);
        let removed: Vec<_> = hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| l.kind == LineKind::Removed)
            .map(|l| l.text.as_str())
            .collect();
        assert_eq!(removed, vec!["a", "b"]);
    }

    #[test]
    fn line_numbers_stay_aligned_across_an_uneven_change() {
        // Two lines replaced by one: numbering must not drift afterwards.
        let hunks = diff_text("a\nb\nc\nd\n", "a\nX\nd\n", CONTEXT);
        let last_context = hunks[0]
            .lines
            .iter()
            .rev()
            .find(|l| l.kind == LineKind::Context)
            .unwrap();
        assert_eq!(last_context.text, "d");
        assert_eq!(last_context.old_no, Some(4), "old side line 4");
        assert_eq!(last_context.new_no, Some(3), "new side line 3");
    }

    #[test]
    fn counts_and_summary_reflect_the_change() {
        let d = file_diff("x.rs", "a\nb\n", "a\nB\nC\n", CONTEXT);
        assert_eq!(d.removed, 1);
        assert_eq!(d.added, 2);
        assert_eq!(d.summary(), "1 hunk(s), +2 -1");
        assert!(!d.is_empty());
    }

    #[test]
    fn an_unchanged_file_summarises_as_no_changes() {
        let d = file_diff("x.rs", "same\n", "same\n", CONTEXT);
        assert!(d.is_empty());
        assert_eq!(d.summary(), "no changes");
    }

    #[test]
    fn display_rows_counts_headers_too() {
        let d = file_diff("x.rs", "a\nb\n", "a\nB\n", CONTEXT);
        let lines: usize = d.hunks.iter().map(|h| h.lines.len()).sum();
        assert_eq!(d.display_rows(), lines + d.hunks.len());
    }

    #[test]
    fn hunk_headers_use_git_s_format() {
        let hunks = diff_text("a\nb\nc\n", "a\nB\nc\n", CONTEXT);
        assert_eq!(hunks[0].header(), "@@ -1,3 +1,3 @@");
    }
}
