//! Spikes 0.2 + 0.4 — terminal capability probe.
//!
//! Answers two blocking questions from DESIGN.md §8 in the *real* environment,
//! which is the only place they can honestly be answered:
//!
//!   0.2  Does herdr forward mouse events into a plugin pane, and do the
//!        coordinates survive intact — including past column 223, where the
//!        legacy X10 mouse encoding breaks and SGR (1006) is required?
//!   0.4  Does rendering hold up — truecolor, unicode widths, box drawing,
//!        resize — under herdr, and over SSH?
//!
//! Run it three times and compare:
//!   1. bare terminal        — baseline
//!   2. inside a herdr pane  — isolates herdr's multiplexing
//!   3. over SSH, in herdr   — isolates the wire
//!
//! Press `q` to quit; a verdict is printed to stdout on exit.

use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
    MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{Frame, Terminal};

/// A registered clickable region — the `HitMap` mechanism from DESIGN.md §3,
/// in miniature. If this works here it works in the real pane.
#[derive(Clone)]
struct Target {
    rect: Rect,
    label: &'static str,
    hits: u32,
}

struct App {
    events: Vec<String>,
    targets: Vec<Target>,
    /// Widest column at which we have observed a mouse event.
    max_col_seen: u16,
    resizes: u32,
    last_size: (u16, u16),
    drag_seen: bool,
    scroll_seen: bool,
    right_click_seen: bool,
    started: Instant,
}

impl App {
    fn new(size: (u16, u16)) -> Self {
        Self {
            events: Vec::new(),
            targets: Vec::new(),
            max_col_seen: 0,
            resizes: 0,
            last_size: size,
            drag_seen: false,
            scroll_seen: false,
            right_click_seen: false,
            started: Instant::now(),
        }
    }

    fn log(&mut self, s: String) {
        self.events.push(s);
        if self.events.len() > 8 {
            self.events.remove(0);
        }
    }

    fn on_mouse(&mut self, m: MouseEvent) {
        self.max_col_seen = self.max_col_seen.max(m.column);

        let kind = match m.kind {
            MouseEventKind::Down(MouseButton::Left) => "L-down",
            MouseEventKind::Down(MouseButton::Right) => {
                self.right_click_seen = true;
                "R-down"
            }
            MouseEventKind::Down(MouseButton::Middle) => "M-down",
            MouseEventKind::Up(_) => "up",
            MouseEventKind::Drag(_) => {
                self.drag_seen = true;
                "drag"
            }
            MouseEventKind::Moved => "move",
            MouseEventKind::ScrollDown => {
                self.scroll_seen = true;
                "scroll-dn"
            }
            MouseEventKind::ScrollUp => {
                self.scroll_seen = true;
                "scroll-up"
            }
            MouseEventKind::ScrollLeft => "scroll-l",
            MouseEventKind::ScrollRight => "scroll-r",
        };

        // Ignore bare movement in the log; it drowns everything else.
        if !matches!(m.kind, MouseEventKind::Moved) {
            self.log(format!("{kind:<10} col={:<4} row={:<4}", m.column, m.row));
        }

        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
            for t in &mut self.targets {
                if t.rect.contains(ratatui::layout::Position::new(m.column, m.row)) {
                    t.hits += 1;
                }
            }
        }
    }
}

fn main() -> io::Result<()> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    // EnableMouseCapture emits SGR (1006) via crossterm, which is what we need
    // past column 223.
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let size = terminal.size().map(|s| (s.width, s.height)).unwrap_or((0, 0));
    let mut app = App::new(size);

    let res = run(&mut terminal, &mut app);

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    print_verdict(&app);
    res
}

fn run(terminal: &mut Terminal<CrosstermBackend<Stdout>>, app: &mut App) -> io::Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    if matches!(k.code, KeyCode::Char('q') | KeyCode::Esc) {
                        return Ok(());
                    }
                    app.log(format!("key        {:?}", k.code));
                }
                Event::Mouse(m) => app.on_mouse(m),
                Event::Resize(w, h) => {
                    app.resizes += 1;
                    app.last_size = (w, h);
                    app.log(format!("resize     {w}x{h}"));
                }
                _ => {}
            }
        }
    }
}

fn ui(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let rows = Layout::vertical([
        Constraint::Length(8),  // environment
        Constraint::Length(6),  // colour
        Constraint::Length(5),  // unicode
        Constraint::Length(5),  // click targets
        Constraint::Min(6),     // event log
    ])
    .split(area);

    env_panel(f, rows[0], app);
    colour_panel(f, rows[1]);
    unicode_panel(f, rows[2]);
    target_panel(f, rows[3], app);
    log_panel(f, rows[4], app);
}

fn env_panel(f: &mut Frame, area: Rect, app: &App) {
    let v = |k: &str| std::env::var(k).unwrap_or_else(|_| "(unset)".into());
    let in_herdr = std::env::var("HERDR_ENV").is_ok();
    let over_ssh = std::env::var("SSH_CONNECTION").is_ok() || std::env::var("SSH_TTY").is_ok();

    let lines = vec![
        Line::from(vec![
            Span::styled("TERM        ", Style::default().add_modifier(Modifier::DIM)),
            Span::raw(v("TERM")),
            Span::styled("   COLORTERM ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled(
                v("COLORTERM"),
                if v("COLORTERM").contains("truecolor") || v("COLORTERM").contains("24bit") {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::Yellow)
                },
            ),
        ]),
        Line::from(vec![
            Span::styled("TERM_PROGRAM ", Style::default().add_modifier(Modifier::DIM)),
            Span::raw(v("TERM_PROGRAM")),
        ]),
        Line::from(vec![
            Span::styled("herdr pane   ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled(
                if in_herdr { "YES" } else { "no" },
                Style::default().fg(if in_herdr { Color::Green } else { Color::DarkGray }),
            ),
            Span::raw(format!(
                "   pane={} plugin={}",
                v("HERDR_PANE_ID"),
                v("HERDR_PLUGIN_ID")
            )),
        ]),
        Line::from(vec![
            Span::styled("over SSH     ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled(
                if over_ssh { "YES" } else { "no" },
                Style::default().fg(if over_ssh { Color::Green } else { Color::DarkGray }),
            ),
        ]),
        Line::from(vec![
            Span::styled("size         ", Style::default().add_modifier(Modifier::DIM)),
            Span::raw(format!(
                "{}x{}   resizes={}",
                app.last_size.0, app.last_size.1, app.resizes
            )),
            Span::styled(
                if app.last_size.0 > 223 {
                    "   (>223 cols: SGR encoding testable)"
                } else {
                    "   (WIDEN to >223 cols to test SGR encoding)"
                },
                Style::default().fg(if app.last_size.0 > 223 {
                    Color::Green
                } else {
                    Color::Yellow
                }),
            ),
        ]),
        Line::from(vec![
            Span::styled("max mouse col", Style::default().add_modifier(Modifier::DIM)),
            Span::raw(format!(" {}", app.max_col_seen)),
            Span::styled(
                if app.max_col_seen > 223 {
                    "   SGR CONFIRMED — coords valid past 223"
                } else {
                    "   click the far-right edge at width >223"
                },
                Style::default().fg(if app.max_col_seen > 223 {
                    Color::Green
                } else {
                    Color::DarkGray
                }),
            ),
        ]),
    ];

    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" 0.4 environment ")),
        area,
    );
}

fn colour_panel(f: &mut Frame, area: Rect) {
    let inner_w = area.width.saturating_sub(2).max(1);

    // Truecolor gradient — banding here means 256-colour fallback is in play.
    let mut tc = Vec::new();
    for i in 0..inner_w {
        let t = i as f32 / inner_w as f32;
        let r = (t * 255.0) as u8;
        let g = ((1.0 - t) * 180.0) as u8;
        let b = 200u8;
        tc.push(Span::styled("█", Style::default().fg(Color::Rgb(r, g, b))));
    }

    // Indexed 256 for comparison.
    let mut idx = Vec::new();
    for i in 0..inner_w {
        let c = 16 + ((i as u32 * 215) / inner_w.max(1) as u32) as u8;
        idx.push(Span::styled("█", Style::default().fg(Color::Indexed(c))));
    }

    let lines = vec![
        Line::from(Span::styled(
            "truecolor (smooth = 24-bit ok, banded = degraded):",
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(tc),
        Line::from(Span::styled(
            "indexed 256 (reference):",
            Style::default().add_modifier(Modifier::DIM),
        )),
        Line::from(idx),
    ];

    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" 0.4 colour ")),
        area,
    );
}

fn unicode_panel(f: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from("box  ┌─┬─┐ ├─┼─┤ └─┴─┘ │ ═╬═ ▏▎▍▌▋▊▉█  ⎡⎣⎤⎦"),
        Line::from("diff ▎+added  ▎-removed  ~modified  ⇢ ↳ ✓ ✗ • ●"),
        Line::from("wide 日本語テキスト | 한국어 | 中文  ← must align with the next line"),
        Line::from("ref  ABCDEFGHIJKL | ABCDEFG | ABCD   ← same visual width if widths are right"),
    ];
    f.render_widget(
        Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" 0.4 unicode / widths ")),
        area,
    );
}

fn target_panel(f: &mut Frame, area: Rect, app: &mut App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 0.2 click targets — click each one ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::horizontal([
        Constraint::Percentage(25),
        Constraint::Percentage(25),
        Constraint::Percentage(25),
        Constraint::Percentage(25),
    ])
    .split(inner);

    let labels = ["far LEFT", "mid-left", "mid-right", "far RIGHT"];

    // Rebuild the hit map every frame — exactly the DESIGN.md §3 model.
    let previous = app.targets.clone();
    app.targets.clear();

    for (i, rect) in cols.iter().enumerate() {
        let hits = previous.get(i).map(|t| t.hits).unwrap_or(0);
        app.targets.push(Target {
            rect: *rect,
            label: labels[i],
            hits,
        });

        let style = if hits > 0 {
            Style::default().fg(Color::Black).bg(Color::Green)
        } else {
            Style::default().fg(Color::White).bg(Color::DarkGray)
        };
        f.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(format!(" {} ", labels[i]), style)),
                Line::from(Span::raw(format!(" hits: {hits}"))),
                Line::from(Span::styled(
                    format!(" cols {}..{}", rect.x, rect.x + rect.width),
                    Style::default().add_modifier(Modifier::DIM),
                )),
            ]),
            *rect,
        );
    }
}

fn log_panel(f: &mut Frame, area: Rect, app: &App) {
    let mut lines: Vec<Line> = app.events.iter().map(|e| Line::from(e.as_str())).collect();
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "no events yet — move and click the mouse, scroll, right-click, resize the window",
            Style::default().add_modifier(Modifier::DIM),
        )));
    }
    f.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" 0.2 event log  (q to quit) "),
        ),
        area,
    );
}

fn print_verdict(app: &App) {
    let in_herdr = std::env::var("HERDR_ENV").is_ok();
    let over_ssh = std::env::var("SSH_CONNECTION").is_ok() || std::env::var("SSH_TTY").is_ok();
    let any_mouse = app.max_col_seen > 0;
    let clicked: u32 = app.targets.iter().map(|t| t.hits).sum();

    let yn = |b: bool| if b { "PASS" } else { "FAIL" };

    println!("\n================ spike 0.2 / 0.4 verdict ================");
    println!("context        : herdr={}  ssh={}  ran {:.0}s",
        if in_herdr { "yes" } else { "no" },
        if over_ssh { "yes" } else { "no" },
        app.started.elapsed().as_secs_f32());
    println!("terminal       : TERM={}  COLORTERM={}",
        std::env::var("TERM").unwrap_or_default(),
        std::env::var("COLORTERM").unwrap_or_default());
    println!("final size     : {}x{}  (resize events: {})",
        app.last_size.0, app.last_size.1, app.resizes);
    println!("---------------------------------------------------------");
    println!("[{}] mouse events received at all", yn(any_mouse));
    println!("[{}] click hit-testing resolved to a target ({clicked} hits)", yn(clicked > 0));
    for t in &app.targets {
        println!("       {:<10} {:>3} hit(s)  cols {}..{}",
            t.label, t.hits, t.rect.x, t.rect.x + t.rect.width);
    }
    println!("[{}] drag events", yn(app.drag_seen));
    println!("[{}] scroll events", yn(app.scroll_seen));
    println!("[{}] right-click events", yn(app.right_click_seen));
    println!("[{}] resize handled", yn(app.resizes > 0));
    if app.last_size.0 > 223 {
        println!("[{}] SGR encoding past column 223 (max col seen: {})",
            yn(app.max_col_seen > 223), app.max_col_seen);
    } else {
        println!("[SKIP] SGR past column 223 — terminal was only {} cols wide", app.last_size.0);
    }
    println!("=========================================================");
    println!("Record this output in spikes/FINDINGS.md for each of the three contexts.");
}
