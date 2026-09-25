//! dxdiary — terminal code viewer for reviewing agent work.
//!
//! Phase 1: file tree, content pane, mouse routing. Git, syntax, and LSP land
//! in later phases; see DESIGN.md.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};
use dxdiary_core::Config;
use dxdiary_tui::App;

const USAGE: &str = "\
dxdiary — terminal code viewer

USAGE:
    dxdiary [PATH]

ARGS:
    PATH    Directory to open. Defaults to the current directory.

OPTIONS:
    -h, --help       Print this help
    -V, --version    Print version
        --caps       Report detected terminal capabilities and exit
        --doctor     Report which language servers are installed and exit
        --herdr-attach
                     Open the worktree herdr passed in HERDR_PLUGIN_CONTEXT_JSON
";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // The terminal is already restored by this point, whether the exit
            // was clean or a panic, so the message is actually visible.
            eprintln!("dxdiary: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// Print what dxdiary detects about this terminal.
///
/// Run it locally and again over SSH inside herdr — the answers should match,
/// and where they do not is where rendering will disappoint.
fn report_caps() -> Result<()> {
    let caps = dxdiary_tui::probe()?;
    let env = |k: &str| std::env::var(k).unwrap_or_else(|_| "(unset)".into());

    println!("colour depth       {:?}", caps.color);
    println!(
        "  source           {}",
        match caps.color_source {
            dxdiary_tui::Source::Queried => "XTGETTCAP — the terminal stated it",
            dxdiary_tui::Source::Inferred => "inferred from kitty keyboard support",
            dxdiary_tui::Source::Environment => "guessed from environment variables",
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
    println!("herdr pane         {}", dxdiary_tui::in_herdr());
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

/// Report which language servers are installed.
///
/// dxdiary ships configuration, not binaries: vendoring clangd,
/// rust-analyzer, and gopls would be hundreds of megabytes and a licensing
/// tangle. This says what is missing and how to get it.
fn report_doctor() -> Result<()> {
    let statuses = dxdiary_lsp::doctor();
    print!("{}", dxdiary_lsp::report(&statuses));

    let missing = statuses.iter().filter(|s| !s.available()).count();
    println!();
    if missing == 0 {
        println!("all {} language servers available", statuses.len());
    } else {
        println!(
            "{missing} of {} missing — those languages fall back to tree-sitter \
             (highlighting and outline still work)",
            statuses.len()
        );
    }
    Ok(())
}

fn run() -> Result<()> {
    let mut root: Option<String> = None;

    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return Ok(());
            }
            "-V" | "--version" => {
                println!("dxdiary {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--caps" => return report_caps(),
            "--doctor" => return report_doctor(),
            // Accepted for the `worktree.created` hook in dxdiary-plugin.toml.
            // The worktree always comes from the context JSON, so the flag only
            // needs to not be an error.
            "--herdr-attach" => {}
            other if other.starts_with('-') => {
                anyhow::bail!("unknown option {other:?}\n\n{USAGE}");
            }
            other => root = Some(other.to_string()),
        }
    }

    // Inside a herdr pane the worktree comes from herdr, not from the working
    // directory: a plugin pane inherits herdr's cwd, not the crewmate's, so
    // trusting cwd would show the wrong tree entirely.
    let herdr = dxdiary_herdr::Context::from_env();
    let root = root
        .map(PathBuf::from)
        .unwrap_or_else(|| herdr.root_or(PathBuf::from(".")));
    let root = root
        .canonicalize()
        .with_context(|| format!("cannot open {}", root.display()))?;

    if !root.is_dir() {
        anyhow::bail!("{} is not a directory", root.display());
    }

    let (config, warning) = Config::load();

    // init() queries the terminal, so it has to happen before the app is built
    // — the colour depth is one of its answers.
    let (mut terminal, caps) = dxdiary_tui::init()?;

    let mut app = App::new(root.clone(), config, caps.color);

    // Attach git if this is a repository. Not being one is fine — dxdiary
    // degrades to a plain file browser rather than refusing to open.
    match dxdiary_vcs::GitRepo::open(&root) {
        Ok(repo) => app.attach_vcs(Box::new(repo)),
        Err(_) => app.status = "not a git repository — file browsing only".into(),
    }

    // Restore where this worktree was left. firstmate keeps each crewmate in
    // its own worktree, so switching between them should return to the file you
    // were reading rather than the top of the tree.
    if let (Some(dir), Some(tree)) = (&herdr.state_dir, &herdr.worktree) {
        if let Some(state) = dxdiary_herdr::load_state(dir, tree) {
            apply_state(&mut app, &root, &state);
        }
    }
    if let Some(label) = herdr.label() {
        app.status = format!("{label} · {}", app.status);
    }

    if let Some(w) = warning {
        app.status = format!("config: {w}");
    }

    let result = app.run(&mut terminal);
    dxdiary_tui::restore()?;

    if let (Some(dir), Some(tree)) = (&herdr.state_dir, &herdr.worktree) {
        dxdiary_herdr::save_state(dir, tree, &capture_state(&app, &root));
    }
    result
}

/// Reopen what a previous session in this worktree was looking at.
fn apply_state(app: &mut App, root: &std::path::Path, state: &dxdiary_herdr::PaneState) {
    app.changed_only = state.changed_only;
    if let Some(rela) = &state.file {
        let path = root.join(rela);
        if path.is_file() {
            app.reveal_and_open(path);
            app.doc_line = state.line.min(app.content_rows().saturating_sub(1));
            app.doc_scroll_y = state.scroll;
        }
    }
}

/// Snapshot for the next session.
fn capture_state(app: &App, root: &std::path::Path) -> dxdiary_herdr::PaneState {
    dxdiary_herdr::PaneState {
        file: app.doc.as_ref().and_then(|d| {
            d.path()
                .strip_prefix(root)
                .ok()
                .map(|p| p.to_string_lossy().replace(char::from(92), "/"))
        }),
        line: app.doc_line,
        scroll: app.doc_scroll_y,
        baseline: Some(format!("{:?}", app.baseline)),
        changed_only: app.changed_only,
        show_blame: app.show_blame,
    }
}
