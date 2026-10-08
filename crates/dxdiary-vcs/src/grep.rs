//! Repo-wide search that respects `.gitignore`.
//!
//! The walk itself lives in `dxdiary-core`, which must not know what git is.
//! This supplies the one thing it cannot work out for itself: which paths the
//! project ignores. Same split as blame -- the model does the work, this crate
//! answers the git questions.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};

use dxdiary_core::grep::Report;

use crate::GitRepo;

/// Search `root` on a worker thread, skipping whatever git ignores.
///
/// The thread opens its own repository handle, as blame does: `gix::Repository`
/// holds a `RefCell` and so is `Send` but not `Sync`, and sharing one across
/// threads is not worth the trouble when opening another is cheap.
///
/// Falls back to the model's own heuristic when there is no repository here.
pub fn spawn(root: PathBuf, query: String, max_bytes: u64) -> Receiver<Report> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let repo = GitRepo::open(&root).ok();
        let matcher = repo.as_ref().and_then(|r| r.ignores());

        let report = match matcher {
            Some(m) => {
                // The predicate is `Fn` but matching needs `&mut`, because the
                // matcher is a stack that mutates as it descends.
                let cell = RefCell::new(m);
                dxdiary_core::grep::search_with(&root, &query, max_bytes, &|path, is_dir| {
                    cell.borrow_mut().is_ignored(path, is_dir)
                })
            }
            None => dxdiary_core::grep::search(&root, &query, max_bytes),
        };
        // A closed receiver just means the user moved on.
        let _ = tx.send(report);
    });
    rx
}
