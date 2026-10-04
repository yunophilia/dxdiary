//! Application state and the event loop.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use dxdiary_core::{ColorDepth, Config, Document, FileTree, HitMap, HitTarget, PaneId};
use dxdiary_vcs::{DiffBaseline, ForkPoint, RepoInfo, Status, Vcs};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::Frame;

use crate::panes;
use crate::term::Tui;

/// How long to block waiting for input before looping.
///
/// Nothing animates, so this only bounds how quickly a quit signal is noticed.
/// Redraws happen on events, never on a timer — over SSH every repaint costs a
/// round trip, so idling must be silent.
const POLL: Duration = Duration::from_millis(500);

/// Quiet time after the last keystroke before the language server is told.
///
/// Every keystroke would be correct but wasteful: the server re-analyses on
/// each change, and nobody wants diagnostics for the half-typed word. Long
/// enough to cover a burst of typing, short enough that the markers feel live.
const CHANGE_DEBOUNCE: Duration = Duration::from_millis(150);

/// Editing mode.
///
/// Modal rather than always-insert: dxdiary is a reviewer first, and every
/// single-key command (`d`, `b`, `c`, `a`) would otherwise have to move to a
/// modifier. `i` enters insert, `Esc` leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Insert,
    /// Typing into the status line -- a search or a line number.
    Prompt,
}

/// What an open prompt is collecting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asking {
    Search,
    Goto,
}

impl Asking {
    /// The character that opened it, shown as the prompt's prefix so there is
    /// never a doubt about which one is open.
    fn prefix(self) -> char {
        match self {
            Asking::Search => '/',
            Asking::Goto => ':',
        }
    }
}

/// An open prompt.
struct Prompt {
    asking: Asking,
    input: String,
    /// Cursor line and scroll offset when it opened.
    ///
    /// Search previews as you type, which moves both; cancelling has to put
    /// them back or `/` followed by `esc` would quietly relocate you.
    restore: (usize, usize),
}

/// An action that would throw away unsaved edits, asked for once.
///
/// Asking a second time performs it. A modal "are you sure?" would need its own
/// key handling and render path; repeating the key is the same confirmation
/// with none of that, and it is what the user's fingers do anyway.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Discard {
    Quit,
    Open,
}

/// What the content pane shows for the open file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentView {
    /// The file as it is now.
    File,
    /// Hunks against the current baseline.
    Diff,
}

pub struct App {
    pub config: Config,
    pub depth: ColorDepth,

    pub tree: FileTree,
    pub tree_sel: usize,
    pub tree_scroll: usize,

    pub doc: Option<Document>,
    /// Cursor line in the content pane, 0-based.
    pub doc_line: usize,
    pub doc_scroll_y: usize,
    pub doc_scroll_x: usize,

    /// Editable text for the open file. The single source of truth for content
    /// once a text file is open; `doc` keeps the non-text cases and the path.
    pub buffer: Option<dxdiary_core::Buffer>,
    pub mode: Mode,

    /// Hunks for the open file against the current baseline, when it changed.
    pub diff: Option<dxdiary_vcs::FileDiff>,
    /// Which of the two the content pane is showing.
    pub view: ContentView,

    // --- blame ----------------------------------------------------------
    /// Blame for the open file, once the worker delivers it.
    pub blame: Option<dxdiary_vcs::Blame>,
    /// Whether the blame gutter is shown (`a`).
    pub show_blame: bool,
    /// Pending result from the background thread. Blame takes up to 2.5 s
    /// (spike 0.3), so it never runs on the render thread.
    blame_rx: Option<std::sync::mpsc::Receiver<anyhow::Result<dxdiary_vcs::Blame>>>,

    // --- lsp ------------------------------------------------------------
    /// One server per language, started on first use and kept for the session.
    lsp: std::collections::HashMap<dxdiary_syntax::Language, dxdiary_lsp::Client>,
    /// Diagnostics for the open file, already filtered.
    pub diagnostics: Vec<lsp_types::Diagnostic>,
    /// What the C nested-function filter hid, if anything.
    pub lsp_note: Option<String>,
    /// Request id of an outstanding hover, so its reply can be recognised.
    hover_id: Option<i64>,
    /// When the buffer last changed without the server being told, in the
    /// `now_ms` clock. `None` when the server is up to date.
    lsp_change_at: Option<u64>,
    /// The destructive action that was refused because of unsaved edits.
    discard_armed: Option<Discard>,

    // --- finding --------------------------------------------------------
    /// The live search, if one is running. Public so the pane can shade
    /// matches.
    pub search: Option<dxdiary_core::Search>,
    prompt: Option<Prompt>,

    // --- syntax ---------------------------------------------------------
    highlighter: dxdiary_syntax::Highlighter,
    /// Detected language of the open file, if it is one dxdiary knows.
    pub language: Option<dxdiary_syntax::Language>,
    /// Highlight spans for the file view, one entry per line.
    pub doc_spans: Vec<Vec<dxdiary_syntax::Span>>,
    /// Spans for each side of the diff, indexed by that side's line numbers.
    pub old_spans: Vec<Vec<dxdiary_syntax::Span>>,
    pub new_spans: Vec<Vec<dxdiary_syntax::Span>>,

    pub focus: PaneId,
    pub status: String,
    pub quit: bool,

    // --- git ------------------------------------------------------------
    /// None when the directory is not a repository. Everything git-related
    /// degrades to "off" rather than failing, so dxdiary still opens.
    pub vcs: Option<Box<dyn Vcs>>,
    pub repo: Option<RepoInfo>,
    pub git: Status,
    pub fork: Option<ForkPoint>,
    pub baseline: DiffBaseline,
    /// Path → status letter, for tree decoration.
    pub badges: BTreeMap<PathBuf, char>,
    /// Restrict the tree to changed files only (`c`).
    pub changed_only: bool,
    /// Indices into `tree.rows()` that are currently displayed. Selection is
    /// an index into *this*, so filtering cannot leave the cursor on a hidden
    /// row.
    visible: Vec<usize>,

    hits: HitMap,

    /// Viewport sizes recorded during the last render. Page movement and click
    /// arithmetic need them, and only the renderer knows the real numbers.
    pub(crate) tree_h: usize,
    pub(crate) content_h: usize,
    pub(crate) content_w: usize,
}

impl App {
    pub fn new(root: PathBuf, config: Config, depth: ColorDepth) -> Self {
        let tree = FileTree::new(root);
        let mut app = Self {
            config,
            depth,
            tree,
            tree_sel: 0,
            tree_scroll: 0,
            doc: None,
            doc_line: 0,
            doc_scroll_y: 0,
            doc_scroll_x: 0,
            buffer: None,
            mode: Mode::Normal,
            diff: None,
            view: ContentView::File,
            blame: None,
            show_blame: false,
            blame_rx: None,
            lsp: std::collections::HashMap::new(),
            diagnostics: Vec::new(),
            lsp_note: None,
            hover_id: None,
            lsp_change_at: None,
            discard_armed: None,
            search: None,
            prompt: None,
            highlighter: dxdiary_syntax::Highlighter::new(),
            language: None,
            doc_spans: Vec::new(),
            old_spans: Vec::new(),
            new_spans: Vec::new(),
            focus: PaneId::Tree,
            status: String::from(
                "↑↓ move · enter open · tab pane · b baseline · c changed · q quit",
            ),
            quit: false,
            vcs: None,
            repo: None,
            git: Status::default(),
            fork: None,
            baseline: DiffBaseline::default(),
            badges: BTreeMap::new(),
            changed_only: false,
            visible: Vec::new(),
            hits: HitMap::new(),
            tree_h: 1,
            content_h: 1,
            content_w: 1,
        };
        app.recompute_visible();
        app
    }

    /// Attach a repository. Called after construction so a missing or broken
    /// repo degrades to a plain file browser instead of refusing to start.
    pub fn attach_vcs(&mut self, vcs: Box<dyn Vcs>) {
        self.repo = vcs.info().ok();
        self.vcs = Some(vcs);
        self.refresh_git();

        if let Some(info) = &self.repo {
            if !info.has_commit_graph {
                // Measured 6.7× on merge-base for an 80k-commit repo, and it
                // costs under a second once (spike 0.3).
                self.status = "no commit-graph — press W to write one (much faster history)".into();
            }
        }
    }

    /// Re-read status, badges, and the fork point.
    pub fn refresh_git(&mut self) {
        let Some(vcs) = &self.vcs else { return };

        match vcs.status() {
            Ok(s) => {
                self.badges = s.badges();
                self.git = s;
            }
            Err(e) => self.status = format!("git status failed: {e}"),
        }
        // A branch with no upstream, trunk, or tag simply has no fork point;
        // that is not an error worth interrupting the user over.
        self.fork = vcs.fork_point(None).ok();
        self.recompute_visible();
    }

    /// Rebuild the displayed row list for the current filter.
    fn recompute_visible(&mut self) {
        let keep_all = !self.changed_only || self.badges.is_empty();
        let selected_path = self.selected_path();

        // git reports repository-relative paths; tree rows carry absolute ones.
        // Compare in git's space, not the filesystem's.
        let root = self.tree.root.clone();
        let badges = &self.badges;

        self.visible = self
            .tree
            .rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                if keep_all {
                    return true;
                }
                let rela = row.path.strip_prefix(&root).unwrap_or(&row.path);
                if row.is_dir {
                    // Keep a directory only if something changed inside it,
                    // otherwise filtering leaves a tree of empty folders.
                    badges.keys().any(|p| p.starts_with(rela))
                } else {
                    badges.contains_key(rela)
                }
            })
            .map(|(i, _)| i)
            .collect();

        // Keep the cursor on the same file where possible; otherwise clamp.
        if let Some(path) = selected_path {
            if let Some(pos) = self
                .visible
                .iter()
                .position(|&i| self.tree.rows()[i].path == path)
            {
                self.tree_sel = pos;
            }
        }
        self.tree_sel = self.tree_sel.min(self.visible.len().saturating_sub(1));
        self.clamp_tree();
    }

    /// Row indices currently displayed, in order.
    pub fn visible_rows(&self) -> &[usize] {
        &self.visible
    }

    /// Expand ancestors so `path` is visible, select it, and open it.
    ///
    /// Goes through here rather than calling `FileTree::reveal` directly: the
    /// tree changing without rebuilding the visible list leaves the two out of
    /// step, and the cursor pointing at the wrong row.
    pub fn reveal_and_open(&mut self, path: PathBuf) {
        self.tree.reveal(&path);
        self.recompute_visible();
        if let Some(pos) = self
            .visible
            .iter()
            .position(|&i| self.tree.rows()[i].path == path)
        {
            self.tree_sel = pos;
            self.clamp_tree();
        }
        self.open(path);
    }

    /// Path under the cursor, if any.
    pub fn selected_path(&self) -> Option<PathBuf> {
        let idx = *self.visible.get(self.tree_sel)?;
        Some(self.tree.rows().get(idx)?.path.clone())
    }

    /// Repository-relative path, for matching against git output.
    fn rela(&self, path: &std::path::Path) -> PathBuf {
        path.strip_prefix(&self.tree.root)
            .unwrap_or(path)
            .to_path_buf()
    }

    /// Badge for a tree row, rolling files up so a collapsed directory still
    /// shows that something inside it changed.
    pub fn badge_for(&self, row: &dxdiary_core::Row) -> Option<char> {
        let rela = self.rela(&row.path);
        if let Some(c) = self.badges.get(&rela) {
            return Some(*c);
        }
        if row.is_dir && self.badges.keys().any(|p| p.starts_with(&rela)) {
            return Some('·');
        }
        None
    }

    pub fn run(&mut self, terminal: &mut Tui) -> Result<()> {
        let mut dirty = true;
        while !self.quit {
            if dirty {
                terminal.draw(|f| self.render(f))?;
                dirty = false;
            }

            // While an edit is waiting to be sent, wake early enough to send it.
            let wait = if self.lsp_change_at.is_some() {
                CHANGE_DEBOUNCE
            } else {
                POLL
            };
            if event::poll(wait)? {
                dirty |= self.handle(event::read()?);
                // Drain the burst before redrawing. A scroll wheel emits many
                // events at once and repainting each one is wasted bandwidth
                // over SSH.
                while event::poll(Duration::ZERO)? {
                    dirty |= self.handle(event::read()?);
                }
            }

            // A background blame may have finished while we were blocked on
            // input. Checking here rather than on a timer keeps idling silent.
            dirty |= self.poll_blame();
            dirty |= self.poll_lsp();
        }
        Ok(())
    }

    /// Returns true if the screen needs repainting.
    pub fn handle(&mut self, ev: Event) -> bool {
        match ev {
            Event::Key(k) if k.kind == KeyEventKind::Press => self.on_key(k),
            Event::Mouse(m) => self.on_mouse(m),
            Event::Resize(..) => true,
            _ => false,
        }
    }

    // ---------------------------------------------------------------- keys

    fn on_key(&mut self, k: KeyEvent) -> bool {
        // Ctrl+C quits from any mode; checked before insert so a runaway
        // session is always escapable.
        if k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('c')) {
            self.quit = true;
            return true;
        }
        if self.mode == Mode::Prompt {
            return self.on_prompt_key(k);
        }
        if self.mode == Mode::Insert {
            return self.on_insert_key(k);
        }
        self.on_normal_key(k)
    }

    /// Keys while typing. Everything printable is text; only the editing
    /// controls are special.
    fn on_insert_key(&mut self, k: KeyEvent) -> bool {
        let now = now_ms();
        let Some(buf) = &mut self.buffer else {
            self.mode = Mode::Normal;
            return true;
        };

        match k.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.status = "normal".into();
            }
            KeyCode::Char(c) if !k.modifiers.contains(KeyModifiers::CONTROL) => {
                buf.insert(&c.to_string(), now)
            }
            KeyCode::Enter => buf.insert(
                "
", now,
            ),
            KeyCode::Tab => buf.insert("	", now),
            KeyCode::Backspace => buf.backspace(now),
            KeyCode::Delete => buf.delete(now),
            KeyCode::Left => buf.move_left(),
            KeyCode::Right => buf.move_right(),
            KeyCode::Up => buf.move_vertical(-1),
            KeyCode::Down => buf.move_vertical(1),
            KeyCode::Home => buf.move_line_start(),
            KeyCode::End => buf.move_line_end(),
            KeyCode::Char('s') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                return self.save();
            }
            _ => return false,
        }
        self.sync_after_edit();
        true
    }

    /// Keep the view, highlighting, and the language server in step with an
    /// edit. Re-highlighting the whole file per keystroke is fine at terminal
    /// sizes and avoids an incremental-parse cache that could go stale.
    fn sync_after_edit(&mut self) {
        let Some(buf) = &self.buffer else { return };
        self.doc_line = buf.cursor.line;

        if let (Some(lang), Some(b)) = (self.language, &self.buffer) {
            let text = b.text();
            self.doc_spans = self.highlighter.highlight(lang, &text);

            // The server is told after a pause, not per keystroke; until then
            // the existing markers stay. They may sit a line off for a moment
            // after an inserted newline, which is what every editor shows, and
            // far less confusing than markers that vanish while typing.
            self.lsp_change_at = Some(now_ms());
        }
        self.clamp_content();
    }

    /// Write the buffer to disk.
    fn save(&mut self) -> bool {
        let Some(buf) = &mut self.buffer else {
            self.status = "nothing to save".into();
            return true;
        };
        if !buf.dirty {
            self.status = "no changes".into();
            return true;
        }
        match buf.save() {
            Ok(()) => {
                let path = buf.path.clone();
                self.status = format!("wrote {}", path.display());
                // The file changed on disk, so git status and the diff are
                // both stale.
                self.refresh_git();
                self.load_diff(&path);
                self.notify_lsp_save(&path);
            }
            Err(e) => self.status = format!("save failed: {e}"),
        }
        self.discard_armed = None;
        true
    }

    /// May `action` go ahead, given the state of the buffer?
    ///
    /// Yes when there is nothing to lose, or when this same action was refused
    /// last time -- the repeat is the confirmation. Otherwise arm it and say
    /// why.
    fn may_discard(&mut self, action: Discard) -> bool {
        if !self.is_dirty() || self.discard_armed == Some(action) {
            self.discard_armed = None;
            return true;
        }
        self.discard_armed = Some(action);
        let name = self
            .buffer
            .as_ref()
            .and_then(|b| b.path.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let again = match action {
            Discard::Quit => "quit again to discard",
            Discard::Open => "open again to discard",
        };
        self.status = format!("{name} has unsaved changes — ctrl-s to save, {again}");
        false
    }

    fn on_normal_key(&mut self, k: KeyEvent) -> bool {
        // Keyboard must remain sufficient on its own: mouse forwarding through
        // herdr works (spike 0.2), but SSH into an unknown terminal may not.
        if k.modifiers.contains(KeyModifiers::CONTROL) {
            return match k.code {
                KeyCode::Char('s') => self.save(),
                KeyCode::Char('r') => self.redo(),
                _ => false,
            };
        }

        match k.code {
            KeyCode::Char('q') => {
                if self.may_discard(Discard::Quit) {
                    self.quit = true;
                }
                true
            }
            // Esc dismisses before it quits. With a search on screen the
            // reflex is to press it to clear the highlight, and losing the
            // session to that would be its own small disaster.
            KeyCode::Esc => {
                if self.search.take().is_some() {
                    self.status = "search cleared".into();
                } else if self.may_discard(Discard::Quit) {
                    self.quit = true;
                }
                true
            }
            KeyCode::Char('/') => self.begin_prompt(Asking::Search),
            KeyCode::Char(':') => self.begin_prompt(Asking::Goto),
            KeyCode::Char('n') => self.step_search(true),
            KeyCode::Char('N') => self.step_search(false),
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    PaneId::Tree => PaneId::Content,
                    PaneId::Content => PaneId::Tree,
                };
                true
            }
            KeyCode::Char('r') => {
                self.tree.refresh();
                self.refresh_git();
                self.status = "refreshed".into();
                true
            }
            KeyCode::Char('b') => self.cycle_baseline(),
            KeyCode::Char('d') => self.toggle_view(),
            KeyCode::Char('a') => self.toggle_blame(),
            KeyCode::Char('i') => self.enter_insert(),
            KeyCode::Char('u') => self.undo(),
            KeyCode::Char('x') => self.delete_char(),
            KeyCode::Char('D') => self.delete_line(),
            KeyCode::Char('K') => self.request_hover(),
            KeyCode::Char('c') => self.toggle_changed_only(),
            KeyCode::Char(']') => self.jump_changed(true),
            KeyCode::Char('[') => self.jump_changed(false),
            KeyCode::Char('W') => self.write_commit_graph(),
            KeyCode::Down | KeyCode::Char('j') => self.move_by(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_by(-1),
            KeyCode::PageDown => self.move_by(self.page() as isize),
            KeyCode::PageUp => self.move_by(-(self.page() as isize)),
            KeyCode::Home | KeyCode::Char('g') => self.move_to(0),
            KeyCode::End | KeyCode::Char('G') => self.move_to(usize::MAX),
            KeyCode::Enter | KeyCode::Char(' ') => self.activate(),
            KeyCode::Right | KeyCode::Char('l') => match self.focus {
                PaneId::Tree => self.expand_selected(),
                PaneId::Content => {
                    self.doc_scroll_x = self.doc_scroll_x.saturating_add(8);
                    self.clamp_content();
                    true
                }
            },
            KeyCode::Left | KeyCode::Char('h') => match self.focus {
                PaneId::Tree => self.collapse_selected(),
                PaneId::Content => {
                    self.doc_scroll_x = self.doc_scroll_x.saturating_sub(8);
                    true
                }
            },
            _ => false,
        }
    }

    fn page(&self) -> usize {
        match self.focus {
            PaneId::Tree => self.tree_h.saturating_sub(1).max(1),
            PaneId::Content => self.content_h.saturating_sub(1).max(1),
        }
    }

    fn move_by(&mut self, delta: isize) -> bool {
        let (cur, max) = match self.focus {
            PaneId::Tree => (self.tree_sel, self.visible.len().saturating_sub(1)),
            PaneId::Content => (self.doc_line, self.content_rows().saturating_sub(1)),
        };
        let next = if delta < 0 {
            cur.saturating_sub(delta.unsigned_abs())
        } else {
            (cur + delta as usize).min(max)
        };
        self.move_to(next)
    }

    fn move_to(&mut self, index: usize) -> bool {
        match self.focus {
            PaneId::Tree => {
                let max = self.visible.len().saturating_sub(1);
                let next = index.min(max);
                if next == self.tree_sel {
                    return false;
                }
                self.tree_sel = next;
                self.clamp_tree();
            }
            PaneId::Content => {
                let max = self.content_rows().saturating_sub(1);
                let next = index.min(max);
                if next == self.doc_line {
                    return false;
                }
                self.doc_line = next;
                self.clamp_content();
            }
        }
        true
    }

    // ---------------------------------------------------------------- mouse

    fn on_mouse(&mut self, m: MouseEvent) -> bool {
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => self.on_click(m.column, m.row),
            MouseEventKind::ScrollDown => self.scroll_at(m.column, m.row, 3),
            MouseEventKind::ScrollUp => self.scroll_at(m.column, m.row, -3),
            _ => false,
        }
    }

    fn on_click(&mut self, col: u16, row: u16) -> bool {
        let Some(hit) = self.hits.resolve(col, row) else {
            return false;
        };

        match hit.target {
            HitTarget::Header(pane) => {
                self.focus = pane;
                true
            }
            HitTarget::Divider => false,
            HitTarget::TreeBody => {
                self.focus = PaneId::Tree;
                let index = self.tree_scroll + hit.local_row as usize;
                if index >= self.visible.len() {
                    return true; // clicked past the last row; just take focus
                }
                self.tree_sel = index;
                // Clicking does the obvious thing: fold a directory, open a
                // file. No double-click, which is unreliable over SSH anyway.
                self.activate();
                true
            }
            HitTarget::ContentBody => {
                self.focus = PaneId::Content;
                let line = self.doc_scroll_y + hit.local_row as usize;
                self.doc_line = line.min(self.content_rows().saturating_sub(1));
                true
            }
        }
    }

    fn scroll_at(&mut self, col: u16, row: u16, delta: isize) -> bool {
        // Scroll whatever is under the pointer, not whatever has focus — that
        // is what every other application does.
        let target = self.hits.resolve(col, row).map(|h| h.target);
        match target {
            Some(HitTarget::TreeBody) => {
                let max = self.visible.len().saturating_sub(self.tree_h);
                self.tree_scroll = shift(self.tree_scroll, delta, max);
                true
            }
            Some(HitTarget::ContentBody) => {
                let max = self.content_rows().saturating_sub(self.content_h);
                self.doc_scroll_y = shift(self.doc_scroll_y, delta, max);
                true
            }
            _ => false,
        }
    }

    // ------------------------------------------------------------- actions

    /// Index into `tree.rows()` for the current selection.
    fn real_row(&self) -> Option<usize> {
        self.visible.get(self.tree_sel).copied()
    }

    fn activate(&mut self) -> bool {
        if self.focus == PaneId::Content {
            return false;
        }
        let Some(idx) = self.real_row() else {
            return false;
        };
        if self.tree.toggle(idx) {
            self.recompute_visible();
            return true;
        }
        // Not a directory, so open it.
        let Some(row) = self.tree.rows().get(idx) else {
            return false;
        };
        let path = row.path.clone();
        self.open(path);
        true
    }

    pub fn open(&mut self, path: PathBuf) {
        if !self.may_discard(Discard::Open) {
            return;
        }
        self.notify_lsp_close();

        let doc = Document::load(&path, self.config.max_file_bytes);
        self.status = match &doc {
            Document::Text(d) => format!("{} · {} lines", path.display(), d.line_count()),
            Document::Binary { bytes, .. } => format!("{} · binary, {bytes} bytes", path.display()),
            Document::TooLarge { bytes, .. } => {
                format!("{} · {bytes} bytes, too large to display", path.display())
            }
            Document::Error { message, .. } => format!("{}: {message}", path.display()),
        };
        self.doc = Some(doc);
        self.doc_line = 0;
        self.doc_scroll_y = 0;
        self.doc_scroll_x = 0;

        // Blame belongs to the previous file; a stale gutter would attribute
        // the wrong commits to the new one.
        self.blame = None;
        self.blame_rx = None;

        self.buffer = match &self.doc {
            Some(Document::Text(d)) => Some(dxdiary_core::Buffer::from_str(
                &path,
                &(d.lines().join(
                    "
",
                ) + if d.no_trailing_newline {
                    ""
                } else {
                    "
"
                }),
            )),
            _ => None,
        };
        self.mode = Mode::Normal;

        self.highlight_doc(&path);
        self.load_diff(&path);
        self.notify_lsp_open(&path);

        if self.show_blame {
            let root = self.tree.root.clone();
            self.blame_rx = Some(dxdiary_vcs::blame::spawn(root, path));
        }
    }

    /// Syntax-highlight the open file.
    fn highlight_doc(&mut self, path: &std::path::Path) {
        self.language = dxdiary_syntax::Language::from_path(path);
        self.doc_spans.clear();

        let (Some(lang), Some(Document::Text(d))) = (self.language, &self.doc) else {
            return;
        };
        let source = d.lines().join("\n");
        self.doc_spans = self.highlighter.highlight(lang, &source);
    }

    /// Highlight both sides of the diff.
    ///
    /// Each side is highlighted as a complete file rather than line by line, so
    /// multi-line strings and block comments come out right; the renderer then
    /// looks spans up by the line numbers already on each [`DiffLine`].
    fn highlight_diff(&mut self) {
        self.old_spans.clear();
        self.new_spans.clear();

        let (Some(lang), Some(diff)) = (self.language, &self.diff) else {
            return;
        };
        if diff.binary {
            return;
        }
        let (old, new) = (diff.old_text.clone(), diff.new_text.clone());
        self.old_spans = self.highlighter.highlight(lang, &old);
        self.new_spans = self.highlighter.highlight(lang, &new);
    }

    /// Compute hunks for `path`, and show them if there are any.
    ///
    /// Opening a changed file lands on the diff, because that is what you came
    /// for; an unchanged file stays on the file view since there is nothing to
    /// show. `d` overrides either way.
    fn load_diff(&mut self, path: &std::path::Path) {
        self.diff = self
            .vcs
            .as_ref()
            .and_then(|v| v.file_diff(path, &self.baseline).ok())
            .filter(|d| !d.is_empty() || d.binary);

        self.view = if self.diff.is_some() {
            ContentView::Diff
        } else {
            ContentView::File
        };
        self.highlight_diff();

        if let Some(d) = &self.diff {
            self.status = format!("{} · {}", path.display(), d.summary());
        }
    }

    /// Toggle between the file and its diff (`d`).
    fn toggle_view(&mut self) -> bool {
        let Some(doc) = &self.doc else {
            return false;
        };
        if self.diff.is_none() {
            // Recompute rather than assume: the baseline may have changed
            // since this file was opened.
            let path = doc.path().to_path_buf();
            self.load_diff(&path);
            if self.diff.is_none() {
                self.status = "no changes against this baseline".into();
                return true;
            }
        }
        self.view = match self.view {
            ContentView::File => ContentView::Diff,
            ContentView::Diff => ContentView::File,
        };
        self.doc_scroll_y = 0;
        self.doc_line = 0;
        true
    }

    /// Toggle the blame gutter (`a`, for annotate).
    ///
    /// Only meaningful in the file view: a diff already says which commit each
    /// side came from, and a blame column beside it would be noise.
    fn toggle_blame(&mut self) -> bool {
        if self.vcs.is_none() {
            self.status = "not a git repository".into();
            return true;
        }
        let Some(doc) = &self.doc else {
            self.status = "no file open".into();
            return true;
        };

        self.show_blame = !self.show_blame;
        if !self.show_blame {
            self.status = "blame off".into();
            return true;
        }

        // Showing blame forces the file view; the diff view has no gutter for it.
        self.view = ContentView::File;

        if self.blame.is_some() {
            self.status = "blame on".into();
            return true;
        }

        let path = doc.path().to_path_buf();
        let root = self.tree.root.clone();
        self.blame_rx = Some(dxdiary_vcs::blame::spawn(root, path));
        self.status = "blaming…".into();
        true
    }

    /// Collect a finished blame, if the worker has one.
    ///
    /// Called from the event loop rather than from `render`, so a slow blame
    /// delays nothing — the file is already on screen. Public so tests and the
    /// screenshot example can await the worker without a live event loop.
    pub fn poll_blame(&mut self) -> bool {
        let Some(rx) = &self.blame_rx else {
            return false;
        };
        match rx.try_recv() {
            Ok(Ok(blame)) => {
                self.status = format!("blame · {} lines", blame.lines.len());
                self.blame = Some(blame);
                self.blame_rx = None;
                true
            }
            Ok(Err(e)) => {
                self.status = format!("blame failed: {e}");
                self.show_blame = false;
                self.blame_rx = None;
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.blame_rx = None;
                false
            }
        }
    }

    /// Block until a pending blame arrives, or the deadline passes.
    ///
    /// For tests and batch rendering only — the interactive path polls instead,
    /// precisely so that it never blocks.
    pub fn await_blame(&mut self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while self.blame_rx.is_some() && std::time::Instant::now() < deadline {
            if self.poll_blame() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.blame.is_some()
    }

    // ----------------------------------------------------------------- lsp

    /// Start (or reuse) a server for `lang` and tell it about the open file.
    ///
    /// A missing server is not an error: highlighting and outline come from
    /// tree-sitter regardless, so the file stays fully usable.
    fn notify_lsp_open(&mut self, path: &std::path::Path) {
        self.diagnostics.clear();
        self.lsp_note = None;

        let Some(lang) = self.language else { return };
        let Some(text) = self.current_text() else {
            return;
        };

        if !self.lsp.contains_key(&lang) {
            let Some(spec) = dxdiary_lsp::spec_for(lang) else {
                return;
            };
            // Verify it runs, not just that it exists: a rustup shim is on
            // PATH and executable even when the component was never installed.
            if dxdiary_lsp::registry::find_on_path(spec.command).is_none() {
                self.status = format!(
                    "{} not installed — tree-sitter only (dxdiary --doctor)",
                    spec.command
                );
                return;
            }
            if let Err(why) = dxdiary_lsp::registry::probe(spec.command) {
                self.status = format!("{} does not run ({why}) — tree-sitter only", spec.command);
                return;
            }
            match dxdiary_lsp::Client::spawn(spec, &self.tree.root) {
                Ok(client) => {
                    self.lsp.insert(lang, client);
                }
                Err(e) => {
                    self.status = format!("{} failed to start: {e}", spec.command);
                    return;
                }
            }
        }

        if let Some(client) = self.lsp.get(&lang) {
            let _ = client.did_open(path, lang.name(), &text);
        }
    }

    /// The text the server should be looking at: the buffer once a text file
    /// is open, since edits land there and nowhere else.
    fn current_text(&self) -> Option<String> {
        self.buffer.as_ref().map(|b| b.text())
    }

    /// Send a pending edit now if it has rested for [`CHANGE_DEBOUNCE`], or
    /// regardless when `force` -- a save must not race its own change.
    fn flush_lsp_change(&mut self, force: bool) {
        let Some(at) = self.lsp_change_at else { return };
        if !force && now_ms().saturating_sub(at) < CHANGE_DEBOUNCE.as_millis() as u64 {
            return;
        }
        self.lsp_change_at = None;
        let (Some(lang), Some(buf)) = (self.language, &self.buffer) else {
            return;
        };
        if let Some(client) = self.lsp.get(&lang) {
            let _ = client.did_change(&buf.path, &buf.text());
        }
    }

    fn notify_lsp_save(&mut self, path: &std::path::Path) {
        self.flush_lsp_change(true);
        let Some(lang) = self.language else { return };
        if let Some(client) = self.lsp.get(&lang) {
            let _ = client.did_save(path);
        }
    }

    /// Close the open file with its server, if it had one.
    fn notify_lsp_close(&mut self) {
        // An unsent edit belongs to a file that is going away.
        self.lsp_change_at = None;
        let (Some(lang), Some(doc)) = (self.language, &self.doc) else {
            return;
        };
        if let Some(client) = self.lsp.get(&lang) {
            let _ = client.did_close(doc.path());
        }
    }

    /// Use an already-running client for `lang` rather than spawning one.
    ///
    /// For tests, which drive the app against a mock server: no real language
    /// server can be assumed on a build machine.
    pub fn attach_lsp(&mut self, lang: dxdiary_syntax::Language, client: dxdiary_lsp::Client) {
        self.lsp.insert(lang, client);
    }

    /// The running client for `lang`, if any. For tests.
    pub fn lsp_client(&self, lang: dxdiary_syntax::Language) -> Option<&dxdiary_lsp::Client> {
        self.lsp.get(&lang)
    }

    /// Collect anything the servers have sent.
    ///
    /// Polled from the event loop, never from `render`: a server can go quiet
    /// for seconds and a frame must not wait on it. Public so tests and the
    /// screenshot example can drive it without a live loop.
    pub fn poll_lsp(&mut self) -> bool {
        self.flush_lsp_change(false);
        let Some(lang) = self.language else {
            return false;
        };
        let Some(client) = self.lsp.get(&lang) else {
            return false;
        };

        let open_uri = self
            .doc
            .as_ref()
            .map(|d| dxdiary_lsp::path_to_uri(d.path()));

        let mut dirty = false;
        let mut incoming: Option<Vec<lsp_types::Diagnostic>> = None;
        let mut hover: Option<String> = None;

        while let Ok(event) = client.events.try_recv() {
            match event {
                dxdiary_lsp::Event::Diagnostics { uri, items } => {
                    // Servers publish for every file they index; only the one
                    // on screen is of interest.
                    if open_uri.as_deref() == Some(uri.as_str()) {
                        incoming = Some(items);
                    }
                }
                dxdiary_lsp::Event::Response { id, result } if Some(id) == self.hover_id => {
                    self.hover_id = None;
                    hover = hover_text(&result);
                    dirty = true;
                }
                dxdiary_lsp::Event::Error { id, message } if Some(id) == self.hover_id => {
                    self.hover_id = None;
                    self.status = format!("hover failed: {message}");
                    dirty = true;
                }
                dxdiary_lsp::Event::Closed(why) => {
                    self.status = format!("language server stopped: {why}");
                    self.lsp.remove(&lang);
                    dirty = true;
                    break;
                }
                _ => {}
            }
        }

        if let Some(items) = incoming {
            // The C filter: clangd's output inside a function containing a GCC
            // nested function is parse-recovery noise (DESIGN.md §2).
            let source = self.current_text().unwrap_or_default();
            let filtered = dxdiary_lsp::filter(lang, &source, items);
            self.lsp_note = filtered.note();
            self.diagnostics = filtered.kept;
            if let Some(note) = &self.lsp_note {
                self.status = note.clone();
            }
            dirty = true;
        }

        if let Some(text) = hover {
            self.status = text;
        }
        dirty
    }

    /// Poll until diagnostics arrive for the open file, or the deadline passes.
    ///
    /// For tests and batch rendering only. A real server may index for seconds
    /// before publishing anything, which is exactly why the interactive path
    /// polls instead of waiting.
    pub fn await_diagnostics(&mut self, timeout: Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            self.poll_lsp();
            if !self.diagnostics.is_empty() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// Ask for hover information at the cursor (`K`).
    fn request_hover(&mut self) -> bool {
        let Some(lang) = self.language else {
            self.status = "no language detected for this file".into();
            return true;
        };
        let Some(doc) = &self.doc else { return false };
        let path = doc.path().to_path_buf();

        let Some(client) = self.lsp.get(&lang) else {
            self.status = "no language server for this file (dxdiary --doctor)".into();
            return true;
        };
        match client.hover(&path, self.doc_line as u32, 0) {
            Ok(id) => {
                self.hover_id = Some(id);
                self.status = "hover…".into();
            }
            Err(e) => self.status = format!("hover failed: {e}"),
        }
        true
    }

    /// Diagnostic severity marker for a line, for the gutter.
    pub fn diagnostic_at(&self, line: usize) -> Option<lsp_types::DiagnosticSeverity> {
        self.diagnostics
            .iter()
            .filter(|d| d.range.start.line as usize <= line && line <= d.range.end.line as usize)
            .filter_map(|d| d.severity)
            // Most severe wins the gutter. `DiagnosticSeverity` has no `Ord`
            // and a private field, so rank explicitly.
            .min_by_key(|s| match *s {
                lsp_types::DiagnosticSeverity::ERROR => 0,
                lsp_types::DiagnosticSeverity::WARNING => 1,
                lsp_types::DiagnosticSeverity::INFORMATION => 2,
                _ => 3,
            })
    }

    // ------------------------------------------------------------- finding

    /// The prompt as it should appear on the status line, if one is open.
    pub fn prompt_line(&self) -> Option<String> {
        let prompt = self.prompt.as_ref()?;
        // A block for the caret: the terminal's own cursor is parked out of
        // the way in raw mode, so the prompt has to draw its own.
        Some(format!(
            "{}{}\u{2588}",
            prompt.asking.prefix(),
            prompt.input
        ))
    }

    fn begin_prompt(&mut self, asking: Asking) -> bool {
        if self.text_line_count() == 0 {
            self.status = "no file open".into();
            return true;
        }
        // Both answers address file lines, so neither means anything against
        // a diff. Blame sets the same precedent.
        self.view = ContentView::File;
        self.focus = PaneId::Content;
        self.mode = Mode::Prompt;
        self.prompt = Some(Prompt {
            asking,
            input: String::new(),
            restore: (self.doc_line, self.doc_scroll_y),
        });
        true
    }

    fn on_prompt_key(&mut self, k: KeyEvent) -> bool {
        let Some(prompt) = &mut self.prompt else {
            self.mode = Mode::Normal;
            return true;
        };
        match k.code {
            KeyCode::Esc => {
                let (line, scroll) = prompt.restore;
                self.prompt = None;
                self.mode = Mode::Normal;
                self.search = None;
                self.doc_line = line;
                self.doc_scroll_y = scroll;
                self.status = "cancelled".into();
            }
            KeyCode::Enter => {
                let Prompt {
                    asking,
                    input,
                    restore,
                } = self.prompt.take().expect("just matched");
                self.mode = Mode::Normal;
                match asking {
                    Asking::Search => self.commit_search(&input, restore.0),
                    Asking::Goto => self.commit_goto(&input),
                }
            }
            KeyCode::Backspace => {
                prompt.input.pop();
                self.preview();
            }
            KeyCode::Char(c) => {
                prompt.input.push(c);
                self.preview();
            }
            _ => return false,
        }
        true
    }

    /// Update as the query is typed, so a search is live rather than modal.
    fn preview(&mut self) {
        let Some(prompt) = &self.prompt else { return };
        if prompt.asking != Asking::Search {
            return;
        }
        let (query, from) = (prompt.input.clone(), prompt.restore.0);
        self.run_search(&query, from);
    }

    fn commit_search(&mut self, query: &str, from: usize) {
        if query.is_empty() {
            self.search = None;
            self.status = "search cancelled".into();
            return;
        }
        let n = self.run_search(query, from);
        self.status = if n == 0 {
            format!("no match for {query:?}")
        } else {
            format!("{n} match{} for {query:?} · n/N to step", plural(n))
        };
    }

    /// Search the open file and select the first hit at or after `from`.
    ///
    /// Returns the number of matches. An empty query or no match leaves
    /// nothing highlighted rather than an empty selection, so the pane never
    /// has to reason about a search that matches nothing.
    fn run_search(&mut self, query: &str, from: usize) -> usize {
        if query.is_empty() {
            self.search = None;
            return 0;
        }
        let lines = self.view_lines().unwrap_or_default();
        let mut found = dxdiary_core::Search::new(&lines, query);
        if found.is_empty() {
            self.search = None;
            return 0;
        }
        found.select_from(from);
        let n = found.len();
        self.search = Some(found);
        self.jump_to_match();
        n
    }

    fn commit_goto(&mut self, input: &str) {
        let trimmed = input.trim();
        match trimmed.parse::<usize>() {
            Ok(0) | Err(_) if !trimmed.is_empty() => {
                self.status = format!("not a line number: {trimmed:?}")
            }
            Ok(n) => {
                let total = self.text_line_count();
                let target = n.min(total.max(1)) - 1;
                self.doc_line = target;
                self.clamp_content();
                self.status = if n > total {
                    format!("line {n} is past the end · {total} lines")
                } else {
                    format!("line {n}")
                };
            }
            Err(_) => self.status = "cancelled".into(),
        }
    }

    fn step_search(&mut self, forward: bool) -> bool {
        let Some(search) = &mut self.search else {
            self.status = "nothing to step through · / to search".into();
            return true;
        };
        if search.is_empty() {
            self.status = "no matches".into();
            return true;
        }
        let wrapped = search.step(forward);
        let at = search.current + 1;
        let total = search.len();
        self.jump_to_match();
        self.status = if wrapped {
            format!("{at}/{total} · wrapped")
        } else {
            format!("{at}/{total}")
        };
        true
    }

    /// Put the cursor on the selected match.
    fn jump_to_match(&mut self) {
        let Some(m) = self.search.as_ref().and_then(|s| s.selected()) else {
            return;
        };
        self.doc_line = m.line;
        // Scroll sideways far enough that the match is on screen, which a
        // long line otherwise hides off to the right.
        if let Some(buf) = &mut self.buffer {
            buf.move_to(m.line, m.start);
        }
        self.clamp_content();
    }

    // ------------------------------------------------------------- editing

    /// Enter insert mode (`i`).
    fn enter_insert(&mut self) -> bool {
        if self.buffer.is_none() {
            self.status = "this file is not editable".into();
            return true;
        }
        // Editing applies to the file, not to a diff of it.
        self.view = ContentView::File;
        self.focus = PaneId::Content;
        self.mode = Mode::Insert;
        if let Some(buf) = &mut self.buffer {
            let line = self.doc_line;
            buf.move_to(line, buf.cursor.column);
        }
        self.status = "-- INSERT --  esc to leave · ctrl-s save".into();
        true
    }

    fn undo(&mut self) -> bool {
        let Some(buf) = &mut self.buffer else {
            return false;
        };
        self.status = if buf.undo() {
            "undo".into()
        } else {
            "nothing to undo".into()
        };
        self.sync_after_edit();
        true
    }

    fn redo(&mut self) -> bool {
        let Some(buf) = &mut self.buffer else {
            return false;
        };
        self.status = if buf.redo() {
            "redo".into()
        } else {
            "nothing to redo".into()
        };
        self.sync_after_edit();
        true
    }

    fn delete_char(&mut self) -> bool {
        let Some(buf) = &mut self.buffer else {
            return false;
        };
        let line = self.doc_line;
        buf.move_to(line, buf.cursor.column);
        buf.delete(now_ms());
        self.sync_after_edit();
        true
    }

    fn delete_line(&mut self) -> bool {
        let Some(buf) = &mut self.buffer else {
            return false;
        };
        let line = self.doc_line;
        buf.move_to(line, 0);
        buf.delete_line(now_ms());
        self.sync_after_edit();
        true
    }

    /// True when the open file has unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.buffer.as_ref().is_some_and(|b| b.dirty)
    }

    /// Lines to render in the file view — from the buffer when there is one,
    /// so an edit is visible immediately and there is only ever one copy of
    /// the text.
    pub fn view_lines(&self) -> Option<Vec<String>> {
        self.buffer.as_ref().map(|b| b.lines())
    }

    /// Rows the content pane can scroll through, for the current view.
    pub fn content_rows(&self) -> usize {
        match (self.view, &self.diff) {
            (ContentView::Diff, Some(d)) => d.display_rows(),
            _ => self
                .buffer
                .as_ref()
                .map(|b| b.line_count())
                .unwrap_or_else(|| self.text_line_count()),
        }
    }

    fn expand_selected(&mut self) -> bool {
        let Some(idx) = self.real_row() else {
            return false;
        };
        let Some(row) = self.tree.rows().get(idx) else {
            return false;
        };
        if row.is_dir && !row.expanded {
            self.tree.toggle(idx);
            self.recompute_visible();
            return true;
        }
        // Already open, or a file: step into it, mirroring file-manager keys.
        self.move_by(1)
    }

    fn collapse_selected(&mut self) -> bool {
        let Some(idx) = self.real_row() else {
            return false;
        };
        let Some(row) = self.tree.rows().get(idx) else {
            return false;
        };
        if row.is_dir && row.expanded {
            self.tree.toggle(idx);
            self.recompute_visible();
            return true;
        }
        // Otherwise jump to the parent directory's row.
        let depth = row.depth;
        if depth == 0 {
            return false;
        }
        let parent = self.visible[..self.tree_sel]
            .iter()
            .rposition(|&i| self.tree.rows()[i].depth < depth);
        if let Some(parent) = parent {
            self.tree_sel = parent;
            self.clamp_tree();
            return true;
        }
        false
    }

    // ----------------------------------------------------------------- git

    /// Cycle the diff baseline (`b`).
    fn cycle_baseline(&mut self) -> bool {
        if self.vcs.is_none() {
            self.status = "not a git repository".into();
            return true;
        }
        self.baseline = self.baseline.next();

        let detail = match (&self.baseline, &self.fork) {
            (DiffBaseline::ForkPoint, Some(f)) => {
                format!(
                    "{} — {} @ {}",
                    self.baseline.label(),
                    f.base,
                    &f.oid[..8.min(f.oid.len())]
                )
            }
            (DiffBaseline::ForkPoint, None) => {
                "vs fork point — no upstream, trunk, or tag to compare against".into()
            }
            _ => self.baseline.label(),
        };

        let count = self
            .vcs
            .as_ref()
            .and_then(|v| v.changes(&self.baseline).ok())
            .map(|c| c.len());
        self.status = match count {
            Some(n) => format!("{detail} · {n} file(s)"),
            None => detail,
        };

        // The open file's hunks are relative to the baseline, so they are now
        // stale. Recompute rather than showing a diff against the old one.
        if let Some(doc) = &self.doc {
            let path = doc.path().to_path_buf();
            let status = std::mem::take(&mut self.status);
            self.load_diff(&path);
            self.status = status;
            self.doc_scroll_y = 0;
            self.doc_line = 0;
        }
        true
    }

    /// Toggle the changed-files-only filter (`c`).
    fn toggle_changed_only(&mut self) -> bool {
        if self.badges.is_empty() {
            self.status = "no changes to filter to".into();
            return true;
        }
        self.changed_only = !self.changed_only;
        self.recompute_visible();
        self.status = if self.changed_only {
            format!("showing {} changed file(s)", self.badges.len())
        } else {
            "showing all files".into()
        };
        true
    }

    /// Jump to the next or previous changed file (`]` / `[`).
    fn jump_changed(&mut self, forward: bool) -> bool {
        if self.badges.is_empty() {
            self.status = "no changed files".into();
            return true;
        }
        let changed: Vec<usize> = self
            .visible
            .iter()
            .enumerate()
            .filter(|(_, &i)| {
                let row = &self.tree.rows()[i];
                !row.is_dir && self.badge_for(row).is_some()
            })
            .map(|(pos, _)| pos)
            .collect();

        if changed.is_empty() {
            self.status = "changed files are inside collapsed folders — press c".into();
            return true;
        }

        // Wrap around, so repeated presses cycle rather than sticking at an end.
        let next = if forward {
            changed
                .iter()
                .find(|&&p| p > self.tree_sel)
                .copied()
                .unwrap_or(changed[0])
        } else {
            changed
                .iter()
                .rev()
                .find(|&&p| p < self.tree_sel)
                .copied()
                .unwrap_or(*changed.last().unwrap())
        };

        self.tree_sel = next;
        self.focus = PaneId::Tree;
        self.clamp_tree();
        if let Some(path) = self.selected_path() {
            self.open(path);
        }
        true
    }

    /// Write a commit-graph (`W`).
    fn write_commit_graph(&mut self) -> bool {
        let Some(vcs) = &self.vcs else {
            self.status = "not a git repository".into();
            return true;
        };
        self.status = match vcs.write_commit_graph() {
            Ok(()) => "commit-graph written — history operations are now much faster".into(),
            Err(e) => format!("commit-graph failed: {e}"),
        };
        self.repo = self.vcs.as_ref().and_then(|v| v.info().ok());
        true
    }

    // ------------------------------------------------------------- helpers

    /// Lines in the open text file, or 0 when there is no text open.
    pub fn text_line_count(&self) -> usize {
        match &self.doc {
            Some(Document::Text(d)) => d.line_count(),
            _ => 0,
        }
    }

    fn clamp_tree(&mut self) {
        self.tree_scroll = clamp_scroll(
            self.tree_sel,
            self.tree_scroll,
            self.tree_h,
            self.visible.len(),
        );
    }

    fn clamp_content(&mut self) {
        self.doc_scroll_y = clamp_scroll(
            self.doc_line,
            self.doc_scroll_y,
            self.content_h,
            self.content_rows(),
        );
    }

    // -------------------------------------------------------------- render

    /// Public so the view layer can be exercised headlessly against
    /// ratatui's `TestBackend` — the click path is the risky part of this
    /// crate and it should not need a TTY to test.
    pub fn render(&mut self, f: &mut Frame) {
        // Move the map out so the pane renderers can borrow it alongside &self.
        // The Vec is reused, so this costs nothing per frame.
        let mut hits = std::mem::take(&mut self.hits);
        hits.clear();

        let area = f.area();
        let [body, status] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);

        let [tree_area, content_area] = Layout::horizontal([
            Constraint::Percentage(self.config.tree_width_percent()),
            Constraint::Min(10),
        ])
        .areas(body);

        // Apply sizes *before* drawing, not after. Layout already knows them,
        // and scroll clamping needs real numbers — running it against the
        // placeholder from construction scrolled panes that should not have
        // moved, hiding their first row on the very first frame.
        self.tree_h = inner_height(tree_area);
        self.content_h = inner_height(content_area);
        self.content_w = content_area.width.saturating_sub(2) as usize;
        self.clamp_tree();
        self.clamp_content();

        panes::render_tree(f, tree_area, self, &mut hits);
        panes::render_content(f, content_area, self, &mut hits);
        panes::render_status(f, status, self);

        self.hits = hits;
    }
}

/// Flatten an LSP hover reply into one status-bar line.
///
/// `contents` has three legal shapes across protocol versions, and servers in
/// the wild use all of them.
fn hover_text(result: &serde_json::Value) -> Option<String> {
    let contents = result.get("contents")?;
    let raw = if let Some(s) = contents.as_str() {
        s.to_string()
    } else if let Some(value) = contents.get("value").and_then(|v| v.as_str()) {
        value.to_string()
    } else {
        contents
            .as_array()
            .and_then(|a| a.first())
            .and_then(|first| first.get("value"))
            .and_then(|v| v.as_str())?
            .to_string()
    };

    // The status bar is one line; markdown fences and blank lines are noise.
    let flat = raw
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("```"))
        .collect::<Vec<_>>()
        .join(" · ");
    (!flat.is_empty()).then_some(flat)
}

/// Milliseconds since process start, for undo coalescing.
fn now_ms() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_millis() as u64
}

fn inner_height(area: Rect) -> usize {
    // Minus the top and bottom border rows.
    area.height.saturating_sub(2).max(1) as usize
}

/// Move `pos` by `delta`, clamped to `0..=max`.
fn shift(pos: usize, delta: isize, max: usize) -> usize {
    if delta < 0 {
        pos.saturating_sub(delta.unsigned_abs())
    } else {
        (pos + delta as usize).min(max)
    }
}

/// Smallest scroll offset that keeps `cursor` inside a viewport of `height`.
fn keep_visible(cursor: usize, scroll: usize, height: usize) -> usize {
    let height = height.max(1);
    if cursor < scroll {
        cursor
    } else if cursor >= scroll + height {
        cursor + 1 - height
    } else {
        scroll
    }
}

/// Keep the cursor visible, and never scroll past the end.
///
/// The upper bound matters after the viewport grows: `keep_visible` alone
/// leaves an already-scrolled pane where it is, since the cursor is still on
/// screen — which strands blank rows at the bottom and hides the first row.
fn clamp_scroll(cursor: usize, scroll: usize, height: usize, total: usize) -> usize {
    let height = height.max(1);
    let max_scroll = total.saturating_sub(height);
    keep_visible(cursor, scroll.min(max_scroll), height).min(max_scroll)
}

/// `match` / `matches`, so status lines read like English.
fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "es"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolling_stops_at_the_ends() {
        assert_eq!(shift(0, -3, 100), 0, "cannot scroll above the top");
        assert_eq!(shift(98, 3, 100), 100, "cannot scroll past the bottom");
        assert_eq!(shift(50, 3, 100), 53);
    }

    #[test]
    fn a_visible_cursor_does_not_move_the_viewport() {
        assert_eq!(keep_visible(5, 0, 10), 0);
        assert_eq!(keep_visible(9, 0, 10), 0, "last visible row stays put");
    }

    #[test]
    fn the_viewport_follows_the_cursor_off_either_edge() {
        assert_eq!(
            keep_visible(10, 0, 10),
            1,
            "one past the bottom scrolls one"
        );
        assert_eq!(
            keep_visible(3, 8, 10),
            3,
            "cursor above the top pulls it up"
        );
    }

    #[test]
    fn a_zero_height_viewport_does_not_divide_by_zero() {
        assert_eq!(keep_visible(7, 0, 0), 7);
    }

    #[test]
    fn a_grown_viewport_scrolls_back_rather_than_stranding_rows() {
        // 2 rows, scrolled to 1, then the pane grows to fit everything. The
        // cursor is still visible so keep_visible alone would leave it — and
        // row 0 would stay hidden behind blank space at the bottom.
        assert_eq!(keep_visible(1, 1, 16), 1, "keep_visible alone stays put");
        assert_eq!(clamp_scroll(1, 1, 16, 2), 0, "clamp_scroll pulls it back");
    }

    #[test]
    fn scroll_never_exceeds_the_last_page() {
        // Cursor at the end, so nothing pulls the viewport back up.
        assert_eq!(clamp_scroll(19, 99, 10, 20), 10, "20 rows, 10 tall");
        assert_eq!(
            clamp_scroll(0, 5, 10, 3),
            0,
            "content shorter than viewport"
        );
    }

    #[test]
    fn showing_the_cursor_wins_over_a_stale_scroll_offset() {
        // An absurd offset with the cursor at the top: the cursor must win,
        // even though the cap alone would leave the viewport at 10.
        assert_eq!(clamp_scroll(0, 99, 10, 20), 0);
    }

    #[test]
    fn clamping_still_follows_the_cursor_off_the_bottom() {
        assert_eq!(clamp_scroll(15, 0, 10, 20), 6);
    }

    // ------------------------------------------------------------------
    // Headless view tests.
    //
    // The click path -- render, register hit regions, resolve a coordinate,
    // act -- is the part of this crate most likely to break silently, and it
    // is exactly what a unit test on pure functions cannot reach. TestBackend
    // renders into an in-memory buffer, so all of it runs without a TTY and
    // therefore runs in CI.
    // ------------------------------------------------------------------

    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use dxdiary_core::ColorDepth;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("dxdiary-app-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("README.md"), "hello\nworld\n").unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() { PROBE }\n").unwrap();
        root
    }

    fn app_for(tag: &str) -> App {
        App::new(fixture(tag), Config::default(), ColorDepth::TrueColor)
    }

    /// Draw one frame into an 80x24 in-memory terminal.
    fn draw(app: &mut App) -> Terminal<TestBackend> {
        let mut term = Terminal::new(TestBackend::new(80, 24)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        term
    }

    fn screen(term: &Terminal<TestBackend>) -> String {
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

    fn click(app: &mut App, column: u16, row: u16) -> bool {
        app.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }))
    }

    #[test]
    fn rendering_registers_clickable_regions() {
        let mut app = app_for("regions");
        assert!(app.hits.is_empty(), "nothing registered before a frame");

        let term = draw(&mut app);

        assert!(!app.hits.is_empty(), "a frame must populate the hit map");
        let text = screen(&term);
        assert!(text.contains("src"), "tree shows the directory:\n{text}");
        assert!(text.contains("README.md"), "tree shows the file:\n{text}");
    }

    #[test]
    fn clicking_a_directory_row_expands_it() {
        let mut app = app_for("expand");
        draw(&mut app);
        let before = app.tree.len();

        // Tree pane inner area starts at (1,1); row 0 is the `src` directory.
        assert!(click(&mut app, 3, 1));

        assert!(
            app.tree.len() > before,
            "expanding should reveal children: {before} -> {}",
            app.tree.len()
        );
        assert_eq!(app.focus, PaneId::Tree);
    }

    #[test]
    fn clicking_a_file_row_opens_it_in_the_content_pane() {
        let mut app = app_for("open");
        draw(&mut app);

        // Expand `src`, re-render so the hit map matches the new row layout,
        // then click the child on row 1.
        click(&mut app, 3, 1);
        draw(&mut app);
        click(&mut app, 5, 2);

        let Some(Document::Text(doc)) = &app.doc else {
            panic!("expected a text document, got {:?}", app.doc);
        };
        assert!(doc.path.ends_with("main.rs"), "opened {:?}", doc.path);

        let term = draw(&mut app);
        let text = screen(&term);
        assert!(text.contains("PROBE"), "file contents render:\n{text}");
    }

    #[test]
    fn clicking_a_pane_header_moves_focus_without_selecting() {
        let mut app = app_for("header");
        draw(&mut app);
        let selected = app.tree_sel;

        // The content pane starts after the tree's 30% of 80 columns.
        assert!(click(&mut app, 40, 0));

        assert_eq!(app.focus, PaneId::Content);
        assert_eq!(
            app.tree_sel, selected,
            "header clicks must not move the cursor"
        );
    }

    #[test]
    fn clicking_past_the_last_row_takes_focus_but_selects_nothing() {
        let mut app = app_for("empty-space");
        draw(&mut app);
        let selected = app.tree_sel;

        click(&mut app, 3, 15); // well below the two rows that exist

        assert_eq!(app.focus, PaneId::Tree);
        assert_eq!(app.tree_sel, selected);
    }

    #[test]
    fn a_click_outside_every_region_is_ignored() {
        let mut app = app_for("outside");
        draw(&mut app);
        // The status bar is the last row and registers no regions.
        assert!(!click(&mut app, 40, 23));
    }

    #[test]
    fn the_viewport_sizes_are_real_after_the_first_frame() {
        let mut app = app_for("sizes");
        draw(&mut app);
        // 24 rows minus the status line minus two borders.
        assert_eq!(app.tree_h, 21);
        assert!(app.content_w > 40, "content pane got {}", app.content_w);
    }
}
