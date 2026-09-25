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
    let mut out = String::with_capacity(line.len());
    for ch in line.chars() {
        match ch {
            '\t' => {
                let pad = tab_width - (out.chars().count() % tab_width.max(1));
                out.extend(std::iter::repeat_n(' ', pad));
            }
            c if c.is_control() => out.push('\u{fffd}'),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
