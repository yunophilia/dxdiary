//! crowsnest — terminal code viewer for reviewing agent work.
//!
//! Phase 1: file tree, content pane, mouse routing. Git, syntax, and LSP land
//! in later phases; see DESIGN.md.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use crowsnest_core::Config;
use crowsnest_tui::App;

const USAGE: &str = "\
crowsnest — terminal code viewer

USAGE:
    crowsnest [PATH]

ARGS:
    PATH    Directory to open. Defaults to the current directory.

OPTIONS:
    -h, --help       Print this help
    -V, --version    Print version
        --caps       Report detected terminal capabilities and exit
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // The terminal is already restored by this point, whether the exit
            // was clean or a panic, so the message is actually visible.
            eprintln!("crowsnest: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Print what crowsnest detects about this terminal.
///
/// Run it locally and again over SSH inside herdr — the answers should match,
/// and where they do not is where rendering will disappoint.
fn report_caps() -> Result<()> {
    let caps = crowsnest_tui::probe()?;
    let env = |k: &str| std::env::var(k).unwrap_or_else(|_| "(unset)".into());

    println!("colour depth       {:?}", caps.color);
    println!(
        "  source           {}",
        match caps.color_source {
            crowsnest_tui::Source::Queried => "XTGETTCAP — the terminal stated it",
            crowsnest_tui::Source::Inferred => "inferred from kitty keyboard support",
            crowsnest_tui::Source::Environment => "guessed from environment variables",
        }
    );
    println!(
        "terminal           {}",
        caps.terminal.as_deref().unwrap_or("(no XTVERSION reply)")
    );
    println!(
        "kitty keyboard     {}",
        if caps.kitty_keyboard {
            "yes — keys disambiguated"
        } else {
            "no — legacy key encoding"
        }
    );
    println!("herdr pane         {}", crowsnest_tui::in_herdr());
    println!();
    println!("TERM               {}", env("TERM"));
    println!("COLORTERM          {}", env("COLORTERM"));
    println!("TERM_PROGRAM       {}", env("TERM_PROGRAM"));
    println!(
        "SSH                {}",
        if std::env::var_os("SSH_CONNECTION").is_some() {
            "yes"
        } else {
            "no"
        }
    );
    Ok(())
}

fn run() -> Result<()> {
    let mut root: Option<PathBuf> = None;

    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("crowsnest {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--caps" => return report_caps(),
            other if other.starts_with('-') => {
                anyhow::bail!("unknown option {other:?}\n\n{USAGE}");
            }
            other => root = Some(PathBuf::from(other)),
        }
    }

    let root = root.unwrap_or_else(|| PathBuf::from("."));
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot open {}", root.display()))?;

    if !root.is_dir() {
        anyhow::bail!("{} is not a directory", root.display());
    }

    let (config, warning) = Config::load();

    // init() queries the terminal, so it has to happen before the app is built
    // — the colour depth is one of its answers.
    let (mut terminal, caps) = crowsnest_tui::init()?;

    let mut app = App::new(root.clone(), config, caps.color);

    // Attach git if this is a repository. Not being one is fine — crowsnest
    // degrades to a plain file browser rather than refusing to open.
    match crowsnest_vcs::GitRepo::open(&root) {
        Ok(repo) => app.attach_vcs(Box::new(repo)),
        Err(_) => app.status = "not a git repository — file browsing only".into(),
    }

    if let Some(w) = warning {
        app.status = format!("config: {w}");
    }

    let result = app.run(&mut terminal);
    crowsnest_tui::restore()?;
    result
}
