//! Git-aware UI behaviour, driven headlessly against a real repository.
//!
//! The interesting logic here is the changed-only filter: selection is an index
//! into the *visible* rows, so filtering has to remap it without landing the
//! cursor on a hidden row. That is easy to get subtly wrong and invisible in a
//! screenshot.

use std::path::{Path, PathBuf};
use std::process::Command;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use dxdiary_core::{ColorDepth, Config};
use dxdiary_tui::App;
use dxdiary_vcs::GitRepo;
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

/// Pin every identity key git consults.
///
/// `author.*` and `committer.*` override `user.*` for their own fields, so
/// setting only `user.name` leaves these fixtures at the mercy of whatever the
/// developer has configured globally.
fn pin_identity(dir: &Path) {
    for (key, value) in [
        ("user.name", "T"),
        ("user.email", "t@example.com"),
        ("author.name", "T"),
        ("author.email", "t@example.com"),
        ("committer.name", "T"),
        ("committer.email", "t@example.com"),
        ("commit.gpgsign", "false"),
    ] {
        git(dir, &["config", key, value]);
    }
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
    let dir = std::env::temp_dir().join(format!("dxdiary-gitui-{tag}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    git(&dir, &["init", "--quiet"]);
    git(&dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    pin_identity(&dir);

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
    assert_eq!(app.baseline, dxdiary_vcs::DiffBaseline::WorkingTree);

    key(&mut app, KeyCode::Char('b'));
    assert_eq!(app.baseline, dxdiary_vcs::DiffBaseline::Index);

    let screen = draw(&mut app);
    assert!(
        screen.contains("staged"),
        "baseline in status bar:\n{screen}"
    );
}

#[test]
fn a_non_repository_still_opens_as_a_file_browser() {
    let dir = std::env::temp_dir().join("dxdiary-gitui-norepo");
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
    let root = std::env::temp_dir().join(format!("dxdiary-diffui-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    git(&root, &["init", "--quiet"]);
    git(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    pin_identity(&root);

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
    assert_eq!(app.view, dxdiary_tui::ContentView::Diff);
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
    assert_eq!(app.view, dxdiary_tui::ContentView::File);
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
    assert_eq!(app.view, dxdiary_tui::ContentView::Diff);
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

    assert_eq!(app.view, dxdiary_tui::ContentView::File);
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
    assert_eq!(app.baseline, dxdiary_vcs::DiffBaseline::Index);
    let d = app.diff.as_ref().expect("staged hunks appear");
    assert_eq!((d.added, d.removed), (1, 1));
}

#[test]
fn the_whole_diff_is_reachable_by_scrolling() {
    let root = std::env::temp_dir().join("dxdiary-diffui-scroll");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--quiet"]);
    git(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    pin_identity(&root);

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
    assert_eq!(app.focus, dxdiary_core::PaneId::Content);

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
    let root = std::env::temp_dir().join(format!("dxdiary-syn-{tag}"));
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
    assert_eq!(app.language, Some(dxdiary_syntax::Language::Rust));
    let total: usize = app.doc_spans.iter().map(|l| l.len()).sum();
    assert!(total > 0, "spans computed: {:?}", app.doc_spans);
}

#[test]
fn a_keyword_is_rendered_in_the_keyword_colour() {
    let theme = Config::default().theme;
    let want = theme
        .syn_keyword
        .to_color(dxdiary_core::ColorDepth::TrueColor);

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
    let root = std::env::temp_dir().join("dxdiary-syn-diff");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--quiet"]);
    git(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    pin_identity(&root);

    write(&root, "lib.rs", "fn keep() {}\nfn edit() { let a = 1; }\n");
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "init"]);
    write(&root, "lib.rs", "fn keep() {}\nfn edit() { let a = 2; }\n");

    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);
    app.attach_vcs(Box::new(GitRepo::open(&root).unwrap()));
    app.reveal_and_open(root.join("lib.rs"));

    assert_eq!(app.view, dxdiary_tui::ContentView::Diff);
    assert!(!app.new_spans.is_empty(), "the new side is highlighted");
    assert!(!app.old_spans.is_empty(), "the old side is highlighted");

    let want = Config::default()
        .theme
        .syn_keyword
        .to_color(dxdiary_core::ColorDepth::TrueColor);
    let colors = row_colors(&mut app, "fn keep");
    assert!(colors.contains(&want), "context line keeps syntax colour");
}

// --------------------------------------------------------------- blame ---

/// Two commits by different authors, so runs and attribution are both visible.
fn blame_app(tag: &str) -> App {
    let root = std::env::temp_dir().join(format!("dxdiary-blame-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();

    git(&root, &["init", "--quiet"]);
    git(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    pin_identity(&root);

    write(&root, "lib.rs", "fn one() {}\n");
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "add one"]);

    // A second author, so a change of attribution is detectable.
    git(&root, &["config", "author.name", "Other"]);
    write(&root, "lib.rs", "fn one() {}\nfn two() {}\nfn three() {}\n");
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "add more"]);

    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);
    app.attach_vcs(Box::new(GitRepo::open(&root).unwrap()));
    app.reveal_and_open(root.join("lib.rs"));
    app
}

#[test]
fn blame_is_off_until_asked_for() {
    let app = blame_app("off");
    assert!(!app.show_blame);
    assert!(app.blame.is_none());
}

#[test]
fn toggling_blame_computes_it_off_the_render_thread() {
    let mut app = blame_app("async");

    assert!(key(&mut app, KeyCode::Char('a')));
    assert!(app.show_blame);
    // The keypress must return immediately -- blame takes up to 2.5 s on a
    // large file (spike 0.3) and cannot be allowed to block a frame.
    assert!(app.blame.is_none(), "not computed synchronously");

    assert!(
        app.await_blame(std::time::Duration::from_secs(30)),
        "the worker delivers"
    );
    assert!(app.blame.is_some());
}

#[test]
fn the_gutter_shows_the_author_of_each_line() {
    let mut app = blame_app("gutter");
    key(&mut app, KeyCode::Char('a'));
    app.await_blame(std::time::Duration::from_secs(30));

    let screen = draw(&mut app);
    assert!(screen.contains('T'), "first author present:\n{screen}");
    assert!(screen.contains("Other"), "second author present:\n{screen}");
    assert!(screen.contains("fn one()"), "source still shown:\n{screen}");
}

#[test]
fn consecutive_lines_from_one_commit_are_not_repeated() {
    let mut app = blame_app("runs");
    key(&mut app, KeyCode::Char('a'));
    app.await_blame(std::time::Duration::from_secs(30));

    let blame = app.blame.as_ref().unwrap();
    assert_eq!(
        blame.get(1).unwrap().commit,
        blame.get(2).unwrap().commit,
        "lines 2 and 3 landed in the same commit"
    );

    let screen = draw(&mut app);
    let third = screen
        .lines()
        .find(|l| l.contains("fn three()"))
        .expect("third line rendered");
    assert!(
        !third.contains("Other"),
        "a repeated commit is left blank: {third:?}"
    );
}

#[test]
fn blame_forces_the_file_view() {
    let mut app = blame_app("view");
    let root = app.tree.root.clone();
    write(
        &root,
        "lib.rs",
        "fn one() {}\nfn two() {}\nfn EDITED() {}\n",
    );
    key(&mut app, KeyCode::Char('r'));
    app.reveal_and_open(root.join("lib.rs"));
    assert_eq!(
        app.view,
        dxdiary_tui::ContentView::Diff,
        "opens on the diff"
    );

    key(&mut app, KeyCode::Char('a'));
    assert_eq!(
        app.view,
        dxdiary_tui::ContentView::File,
        "the diff has no blame gutter, so blame switches views"
    );
}

#[test]
fn opening_another_file_discards_the_previous_blame() {
    let mut app = blame_app("switch");
    key(&mut app, KeyCode::Char('a'));
    app.await_blame(std::time::Duration::from_secs(30));
    assert!(app.blame.is_some());

    let root = app.tree.root.clone();
    write(&root, "other.rs", "fn other() {}\n");
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "other"]);
    key(&mut app, KeyCode::Char('r'));
    app.reveal_and_open(root.join("other.rs"));

    // Stale blame would attribute the wrong commits to the new file.
    assert!(
        app.blame.is_none() || app.blame.as_ref().unwrap().path.ends_with("other.rs"),
        "no stale attribution"
    );
    assert!(app.await_blame(std::time::Duration::from_secs(30)));
    assert!(app.blame.as_ref().unwrap().path.ends_with("other.rs"));
}

#[test]
fn blame_outside_a_repository_reports_rather_than_panicking() {
    let dir = std::env::temp_dir().join("dxdiary-blame-norepo");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();

    let mut app = App::new(dir.clone(), Config::default(), ColorDepth::TrueColor);
    app.reveal_and_open(dir.join("a.txt"));

    key(&mut app, KeyCode::Char('a'));
    assert!(
        app.status.contains("not a git repository"),
        "{}",
        app.status
    );
    assert!(!app.show_blame);
}

// ------------------------------------------------------------- editing ---

fn edit_app(tag: &str, body: &str) -> App {
    let root = std::env::temp_dir().join(format!("dxdiary-edit-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    write(&root, "code.rs", body);

    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);
    app.reveal_and_open(root.join("code.rs"));
    app
}

fn typed(app: &mut App, text: &str) {
    for ch in text.chars() {
        let code = if ch == '\n' {
            KeyCode::Enter
        } else {
            KeyCode::Char(ch)
        };
        key(app, code);
    }
}

#[test]
fn opening_a_text_file_makes_it_editable() {
    let app = edit_app("open", "fn main() {}\n");
    assert!(app.buffer.is_some());
    assert_eq!(app.mode, dxdiary_tui::Mode::Normal);
    assert!(!app.is_dirty());
}

#[test]
fn i_enters_insert_mode_and_esc_leaves_it() {
    let mut app = edit_app("modes", "abc\n");
    assert!(key(&mut app, KeyCode::Char('i')));
    assert_eq!(app.mode, dxdiary_tui::Mode::Insert);

    key(&mut app, KeyCode::Esc);
    assert_eq!(app.mode, dxdiary_tui::Mode::Normal);
}

#[test]
fn esc_leaves_insert_rather_than_quitting() {
    // In normal mode Esc quits; in insert it must not, or every typo would
    // close the editor.
    let mut app = edit_app("esc", "abc\n");
    key(&mut app, KeyCode::Char('i'));
    key(&mut app, KeyCode::Esc);
    assert!(!app.quit, "Esc left insert mode instead of quitting");
}

#[test]
fn typed_text_reaches_the_buffer_and_the_screen() {
    let mut app = edit_app("typing", "\n");
    key(&mut app, KeyCode::Char('i'));
    typed(&mut app, "hello");

    assert_eq!(app.buffer.as_ref().unwrap().line(0), "hello");
    assert!(app.is_dirty());

    let screen = draw(&mut app);
    assert!(screen.contains("hello"), "edit is visible:\n{screen}");
    assert!(screen.contains("INSERT"), "mode is shown:\n{screen}");
    assert!(screen.contains('●'), "unsaved marker:\n{screen}");
}

#[test]
fn a_letter_that_is_a_command_in_normal_mode_is_just_text_in_insert() {
    // `d`, `a`, `b`, `c`, `u`, `x` are all commands; none may leak into typing.
    let mut app = edit_app("letters", "\n");
    key(&mut app, KeyCode::Char('i'));
    typed(&mut app, "dabcux");
    assert_eq!(app.buffer.as_ref().unwrap().line(0), "dabcux");
}

#[test]
fn undo_and_redo_work_from_normal_mode() {
    let mut app = edit_app("undo", "start\n");
    key(&mut app, KeyCode::Char('i'));
    typed(&mut app, "XY");
    key(&mut app, KeyCode::Esc);
    assert!(app.buffer.as_ref().unwrap().line(0).starts_with("XY"));

    key(&mut app, KeyCode::Char('u'));
    assert_eq!(app.buffer.as_ref().unwrap().line(0), "start");

    // Ctrl+R redoes.
    app.handle(Event::Key(KeyEvent {
        code: KeyCode::Char('r'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }));
    assert!(app.buffer.as_ref().unwrap().line(0).starts_with("XY"));
}

#[test]
fn x_deletes_a_character_and_shift_d_deletes_a_line() {
    let mut app = edit_app("delete", "abc\nsecond\n");
    key(&mut app, KeyCode::Char('x'));
    assert_eq!(app.buffer.as_ref().unwrap().line(0), "bc");

    key(&mut app, KeyCode::Char('D'));
    assert_eq!(app.buffer.as_ref().unwrap().lines(), vec!["second"]);
}

#[test]
fn ctrl_s_writes_the_file_and_clears_the_marker() {
    let mut app = edit_app("save", "before\n");
    key(&mut app, KeyCode::Char('i'));
    typed(&mut app, "X");
    key(&mut app, KeyCode::Esc);
    assert!(app.is_dirty());

    let path = app.buffer.as_ref().unwrap().path.clone();
    app.handle(Event::Key(KeyEvent {
        code: KeyCode::Char('s'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }));

    assert!(!app.is_dirty(), "{}", app.status);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "Xbefore\n");
}

#[test]
fn editing_rehighlights_so_new_syntax_is_coloured() {
    let mut app = edit_app("rehighlight", "let x = 1;\n");
    let before: usize = app.doc_spans.iter().map(|l| l.len()).sum();

    key(&mut app, KeyCode::Char('i'));
    typed(&mut app, "\nfn added() {}");
    key(&mut app, KeyCode::Esc);

    let after: usize = app.doc_spans.iter().map(|l| l.len()).sum();
    assert!(after > before, "new code got spans: {before} -> {after}");
}

#[test]
fn insert_mode_switches_away_from_the_diff_view() {
    // A diff is not editable; typing into one would have nowhere to go.
    let root = std::env::temp_dir().join("dxdiary-edit-diffview");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "--quiet"]);
    git(&root, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    pin_identity(&root);
    write(&root, "a.rs", "fn a() {}\n");
    git(&root, &["add", "."]);
    git(&root, &["commit", "--quiet", "-m", "init"]);
    write(&root, "a.rs", "fn a() { /* edited */ }\n");

    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);
    app.attach_vcs(Box::new(GitRepo::open(&root).unwrap()));
    app.reveal_and_open(root.join("a.rs"));
    assert_eq!(app.view, dxdiary_tui::ContentView::Diff);

    key(&mut app, KeyCode::Char('i'));
    assert_eq!(app.view, dxdiary_tui::ContentView::File);
    assert_eq!(app.mode, dxdiary_tui::Mode::Insert);
}

#[test]
fn a_binary_file_is_not_editable() {
    let root = std::env::temp_dir().join("dxdiary-edit-binary");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("blob.bin"), [0u8, 1, 2, 0]).unwrap();

    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);
    app.reveal_and_open(root.join("blob.bin"));
    assert!(app.buffer.is_none());

    key(&mut app, KeyCode::Char('i'));
    assert_eq!(app.mode, dxdiary_tui::Mode::Normal);
    assert!(app.status.contains("not editable"), "{}", app.status);
}

#[test]
fn ctrl_c_quits_even_from_insert_mode() {
    let mut app = edit_app("escape-hatch", "x\n");
    key(&mut app, KeyCode::Char('i'));
    app.handle(Event::Key(KeyEvent {
        code: KeyCode::Char('c'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }));
    assert!(app.quit, "a runaway session must always be escapable");
}

// ---------------------------------------------------------------- lsp sync

/// An app with the mock language server attached for Rust, or `None` when
/// python is unavailable -- the test then skips rather than fails, as the
/// client's own round-trip tests do.
fn lsp_app(tag: &str, body: &str) -> Option<App> {
    let python = ["python3", "python"]
        .into_iter()
        .find(|c| dxdiary_lsp::registry::find_on_path(c).is_some())?;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../dxdiary-lsp/tests/mock_server.py");

    let root = std::env::temp_dir().join(format!("dxdiary-lsp-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    write(&root, "code.rs", body);
    write(&root, "other.rs", "fn other() {}\n");

    let mut cmd = Command::new(python);
    cmd.arg(&script);
    let spec = dxdiary_lsp::spec_for(dxdiary_syntax::Language::Rust).unwrap();
    let client = dxdiary_lsp::Client::from_command(cmd, spec, &root).expect("mock starts");

    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);
    app.attach_lsp(dxdiary_syntax::Language::Rust, client);
    app.reveal_and_open(root.join("code.rs"));
    Some(app)
}

/// Poll until a diagnostic with `message` is showing, or give up.
fn await_message(app: &mut App, message: &str) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        app.poll_lsp();
        if app.diagnostics.iter().any(|d| d.message == message) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    false
}

fn version_of(app: &App, path: &Path) -> Option<i32> {
    app.lsp_client(dxdiary_syntax::Language::Rust)
        .unwrap()
        .version_of(path)
}

fn ctrl(app: &mut App, ch: char) -> bool {
    app.handle(Event::Key(KeyEvent {
        code: KeyCode::Char(ch),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }))
}

#[test]
fn opening_a_file_tells_the_server_and_shows_its_diagnostics() {
    let Some(mut app) = lsp_app("open", "fn main() {}\nlet x =\n") else {
        eprintln!("skipped: python not available");
        return;
    };
    assert!(await_message(&mut app, "mock diagnostic"), "{}", app.status);
    assert_eq!(app.diagnostics[0].range.start.line, 1);
    assert!(
        app.diagnostic_at(1).is_some() && app.diagnostic_at(0).is_none(),
        "the gutter marks exactly the diagnosed line"
    );
}

#[test]
fn edits_reach_the_server_after_a_pause_not_per_keystroke() {
    let Some(mut app) = lsp_app("change", "fn main() {}\n") else {
        eprintln!("skipped: python not available");
        return;
    };
    assert!(await_message(&mut app, "mock diagnostic"));
    let path = app.buffer.as_ref().unwrap().path.clone();
    assert_eq!(version_of(&app, &path), Some(1));

    key(&mut app, KeyCode::Char('i'));
    typed(&mut app, "let a = 1;\nlet b = 2;\n");
    key(&mut app, KeyCode::Esc);

    // Typing alone sends nothing: many keystrokes, still version 1, and the
    // markers from before the edit are still on screen rather than wiped.
    app.poll_lsp();
    assert_eq!(
        version_of(&app, &path),
        Some(1),
        "no change sent before the debounce"
    );
    assert!(!app.diagnostics.is_empty(), "markers survive an edit");

    // After the pause, exactly one change carrying the whole text. The mock
    // answers with a diagnostic on the last line of what it received: the
    // buffer now has three lines, so line 3 is the empty tail.
    assert!(await_message(&mut app, "v2"), "{}", app.status);
    assert_eq!(
        version_of(&app, &path),
        Some(2),
        "one change for the whole burst"
    );
    assert_eq!(app.diagnostics[0].range.start.line, 3);
}

#[test]
fn saving_flushes_the_pending_change_before_announcing_the_save() {
    let Some(mut app) = lsp_app("save", "x\n") else {
        eprintln!("skipped: python not available");
        return;
    };
    assert!(await_message(&mut app, "mock diagnostic"));
    let path = app.buffer.as_ref().unwrap().path.clone();

    key(&mut app, KeyCode::Char('i'));
    typed(&mut app, "y");
    key(&mut app, KeyCode::Esc);
    // Save immediately, inside the debounce window: the change must not be
    // lost or arrive after the save.
    ctrl(&mut app, 's');
    assert!(!app.is_dirty(), "{}", app.status);

    assert_eq!(
        version_of(&app, &path),
        Some(2),
        "the edit went out with the save"
    );
    assert!(await_message(&mut app, "saved"), "{}", app.status);
}

#[test]
fn switching_files_closes_the_old_one_with_the_server() {
    let Some(mut app) = lsp_app("close", "x\n") else {
        eprintln!("skipped: python not available");
        return;
    };
    assert!(await_message(&mut app, "mock diagnostic"));
    let first = app.buffer.as_ref().unwrap().path.clone();
    let root = first.parent().unwrap().to_path_buf();

    app.reveal_and_open(root.join("other.rs"));
    assert_eq!(version_of(&app, &first), None, "closed");
    assert_eq!(version_of(&app, &root.join("other.rs")), Some(1), "opened");
}

// ------------------------------------------------------------ unsaved guard

fn name_of(app: &App) -> String {
    app.buffer
        .as_ref()
        .unwrap()
        .path
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn quitting_with_unsaved_edits_asks_and_a_repeat_confirms() {
    let mut app = edit_app("guard-quit", "x\n");
    key(&mut app, KeyCode::Char('i'));
    typed(&mut app, "y");
    key(&mut app, KeyCode::Esc);

    key(&mut app, KeyCode::Char('q'));
    assert!(!app.quit, "first quit must not lose the edit");
    assert!(app.status.contains("unsaved"), "{}", app.status);
    assert!(
        app.status.contains("code.rs"),
        "names the file: {}",
        app.status
    );

    key(&mut app, KeyCode::Char('q'));
    assert!(app.quit, "the repeat is the confirmation");
}

#[test]
fn saving_disarms_the_quit_confirmation() {
    let mut app = edit_app("guard-save", "x\n");
    key(&mut app, KeyCode::Char('i'));
    typed(&mut app, "y");
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('q'));
    assert!(!app.quit);

    ctrl(&mut app, 's');
    key(&mut app, KeyCode::Char('q'));
    assert!(app.quit, "nothing left to lose after a save");
}

#[test]
fn opening_another_file_with_unsaved_edits_asks_first() {
    let mut app = edit_app("guard-open", "x\n");
    let root = app
        .buffer
        .as_ref()
        .unwrap()
        .path
        .parent()
        .unwrap()
        .to_path_buf();
    write(&root, "second.rs", "fn second() {}\n");
    app.tree.refresh();

    key(&mut app, KeyCode::Char('i'));
    typed(&mut app, "y");
    key(&mut app, KeyCode::Esc);

    app.reveal_and_open(root.join("second.rs"));
    assert!(app.is_dirty(), "the edit is still there");
    assert_eq!(name_of(&app), "code.rs", "the dirty file stays open");
    assert!(app.status.contains("unsaved"), "{}", app.status);

    app.reveal_and_open(root.join("second.rs"));
    assert_eq!(name_of(&app), "second.rs");
    assert!(!app.is_dirty());
}

#[test]
fn a_quit_refusal_does_not_stand_in_for_an_open_confirmation() {
    // Arming is per action: refusing to quit must not make the next open
    // silently discard the edit.
    let mut app = edit_app("guard-cross", "x\n");
    let root = app
        .buffer
        .as_ref()
        .unwrap()
        .path
        .parent()
        .unwrap()
        .to_path_buf();
    write(&root, "second.rs", "fn second() {}\n");
    app.tree.refresh();

    key(&mut app, KeyCode::Char('i'));
    typed(&mut app, "y");
    key(&mut app, KeyCode::Esc);
    key(&mut app, KeyCode::Char('q'));
    assert!(!app.quit);

    app.reveal_and_open(root.join("second.rs"));
    assert_eq!(name_of(&app), "code.rs");
}

/// The colours of every character of `needle` on the row containing it.
fn colors_over(app: &mut App, needle: &str) -> Vec<ratatui::style::Color> {
    let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    for y in 0..buf.area.height {
        let text: String = (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<Vec<_>>()
            .join("");
        if let Some(byte) = text.find(needle) {
            let col = text[..byte].chars().count();
            return (0..needle.chars().count())
                .map(|i| buf[((col + i) as u16, y)].fg)
                .collect();
        }
    }
    panic!("no row containing {needle:?}");
}

#[test]
fn a_keyword_after_a_tab_indent_is_coloured_at_the_right_column() {
    // Go is gofmt'd with tabs, so this is the common case, not an edge one.
    // Highlight spans are character offsets into the raw line while the pane
    // renders the line tab-expanded: without a mapping between the two, every
    // colour on the row lands `tab_width - 1` columns early per tab.
    let mut app = syntax_app("tab-go", "main.go", "func f() {\n\treturn\n}\n");
    let indented = colors_over(&mut app, "return");

    let mut plain = syntax_app("tab-go-plain", "other.go", "func f() {\nreturn\n}\n");
    let expected = colors_over(&mut plain, "return");

    assert_eq!(
        indented, expected,
        "a tab-indented keyword must be coloured over its whole width"
    );
}

// ------------------------------------------------------------------ search

/// An app on a file whose lines are easy to assert about.
fn find_app(tag: &str, body: &str) -> App {
    let root = std::env::temp_dir().join(format!("dxdiary-find-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    write(&root, "code.rs", body);
    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);
    app.reveal_and_open(root.join("code.rs"));
    app
}

/// Type a search and commit it.
fn search_for(app: &mut App, query: &str) {
    key(app, KeyCode::Char('/'));
    typed(app, query);
    key(app, KeyCode::Enter);
}

const HAYSTACK: &str = "fn alpha() {}\nfn beta() {}\nfn alpha_two() {}\nfn gamma() {}\n";

#[test]
fn slash_opens_a_prompt_that_shows_what_is_typed() {
    let mut app = find_app("prompt", HAYSTACK);
    assert_eq!(app.prompt_line(), None, "no prompt until asked for");

    key(&mut app, KeyCode::Char('/'));
    assert_eq!(app.mode, dxdiary_tui::Mode::Prompt);
    typed(&mut app, "alp");
    assert_eq!(app.prompt_line().unwrap(), "/alp\u{2588}");

    // The prompt owns the status line while it is open.
    assert!(draw(&mut app).contains("/alp"));
}

#[test]
fn a_committed_search_moves_the_cursor_and_counts_the_matches() {
    let mut app = find_app("count", HAYSTACK);
    search_for(&mut app, "alpha");

    assert_eq!(app.mode, dxdiary_tui::Mode::Normal, "the prompt closed");
    assert_eq!(app.search.as_ref().unwrap().len(), 2);
    assert_eq!(app.doc_line, 0, "cursor on the first match");
    assert!(app.status.contains("2 matches"), "{}", app.status);
}

#[test]
fn one_match_is_reported_in_the_singular() {
    let mut app = find_app("singular", HAYSTACK);
    search_for(&mut app, "beta");
    assert!(
        app.status.contains("1 match for") && !app.status.contains("matches"),
        "{}",
        app.status
    );
}

#[test]
fn n_steps_forward_and_wraps_with_a_word_about_it() {
    let mut app = find_app("step", HAYSTACK);
    search_for(&mut app, "alpha");
    assert_eq!(app.doc_line, 0);

    key(&mut app, KeyCode::Char('n'));
    assert_eq!(app.doc_line, 2);
    assert!(app.status.contains("2/2"), "{}", app.status);

    key(&mut app, KeyCode::Char('n'));
    assert_eq!(app.doc_line, 0, "wrapped to the first");
    assert!(app.status.contains("wrapped"), "{}", app.status);
}

#[test]
fn shift_n_steps_backward() {
    let mut app = find_app("back", HAYSTACK);
    search_for(&mut app, "alpha");
    key(&mut app, KeyCode::Char('N'));
    assert_eq!(app.doc_line, 2, "backward from the first wraps to the last");
}

#[test]
fn stepping_without_a_search_says_what_to_press() {
    let mut app = find_app("nostep", HAYSTACK);
    key(&mut app, KeyCode::Char('n'));
    assert!(app.status.contains('/'), "{}", app.status);
    assert!(app.search.is_none());
}

#[test]
fn a_query_with_no_match_says_so_and_leaves_nothing_highlighted() {
    let mut app = find_app("miss", HAYSTACK);
    search_for(&mut app, "zzz");
    assert!(app.search.is_none(), "no stale highlight");
    assert!(app.status.contains("no match"), "{}", app.status);
}

#[test]
fn the_search_previews_while_typing() {
    // Live, not modal: the point is to see where you are going before
    // committing to it.
    let mut app = find_app("live", HAYSTACK);
    key(&mut app, KeyCode::Char('/'));
    typed(&mut app, "gamma");
    assert_eq!(app.doc_line, 3, "moved before Enter was pressed");
    assert_eq!(app.search.as_ref().unwrap().len(), 1);
}

#[test]
fn cancelling_a_search_puts_the_cursor_back() {
    let mut app = find_app("cancel", HAYSTACK);
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Down);
    let before = app.doc_line;
    assert_eq!(before, 1);

    key(&mut app, KeyCode::Char('/'));
    typed(&mut app, "gamma");
    assert_eq!(app.doc_line, 3, "previewed away from where we were");

    key(&mut app, KeyCode::Esc);
    assert_eq!(app.mode, dxdiary_tui::Mode::Normal);
    assert_eq!(app.doc_line, before, "esc restored the cursor");
    assert!(app.search.is_none());
    assert!(!app.quit, "esc in a prompt must not quit");
}

#[test]
fn backspace_narrows_the_query_back_down() {
    let mut app = find_app("backspace", HAYSTACK);
    key(&mut app, KeyCode::Char('/'));
    typed(&mut app, "alphax");
    assert!(app.search.is_none(), "no match for the longer query");

    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.prompt_line().unwrap(), "/alpha\u{2588}");
    assert_eq!(app.search.as_ref().unwrap().len(), 2);
}

#[test]
fn a_search_starts_from_the_cursor_not_the_top_of_the_file() {
    let mut app = find_app("from-cursor", HAYSTACK);
    key(&mut app, KeyCode::Tab);
    for _ in 0..2 {
        key(&mut app, KeyCode::Down);
    }
    assert_eq!(app.doc_line, 2);

    key(&mut app, KeyCode::Char('/'));
    typed(&mut app, "alpha");
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        app.doc_line, 2,
        "selected the match at the cursor, not line 0"
    );
}

#[test]
fn esc_clears_a_search_before_it_quits() {
    let mut app = find_app("esc-clears", HAYSTACK);
    search_for(&mut app, "alpha");

    key(&mut app, KeyCode::Esc);
    assert!(app.search.is_none(), "the highlight went");
    assert!(!app.quit, "and the session did not");

    key(&mut app, KeyCode::Esc);
    assert!(app.quit, "with nothing to dismiss, esc still quits");
}

#[test]
fn q_quits_even_with_a_search_on_screen() {
    let mut app = find_app("q-quits", HAYSTACK);
    search_for(&mut app, "alpha");
    key(&mut app, KeyCode::Char('q'));
    assert!(app.quit, "q is unconditional; only esc dismisses first");
}

#[test]
fn matches_are_shaded_and_the_selected_one_differently() {
    let mut app = find_app("shade", HAYSTACK);
    search_for(&mut app, "alpha");

    let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();

    let mut on_first = Vec::new();
    let mut on_second = Vec::new();
    for y in 0..buf.area.height {
        let text: String = (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<Vec<_>>()
            .join("");
        // `alpha` appears on the row for line 1 and the row for line 3.
        if let Some(byte) = text.find("alpha") {
            let col = text[..byte].chars().count();
            let bgs: Vec<_> = (0..5).map(|i| buf[((col + i) as u16, y)].bg).collect();
            if on_first.is_empty() {
                on_first = bgs;
            } else if on_second.is_empty() {
                on_second = bgs;
            }
        }
    }

    assert_eq!(on_first.len(), 5, "found the first match row");
    assert_eq!(on_second.len(), 5, "found the second match row");
    assert!(
        on_first.iter().all(|c| *c == on_first[0]),
        "the whole match is shaded, not part of it: {on_first:?}"
    );
    assert_ne!(
        on_first[0], on_second[0],
        "the selected match is distinguishable from the others"
    );
}

#[test]
fn a_match_after_a_tab_is_shaded_at_the_right_columns() {
    // The same raw-versus-expanded trap as syntax colouring: both now map
    // through one column table.
    let mut app = find_app("shade-tab", "fn f() {\n\talpha\n}\n");
    search_for(&mut app, "alpha");

    let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();

    for y in 0..buf.area.height {
        let text: String = (0..buf.area.width)
            .map(|x| buf[(x, y)].symbol())
            .collect::<Vec<_>>()
            .join("");
        if let Some(byte) = text.find("alpha") {
            let col = text[..byte].chars().count();
            let bgs: Vec<_> = (0..5).map(|i| buf[((col + i) as u16, y)].bg).collect();
            assert!(
                bgs.iter().all(|c| *c == bgs[0]),
                "shading drifted across the tab: {bgs:?}"
            );
            // And the column before the match is not shaded.
            assert_ne!(buf[((col - 1) as u16, y)].bg, bgs[0], "shading ran wide");
            return;
        }
    }
    panic!("no row containing the match");
}

#[test]
fn searching_from_the_diff_view_switches_to_the_file() {
    let mut app = diff_app("find-in-diff");
    assert_eq!(
        app.view,
        dxdiary_tui::ContentView::Diff,
        "the fixture opens changed"
    );

    search_for(&mut app, "fn");
    assert_eq!(
        app.view,
        dxdiary_tui::ContentView::File,
        "matches are file lines, so the file is what gets shown"
    );
}

// -------------------------------------------------------------- goto line

#[test]
fn colon_jumps_to_a_line_number() {
    let mut app = find_app("goto", HAYSTACK);
    key(&mut app, KeyCode::Char(':'));
    typed(&mut app, "3");
    assert_eq!(app.prompt_line().unwrap(), ":3\u{2588}");
    key(&mut app, KeyCode::Enter);

    assert_eq!(app.doc_line, 2, "line numbers are 1-based on screen");
    assert!(app.status.contains("line 3"), "{}", app.status);
}

#[test]
fn a_line_past_the_end_lands_on_the_last_one_and_says_so() {
    let mut app = find_app("goto-past", HAYSTACK);
    key(&mut app, KeyCode::Char(':'));
    typed(&mut app, "900");
    key(&mut app, KeyCode::Enter);

    assert_eq!(app.doc_line, app.text_line_count() - 1);
    assert!(app.status.contains("past the end"), "{}", app.status);
}

#[test]
fn a_goto_that_is_not_a_number_is_refused_without_moving() {
    let mut app = find_app("goto-junk", HAYSTACK);
    key(&mut app, KeyCode::Char(':'));
    typed(&mut app, "abc");
    key(&mut app, KeyCode::Enter);

    assert_eq!(app.doc_line, 0, "stayed put");
    assert!(app.status.contains("not a line number"), "{}", app.status);
}

#[test]
fn line_zero_is_not_a_line() {
    let mut app = find_app("goto-zero", HAYSTACK);
    key(&mut app, KeyCode::Char(':'));
    typed(&mut app, "0");
    key(&mut app, KeyCode::Enter);
    assert!(app.status.contains("not a line number"), "{}", app.status);
}

#[test]
fn typing_a_command_letter_into_a_prompt_is_just_text() {
    // `q`, `d` and `b` are commands in normal mode. Inside a prompt they
    // must not fire, or no query containing them could ever be typed.
    let mut app = find_app("prompt-letters", "fn quit_db() {}\n");
    key(&mut app, KeyCode::Char('/'));
    typed(&mut app, "quit_db");
    assert!(!app.quit, "q did not quit");
    assert_eq!(app.prompt_line().unwrap(), "/quit_db\u{2588}");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.search.as_ref().unwrap().len(), 1);
}

// ------------------------------------------------------- go to definition

/// Poll until an outstanding LSP reply has been acted on, or give up.
fn settle(app: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        app.poll_lsp();
        if app.status.starts_with("definition ·") || app.status.contains("failed") {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn ctrl_bracket_jumps_to_a_definition_in_another_file() {
    let Some(mut app) = lsp_app("goto-def", "fn main() {\n    helper();\n}\n") else {
        eprintln!("skipped: python not available");
        return;
    };
    // The mock answers every definition request with defined.rs:3.
    write(
        &app.tree.root.clone(),
        "defined.rs",
        "a\nb\nfn helper() {}\n",
    );
    app.tree.refresh();

    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Down);
    assert_eq!(app.doc_line, 1, "on the call site");

    ctrl(&mut app, ']');
    settle(&mut app);

    assert_eq!(name_of(&app), "defined.rs", "{}", app.status);
    assert_eq!(app.doc_line, 2, "the line the server named, 0-based");
}

#[test]
fn ctrl_o_comes_back_from_a_jump() {
    let Some(mut app) = lsp_app("goto-back", "fn main() {\n    helper();\n}\n") else {
        eprintln!("skipped: python not available");
        return;
    };
    write(
        &app.tree.root.clone(),
        "defined.rs",
        "a\nb\nfn helper() {}\n",
    );
    app.tree.refresh();

    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Down);
    ctrl(&mut app, ']');
    settle(&mut app);
    assert_eq!(name_of(&app), "defined.rs");

    ctrl(&mut app, 'o');
    assert_eq!(name_of(&app), "code.rs", "{}", app.status);
    assert_eq!(app.doc_line, 1, "back on the call site, not the top");
}

#[test]
fn going_back_with_nowhere_to_go_says_so() {
    let Some(mut app) = lsp_app("goto-nowhere", "fn main() {}\n") else {
        eprintln!("skipped: python not available");
        return;
    };
    ctrl(&mut app, 'o');
    assert!(app.status.contains("no jump"), "{}", app.status);
}

#[test]
fn a_definition_in_a_file_that_is_not_there_is_reported() {
    // The mock names defined.rs; this fixture never creates it.
    let Some(mut app) = lsp_app("goto-missing", "fn main() {\n    helper();\n}\n") else {
        eprintln!("skipped: python not available");
        return;
    };
    ctrl(&mut app, ']');
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && !app.status.contains("not on disk") {
        app.poll_lsp();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(app.status.contains("not on disk"), "{}", app.status);
    assert_eq!(name_of(&app), "code.rs", "stayed put");
}

#[test]
fn asking_for_a_definition_on_a_file_with_no_language_says_so() {
    // Deliberately not a .rs file: whether any real language server is
    // installed varies by machine, but "no language" is decided by the
    // extension alone, so this asserts something true everywhere.
    let root = std::env::temp_dir().join("dxdiary-find-no-lang");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    write(&root, "notes.txt", "some prose\n");
    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);
    app.reveal_and_open(root.join("notes.txt"));

    ctrl(&mut app, ']');
    assert!(app.status.contains("no language"), "{}", app.status);
}

#[test]
fn a_jump_is_refused_rather_than_losing_unsaved_edits() {
    let Some(mut app) = lsp_app("goto-dirty", "fn main() {\n    helper();\n}\n") else {
        eprintln!("skipped: python not available");
        return;
    };
    write(
        &app.tree.root.clone(),
        "defined.rs",
        "a\nb\nfn helper() {}\n",
    );
    app.tree.refresh();

    key(&mut app, KeyCode::Char('i'));
    typed(&mut app, "x");
    key(&mut app, KeyCode::Esc);
    assert!(app.is_dirty());

    ctrl(&mut app, ']');
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && !app.status.contains("unsaved") {
        app.poll_lsp();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(name_of(&app), "code.rs", "{}", app.status);
    assert!(app.is_dirty(), "the edit survived");

    // And nothing was pushed onto the jump list for a jump that never
    // happened, so ctrl-o has nowhere to go.
    ctrl(&mut app, 'o');
    assert!(app.status.contains("no jump"), "{}", app.status);
}

#[test]
fn hover_asks_about_the_column_the_cursor_is_on() {
    // The mock echoes the position back, which is the only way to see from
    // outside that the request carried the cursor and not column zero.
    let Some(mut app) = lsp_app("hover-col", "fn main() {\n    helper();\n}\n") else {
        eprintln!("skipped: python not available");
        return;
    };
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Down);
    for _ in 0..6 {
        key(&mut app, KeyCode::Right);
    }
    key(&mut app, KeyCode::Char('K'));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while std::time::Instant::now() < deadline && !app.status.starts_with("hover at") {
        app.poll_lsp();
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(app.status, "hover at 1:6", "{}", app.status);
}

// --------------------------------------------------------- the caret column

/// Row index and columns of the reversed cell, if there is exactly one.
fn caret_cell(app: &mut App) -> Option<(u16, u16)> {
    let mut term = Terminal::new(TestBackend::new(90, 16)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let mut found = Vec::new();
    for y in 0..buf.area.height {
        for x in 0..buf.area.width {
            if buf[(x, y)]
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)
            {
                found.push((y, x));
            }
        }
    }
    assert!(found.len() <= 1, "more than one caret on screen: {found:?}");
    found.first().copied()
}

#[test]
fn l_and_h_move_the_cursor_along_the_line() {
    let mut app = find_app("columns", "abcdef\n");
    key(&mut app, KeyCode::Tab);

    let start = caret_cell(&mut app).expect("a caret on the focused pane");
    for _ in 0..3 {
        key(&mut app, KeyCode::Char('l'));
    }
    let moved = caret_cell(&mut app).expect("caret still shown");
    assert_eq!(moved.0, start.0, "same row");
    assert_eq!(moved.1, start.1 + 3, "three columns right");

    key(&mut app, KeyCode::Char('h'));
    assert_eq!(caret_cell(&mut app).unwrap().1, start.1 + 2);
}

#[test]
fn the_cursor_stops_at_the_end_of_the_line() {
    let mut app = find_app("clamp-col", "ab\n");
    key(&mut app, KeyCode::Tab);
    for _ in 0..10 {
        key(&mut app, KeyCode::Char('l'));
    }
    // Two characters, so the furthest the cursor goes is just past the last.
    assert_eq!(app.cursor_display_column(), 2);
}

#[test]
fn there_is_no_caret_while_the_tree_has_focus() {
    // Two carets would be a lie about where typing goes.
    let mut app = find_app("no-caret", "abc\n");
    assert_eq!(caret_cell(&mut app), None, "focus starts on the tree");
    key(&mut app, KeyCode::Tab);
    assert!(caret_cell(&mut app).is_some());
}

#[test]
fn the_caret_lands_past_a_tab_at_its_expanded_column() {
    let mut app = find_app("caret-tab", "\tx\n");
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.cursor_display_column(), 0, "on the tab itself");
    key(&mut app, KeyCode::Char('l'));
    assert_eq!(
        app.cursor_display_column(),
        4,
        "one character right is four columns right, across a tab"
    );
}

#[test]
fn the_view_follows_the_cursor_off_the_right_edge() {
    let long = format!("{}needle\n", "x".repeat(300));
    let mut app = find_app("follow", &long);
    key(&mut app, KeyCode::Tab);
    // Draw once so the pane width is known.
    let _ = draw(&mut app);
    assert_eq!(app.doc_scroll_x, 0);

    // Past the end of the word, not onto its first character: the cursor
    // sits at the right edge of the viewport, so only its own column is
    // guaranteed on screen.
    for _ in 0..306 {
        key(&mut app, KeyCode::Char('l'));
    }
    assert!(
        app.doc_scroll_x > 0,
        "the viewport never followed the cursor"
    );
    assert!(
        draw(&mut app).contains("needle"),
        "the cursor scrolled somewhere the text is not"
    );
}

#[test]
fn shift_arrow_pans_without_moving_the_cursor() {
    let long = format!("{}\n", "x".repeat(300));
    let mut app = find_app("pan", &long);
    key(&mut app, KeyCode::Tab);
    let _ = draw(&mut app);

    let before = app.cursor_display_column();
    app.handle(Event::Key(KeyEvent {
        code: KeyCode::Right,
        modifiers: KeyModifiers::SHIFT,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    }));
    assert_eq!(app.doc_scroll_x, 8, "panned a jump");
    assert_eq!(app.cursor_display_column(), before, "cursor stayed put");
}

#[test]
fn moving_down_keeps_the_column_for_the_servers_benefit() {
    // hover and go-to-definition ask about the buffer cursor, so it has to
    // follow the line the eye is on rather than lagging on the old one.
    let mut app = find_app("sync", "abcdef\nabcdef\n");
    key(&mut app, KeyCode::Tab);
    for _ in 0..3 {
        key(&mut app, KeyCode::Char('l'));
    }
    key(&mut app, KeyCode::Down);
    assert_eq!(app.doc_line, 1);
    let buf = app.buffer.as_ref().unwrap();
    assert_eq!(buf.cursor.line, 1, "the buffer cursor came along");
    assert_eq!(buf.cursor.column, 3);
}

// ------------------------------------------------------- repo-wide search

/// A small worktree with the needle in two files, plus a decoy under
/// `target/` that must never be reported.
fn grep_app(tag: &str) -> App {
    let root = std::env::temp_dir().join(format!("dxdiary-grepui-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("target")).unwrap();
    write(&root, "a.rs", "fn one() {}\nfn needle_here() {}\n");
    write(
        &root,
        "src/b.rs",
        "fn two() {}\nfn three() {}\n    needle_indented();\n",
    );
    write(&root, "target/junk.rs", "fn needle_in_build_output() {}\n");

    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);
    app.reveal_and_open(root.join("a.rs"));
    app
}

/// Run a repo-wide search and wait for the worker, which is threaded.
fn grep_for(app: &mut App, query: &str) {
    key(app, KeyCode::Char('*'));
    typed(app, query);
    key(app, KeyCode::Enter);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline {
        if app.poll_grep() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("the repo-wide search never reported: {}", app.status);
}

fn click_at(app: &mut App, col: u16, row: u16) -> bool {
    app.handle(Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }))
}

#[test]
fn star_opens_a_prompt_of_its_own() {
    let mut app = grep_app("prompt");
    key(&mut app, KeyCode::Char('*'));
    assert_eq!(app.mode, dxdiary_tui::Mode::Prompt);
    typed(&mut app, "need");
    assert_eq!(
        app.prompt_line().unwrap(),
        "*need\u{2588}",
        "a different prefix from / so there is no doubt which is open"
    );
}

#[test]
fn a_repo_wide_search_finds_hits_in_several_files() {
    let mut app = grep_app("across");
    grep_for(&mut app, "needle");

    let report = app.grep.as_ref().expect("results");
    assert_eq!(report.len(), 2, "{:?}", report.hits);
    assert_eq!(report.file_count(), 2);
    assert!(app.status.contains("2 hits in 2 files"), "{}", app.status);
}

#[test]
fn build_output_is_not_reported() {
    // The tree hides target/, so a result pointing into it would be a row you
    // cannot reach any other way.
    let mut app = grep_app("skip");
    grep_for(&mut app, "needle");
    assert!(
        app.grep
            .as_ref()
            .unwrap()
            .hits
            .iter()
            .all(|h| !h.path.to_string_lossy().contains("target")),
        "{:?}",
        app.grep.as_ref().unwrap().hits
    );
}

#[test]
fn the_results_take_over_the_tree_pane() {
    let mut app = grep_app("pane");
    let before = draw(&mut app);
    assert!(before.contains("a.rs"), "the tree was showing");

    grep_for(&mut app, "needle");
    let after = draw(&mut app);
    assert!(
        after.contains("2 hits"),
        "the title says what is listed:\n{after}"
    );
    assert!(
        after.contains("needle_here"),
        "the matching line is readable, not just the path:\n{after}"
    );
}

#[test]
fn a_query_with_no_match_does_not_take_the_pane_over() {
    let mut app = grep_app("nomatch");
    grep_for(&mut app, "zzzznope");
    assert!(app.grep.is_none(), "an empty list is not worth a pane");
    assert!(app.status.contains("no match"), "{}", app.status);
    assert!(draw(&mut app).contains("a.rs"), "the tree is still there");
}

#[test]
fn enter_on_a_result_opens_that_file_at_that_line() {
    let mut app = grep_app("open");
    grep_for(&mut app, "needle");

    // The second hit is in src/b.rs, on its third line.
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);

    assert_eq!(name_of(&app), "b.rs", "{}", app.status);
    assert_eq!(app.doc_line, 2, "0-based, so line 3");
}

#[test]
fn opening_a_result_highlights_the_same_query_in_the_file() {
    // Arriving from a result should show every other hit on the way past,
    // rather than landing with nothing marked.
    let mut app = grep_app("highlight");
    grep_for(&mut app, "needle");
    key(&mut app, KeyCode::Enter);

    let search = app.search.as_ref().expect("the file search was seeded");
    assert_eq!(search.query, "needle");
    assert!(!search.is_empty());
}

#[test]
fn clicking_a_result_opens_it() {
    let mut app = grep_app("click");
    grep_for(&mut app, "needle");
    // Draw so the hit map knows where the pane is.
    let _ = draw(&mut app);

    // Row 1 of the pane body is the second result; the border is row 0.
    assert!(click_at(&mut app, 3, 2));
    assert_eq!(name_of(&app), "b.rs", "{}", app.status);
}

#[test]
fn moving_through_results_says_which_file_each_is_in() {
    // The rows show a basename only, so the path has to surface somewhere.
    let mut app = grep_app("paths");
    grep_for(&mut app, "needle");
    key(&mut app, KeyCode::Down);
    assert!(
        app.status.contains("src/b.rs:3"),
        "the full relative path and line: {}",
        app.status
    );
}

#[test]
fn esc_closes_the_results_and_puts_the_tree_cursor_back() {
    let mut app = grep_app("close");
    key(&mut app, KeyCode::Down);
    let before = app.tree_sel;
    assert!(before > 0, "moved somewhere worth restoring");

    grep_for(&mut app, "needle");
    assert_eq!(app.tree_sel, 0, "results start at the top");

    key(&mut app, KeyCode::Esc);
    assert!(app.grep.is_none());
    assert_eq!(app.tree_sel, before, "the tree cursor came back");
    assert!(!app.quit, "esc closed the results rather than quitting");
}

#[test]
fn esc_closes_results_before_it_clears_a_search() {
    // Both are dismissible, and the results are the thing most recently put
    // on screen, so they go first.
    let mut app = grep_app("order");
    grep_for(&mut app, "needle");
    key(&mut app, KeyCode::Enter);
    assert!(app.search.is_some() && app.grep.is_some());

    key(&mut app, KeyCode::Esc);
    assert!(app.grep.is_none(), "results closed");
    assert!(app.search.is_some(), "the file highlight survived");

    key(&mut app, KeyCode::Esc);
    assert!(app.search.is_none(), "then the highlight");
    assert!(!app.quit);
}

#[test]
fn results_scroll_rather_than_clamping_to_the_tree_length() {
    // tree_sel indexes hits while results show; if the clamp still used the
    // file tree's length the cursor could not reach the later hits.
    let root = std::env::temp_dir().join("dxdiary-grepui-many");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    write(&root, "one.rs", &"needle\n".repeat(40));
    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);
    app.reveal_and_open(root.join("one.rs"));

    grep_for(&mut app, "needle");
    assert_eq!(app.tree_rows(), 40, "one file, forty hits");

    key(&mut app, KeyCode::Char('G'));
    assert_eq!(app.tree_sel, 39, "the last hit is reachable");
}

#[test]
fn a_repo_wide_search_works_with_no_file_open() {
    // Unlike / and :, it does not address lines in an open document.
    let root = std::env::temp_dir().join("dxdiary-grepui-nofile");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    write(&root, "a.rs", "needle\n");
    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);

    assert!(app.buffer.is_none(), "nothing open");
    grep_for(&mut app, "needle");
    assert_eq!(app.grep.as_ref().unwrap().len(), 1);
}

#[test]
fn opening_a_result_is_refused_rather_than_losing_unsaved_edits() {
    let mut app = grep_app("dirty");
    key(&mut app, KeyCode::Tab);
    key(&mut app, KeyCode::Char('i'));
    typed(&mut app, "x");
    key(&mut app, KeyCode::Esc);
    assert!(app.is_dirty());

    grep_for(&mut app, "needle");
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Enter);

    assert_eq!(name_of(&app), "a.rs", "stayed on the dirty file");
    assert!(app.is_dirty(), "the edit survived");
    assert!(app.status.contains("unsaved"), "{}", app.status);
}

// ----------------------------------------------------------- the divider

/// Column the pane seam sits at, read off a rendered frame.
///
/// Found rather than computed, so the test checks where the divider actually
/// is rather than agreeing with the layout code about where it should be.
fn seam_column(app: &mut App, width: u16) -> u16 {
    let mut term = Terminal::new(TestBackend::new(width, 16)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    // Row 1 is inside both panes' bodies; the seam is the two border columns.
    for x in 1..width - 1 {
        if buf[(x, 1)].symbol() == "│" {
            return x + 1;
        }
    }
    panic!("no seam found");
}

fn drag(app: &mut App, kind: MouseEventKind, col: u16, row: u16) -> bool {
    app.handle(Event::Mouse(MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }))
}

#[test]
fn the_divider_can_be_dragged_to_a_new_width() {
    let mut app = find_app("drag", "fn main() {}\n");
    let before = seam_column(&mut app, 100);
    assert!(
        (28..=32).contains(&before),
        "default split at ~30%: {before}"
    );

    // Grab the seam, move right, let go.
    assert!(!drag(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        before - 1,
        1
    ));
    drag(&mut app, MouseEventKind::Drag(MouseButton::Left), 60, 1);
    drag(&mut app, MouseEventKind::Up(MouseButton::Left), 60, 1);

    let after = seam_column(&mut app, 100);
    assert!(
        (58..=62).contains(&after),
        "the divider followed the mouse: {before} -> {after}"
    );
}

#[test]
fn a_drag_that_did_not_start_on_the_divider_is_ignored() {
    // Dragging across a pane is a text selection as far as the user is
    // concerned; it must not shove the layout around.
    let mut app = find_app("drag-elsewhere", "fn main() {}\n");
    let before = seam_column(&mut app, 100);

    drag(&mut app, MouseEventKind::Down(MouseButton::Left), 5, 2);
    drag(&mut app, MouseEventKind::Drag(MouseButton::Left), 70, 2);
    drag(&mut app, MouseEventKind::Up(MouseButton::Left), 70, 2);

    assert_eq!(seam_column(&mut app, 100), before, "the split held still");
}

#[test]
fn a_drag_stops_when_the_button_is_released() {
    let mut app = find_app("drag-release", "fn main() {}\n");
    let seam = seam_column(&mut app, 100);

    drag(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        seam - 1,
        1,
    );
    drag(&mut app, MouseEventKind::Drag(MouseButton::Left), 50, 1);
    drag(&mut app, MouseEventKind::Up(MouseButton::Left), 50, 1);
    let settled = seam_column(&mut app, 100);

    // Further motion with no button held must not keep resizing.
    drag(&mut app, MouseEventKind::Drag(MouseButton::Left), 20, 1);
    assert_eq!(seam_column(&mut app, 100), settled, "the drag had ended");
}

#[test]
fn neither_pane_can_be_dragged_away_to_nothing() {
    let mut app = find_app("drag-clamp", "fn main() {}\n");
    let seam = seam_column(&mut app, 100);
    drag(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        seam - 1,
        1,
    );

    drag(&mut app, MouseEventKind::Drag(MouseButton::Left), 0, 1);
    let far_left = seam_column(&mut app, 100);
    assert!(far_left >= 10, "the tree keeps a usable width: {far_left}");

    drag(&mut app, MouseEventKind::Drag(MouseButton::Left), 99, 1);
    let far_right = seam_column(&mut app, 100);
    assert!(far_right <= 81, "the content pane survives: {far_right}");
}

#[test]
fn angle_brackets_move_the_split_from_the_keyboard() {
    // The mouse is an enhancement everywhere else here, and so it is here:
    // an SSH hop into a terminal that does not report drags must still work.
    let mut app = find_app("nudge", "fn main() {}\n");
    let before = seam_column(&mut app, 100);

    key(&mut app, KeyCode::Char('>'));
    let wider = seam_column(&mut app, 100);
    assert!(wider > before, "{before} -> {wider}");

    key(&mut app, KeyCode::Char('<'));
    assert_eq!(seam_column(&mut app, 100), before, "and back again");
}

#[test]
fn nudging_stops_at_the_same_limits_as_dragging() {
    let mut app = find_app("nudge-clamp", "fn main() {}\n");
    for _ in 0..40 {
        key(&mut app, KeyCode::Char('>'));
    }
    assert!(seam_column(&mut app, 100) <= 81);
    for _ in 0..40 {
        key(&mut app, KeyCode::Char('<'));
    }
    assert!(seam_column(&mut app, 100) >= 10);
}

#[test]
fn results_widen_the_pane_only_until_the_width_is_set_by_hand() {
    // The widening is a default, not a rule. A divider that springs back
    // after you move it is worse than one that does not move at all.
    let mut app = grep_app("width-pin");
    let normal = seam_column(&mut app, 100);

    grep_for(&mut app, "needle");
    let widened = seam_column(&mut app, 100);
    assert!(
        widened > normal,
        "results got more room: {normal} -> {widened}"
    );

    key(&mut app, KeyCode::Char('<'));
    let chosen = seam_column(&mut app, 100);
    assert!(chosen < widened, "the nudge took effect while results show");

    key(&mut app, KeyCode::Esc);
    assert!(app.grep.is_none());
    assert_eq!(
        seam_column(&mut app, 100),
        chosen,
        "closing the results left the width where it was put"
    );
}

#[test]
fn resizing_keeps_the_cursor_on_screen() {
    // Both panes clamp their scroll against their own size, so a resize that
    // did not re-clamp could leave the cursor off the bottom.
    let mut app = find_app("resize-clamp", &"x\n".repeat(200));
    key(&mut app, KeyCode::Tab);
    let _ = draw(&mut app);
    key(&mut app, KeyCode::Char('G'));
    let _ = draw(&mut app);

    for _ in 0..6 {
        key(&mut app, KeyCode::Char('>'));
    }
    let shown = draw(&mut app);
    assert!(
        shown.contains(&format!("{}", app.doc_line + 1)),
        "the cursor line is still rendered after the resize"
    );
}
