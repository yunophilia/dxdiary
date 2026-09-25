//! Turning a parse into per-line coloured spans.
//!
//! The renderer wants "for line N, a list of (column range, role)". tree-sitter
//! emits a flat stream of byte-range events instead, so this module converts —
//! splitting any span that crosses a newline, since a terminal line is the unit
//! of rendering.

use std::collections::HashMap;

use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter as TsHighlighter};

use crate::language::Language;

/// Capture names requested from the grammars, in priority order.
///
/// tree-sitter reports the *index* into this list, so the order here defines
/// the mapping. More specific names must precede their prefixes — a grammar
/// capturing `function.builtin` should not be matched by plain `function`.
const CAPTURES: &[&str] = &[
    "attribute",
    "comment",
    "constant.builtin",
    "constant",
    "constructor",
    "function.builtin",
    "function.method",
    "function",
    "keyword",
    "label",
    "number",
    "operator",
    "property",
    "punctuation.bracket",
    "punctuation.delimiter",
    "punctuation",
    "string.special",
    "string",
    "tag",
    "type.builtin",
    "type",
    "variable.builtin",
    "variable.parameter",
    "variable",
];

/// What a span means, collapsed to the handful of colours a terminal theme
/// actually distinguishes. Finer capture names exist, but a dozen near-identical
/// hues make code harder to read, not easier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Keyword,
    String,
    Comment,
    Function,
    Type,
    Number,
    Constant,
    Operator,
    Punctuation,
    Variable,
    Attribute,
    Plain,
}

impl Role {
    fn from_capture(name: &str) -> Role {
        match name.split('.').next().unwrap_or(name) {
            "keyword" => Role::Keyword,
            "string" => Role::String,
            "comment" => Role::Comment,
            "function" | "constructor" => Role::Function,
            "type" => Role::Type,
            "number" => Role::Number,
            "constant" | "label" | "tag" => Role::Constant,
            "operator" => Role::Operator,
            "punctuation" => Role::Punctuation,
            "property" | "variable" => Role::Variable,
            "attribute" => Role::Attribute,
            _ => Role::Plain,
        }
    }
}

/// A coloured run within one line, measured in **characters**, not bytes.
///
/// The renderer slices by character for horizontal scrolling, so byte offsets
/// would desynchronise on any non-ASCII line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub role: Role,
}

#[derive(Default)]
pub struct Highlighter {
    /// Compiled queries, cached per language.
    ///
    /// The `tree_sitter_highlight::Highlighter` itself is *not* held here: it
    /// needs `&mut self` while the configuration needs `&self`, and borrowing
    /// both from one struct requires unsafe. Constructing one per call costs an
    /// allocation on file open, which is nothing next to parsing.
    configs: HashMap<Language, HighlightConfiguration>,
}

impl Highlighter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build (and cache) the query configuration for a language.
    ///
    /// Compiling a highlight query is expensive enough to matter when scrolling
    /// through a tree, so it happens once per language per session.
    fn ensure_config(&mut self, lang: Language) -> anyhow::Result<()> {
        if let std::collections::hash_map::Entry::Vacant(slot) = self.configs.entry(lang) {
            let mut cfg = HighlightConfiguration::new(
                lang.grammar(),
                lang.name(),
                &lang.highlight_query(),
                lang.injection_query(),
                "",
            )?;
            cfg.configure(CAPTURES);
            slot.insert(cfg);
        }
        Ok(())
    }

    /// Highlight `source`, returning one span list per line.
    ///
    /// Returns an empty vec on any parse or query failure: highlighting is a
    /// nicety, and a file that trips the grammar should still be readable in
    /// plain text rather than not displayed at all.
    pub fn highlight(&mut self, lang: Language, source: &str) -> Vec<Vec<Span>> {
        let line_count = source.lines().count();
        match self.try_highlight(lang, source) {
            Ok(spans) => spans,
            Err(_) => vec![Vec::new(); line_count],
        }
    }

    fn try_highlight(&mut self, lang: Language, source: &str) -> anyhow::Result<Vec<Vec<Span>>> {
        self.ensure_config(lang)?;
        let cfg = &self.configs[&lang];

        let mut ts = TsHighlighter::new();
        // The two `None`s are the cancellation flag and the timeout.
        let events = ts.highlight(cfg, source.as_bytes(), None, None, |_| None)?;

        let starts = line_starts(source);
        let mut lines: Vec<Vec<Span>> = vec![Vec::new(); starts.len()];
        let mut stack: Vec<Role> = Vec::new();

        for event in events {
            match event? {
                HighlightEvent::HighlightStart(h) => {
                    stack.push(Role::from_capture(CAPTURES[h.0]));
                }
                HighlightEvent::HighlightEnd => {
                    stack.pop();
                }
                HighlightEvent::Source { start, end } => {
                    // Innermost capture wins; unhighlighted source needs no span.
                    let Some(&role) = stack.last() else { continue };
                    if role == Role::Plain {
                        continue;
                    }
                    push_span(&mut lines, source, &starts, start, end, role);
                }
            }
        }
        Ok(lines)
    }
}

/// Byte offset of the start of each line.
fn line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0usize];
    for (i, b) in source.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    // A trailing newline opens a line that has no content; drop it so the
    // count matches `source.lines()`.
    if starts.len() > 1 && *starts.last().unwrap() == source.len() {
        starts.pop();
    }
    starts
}

/// Record a byte range as one or more per-line character spans.
fn push_span(
    lines: &mut [Vec<Span>],
    source: &str,
    starts: &[usize],
    start: usize,
    end: usize,
    role: Role,
) {
    let first = starts.partition_point(|&s| s <= start).saturating_sub(1);

    for (idx, &line_start) in starts.iter().enumerate().skip(first) {
        if line_start >= end && idx > first {
            break;
        }
        let line_end = starts.get(idx + 1).copied().unwrap_or(source.len() + 1) - 1;
        let line_end = line_end.min(source.len());

        let seg_start = start.max(line_start);
        let seg_end = end.min(line_end);
        if seg_start >= seg_end {
            continue;
        }

        // Byte offsets to character offsets, so the renderer can slice by char.
        let line = &source[line_start..line_end];
        let to_chars = |byte: usize| line[..byte - line_start].chars().count();

        if let Some(slot) = lines.get_mut(idx) {
            slot.push(Span {
                start: to_chars(seg_start),
                end: to_chars(seg_end),
                role,
            });
        }
        if end <= line_end {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles_on(spans: &[Vec<Span>], line: usize) -> Vec<Role> {
        spans[line].iter().map(|s| s.role).collect()
    }

    /// Text a span actually covers, so tests assert on what the user sees.
    fn text_of(source: &str, line: usize, span: Span) -> String {
        source
            .lines()
            .nth(line)
            .unwrap()
            .chars()
            .skip(span.start)
            .take(span.end - span.start)
            .collect()
    }

    #[test]
    fn rust_keywords_strings_and_comments_are_distinguished() {
        let src = "// a comment\nfn main() {\n    let s = \"hello\";\n}\n";
        let mut h = Highlighter::new();
        let spans = h.highlight(Language::Rust, src);

        assert_eq!(spans.len(), 4, "one entry per line");
        assert!(
            roles_on(&spans, 0).contains(&Role::Comment),
            "{:?}",
            spans[0]
        );
        assert!(
            roles_on(&spans, 1).contains(&Role::Keyword),
            "{:?}",
            spans[1]
        );
        assert!(
            roles_on(&spans, 2).contains(&Role::String),
            "{:?}",
            spans[2]
        );
    }

    #[test]
    fn spans_line_up_with_the_text_they_cover() {
        let src = "let x = 42;\n";
        let mut h = Highlighter::new();
        let spans = h.highlight(Language::Rust, src);

        // Asserted by covered text, not by role: grammars disagree about which
        // capture an integer literal gets (Rust calls it `constant`, not
        // `number`), and the point here is that offsets are right.
        let covering_42 = spans[0]
            .iter()
            .find(|&&s| text_of(src, 0, s) == "42")
            .copied();
        assert!(covering_42.is_some(), "a span covers `42`: {:?}", spans[0]);

        let keyword = spans[0]
            .iter()
            .find(|s| s.role == Role::Keyword)
            .copied()
            .expect("`let` is a keyword");
        assert_eq!(text_of(src, 0, keyword), "let");
    }

    #[test]
    fn a_span_crossing_a_newline_is_split_per_line() {
        // A block comment spanning three lines must produce a span on each.
        let src = "/* one\n   two\n   three */\nfn f() {}\n";
        let mut h = Highlighter::new();
        let spans = h.highlight(Language::Rust, src);

        for line in 0..3 {
            assert!(
                roles_on(&spans, line).contains(&Role::Comment),
                "line {line}: {:?}",
                spans[line]
            );
        }
    }

    #[test]
    fn offsets_are_characters_not_bytes() {
        // The comment starts after a multi-byte string. Byte offsets would put
        // the span several columns too far right.
        let src = "let s = \"héllo wörld\"; // tail\n";
        let mut h = Highlighter::new();
        let spans = h.highlight(Language::Rust, src);

        let comment = spans[0]
            .iter()
            .find(|s| s.role == Role::Comment)
            .copied()
            .expect("trailing comment");
        assert_eq!(text_of(src, 0, comment), "// tail");
    }

    #[test]
    fn every_language_highlights_something() {
        let samples = [
            (Language::C, "int main(void) { return 0; }\n"),
            (Language::Cpp, "int main() { return 0; }\n"),
            (Language::Go, "package main\nfunc main() {}\n"),
            (Language::Python, "def main():\n    return 1\n"),
            (Language::Rust, "fn main() { let x = 1; }\n"),
        ];
        let mut h = Highlighter::new();
        for (lang, src) in samples {
            let spans = h.highlight(lang, src);
            let total: usize = spans.iter().map(|l| l.len()).sum();
            assert!(total > 0, "{} produced no spans", lang.name());
        }
    }

    #[test]
    fn broken_source_still_yields_one_entry_per_line() {
        // Highlighting is a nicety; unparseable input must not lose the file.
        let src = "fn ( { { { unclosed\nsecond line\n";
        let mut h = Highlighter::new();
        assert_eq!(h.highlight(Language::Rust, src).len(), 2);
    }

    #[test]
    fn empty_input_is_handled() {
        let mut h = Highlighter::new();
        assert!(h.highlight(Language::Rust, "").iter().all(|l| l.is_empty()));
    }

    #[test]
    fn gcc_nested_functions_still_highlight() {
        // The case clangd cannot parse at all (DESIGN.md §2).
        let src =
            "int outer(int n) {\n    int inner(int x) { return x; }\n    return inner(n);\n}\n";
        let mut h = Highlighter::new();
        let spans = h.highlight(Language::C, src);
        assert!(
            roles_on(&spans, 1).contains(&Role::Function)
                || roles_on(&spans, 1).contains(&Role::Type),
            "nested function line highlighted: {:?}",
            spans[1]
        );
    }
}
