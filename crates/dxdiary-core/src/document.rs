//! A file loaded for viewing.
//!
//! Phase 1 stores lines as plain `String`s. Phase 7 (editing) swaps the backing
//! store for a rope; keeping every consumer behind `lines()` and `line_count()`
//! means that is a change here, not everywhere.

use std::path::{Path, PathBuf};

/// Bytes inspected when deciding whether a file is binary.
const SNIFF_BYTES: usize = 8192;

#[derive(Debug)]
pub enum Document {
    Text(TextDocument),
    /// Loaded, but not something to render as text.
    Binary {
        path: PathBuf,
        bytes: u64,
    },
    /// Deliberately not loaded.
    TooLarge {
        path: PathBuf,
        bytes: u64,
    },
    Error {
        path: PathBuf,
        message: String,
    },
}

#[derive(Debug)]
pub struct TextDocument {
    pub path: PathBuf,
    lines: Vec<String>,
    /// Longest line in characters — the horizontal scroll bound.
    pub max_line_chars: usize,
    /// True if the file had no trailing newline, so we do not invent a line.
    pub no_trailing_newline: bool,
}

impl TextDocument {
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, n: usize) -> Option<&str> {
        self.lines.get(n).map(String::as_str)
    }
}

impl Document {
    pub fn path(&self) -> &Path {
        match self {
            Document::Text(d) => &d.path,
            Document::Binary { path, .. }
            | Document::TooLarge { path, .. }
            | Document::Error { path, .. } => path,
        }
    }

    /// Read a file, refusing anything oversized or binary.
    pub fn load(path: impl Into<PathBuf>, max_bytes: u64) -> Self {
        let path = path.into();

        let bytes = match std::fs::metadata(&path) {
            Ok(m) => m.len(),
            Err(e) => {
                return Document::Error {
                    path,
                    message: e.to_string(),
                }
            }
        };

        if bytes > max_bytes {
            return Document::TooLarge { path, bytes };
        }

        let raw = match std::fs::read(&path) {
            Ok(r) => r,
            Err(e) => {
                return Document::Error {
                    path,
                    message: e.to_string(),
                }
            }
        };

        if is_binary(&raw) {
            return Document::Binary { path, bytes };
        }

        // from_utf8_lossy rather than a hard error: a stray invalid byte in an
        // otherwise readable file should not make it unviewable.
        let text = String::from_utf8_lossy(&raw);
        let no_trailing_newline = !text.is_empty() && !text.ends_with('\n');

        let mut lines: Vec<String> = text
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect();

        // `split` yields a trailing empty piece for a newline-terminated file.
        if !no_trailing_newline && lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }

        let max_line_chars = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);

        Document::Text(TextDocument {
            path,
            lines,
            max_line_chars,
            no_trailing_newline,
        })
    }
}

/// A NUL byte in the first few KB means binary. Same heuristic git uses, and it
/// beats extension sniffing for files agents produce with no extension at all.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(SNIFF_BYTES).any(|&b| b == 0)
}

/// Expand tabs and strip control characters so one source line occupies exactly
/// the width it appears to. Control bytes in agent output would otherwise move
/// the cursor and corrupt the frame.
pub fn render_line(line: &str, tab_width: usize) -> String {
    render_line_mapped(line, tab_width).0
}

/// Expand a line for display and report where each raw character landed.
///
/// `offsets[i]` is the display column of raw character `i`, with a final
/// element holding the total width so a span's exclusive end maps without a
/// special case.
///
/// Syntax highlighting produces character offsets into the *raw* line while
/// the pane renders the *expanded* one. Without this mapping every colour on a
/// tab-indented row lands `tab_width - 1` columns early per tab -- which is
/// every row of gofmt'd Go, not an edge case.
pub fn render_line_mapped(line: &str, tab_width: usize) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(line.len());
    let mut offsets = Vec::with_capacity(line.len() + 1);
    // Tracked rather than recounted: `out.chars().count()` per character made
    // this quadratic in line length.
    let mut width = 0usize;

    for ch in line.chars() {
        offsets.push(width);
        match ch {
            '\t' => {
                let pad = tab_width - (width % tab_width.max(1));
                out.extend(std::iter::repeat_n(' ', pad));
                width += pad;
            }
            c if c.is_control() => {
                out.push('\u{fffd}');
                width += 1;
            }
            c => {
                out.push(c);
                width += 1;
            }
        }
    }
    offsets.push(width);
    (out, offsets)
}

/// Raw character index displayed at `column`.
///
/// The inverse of the map [`render_line_mapped`] returns, for turning a click
/// into a cursor position. A column inside a tab's run of spaces resolves to
/// the tab itself, which is the character actually there; a column past the
/// end resolves to one past the last character, where a cursor legitimately
/// sits.
pub fn raw_column_at(offsets: &[usize], column: usize) -> usize {
    // `offsets` is non-decreasing and ends with the line's total width, which
    // is a sentinel rather than a character: a click inside the text must
    // never resolve to it.
    let last_char = offsets.len().saturating_sub(1);
    if offsets.last().is_some_and(|&width| column >= width) {
        return last_char;
    }
    for i in (0..last_char).rev() {
        if offsets[i] <= column {
            return i;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_column_maps_back_to_the_character_shown_there() {
        let (_, offsets) = render_line_mapped("abc", 4);
        assert_eq!(raw_column_at(&offsets, 0), 0);
        assert_eq!(raw_column_at(&offsets, 2), 2);
    }

    #[test]
    fn a_column_inside_a_tab_resolves_to_the_tab() {
        // "\tx" renders as four spaces then x. Clicking any of those spaces
        // is a click on the tab, because that is the character there.
        let (_, offsets) = render_line_mapped("\tx", 4);
        for column in 0..4 {
            assert_eq!(raw_column_at(&offsets, column), 0, "column {column}");
        }
        assert_eq!(raw_column_at(&offsets, 4), 1, "x itself");
    }

    #[test]
    fn a_column_past_the_end_lands_one_past_the_last_character() {
        let (_, offsets) = render_line_mapped("ab", 4);
        assert_eq!(raw_column_at(&offsets, 2), 2, "just past the end");
        assert_eq!(raw_column_at(&offsets, 99), 2, "far past, same place");
    }

    #[test]
    fn an_empty_line_resolves_to_its_only_position() {
        let (_, offsets) = render_line_mapped("", 4);
        assert_eq!(raw_column_at(&offsets, 0), 0);
        assert_eq!(raw_column_at(&offsets, 7), 0);
    }

    #[test]
    fn the_two_column_maps_are_inverses() {
        for line in ["abc", "\tx", "a\tb\tc", "\u{3b1}\u{3b2}\u{3b3}", "\t\t"] {
            let (_, offsets) = render_line_mapped(line, 4);
            for raw in 0..line.chars().count() {
                assert_eq!(
                    raw_column_at(&offsets, offsets[raw]),
                    raw,
                    "{line:?} character {raw}"
                );
            }
        }
    }

    #[test]
    fn the_column_map_tracks_tab_expansion() {
        let (text, offsets) = render_line_mapped("\tab", 4);
        assert_eq!(text, "    ab");
        // Raw char 0 is the tab at column 0; `a` lands at column 4.
        assert_eq!(
            offsets,
            vec![0, 4, 5, 6],
            "trailing entry is the full width"
        );
    }

    #[test]
    fn a_tab_advances_to_the_next_stop_not_a_fixed_width() {
        let (text, offsets) = render_line_mapped("ab\tc", 4);
        assert_eq!(text, "ab  c");
        assert_eq!(offsets, vec![0, 1, 2, 4, 5]);
    }

    #[test]
    fn the_map_covers_every_character_of_a_line_without_tabs() {
        let (text, offsets) = render_line_mapped("abc", 4);
        assert_eq!(text, "abc");
        assert_eq!(offsets, vec![0, 1, 2, 3], "an identity map, plus the width");
    }

    #[test]
    fn a_control_character_occupies_one_column_like_its_replacement() {
        let (text, offsets) = render_line_mapped("a\u{7}b", 4);
        assert_eq!(text, "a\u{fffd}b");
        assert_eq!(offsets, vec![0, 1, 2, 3]);
    }

    #[test]
    fn expansion_and_the_mapped_form_agree() {
        for line in ["", "\t", "\t\t", "x\ty", "\u{3b1}\t\u{3b2}", "no tabs here"] {
            assert_eq!(
                render_line(line, 4),
                render_line_mapped(line, 4).0,
                "render_line delegates, so the two cannot drift: {line:?}"
            );
        }
    }

    fn write(tag: &str, content: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!("dxdiary-doc-{tag}"));
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn loads_text_and_counts_lines() {
        let p = write("text", b"one\ntwo\nthree\n");
        let Document::Text(d) = Document::load(&p, 1 << 20) else {
            panic!("expected text");
        };
        assert_eq!(d.line_count(), 3);
        assert_eq!(d.line(1), Some("two"));
        assert!(!d.no_trailing_newline);
    }

    #[test]
    fn a_missing_final_newline_does_not_add_a_phantom_line() {
        let p = write("no-nl", b"one\ntwo");
        let Document::Text(d) = Document::load(&p, 1 << 20) else {
            panic!()
        };
        assert_eq!(d.line_count(), 2);
        assert!(d.no_trailing_newline);
    }

    #[test]
    fn crlf_line_endings_do_not_leave_stray_carriage_returns() {
        let p = write("crlf", b"one\r\ntwo\r\n");
        let Document::Text(d) = Document::load(&p, 1 << 20) else {
            panic!()
        };
        assert_eq!(d.lines(), &["one".to_string(), "two".to_string()]);
    }

    #[test]
    fn nul_bytes_mark_a_file_binary() {
        let p = write("bin", b"\x7fELF\x00\x00stuff");
        assert!(matches!(
            Document::load(&p, 1 << 20),
            Document::Binary { .. }
        ));
    }

    #[test]
    fn oversized_files_are_not_read() {
        let p = write("big", &vec![b'x'; 4096]);
        assert!(matches!(
            Document::load(&p, 1024),
            Document::TooLarge { bytes: 4096, .. }
        ));
    }

    #[test]
    fn a_missing_file_reports_an_error_instead_of_panicking() {
        let p = std::env::temp_dir().join("dxdiary-doc-does-not-exist");
        let _ = std::fs::remove_file(&p);
        assert!(matches!(
            Document::load(&p, 1 << 20),
            Document::Error { .. }
        ));
    }

    #[test]
    fn empty_files_have_no_lines() {
        let p = write("empty", b"");
        let Document::Text(d) = Document::load(&p, 1 << 20) else {
            panic!()
        };
        assert_eq!(d.line_count(), 0);
        assert_eq!(d.max_line_chars, 0);
    }

    #[test]
    fn tabs_expand_to_the_next_stop_not_a_fixed_width() {
        assert_eq!(render_line("ab\tc", 4), "ab  c");
        assert_eq!(render_line("a\tb", 4), "a   b");
        assert_eq!(render_line("\tx", 4), "    x");
    }

    #[test]
    fn control_characters_cannot_corrupt_the_frame() {
        let out = render_line("a\x1b[31mb\x07", 4);
        assert!(!out.contains('\x1b'), "escape survived: {out:?}");
        assert!(!out.contains('\x07'));
    }
}
