//! Running as a herdr plugin pane.
//!
//! The point of DESIGN.md §7: firstmate dispatches each crewmate into its own
//! git worktree, and herdr tells a plugin pane which worktree it belongs to via
//! `HERDR_PLUGIN_CONTEXT_JSON`. Reading that is what makes crowsnest follow the
//! fleet instead of needing to be navigated by hand.
//!
//! Everything here degrades to `None` outside herdr, so the same binary runs
//! standalone with no branching at the call site.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// What herdr injected into this process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Context {
    /// `HERDR_ENV=1` is set for anything herdr spawns.
    pub in_herdr: bool,
    /// The worktree this pane is attached to — the firstmate crewmate's.
    pub worktree: Option<PathBuf>,
    /// Agent id, when the pane is bound to one.
    pub agent: Option<String>,
    pub pane_id: Option<String>,
    pub plugin_id: Option<String>,
    /// Where a plugin may persist state between sessions.
    pub state_dir: Option<PathBuf>,
}

impl Context {
    pub fn from_env() -> Self {
        Self::from_parts(
            std::env::var_os("HERDR_ENV").is_some(),
            std::env::var("HERDR_PLUGIN_CONTEXT_JSON").ok().as_deref(),
            std::env::var_os("HERDR_PANE_ID").map(|v| v.to_string_lossy().into_owned()),
            std::env::var_os("HERDR_PLUGIN_ID").map(|v| v.to_string_lossy().into_owned()),
            std::env::var_os("HERDR_PLUGIN_STATE_DIR").map(PathBuf::from),
        )
    }

    /// Split out so the JSON parsing is testable without touching the
    /// environment, which is process-global and hostile to parallel tests.
    pub fn from_parts(
        in_herdr: bool,
        context_json: Option<&str>,
        pane_id: Option<String>,
        plugin_id: Option<String>,
        state_dir: Option<PathBuf>,
    ) -> Self {
        let parsed: Option<serde_json::Value> =
            context_json.and_then(|s| serde_json::from_str(s).ok());

        // herdr has spelled the worktree several ways across versions, and a
        // pane that silently fails to bind is worse than one that tries a
        // couple of likely keys.
        let worktree = parsed.as_ref().and_then(|v| {
            ["worktree", "worktree_path", "cwd"]
                .iter()
                .find_map(|key| match v.get(key) {
                    Some(serde_json::Value::String(s)) => Some(PathBuf::from(s)),
                    // Or an object with a `path`.
                    Some(serde_json::Value::Object(o)) => {
                        o.get("path").and_then(|p| p.as_str()).map(PathBuf::from)
                    }
                    _ => None,
                })
        });

        let agent = parsed.as_ref().and_then(|v| match v.get("agent") {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(serde_json::Value::Object(o)) => o
                .get("id")
                .or_else(|| o.get("name"))
                .and_then(|x| x.as_str())
                .map(str::to_string),
            _ => None,
        });

        Context {
            in_herdr,
            worktree,
            agent,
            pane_id,
            plugin_id,
            state_dir,
        }
    }

    /// The directory crowsnest should open.
    ///
    /// herdr's worktree wins over the process working directory: a plugin pane
    /// inherits herdr's cwd, not the crewmate's, so trusting cwd would show the
    /// wrong tree entirely.
    pub fn root_or(&self, fallback: PathBuf) -> PathBuf {
        self.worktree.clone().unwrap_or(fallback)
    }

    /// A short label for the status bar.
    pub fn label(&self) -> Option<String> {
        if !self.in_herdr {
            return None;
        }
        Some(match (&self.agent, &self.pane_id) {
            (Some(a), _) => format!("herdr:{a}"),
            (None, Some(p)) => format!("herdr:{p}"),
            _ => "herdr".to_string(),
        })
    }
}

/// Per-worktree view state, so switching between crewmates returns you to
/// where you were rather than to the top of the tree.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneState {
    /// Repository-relative path of the open file.
    pub file: Option<String>,
    pub line: usize,
    pub scroll: usize,
    /// Serialised `DiffBaseline` discriminant, kept as a string so this crate
    /// does not depend on the vcs layer.
    pub baseline: Option<String>,
    pub changed_only: bool,
    pub show_blame: bool,
}

/// State file for one worktree.
///
/// Named after a hash of the worktree path: two crewmates can have the same
/// directory *name* under different parents, and colliding their state would
/// send you to the wrong file.
pub fn state_path(state_dir: &std::path::Path, worktree: &std::path::Path) -> PathBuf {
    let key = worktree.to_string_lossy();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in key.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    state_dir.join(format!("pane-{hash:016x}.json"))
}

pub fn load_state(state_dir: &std::path::Path, worktree: &std::path::Path) -> Option<PaneState> {
    let text = std::fs::read_to_string(state_path(state_dir, worktree)).ok()?;
    serde_json::from_str(&text).ok()
}

/// Persist state, best effort.
///
/// A failure here means the next session starts at the top of the tree, which
/// is not worth interrupting anyone over.
pub fn save_state(state_dir: &std::path::Path, worktree: &std::path::Path, state: &PaneState) {
    let _ = std::fs::create_dir_all(state_dir);
    if let Ok(text) = serde_json::to_string_pretty(state) {
        let _ = std::fs::write(state_path(state_dir, worktree), text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outside_herdr_everything_is_absent() {
        let ctx = Context::from_parts(false, None, None, None, None);
        assert!(!ctx.in_herdr);
        assert!(ctx.worktree.is_none());
        assert!(ctx.label().is_none());
    }

    #[test]
    fn the_worktree_is_read_from_the_context_json() {
        let ctx = Context::from_parts(
            true,
            Some(r#"{"worktree": "/tmp/crew/feature-a", "agent": "crewmate-3"}"#),
            Some("w1:p2".into()),
            Some("crowsnest".into()),
            None,
        );
        assert_eq!(ctx.worktree, Some(PathBuf::from("/tmp/crew/feature-a")));
        assert_eq!(ctx.agent.as_deref(), Some("crewmate-3"));
        assert_eq!(ctx.label().as_deref(), Some("herdr:crewmate-3"));
    }

    #[test]
    fn the_worktree_may_be_an_object_with_a_path() {
        let ctx = Context::from_parts(
            true,
            Some(r#"{"worktree": {"path": "/tmp/crew/b", "branch": "feat"}}"#),
            None,
            None,
            None,
        );
        assert_eq!(ctx.worktree, Some(PathBuf::from("/tmp/crew/b")));
    }

    #[test]
    fn an_agent_object_yields_its_id() {
        let ctx = Context::from_parts(
            true,
            Some(r#"{"agent": {"id": "a7", "name": "parser"}}"#),
            None,
            None,
            None,
        );
        assert_eq!(ctx.agent.as_deref(), Some("a7"));
    }

    #[test]
    fn malformed_context_json_does_not_break_startup() {
        // herdr changing its payload must degrade to "no binding", not a crash.
        let ctx = Context::from_parts(true, Some("{not json"), None, None, None);
        assert!(ctx.in_herdr, "still inside herdr");
        assert!(ctx.worktree.is_none());
    }

    #[test]
    fn the_worktree_overrides_the_working_directory() {
        let ctx = Context::from_parts(
            true,
            Some(r#"{"worktree": "/tmp/crew/x"}"#),
            None,
            None,
            None,
        );
        assert_eq!(
            ctx.root_or(PathBuf::from("/somewhere/else")),
            PathBuf::from("/tmp/crew/x"),
            "a plugin pane inherits herdr's cwd, not the crewmate's"
        );
    }

    #[test]
    fn without_a_worktree_the_fallback_is_used() {
        let ctx = Context::from_parts(true, None, None, None, None);
        assert_eq!(ctx.root_or(PathBuf::from("/here")), PathBuf::from("/here"));
    }

    #[test]
    fn the_label_falls_back_to_the_pane_id() {
        let ctx = Context::from_parts(true, Some("{}"), Some("w1:p9".into()), None, None);
        assert_eq!(ctx.label().as_deref(), Some("herdr:w1:p9"));
    }

    #[test]
    fn state_round_trips_per_worktree() {
        let dir = std::env::temp_dir().join("crowsnest-herdr-state");
        let _ = std::fs::remove_dir_all(&dir);

        let a = std::path::Path::new("/tmp/crew/alpha");
        let b = std::path::Path::new("/tmp/crew/beta");

        let state_a = PaneState {
            file: Some("src/main.rs".into()),
            line: 42,
            scroll: 30,
            baseline: Some("ForkPoint".into()),
            changed_only: true,
            show_blame: false,
        };
        save_state(&dir, a, &state_a);
        assert_eq!(load_state(&dir, a), Some(state_a));

        // A different worktree must not see it.
        assert_eq!(load_state(&dir, b), None);
    }

    #[test]
    fn worktrees_with_the_same_directory_name_do_not_collide() {
        // Two crewmates both working on "feature", under different parents.
        let one = std::path::Path::new("/tmp/crew-1/feature");
        let two = std::path::Path::new("/tmp/crew-2/feature");
        let dir = std::path::Path::new("/tmp/state");

        assert_ne!(
            state_path(dir, one),
            state_path(dir, two),
            "state is keyed on the full path, not the basename"
        );
    }

    #[test]
    fn missing_state_is_not_an_error() {
        let dir = std::env::temp_dir().join("crowsnest-herdr-absent");
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(load_state(&dir, std::path::Path::new("/nope")), None);
    }
}
