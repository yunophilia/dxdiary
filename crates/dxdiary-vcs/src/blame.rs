//! Per-line blame.
//!
//! Spike 0.3 measured this at 543 ms for a 1,931-line file and **2.5 s** for a
//! 7,858-line one, and unlike merge-base a commit-graph does not help. So blame
//! never runs on the render thread: [`spawn`] moves it to its own thread with
//! its own repository handle, and the UI polls for the result.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};

use anyhow::{Context, Result};
use gix::bstr::BStr;

use crate::git::GitRepo;

/// Who last touched one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameLine {
    /// 0-based line in the blamed file.
    pub line: usize,
    /// Abbreviated commit id.
    pub commit: String,
    pub author: String,
    /// Relative age, e.g. `3 days ago`.
    pub when: String,
    /// First line of the commit message.
    pub summary: String,
}

/// Blame for a whole file, indexed by line.
#[derive(Debug, Clone, Default)]
pub struct Blame {
    pub path: PathBuf,
    pub lines: Vec<BlameLine>,
}

impl Blame {
    pub fn get(&self, line: usize) -> Option<&BlameLine> {
        self.lines.get(line)
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

/// Run blame on a background thread.
///
/// Takes a repository *path* rather than a handle: `gix::Repository` is `Send`
/// but not `Sync`, so the worker opens its own.
pub fn spawn(repo_root: PathBuf, file: PathBuf) -> Receiver<Result<Blame>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = GitRepo::open(&repo_root).and_then(|repo| repo.blame(&file));
        // A closed receiver just means the user moved on; nothing to report.
        let _ = tx.send(result);
    });
    rx
}

impl GitRepo {
    /// Blame every line of `path` at HEAD.
    pub fn blame(&self, path: &Path) -> Result<Blame> {
        let rela = self.relative_path(path);
        let head = self.repo().head_id().context("HEAD has no commit")?;

        let spec = rela.to_string_lossy().replace('\\', "/");
        let mut cache = self.repo().diff_resource_cache_for_tree_diff()?;

        let outcome = gix::blame::file(
            &self.repo().objects,
            head.detach(),
            None,
            &mut cache,
            BStr::new(spec.as_bytes()),
            gix::blame::Options::default(),
        )
        .with_context(|| format!("blaming {spec}"))?;

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);

        // Entries cover ranges, so commit metadata is looked up once per hunk
        // rather than once per line.
        let mut lines: Vec<Option<BlameLine>> = Vec::new();
        for entry in outcome.entries {
            let info = self.commit_info(entry.commit_id, now);
            for offset in 0..entry.len.get() {
                let line = (entry.start_in_blamed_file + offset) as usize;
                if lines.len() <= line {
                    lines.resize(line + 1, None);
                }
                lines[line] = Some(BlameLine {
                    line,
                    ..info.clone()
                });
            }
        }

        Ok(Blame {
            path: rela,
            // Gaps would mean a line no entry covered; fill rather than shift
            // everything after it, which would misattribute the whole file.
            lines: lines
                .into_iter()
                .enumerate()
                .map(|(i, l)| l.unwrap_or_else(|| unknown(i)))
                .collect(),
        })
    }

    /// Author, date, and summary for a commit, degrading to placeholders
    /// rather than failing the whole blame over one unreadable object.
    fn commit_info(&self, id: gix::ObjectId, now: i64) -> BlameLine {
        let short = id.to_string()[..8.min(id.to_string().len())].to_string();

        // Two different error types, so this cannot be one `and_then`.
        let Ok(commit) = self
            .repo()
            .find_object(id)
            .ok()
            .and_then(|o| o.try_into_commit().ok())
            .ok_or(())
        else {
            return BlameLine {
                line: 0,
                commit: short,
                author: "?".into(),
                when: String::new(),
                summary: String::new(),
            };
        };

        let author = commit
            .author()
            .map(|a| a.name.to_string())
            .unwrap_or_else(|_| "?".into());
        let seconds = commit.time().map(|t| t.seconds).unwrap_or(0);
        let summary = commit
            .message()
            .map(|m| m.summary().to_string())
            .unwrap_or_default();

        BlameLine {
            line: 0,
            commit: short,
            author,
            when: relative_time(now - seconds),
            summary,
        }
    }
}

fn unknown(line: usize) -> BlameLine {
    BlameLine {
        line,
        commit: "········".into(),
        author: "?".into(),
        when: String::new(),
        summary: "not committed".into(),
    }
}

/// Human-readable age, in the coarse units a blame gutter has room for.
pub fn relative_time(seconds_ago: i64) -> String {
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;

    let s = seconds_ago.max(0);
    let (n, unit) = match s {
        s if s < MINUTE => return "just now".into(),
        s if s < HOUR => (s / MINUTE, "minute"),
        s if s < DAY => (s / HOUR, "hour"),
        s if s < MONTH => (s / DAY, "day"),
        s if s < YEAR => (s / MONTH, "month"),
        s => (s / YEAR, "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times_read_naturally() {
        assert_eq!(relative_time(5), "just now");
        assert_eq!(relative_time(60), "1 minute ago");
        assert_eq!(relative_time(3 * 60), "3 minutes ago");
        assert_eq!(relative_time(3600), "1 hour ago");
        assert_eq!(relative_time(26 * 3600), "1 day ago");
        assert_eq!(relative_time(40 * 24 * 3600), "1 month ago");
        assert_eq!(relative_time(400 * 24 * 3600), "1 year ago");
    }

    #[test]
    fn a_clock_skewed_future_commit_does_not_underflow() {
        assert_eq!(relative_time(-500), "just now");
    }

    #[test]
    fn an_empty_blame_reports_empty() {
        assert!(Blame::default().is_empty());
        assert!(Blame::default().get(0).is_none());
    }
}
