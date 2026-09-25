//! Model layer for dxdiary: state and rules, no rendering.
//!
//! Everything here is testable without a terminal, which is most of why the
//! split exists — the view layer is the part that needs a TTY to exercise.

pub mod buffer;
pub mod config;
pub mod document;
pub mod hit;
pub mod tree;

pub use buffer::{Buffer, Cursor};
pub use config::{ColorDepth, Config, Rgb, Theme};
pub use document::{Document, TextDocument};
pub use hit::{Hit, HitMap, HitTarget, PaneId};
pub use tree::{FileTree, Row};
