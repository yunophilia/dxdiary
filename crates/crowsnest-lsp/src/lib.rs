//! Language Server Protocol client.
//!
//! Read-only by design for now: `initialize`, `didOpen`, `hover`, `definition`,
//! `documentSymbol`, and `publishDiagnostics`. No `didChange`, because nothing
//! mutates a buffer yet — which removes incremental sync, the fiddliest part of
//! an LSP client, until editing actually needs it.
//!
//! No async runtime. A reader thread feeds a channel and the UI polls, the same
//! shape blame uses.

pub mod client;
pub mod filter;
pub mod protocol;
pub mod registry;

pub use client::{path_to_uri, Client, Event};
pub use filter::{filter, Filtered};
pub use registry::{doctor, report, spec_for, ServerSpec, Status, SERVERS};
