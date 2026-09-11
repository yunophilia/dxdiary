//! Render one frame to stdout and exit. No TTY required.
//!
//! Useful for eyeballing layout changes, for CI smoke tests, and for attaching
//! to a bug report without a screen capture.
//!
//!     cargo run -p crowsnest-tui --example screenshot -- [PATH] [WIDTHxHEIGHT]

use crowsnest_core::{ColorDepth, Config};
use crowsnest_tui::App;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let root = args.next().unwrap_or_else(|| ".".into());
    let size = args.next().unwrap_or_else(|| "100x30".into());

    let (w, h) = size
        .split_once('x')
        .and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
        .unwrap_or((100u16, 30u16));

    let root = std::path::PathBuf::from(root).canonicalize()?;
    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);

    if let Ok(repo) = crowsnest_vcs::GitRepo::open(&root) {
        app.attach_vcs(Box::new(repo));
    }

    // Open something so the content pane is not empty. Prefer a changed file,
    // since that is what a screenshot of a review tool should show.
    let target = args.next().map(|p| root.join(p)).or_else(|| {
        app.badges
            .keys()
            .next()
            .map(|rela| root.join(rela))
            .or_else(|| {
                app.tree
                    .rows()
                    .iter()
                    .find(|r| !r.is_dir)
                    .map(|r| r.path.clone())
            })
    });

    if let Some(path) = target {
        app.reveal_and_open(path);
    }

    // `CROWSNEST_SHOT=blame` renders the blame gutter, waiting for the
    // background worker the interactive loop would otherwise poll for.
    if std::env::var("CROWSNEST_SHOT").as_deref() == Ok("blame") {
        app.handle(crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('a'),
                crossterm::event::KeyModifiers::NONE,
            ),
        ));
        app.await_blame(std::time::Duration::from_secs(30));
    }

    // `CROWSNEST_SHOT=lsp` waits for a language server to publish. Real servers
    // index for seconds before saying anything, so a batch render needs to ask.
    if std::env::var("CROWSNEST_SHOT").as_deref() == Ok("lsp") {
        let got = app.await_diagnostics(std::time::Duration::from_secs(90));
        eprintln!("diagnostics arrived: {got}");
    }

    // `CROWSNEST_SHOT=lsp-edit` goes one step further: once the server has
    // reported on the file as it is on disk, replace its second line with
    // `CROWSNEST_LINE` and wait for the server to reconsider. Against a real
    // rust-analyzer this proves didChange end to end -- a type error that the
    // edit fixes must take its marker with it.
    if std::env::var("CROWSNEST_SHOT").as_deref() == Ok("lsp-edit") {
        let got = app.await_diagnostics(std::time::Duration::from_secs(90));
        eprintln!("diagnostics before edit: {got} ({})", app.diagnostics.len());
        let before = app.diagnostics.len();

        let line = std::env::var("CROWSNEST_LINE").unwrap_or_default();
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let press = |app: &mut crowsnest_tui::App, code| {
            app.handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)));
        };
        // Focus starts on the tree, where Down moves the selection.
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('D'));
        press(&mut app, KeyCode::Char('i'));
        for ch in line.chars() {
            press(&mut app, KeyCode::Char(ch));
        }
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);

        // Poll until the server publishes for the edited version, i.e. the
        // count changes, or give up.
        let settle = |app: &mut crowsnest_tui::App, before: usize, what: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while std::time::Instant::now() < deadline {
                app.poll_lsp();
                if app.diagnostics.len() != before {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            eprintln!("diagnostics after {what}: {}", app.diagnostics.len());
            for d in &app.diagnostics {
                eprintln!(
                    "  line {} [{}] {}",
                    d.range.start.line,
                    d.source.as_deref().unwrap_or("?"),
                    d.message.lines().next().unwrap_or("")
                );
            }
        };
        settle(&mut app, before, "edit");

        // `CROWSNEST_SAVE=1` also writes the file. rust-analyzer runs `cargo
        // check` on didSave, not on didChange, so the `[rustc]` diagnostics
        // only move here. This modifies the file given: use a scratch copy.
        if std::env::var("CROWSNEST_SAVE").as_deref() == Ok("1") {
            let before = app.diagnostics.len();
            app.handle(Event::Key(KeyEvent::new(
                KeyCode::Char('s'),
                KeyModifiers::CONTROL,
            )));
            eprintln!("{}", app.status);
            settle(&mut app, before, "save");
        }
    }

    let mut term = Terminal::new(TestBackend::new(w, h))?;
    term.draw(|f| app.render(f))?;

    let buf = term.backend().buffer();
    for y in 0..buf.area.height {
        let mut line = String::new();
        for x in 0..buf.area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        println!("{}", line.trim_end());
    }
    Ok(())
}
