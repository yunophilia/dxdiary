//! Terminal UI for crowsnest.

pub mod app;
pub mod panes;
pub mod query;
pub mod term;

pub use app::{App, ContentView, Mode};
pub use query::Probe;
pub use term::{
    detect_color_depth_from_env, in_herdr, init, probe, resolve, restore, Caps, Source, Tui,
};
