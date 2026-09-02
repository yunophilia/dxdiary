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
    let dir = std::env::temp_dir().join(format!("crowsnest-gitui-{tag}"));
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
    pin_identity(&root);

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

// --------------------------------------------------------------- blame ---

/// Two commits by different authors, so runs and attribution are both visible.
fn blame_app(tag: &str) -> App {
    let root = std::env::temp_dir().join(format!("crowsnest-blame-{tag}"));
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
        crowsnest_tui::ContentView::Diff,
        "opens on the diff"
    );

    key(&mut app, KeyCode::Char('a'));
    assert_eq!(
        app.view,
        crowsnest_tui::ContentView::File,
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
    let dir = std::env::temp_dir().join("crowsnest-blame-norepo");
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
    let root = std::env::temp_dir().join(format!("crowsnest-edit-{tag}"));
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
    assert_eq!(app.mode, crowsnest_tui::Mode::Normal);
    assert!(!app.is_dirty());
}

#[test]
fn i_enters_insert_mode_and_esc_leaves_it() {
    let mut app = edit_app("modes", "abc\n");
    assert!(key(&mut app, KeyCode::Char('i')));
    assert_eq!(app.mode, crowsnest_tui::Mode::Insert);

    key(&mut app, KeyCode::Esc);
    assert_eq!(app.mode, crowsnest_tui::Mode::Normal);
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
    let root = std::env::temp_dir().join("crowsnest-edit-diffview");
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
    assert_eq!(app.view, crowsnest_tui::ContentView::Diff);

    key(&mut app, KeyCode::Char('i'));
    assert_eq!(app.view, crowsnest_tui::ContentView::File);
    assert_eq!(app.mode, crowsnest_tui::Mode::Insert);
}

#[test]
fn a_binary_file_is_not_editable() {
    let root = std::env::temp_dir().join("crowsnest-edit-binary");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("blob.bin"), [0u8, 1, 2, 0]).unwrap();

    let mut app = App::new(root.clone(), Config::default(), ColorDepth::TrueColor);
    app.reveal_and_open(root.join("blob.bin"));
    assert!(app.buffer.is_none());

    key(&mut app, KeyCode::Char('i'));
    assert_eq!(app.mode, crowsnest_tui::Mode::Normal);
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
