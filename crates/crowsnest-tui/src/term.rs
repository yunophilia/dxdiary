//! Terminal lifecycle and capability detection.

use std::io::{self, IsTerminal, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::{execute, terminal};
use crowsnest_core::ColorDepth;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::query::{self, Probe};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// How long to wait for the terminal to answer capability queries. Generous
/// enough for a slow SSH link, short enough not to be felt at startup — and
/// normally irrelevant, since DA1 ends the read as soon as replies arrive.
const QUERY_TIMEOUT: Duration = Duration::from_millis(250);

/// Whether the kitty keyboard flags were actually pushed. `restore` runs from
/// the panic hook too, so this cannot live in a value the caller owns.
static KITTY_PUSHED: AtomicBool = AtomicBool::new(false);

/// Where a capability answer came from. Reported by `--caps`, because "we
/// guessed" and "the terminal told us" deserve different levels of trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// XTGETTCAP — the terminal stated it.
    Queried,
    /// Inferred from the kitty keyboard protocol being supported.
    Inferred,
    /// Guessed from environment variables.
    Environment,
}

/// What the terminal turned out to support.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caps {
    pub color: ColorDepth,
    pub color_source: Source,
    /// Kitty keyboard protocol — disambiguated keys, no Ctrl+I/Tab collision.
    pub kitty_keyboard: bool,
    /// Terminal name and version from XTVERSION, when it answered.
    pub terminal: Option<String>,
}

/// Enter raw mode and the alternate screen, with mouse reporting on.
///
/// Installs a panic hook first. A panic in raw mode otherwise leaves the
/// terminal unusable and the message invisible — the user is left with a dead
/// shell and no idea why.
pub fn init() -> Result<(Tui, Caps)> {
    install_panic_hook();
    terminal::enable_raw_mode()?;

    // Query before the alternate screen: the replies are read off stdin, and a
    // terminal that ignores a query must not leave stray bytes on the drawing
    // surface.
    let caps = probe_caps();

    let mut stdout = io::stdout();
    // crossterm requests SGR (1006) mouse encoding, which is required for
    // coordinates past column 223 — see DESIGN.md §1.
    execute!(stdout, terminal::EnterAlternateScreen, EnableMouseCapture)?;

    if caps.kitty_keyboard {
        // Only disambiguation. Key-release and repeat reporting would double
        // the event volume for no benefit here, and every extra byte costs a
        // round trip over SSH.
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        KITTY_PUSHED.store(true, Ordering::SeqCst);
    }

    Ok((Terminal::new(CrosstermBackend::new(stdout))?, caps))
}

pub fn restore() -> Result<()> {
    let mut stdout = io::stdout();
    // Pop before leaving the alternate screen, mirroring the push order.
    // swap() makes this idempotent: restore runs from both the normal exit
    // path and the panic hook, and popping twice would unbalance the
    // terminal's own stack.
    if KITTY_PUSHED.swap(false, Ordering::SeqCst) {
        let _ = execute!(stdout, PopKeyboardEnhancementFlags);
    }
    terminal::disable_raw_mode()?;
    execute!(stdout, terminal::LeaveAlternateScreen, DisableMouseCapture)?;
    Ok(())
}

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Best effort: if restoring fails there is nothing useful left to do,
        // and the panic message still matters more.
        let _ = restore();
        previous(info);
    }));
}

/// Ask the terminal what it supports. Requires raw mode.
fn probe_caps() -> Caps {
    // Piped or redirected there is nobody to answer, and the query bytes would
    // leak into stdout as `[?u[c`, corrupting `--caps > report.txt`.
    if !io::stdout().is_terminal() {
        return from_env();
    }

    let probe = query::probe_terminal(QUERY_TIMEOUT);
    let kitty = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    resolve(probe, kitty)
}

/// Same as `probe_caps`, but manages raw mode itself and leaves the screen
/// alone. Backs `crowsnest --caps`.
///
/// Worth having as a first-class command: capability detection is exactly what
/// breaks over SSH or inside a multiplexer, and "what does crowsnest think this
/// terminal is" should be answerable without launching the whole UI.
pub fn probe() -> Result<Caps> {
    if !io::stdout().is_terminal() {
        return Ok(from_env());
    }
    terminal::enable_raw_mode()?;
    let caps = probe_caps();
    terminal::disable_raw_mode()?;
    Ok(caps)
}

/// Combine the query results, most trustworthy signal first.
pub fn resolve(probe: Probe, kitty_keyboard: bool) -> Caps {
    // 1. XTGETTCAP is the terminal stating its own capability. Measured
    //    working on WezTerm, which answers `RGB=8/8/8` (spike 05).
    if probe.responded && probe.truecolor {
        return Caps {
            color: ColorDepth::TrueColor,
            color_source: Source::Queried,
            kitty_keyboard,
            terminal: probe.version,
        };
    }

    // 2. Kitty keyboard support implies a modern truecolor terminal. A weaker
    //    signal than it first appears -- WezTerm does not answer this query at
    //    all -- but when it does fire it is reliable.
    if kitty_keyboard {
        return Caps {
            color: ColorDepth::TrueColor,
            color_source: Source::Inferred,
            kitty_keyboard: true,
            terminal: probe.version,
        };
    }

    // 3. Guesswork. Reached when the terminal ignored both queries — a bare
    //    `TERM=xterm` over SSH, or something inside a multiplexer that
    //    swallowed them.
    Caps {
        color: detect_color_depth_from_env(),
        color_source: Source::Environment,
        kitty_keyboard,
        terminal: probe.version,
    }
}

fn from_env() -> Caps {
    Caps {
        color: detect_color_depth_from_env(),
        color_source: Source::Environment,
        kitty_keyboard: false,
        terminal: None,
    }
}

/// Environment-based fallback for terminals that did not answer.
///
/// Users can force the result with `COLORTERM=truecolor`.
pub fn detect_color_depth_from_env() -> ColorDepth {
    let var = |k: &str| std::env::var(k).unwrap_or_default().to_lowercase();

    let colorterm = var("COLORTERM");
    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        return ColorDepth::TrueColor;
    }

    let program = var("TERM_PROGRAM");
    if matches!(
        program.as_str(),
        "wezterm" | "iterm.app" | "vscode" | "ghostty"
    ) {
        return ColorDepth::TrueColor;
    }
    if std::env::var_os("WEZTERM_EXECUTABLE").is_some()
        || std::env::var_os("KITTY_WINDOW_ID").is_some()
    {
        return ColorDepth::TrueColor;
    }

    let term = var("TERM");
    if term.contains("truecolor") || term.contains("direct") {
        return ColorDepth::TrueColor;
    }
    if term.contains("256color") {
        return ColorDepth::Indexed256;
    }
    if term.is_empty() || term == "dumb" {
        return ColorDepth::Ansi16;
    }
    ColorDepth::Indexed256
}

/// Whether crowsnest is running as a herdr plugin pane. Used in Phase 6 to bind
/// to the pane's worktree instead of guessing from the working directory.
pub fn in_herdr() -> bool {
    std::env::var_os("HERDR_ENV").is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wezterm_probe() -> Probe {
        Probe {
            truecolor: true,
            version: Some("WezTerm 20240203-110809-5046fc22".into()),
            responded: true,
        }
    }

    #[test]
    fn a_queried_answer_beats_everything_else() {
        // WezTerm's real behaviour: answers XTGETTCAP, ignores kitty keyboard.
        let caps = resolve(wezterm_probe(), false);
        assert_eq!(caps.color, ColorDepth::TrueColor);
        assert_eq!(caps.color_source, Source::Queried);
        assert!(!caps.kitty_keyboard);
        assert!(caps.terminal.unwrap().starts_with("WezTerm"));
    }

    #[test]
    fn kitty_support_still_implies_truecolor_when_xtgettcap_is_silent() {
        let caps = resolve(Probe::default(), true);
        assert_eq!(caps.color, ColorDepth::TrueColor);
        assert_eq!(caps.color_source, Source::Inferred);
    }

    #[test]
    fn a_terminal_that_denies_rgb_falls_through_to_the_environment() {
        let denied = Probe {
            truecolor: false,
            version: None,
            responded: true,
        };
        assert_eq!(resolve(denied, false).color_source, Source::Environment);
    }

    #[test]
    fn silence_falls_through_to_the_environment() {
        assert_eq!(
            resolve(Probe::default(), false).color_source,
            Source::Environment
        );
    }

    // detect_color_depth_from_env reads process-global state, so these share
    // one test rather than racing each other across threads.
    #[test]
    fn env_fallback_ranks_colorterm_above_term() {
        let keys = [
            "COLORTERM",
            "TERM",
            "TERM_PROGRAM",
            "WT_SESSION",
            "WEZTERM_EXECUTABLE",
            "KITTY_WINDOW_ID",
        ];
        let saved: Vec<_> = keys.iter().map(|k| (*k, std::env::var_os(k))).collect();
        let clear = || {
            for k in keys {
                std::env::remove_var(k);
            }
        };

        clear();
        std::env::set_var("TERM", "xterm-256color");
        assert_eq!(detect_color_depth_from_env(), ColorDepth::Indexed256);

        std::env::set_var("COLORTERM", "truecolor");
        assert_eq!(detect_color_depth_from_env(), ColorDepth::TrueColor);

        clear();
        std::env::set_var("TERM", "xterm-256color");
        std::env::set_var("TERM_PROGRAM", "WezTerm");
        assert_eq!(
            detect_color_depth_from_env(),
            ColorDepth::TrueColor,
            "TERM_PROGRAM match is case-insensitive"
        );

        clear();
        std::env::set_var("TERM", "dumb");
        assert_eq!(detect_color_depth_from_env(), ColorDepth::Ansi16);

        clear();
        for (k, v) in saved {
            if let Some(v) = v {
                std::env::set_var(k, v);
            }
        }
    }
}
