//! Language Server Protocol client.
//!
//! Speaks `initialize`, `didOpen`/`didChange`/`didSave`/`didClose`, `hover`,
//! `definition`, `documentSymbol`, and `publishDiagnostics`. Document sync is
//! always the whole text, never a range: on a pipe to a local process the
//! bandwidth is nothing, and an edit log that must exactly mirror the buffer is
//! the fiddliest part of an LSP client, with bugs that show up as diagnostics
//! on the wrong line.
//!
//! No async runtime. A reader thread feeds a channel and the UI polls, the same
//! shape blame uses.

pub mod client;
pub mod filter;
pub mod protocol;
pub mod registry;

pub use client::{path_to_uri, uri_to_path, Client, Event};
pub use filter::{filter, Filtered};
pub use registry::{doctor, report, spec_for, ServerSpec, Status, SERVERS};
