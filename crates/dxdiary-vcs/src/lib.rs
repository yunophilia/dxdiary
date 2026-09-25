//! Version control for dxdiary.
//!
//! Everything above this crate talks to the [`Vcs`] trait, never to gix
//! directly — gix moves faster than the rest of the stack, and confining it to
//! one module keeps a backend swap to a single crate.

pub mod blame;
pub mod diff;
pub mod git;
pub mod types;

pub use blame::{Blame, BlameLine};
pub use diff::{DiffLine, FileDiff, Hunk, LineKind, CONTEXT};
pub use git::GitRepo;
pub use types::{
    is_under, BaseSource, ChangeKind, DiffBaseline, FileChange, ForkPoint, RepoInfo, Status, Vcs,
    TRUNK_CANDIDATES,
};
