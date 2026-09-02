//! An editable text buffer.
//!
//! Rope-backed, because an insert in the middle of a large file should not copy
//! the file. DESIGN.md called for ropes from day one so this phase would be an
//! extension rather than a rewrite; [`Document`](crate::document) keeps its
//! read-only shape and this sits beside it for files being edited.
//!
//! Undo is grouped, not per-keystroke: typing a word and pressing undo should
//! remove the word, not the last letter.

use std::path::{Path, PathBuf};

use ropey::Rope;

/// How long a pause breaks an undo group.
///
/// Typing continuously coalesces; stopping to think starts a new group. Time
/// is passed in rather than read from a clock so the logic stays testable.
pub const COALESCE_MS: u64 = 600;

/// A cursor position, in characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Cursor {
    pub line: usize,
    /// Character offset within the line, not byte offset.
    pub column: usize,
}

/// One reversible change.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Edit {
    /// Character offset in the whole rope.
    at: usize,
    removed: String,
    inserted: String,
    /// Cursor before the edit, so undo returns you where you were.
    cursor_before: Cursor,
}

/// A group of edits undone together.
#[derive(Debug, Clone, Default)]
struct Group {
    edits: Vec<Edit>,
    /// Milliseconds of the last edit in this group.
    last_ms: u64,
}

pub struct Buffer {
    rope: Rope,
    pub path: PathBuf,
    pub cursor: Cursor,
    /// Contents differ from what is on disk.
    pub dirty: bool,
    undo: Vec<Group>,
    redo: Vec<Group>,
    /// Column the cursor "wants", preserved across short lines.
    ///
    /// Without this, moving down through a short line and back permanently
    /// forgets the original column — the behaviour every editor gets right and
    /// every naive implementation gets wrong.
    goal_column: Option<usize>,
}

impl Buffer {
    pub fn from_str(path: impl Into<PathBuf>, text: &str) -> Self {
        Self {
            rope: Rope::from_str(text),
            path: path.into(),
            cursor: Cursor::default(),
            dirty: false,
            undo: Vec::new(),
            redo: Vec::new(),
            goal_column: None,
        }
    }

    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)?;
        Ok(Self::from_str(path, &text))
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// Number of lines, not counting the phantom line a trailing newline
    /// creates — `lines()` on the rope would report one more.
    pub fn line_count(&self) -> usize {
        let n = self.rope.len_lines();
        if n > 1 && self.rope.line(n - 1).len_chars() == 0 {
            n - 1
        } else {
            n
        }
    }

    pub fn line(&self, index: usize) -> String {
        if index >= self.rope.len_lines() {
            return String::new();
        }
        let line = self.rope.line(index);
        let s = line.to_string();
        s.strip_suffix('\n')
            .map(|s| s.strip_suffix('\r').unwrap_or(s))
            .unwrap_or(&s)
            .to_string()
    }

    pub fn line_len(&self, index: usize) -> usize {
        self.line(index).chars().count()
    }

    pub fn lines(&self) -> Vec<String> {
        (0..self.line_count()).map(|i| self.line(i)).collect()
    }

    /// Character offset of a cursor within the whole rope.
    fn offset_of(&self, cursor: Cursor) -> usize {
        let line = cursor.line.min(self.rope.len_lines().saturating_sub(1));
        let start = self.rope.line_to_char(line);
        start + cursor.column.min(self.line_len(line))
    }

    fn cursor_at(&self, offset: usize) -> Cursor {
        let offset = offset.min(self.rope.len_chars());
        let line = self.rope.char_to_line(offset);
        Cursor {
            line,
            column: offset - self.rope.line_to_char(line),
        }
    }

    // ------------------------------------------------------------ editing

    /// Apply an edit and record it for undo.
    fn apply(&mut self, at: usize, remove: usize, insert: &str, now_ms: u64) {
        let removed = if remove > 0 {
            self.rope.slice(at..at + remove).to_string()
        } else {
            String::new()
        };

        let edit = Edit {
            at,
            removed,
            inserted: insert.to_string(),
            cursor_before: self.cursor,
        };

        if remove > 0 {
            self.rope.remove(at..at + remove);
        }
        if !insert.is_empty() {
            self.rope.insert(at, insert);
        }

        self.push_undo(edit, now_ms);
        self.dirty = true;
        // Any new edit invalidates the redo stack; keeping it would let undo
        // and redo disagree about history.
        self.redo.clear();
    }

    fn push_undo(&mut self, edit: Edit, now_ms: u64) {
        let coalesce = self
            .undo
            .last()
            .is_some_and(|g| now_ms.saturating_sub(g.last_ms) < COALESCE_MS);

        if coalesce {
            if let Some(group) = self.undo.last_mut() {
                group.edits.push(edit);
                group.last_ms = now_ms;
                return;
            }
        }
        self.undo.push(Group {
            edits: vec![edit],
            last_ms: now_ms,
        });
    }

    /// Insert text at the cursor.
    pub fn insert(&mut self, text: &str, now_ms: u64) {
        let at = self.offset_of(self.cursor);
        self.apply(at, 0, text, now_ms);
        self.cursor = self.cursor_at(at + text.chars().count());
        self.goal_column = None;
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self, now_ms: u64) {
        let at = self.offset_of(self.cursor);
        if at == 0 {
            return;
        }
        self.apply(at - 1, 1, "", now_ms);
        self.cursor = self.cursor_at(at - 1);
        self.goal_column = None;
    }

    /// Delete the character under the cursor.
    pub fn delete(&mut self, now_ms: u64) {
        let at = self.offset_of(self.cursor);
        if at >= self.rope.len_chars() {
            return;
        }
        self.apply(at, 1, "", now_ms);
        self.cursor = self.cursor_at(at);
        self.goal_column = None;
    }

    /// Delete the whole line the cursor is on.
    pub fn delete_line(&mut self, now_ms: u64) {
        if self.rope.len_chars() == 0 {
            return;
        }
        let line = self.cursor.line.min(self.line_count().saturating_sub(1));
        let start = self.rope.line_to_char(line);
        let end = if line + 1 < self.rope.len_lines() {
            self.rope.line_to_char(line + 1)
        } else {
            self.rope.len_chars()
        };
        self.apply(start, end - start, "", now_ms);
        self.cursor = self.cursor_at(start.min(self.rope.len_chars()));
        self.goal_column = None;
    }

    // --------------------------------------------------------- undo/redo

    pub fn undo(&mut self) -> bool {
        let Some(group) = self.undo.pop() else {
            return false;
        };
        // Reverse order: later edits in a group sit on top of earlier ones.
        for edit in group.edits.iter().rev() {
            let inserted = edit.inserted.chars().count();
            if inserted > 0 {
                self.rope.remove(edit.at..edit.at + inserted);
            }
            if !edit.removed.is_empty() {
                self.rope.insert(edit.at, &edit.removed);
            }
        }
        self.cursor = group
            .edits
            .first()
            .map(|e| e.cursor_before)
            .unwrap_or_default();
        self.clamp_cursor();
        self.redo.push(group);
        self.dirty = true;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(group) = self.redo.pop() else {
            return false;
        };
        for edit in &group.edits {
            let removed = edit.removed.chars().count();
            if removed > 0 {
                self.rope.remove(edit.at..edit.at + removed);
            }
            if !edit.inserted.is_empty() {
                self.rope.insert(edit.at, &edit.inserted);
            }
        }
        if let Some(last) = group.edits.last() {
            self.cursor = self.cursor_at(last.at + last.inserted.chars().count());
        }
        self.clamp_cursor();
        self.undo.push(group);
        self.dirty = true;
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    // --------------------------------------------------------- movement

    pub fn move_to(&mut self, line: usize, column: usize) {
        self.cursor = Cursor { line, column };
        self.goal_column = None;
        self.clamp_cursor();
    }

    pub fn move_left(&mut self) {
        if self.cursor.column > 0 {
            self.cursor.column -= 1;
        } else if self.cursor.line > 0 {
            self.cursor.line -= 1;
            self.cursor.column = self.line_len(self.cursor.line);
        }
        self.goal_column = None;
    }

    pub fn move_right(&mut self) {
        if self.cursor.column < self.line_len(self.cursor.line) {
            self.cursor.column += 1;
        } else if self.cursor.line + 1 < self.line_count() {
            self.cursor.line += 1;
            self.cursor.column = 0;
        }
        self.goal_column = None;
    }

    pub fn move_vertical(&mut self, delta: isize) {
        let goal = self.goal_column.unwrap_or(self.cursor.column);
        let next = if delta < 0 {
            self.cursor.line.saturating_sub(delta.unsigned_abs())
        } else {
            (self.cursor.line + delta as usize).min(self.line_count().saturating_sub(1))
        };
        self.cursor.line = next;
        self.cursor.column = goal.min(self.line_len(next));
        // Remember the goal so passing through a short line does not lose it.
        self.goal_column = Some(goal);
    }

    pub fn move_line_start(&mut self) {
        self.cursor.column = 0;
        self.goal_column = None;
    }

    pub fn move_line_end(&mut self) {
        self.cursor.column = self.line_len(self.cursor.line);
        self.goal_column = None;
    }

    fn clamp_cursor(&mut self) {
        let last = self.line_count().saturating_sub(1);
        self.cursor.line = self.cursor.line.min(last);
        self.cursor.column = self.cursor.column.min(self.line_len(self.cursor.line));
    }

    // ------------------------------------------------------------- saving

    /// Write to disk.
    ///
    /// Writes a sibling temp file and renames, so an interrupted save cannot
    /// leave a truncated source file behind. Rename within a directory is
    /// atomic on every filesystem crowsnest targets.
    pub fn save(&mut self) -> std::io::Result<()> {
        let dir = self.path.parent().unwrap_or(Path::new("."));
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unnamed".into());
        let temp = dir.join(format!(".{name}.crowsnest-tmp"));

        std::fs::write(&temp, self.text())?;
        std::fs::rename(&temp, &self.path)?;
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(text: &str) -> Buffer {
        Buffer::from_str("/tmp/test.rs", text)
    }

    /// Distinct timestamps, far enough apart to never coalesce.
    fn separate(n: u64) -> u64 {
        n * (COALESCE_MS + 100)
    }

    #[test]
    fn lines_do_not_include_a_phantom_trailing_line() {
        let b = buf("one\ntwo\n");
        assert_eq!(b.line_count(), 2);
        assert_eq!(b.lines(), vec!["one", "two"]);
    }

    #[test]
    fn a_file_without_a_trailing_newline_keeps_its_last_line() {
        let b = buf("one\ntwo");
        assert_eq!(b.line_count(), 2);
        assert_eq!(b.line(1), "two");
    }

    #[test]
    fn inserting_at_the_cursor_moves_it_past_what_was_typed() {
        let mut b = buf("hello\n");
        b.move_to(0, 5);
        b.insert(" world", 0);
        assert_eq!(b.line(0), "hello world");
        assert_eq!(
            b.cursor,
            Cursor {
                line: 0,
                column: 11
            }
        );
        assert!(b.dirty);
    }

    #[test]
    fn inserting_a_newline_splits_the_line() {
        let mut b = buf("ab\n");
        b.move_to(0, 1);
        b.insert("\n", 0);
        assert_eq!(b.lines(), vec!["a", "b"]);
        assert_eq!(b.cursor, Cursor { line: 1, column: 0 });
    }

    #[test]
    fn backspace_at_the_start_of_a_line_joins_it_to_the_previous() {
        let mut b = buf("ab\ncd\n");
        b.move_to(1, 0);
        b.backspace(0);
        assert_eq!(b.lines(), vec!["abcd"]);
        assert_eq!(b.cursor, Cursor { line: 0, column: 2 });
    }

    #[test]
    fn backspace_at_the_very_start_does_nothing() {
        let mut b = buf("ab\n");
        b.move_to(0, 0);
        b.backspace(0);
        assert_eq!(b.text(), "ab\n");
        assert!(!b.dirty, "a no-op must not mark the file modified");
    }

    #[test]
    fn delete_removes_the_character_under_the_cursor() {
        let mut b = buf("abc\n");
        b.move_to(0, 1);
        b.delete(0);
        assert_eq!(b.line(0), "ac");
        assert_eq!(b.cursor, Cursor { line: 0, column: 1 });
    }

    #[test]
    fn deleting_a_line_removes_it_entirely() {
        let mut b = buf("one\ntwo\nthree\n");
        b.move_to(1, 0);
        b.delete_line(0);
        assert_eq!(b.lines(), vec!["one", "three"]);
    }

    #[test]
    fn undo_restores_the_previous_text_and_cursor() {
        let mut b = buf("hello\n");
        b.move_to(0, 5);
        b.insert("!", 0);
        assert_eq!(b.line(0), "hello!");

        assert!(b.undo());
        assert_eq!(b.line(0), "hello");
        assert_eq!(b.cursor, Cursor { line: 0, column: 5 }, "back where it was");
    }

    #[test]
    fn typing_a_word_undoes_as_one_group() {
        let mut b = buf("");
        // Same instant: continuous typing.
        for ch in "hello".chars() {
            b.insert(&ch.to_string(), 10);
        }
        assert_eq!(b.text(), "hello");

        assert!(b.undo());
        assert_eq!(b.text(), "", "the whole word, not one letter");
    }

    #[test]
    fn a_pause_starts_a_new_undo_group() {
        let mut b = buf("");
        b.insert("one", separate(1));
        b.insert("two", separate(2));

        b.undo();
        assert_eq!(b.text(), "one", "only the second group came off");
        b.undo();
        assert_eq!(b.text(), "");
    }

    #[test]
    fn redo_reapplies_what_undo_removed() {
        let mut b = buf("x");
        b.move_to(0, 1);
        b.insert("y", 0);

        b.undo();
        assert_eq!(b.text(), "x");
        assert!(b.redo());
        assert_eq!(b.text(), "xy");
    }

    #[test]
    fn a_new_edit_after_undo_discards_the_redo_stack() {
        let mut b = buf("");
        b.insert("a", separate(1));
        b.undo();
        assert!(b.can_redo());

        b.insert("b", separate(2));
        assert!(!b.can_redo(), "history diverged; redo would be incoherent");
        assert_eq!(b.text(), "b");
    }

    #[test]
    fn undo_on_an_untouched_buffer_reports_nothing_to_do() {
        let mut b = buf("x\n");
        assert!(!b.undo());
        assert!(!b.can_undo());
    }

    #[test]
    fn moving_down_through_a_short_line_remembers_the_column() {
        let mut b = buf("longer line\nab\nlonger line\n");
        b.move_to(0, 9);

        b.move_vertical(1);
        assert_eq!(b.cursor.column, 2, "clamped to the short line");

        b.move_vertical(1);
        assert_eq!(
            b.cursor.column, 9,
            "the original column comes back on a long line"
        );
    }

    #[test]
    fn an_explicit_horizontal_move_forgets_the_goal_column() {
        let mut b = buf("longer line\nab\nlonger line\n");
        b.move_to(0, 9);
        b.move_vertical(1);
        b.move_left();
        b.move_vertical(1);
        assert_eq!(b.cursor.column, 1, "goal was reset by moving sideways");
    }

    #[test]
    fn moving_right_off_a_line_wraps_to_the_next() {
        let mut b = buf("ab\ncd\n");
        b.move_to(0, 2);
        b.move_right();
        assert_eq!(b.cursor, Cursor { line: 1, column: 0 });
    }

    #[test]
    fn moving_left_off_a_line_wraps_to_the_previous_end() {
        let mut b = buf("ab\ncd\n");
        b.move_to(1, 0);
        b.move_left();
        assert_eq!(b.cursor, Cursor { line: 0, column: 2 });
    }

    #[test]
    fn movement_stops_at_the_ends_of_the_buffer() {
        let mut b = buf("only\n");
        b.move_to(0, 0);
        b.move_left();
        assert_eq!(b.cursor, Cursor { line: 0, column: 0 });

        b.move_line_end();
        b.move_right();
        assert_eq!(b.cursor.column, 4, "nowhere further to go");
    }

    #[test]
    fn editing_multi_byte_text_uses_character_offsets() {
        // Byte offsets would split a codepoint and panic.
        let mut b = buf("héllo wörld\n");
        b.move_to(0, 6);
        b.insert("★", 0);
        assert_eq!(b.line(0), "héllo ★wörld");

        b.undo();
        assert_eq!(b.line(0), "héllo wörld");
    }

    #[test]
    fn saving_writes_the_file_and_clears_the_dirty_flag() {
        let dir = std::env::temp_dir().join("crowsnest-buffer-save");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.txt");

        let mut b = Buffer::from_str(&path, "before\n");
        b.move_to(0, 6);
        b.insert(" after", 0);
        assert!(b.dirty);

        b.save().unwrap();
        assert!(!b.dirty);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "before after\n");
    }

    #[test]
    fn saving_leaves_no_temporary_file_behind() {
        let dir = std::env::temp_dir().join("crowsnest-buffer-temp");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.txt");

        let mut b = Buffer::from_str(&path, "x\n");
        b.insert("y", 0);
        b.save().unwrap();

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("crowsnest-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "found {leftovers:?}");
    }

    #[test]
    fn opening_a_file_round_trips_through_save() {
        let dir = std::env::temp_dir().join("crowsnest-buffer-open");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("in.rs");
        std::fs::write(&path, "fn main() {}\n").unwrap();

        let mut b = Buffer::open(&path).unwrap();
        assert_eq!(b.line(0), "fn main() {}");
        assert!(!b.dirty, "just opened");

        b.move_to(0, 12);
        b.insert("\n", 0);
        b.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fn main() {}\n\n");
    }
}
