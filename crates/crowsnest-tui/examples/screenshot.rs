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
