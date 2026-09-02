//! Syntax highlighting, and the structural facts crowsnest needs from a parse.
//!
//! tree-sitter rather than a regex grammar for two reasons: it is incremental,
//! and — per DESIGN.md §2 — it is the *authority* for C, because clangd cannot
//! parse GCC nested functions at all. [`nested_functions`] is the detector that
//! makes that workaround possible.

pub mod highlight;
pub mod language;
pub mod nested;

pub use highlight::{Highlighter, Role, Span};
pub use language::Language;
pub use nested::{nested_functions, NestedFn};
