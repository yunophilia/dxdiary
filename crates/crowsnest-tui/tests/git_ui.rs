//! Git-aware UI behaviour, driven headlessly against a real repository.
//!
//! The interesting logic here is the changed-only filter: selection is an index
//! into the *visible* rows, so filtering has to remap it without landing the
//! cursor on a hidden row. That is easy to get subtly wrong and invisible in a
//! screenshot.

use std::path::{Path, PathBuf};
use std::process::Command;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crowsnest_core::{ColorDepth, Config};
use crowsnest_tui::App;
use crowsnest_vcs::GitRepo;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("git {args:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn write(dir: &Path, rel: &str, content: &str) {
    let path = dir.join(rel);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

/// A repo with one clean file, one modified, one untracked, and a nested
/// modified file so directory roll-up has something to show.
fn fixture(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("crowsnest-gitui-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    git(&dir, &["init", "--quiet"]);
    git(&dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(&dir, &["config", "user.email", "t@example.com"]);
    git(&dir, &["config", "user.name", "T"]);
    git(&dir, &["config", "commit.gpgsign", "false"]);

    write(&dir, "clean.txt", "untouched\n");
    write(&dir, "edited.txt", "before\n");
    write(&dir, "src/deep.rs", "fn a() {}\n");
    git(&dir, &["add", "."]);
    git(&dir, &["commit", "--quiet", "-m", "initial"]);

    write(&dir, "edited.txt", "after\n");
    write(&dir, "src/deep.rs", "fn a() { /* changed */ }\n");
    write(&dir, "brand-new.txt", "new\n");
    dir
}

fn app_for(tag: &str) -> App {
    let root = fixture(tag);
    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);
    app.attach_vcs(Box::new(GitRepo::open(&root).unwrap()));
    app
}

fn draw(app: &mut App) -> String {
    let mut term = Terminal::new(TestBackend::new(90, 24)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let mut out = String::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

fn key(app: &mut App, code: KeyCode) -> bool {
    app.handle(Event::Key(KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }))
}

#[test]
fn status_is_read_on_attach() {
    let app = app_for("attach");
    assert_eq!(app.git.unstaged.len(), 2, "edited.txt and src/deep.rs");
    assert_eq!(app.git.untracked.len(), 1, "brand-new.txt");
    assert_eq!(app.repo.as_ref().unwrap().branch.as_deref(), Some("main"));
}

#[test]
fn badges_and_branch_render() {
    let mut app = app_for("render");
    let screen = draw(&mut app);

    assert!(
        screen.contains("M   edited.txt"),
        "modified badge:\n{screen}"
    );
    assert!(
        screen.contains("?   brand-new.txt"),
        "untracked badge:\n{screen}"
    );
    assert!(screen.contains(" main "), "branch in status bar:\n{screen}");
}

#[test]
fn a_clean_file_gets_no_badge() {
    let mut app = app_for("clean-file");
    let screen = draw(&mut app);
    let line = screen
        .lines()
        .find(|l| l.contains("clean.txt"))
        .expect("clean.txt is listed");
    assert!(
        line.trim_start().starts_with("clean.txt") || line.contains("  clean.txt"),
        "no status letter on a clean file: {line:?}"
    );
}

#[test]
fn a_collapsed_directory_shows_that_something_inside_changed() {
    let mut app = app_for("rollup");
    let screen = draw(&mut app);
    let line = screen
        .lines()
        .find(|l| l.contains("src"))
        .expect("src is listed");
    assert!(line.contains('·'), "roll-up marker on src: {line:?}");
}

#[test]
fn changed_only_hides_clean_files_and_restores_them() {
    let mut app = app_for("filter");
    let before = app.visible_rows().len();

    assert!(key(&mut app, KeyCode::Char('c')));
    assert!(app.changed_only);

    let screen = draw(&mut app);
    assert!(
        !screen.contains("clean.txt"),
        "clean file hidden:\n{screen}"
    );
    assert!(
        screen.contains("edited.txt"),
        "changed file kept:\n{screen}"
    );
    assert!(
        screen.contains("src"),
        "directory with changes kept:\n{screen}"
    );

    key(&mut app, KeyCode::Char('c'));
    assert!(!app.changed_only);
    assert_eq!(app.visible_rows().len(), before, "filter fully reverses");
}

#[test]
fn the_cursor_never_lands_on_a_hidden_row() {
    let mut app = app_for("cursor");

    // Put the cursor on the clean file, then filter it away.
    for _ in 0..app.tree.len() {
        if app
            .selected_path()
            .is_some_and(|p| p.ends_with("clean.txt"))
        {
            break;
        }
        key(&mut app, KeyCode::Down);
    }
    assert!(app.selected_path().unwrap().ends_with("clean.txt"));

    key(&mut app, KeyCode::Char('c'));

    let selected = app.selected_path().expect("something is still selected");
    assert!(
        !selected.ends_with("clean.txt"),
        "cursor moved off the hidden row, got {selected:?}"
    );
    assert!(!app.visible_rows().is_empty());
}

#[test]
fn jumping_between_changed_files_wraps_around() {
    let mut app = app_for("jump");
    key(&mut app, KeyCode::Char('c')); // filter, so all changes are visible

    key(&mut app, KeyCode::Char(']'));
    let first = app.selected_path().unwrap();

    // Cycle all the way round; we must return to where we started.
    let mut seen = vec![first.clone()];
    for _ in 0..8 {
        key(&mut app, KeyCode::Char(']'));
        let p = app.selected_path().unwrap();
        if p == first {
            break;
        }
        seen.push(p);
    }
    assert!(seen.len() >= 2, "visited several changed files: {seen:?}");
    assert_eq!(app.selected_path().unwrap(), first, "wrapped back to start");
}

#[test]
fn jumping_opens_the_file_it_lands_on() {
    let mut app = app_for("jump-open");
    key(&mut app, KeyCode::Char('c'));
    key(&mut app, KeyCode::Char(']'));
    assert!(app.doc.is_some(), "landing on a changed file opens it");
}

#[test]
fn baseline_cycles_and_is_reported() {
    let mut app = app_for("baseline");
    assert_eq!(app.baseline, crowsnest_vcs::DiffBaseline::WorkingTree);

    key(&mut app, KeyCode::Char('b'));
    assert_eq!(app.baseline, crowsnest_vcs::DiffBaseline::Index);

    let screen = draw(&mut app);
    assert!(
        screen.contains("staged"),
        "baseline in status bar:\n{screen}"
    );
}

#[test]
fn a_non_repository_still_opens_as_a_file_browser() {
    let dir = std::env::temp_dir().join("crowsnest-gitui-norepo");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), "x").unwrap();

    let mut app = App::new(dir, Config::default(), ColorDepth::TrueColor);
    assert!(app.repo.is_none());

    let screen = draw(&mut app);
    assert!(screen.contains("a.txt"), "files still listed:\n{screen}");

    // Git keys must report rather than panic.
    key(&mut app, KeyCode::Char('b'));
    assert!(
        app.status.contains("not a git repository"),
        "{}",
        app.status
    );
}

#[test]
fn refresh_picks_up_changes_made_outside() {
    let mut app = app_for("refresh");
    let root = app.tree.root.clone();
    assert_eq!(app.git.untracked.len(), 1);

    std::fs::write(root.join("appeared.txt"), "later\n").unwrap();
    key(&mut app, KeyCode::Char('r'));

    assert_eq!(app.git.untracked.len(), 2, "new untracked file is noticed");
}

#[test]
fn writing_a_commit_graph_is_offered_then_confirmed() {
    let mut app = app_for("commit-graph");
    assert!(
        app.status.contains("commit-graph"),
        "a repo without one is told: {}",
        app.status
    );

    key(&mut app, KeyCode::Char('W'));
    assert!(
        app.repo.as_ref().unwrap().has_commit_graph,
        "{}",
        app.status
    );
}

// ------------------------------------------------------------ diff view ---

/// A repo whose one tracked file has been edited in two places.
fn diff_app(tag: &str) -> App {
    let root = std::env::temp_dir().join(format!("crowsnest-diffui-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    git(&root, &["init", "--quiet"]);
    git(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(&root, &["config", "user.email", "t@example.com"]);
    git(&root, &["config", "user.name", "T"]);
    git(&root, &["config", "commit.gpgsign", "false"]);

    write(&root, "code.rs", "fn a() {}\nfn b() {}\nfn c() {}\n");
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "initial"]);
    write(&root, "code.rs", "fn a() {}\nfn CHANGED() {}\nfn c() {}\n");

    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);
    app.attach_vcs(Box::new(GitRepo::open(&root).unwrap()));
    app.reveal_and_open(root.join("code.rs"));
    app
}

#[test]
fn opening_a_changed_file_lands_on_the_diff() {
    let app = diff_app("lands");
    assert_eq!(app.view, crowsnest_tui::ContentView::Diff);
    let d = app.diff.as_ref().expect("hunks computed");
    assert_eq!((d.added, d.removed), (1, 1));
}

#[test]
fn the_diff_renders_sigils_and_both_line_numbers() {
    let mut app = diff_app("render");
    let screen = draw(&mut app);

    assert!(screen.contains("@@ -1,3 +1,3 @@"), "hunk header:\n{screen}");
    assert!(screen.contains("- fn b() {}"), "removal:\n{screen}");
    assert!(screen.contains("+ fn CHANGED() {}"), "addition:\n{screen}");
    // Context lines carry a number on both sides.
    assert!(screen.contains(" 1  1 "), "dual gutter:\n{screen}");
}

#[test]
fn d_toggles_between_the_file_and_its_diff() {
    let mut app = diff_app("toggle");

    assert!(key(&mut app, KeyCode::Char('d')));
    assert_eq!(app.view, crowsnest_tui::ContentView::File);
    let screen = draw(&mut app);
    assert!(
        !screen.contains("@@ "),
        "file view has no hunk header:\n{screen}"
    );
    assert!(
        screen.contains("fn CHANGED()"),
        "shows current content:\n{screen}"
    );

    key(&mut app, KeyCode::Char('d'));
    assert_eq!(app.view, crowsnest_tui::ContentView::Diff);
}

#[test]
fn an_unchanged_file_opens_as_a_file_not_a_diff() {
    let mut app = diff_app("unchanged");
    let root = app.tree.root.clone();
    write(&root, "extra.txt", "content\n");
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "add extra"]);

    key(&mut app, KeyCode::Char('r'));
    app.reveal_and_open(root.join("extra.txt"));

    assert_eq!(app.view, crowsnest_tui::ContentView::File);
    assert!(app.diff.is_none());

    key(&mut app, KeyCode::Char('d'));
    assert!(app.status.contains("no changes"), "{}", app.status);
}

#[test]
fn changing_the_baseline_recomputes_the_open_diff() {
    let mut app = diff_app("baseline-recompute");
    assert!(app.diff.as_ref().unwrap().added > 0, "unstaged edit shows");

    // Stage it: index -> worktree becomes clean, HEAD -> index carries it.
    git(&app.tree.root.clone(), &["add", "code.rs"]);
    key(&mut app, KeyCode::Char('r'));
    app.reveal_and_open(app.tree.root.join("code.rs"));
    assert!(app.diff.is_none(), "nothing unstaged left");

    key(&mut app, KeyCode::Char('b')); // -> Index
    assert_eq!(app.baseline, crowsnest_vcs::DiffBaseline::Index);
    let d = app.diff.as_ref().expect("staged hunks appear");
    assert_eq!((d.added, d.removed), (1, 1));
}

#[test]
fn the_whole_diff_is_reachable_by_scrolling() {
    let root = std::env::temp_dir().join("crowsnest-diffui-scroll");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--quiet"]);
    git(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(&root, &["config", "user.email", "t@e.com"]);
    git(&root, &["config", "user.name", "T"]);
    git(&root, &["config", "commit.gpgsign", "false"]);

    // Far enough apart to make several separate hunks.
    let before: String = (1..=200).map(|n| format!("line {n}\n")).collect();
    write(&root, "big.txt", &before);
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "big"]);
    let after = before
        .replace("line 10\n", "TEN\n")
        .replace("line 100\n", "HUNDRED\n")
        .replace("line 190\n", "NINETY\n");
    write(&root, "big.txt", &after);

    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);
    app.attach_vcs(Box::new(GitRepo::open(&root).unwrap()));
    app.reveal_and_open(root.join("big.txt"));

    let d = app.diff.as_ref().unwrap();
    assert_eq!(d.hunks.len(), 3, "three separate hunks");
    let rows = d.display_rows();

    draw(&mut app); // establish the viewport
    let first = draw(&mut app);
    assert!(first.contains("TEN"), "first hunk visible:\n{first}");

    // Focus the content pane first — Down moves the tree cursor otherwise.
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.focus, crowsnest_core::PaneId::Content);

    // Scroll to the end; the last hunk must come into view.
    for _ in 0..rows {
        key(&mut app, KeyCode::Down);
    }
    let last = draw(&mut app);
    assert!(last.contains("NINETY"), "last hunk reachable:\n{last}");
}

// -------------------------------------------------------------- syntax ---

/// Foreground colours of one rendered row, one entry per cell.
///
/// Screenshots only capture symbols, so colour has to be asserted against the
/// buffer directly — otherwise the whole highlight pipeline could be silently
/// dead and every text assertion would still pass.
fn row_colors(app: &mut App, needle: &str) -> Vec<ratatui::style::Color> {
    let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();

    for y in 0..buf.area.height {
        let text: String = (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<Vec<_>>()
            .join("");
        if text.contains(needle) {
            return (0..buf.area.width).map(|x| buf[(x, y)].fg).collect();
        }
    }
    panic!("no row containing {needle:?}");
}

/// Distinct colours on a row. `ratatui::style::Color` is not `Ord`, so this
/// dedupes on the debug rendering rather than in a set.
fn distinct(colors: &[ratatui::style::Color]) -> std::collections::BTreeSet<String> {
    colors.iter().map(|c| format!("{c:?}")).collect()
}

fn syntax_app(tag: &str, file: &str, body: &str) -> App {
    let root = std::env::temp_dir().join(format!("crowsnest-syn-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    write(&root, file, body);

    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);
    app.reveal_and_open(root.join(file));
    app
}

#[test]
fn source_files_get_a_language_and_highlight_spans() {
    let app = syntax_app("detect", "main.rs", "fn main() {\n    let x = 1;\n}\n");
    assert_eq!(app.language, Some(crowsnest_syntax::Language::Rust));
    let total: usize = app.doc_spans.iter().map(|l| l.len()).sum();
    assert!(total > 0, "spans computed: {:?}", app.doc_spans);
}

#[test]
fn a_keyword_is_rendered_in_the_keyword_colour() {
    let theme = Config::default().theme;
    let want = theme
        .syn_keyword
        .to_color(crowsnest_core::ColorDepth::TrueColor);

    let mut app = syntax_app("colour", "main.rs", "fn main() {}\n");
    let colors = row_colors(&mut app, "fn main");

    assert!(
        colors.contains(&want),
        "the keyword colour reaches the buffer; got {:?}",
        distinct(&colors)
    );
}

#[test]
fn a_comment_and_a_keyword_get_different_colours() {
    let mut app = syntax_app("distinct", "main.rs", "// note\nfn main() {}\n");
    let comment = row_colors(&mut app, "// note");
    let keyword = row_colors(&mut app, "fn main");

    assert_ne!(
        distinct(&comment),
        distinct(&keyword),
        "comment and code are not painted the same"
    );
}

#[test]
fn an_unknown_extension_renders_plainly_rather_than_failing() {
    let app = syntax_app("unknown", "notes.txt", "just some prose\n");
    assert_eq!(app.language, None);
    assert!(app.doc_spans.is_empty());
}

#[test]
fn context_lines_in_a_diff_are_syntax_highlighted() {
    let root = std::env::temp_dir().join("crowsnest-syn-diff");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--quiet"]);
    git(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    git(&root, &["config", "user.email", "t@e.com"]);
    git(&root, &["config", "user.name", "T"]);
    git(&root, &["config", "commit.gpgsign", "false"]);

    write(&root, "lib.rs", "fn keep() {}\nfn edit() { let a = 1; }\n");
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "init"]);
    write(&root, "lib.rs", "fn keep() {}\nfn edit() { let a = 2; }\n");

    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);
    app.attach_vcs(Box::new(GitRepo::open(&root).unwrap()));
    app.reveal_and_open(root.join("lib.rs"));

    assert_eq!(app.view, crowsnest_tui::ContentView::Diff);
    assert!(!app.new_spans.is_empty(), "the new side is highlighted");
    assert!(!app.old_spans.is_empty(), "the old side is highlighted");

    let want = Config::default()
        .theme
        .syn_keyword
        .to_color(crowsnest_core::ColorDepth::TrueColor);
    let colors = row_colors(&mut app, "fn keep");
    assert!(colors.contains(&want), "context line keeps syntax colour");
}
