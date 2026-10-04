//! Finding a literal string in an open file.
//!
//! Literal, not regex. The job here is "where else does this symbol appear",
//! which is what a reviewer asks a hundred times a day, and a regex engine
//! would add a dependency plus a syntax to get wrong for a case that `/` in
//! `less` has never needed. Regex can come later behind its own prefix if it
//! earns it.
//!
//! Offsets are **raw character indices** into the line, matching
//! `dxdiary_syntax::Span`, so the view maps both through the same column map
//! from `document::render_line_mapped`.

/// One occurrence, as a half-open range of characters on one line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// A query and everywhere it occurs, with one occurrence selected.
#[derive(Clone, Debug, Default)]
pub struct Search {
    pub query: String,
    pub matches: Vec<Match>,
    /// Index into `matches`. Meaningless when `matches` is empty.
    pub current: usize,
}

/// Should this query be matched case-sensitively?
///
/// Smart case, as vim and ripgrep do it: an all-lowercase query matches
/// anything, and typing a capital means you meant it. Searching for `new` in
/// Rust should find `New` too; searching for `Buffer` should not stop on
/// `buffer`.
pub fn case_sensitive(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

/// Do two characters match, under `sensitive`?
///
/// Compares the full case-folding iterators rather than one folded character,
/// so a multi-character fold is handled without ever changing an offset --
/// which is the reason this is per character and not `to_lowercase()` on whole
/// lines. Folding a line can change its length, and every offset here has to
/// stay an index into the original.
fn same(a: char, b: char, sensitive: bool) -> bool {
    if sensitive || a == b {
        return a == b;
    }
    a.to_lowercase().eq(b.to_lowercase())
}

impl Search {
    /// Find every occurrence of `query` in `lines`.
    ///
    /// Overlapping occurrences are not reported: after a hit the scan resumes
    /// past it, so `aa` in `aaa` is one match, not two. That is what you want
    /// when stepping through with `n`.
    pub fn new(lines: &[String], query: &str) -> Self {
        let mut matches = Vec::new();
        if !query.is_empty() {
            let sensitive = case_sensitive(query);
            let needle: Vec<char> = query.chars().collect();

            for (n, line) in lines.iter().enumerate() {
                let hay: Vec<char> = line.chars().collect();
                if hay.len() < needle.len() {
                    continue;
                }
                let mut i = 0;
                while i + needle.len() <= hay.len() {
                    let hit = needle
                        .iter()
                        .enumerate()
                        .all(|(k, &c)| same(hay[i + k], c, sensitive));
                    if hit {
                        matches.push(Match {
                            line: n,
                            start: i,
                            end: i + needle.len(),
                        });
                        i += needle.len();
                    } else {
                        i += 1;
                    }
                }
            }
        }

        Self {
            query: query.to_string(),
            matches,
            current: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    pub fn len(&self) -> usize {
        self.matches.len()
    }

    /// The selected occurrence.
    pub fn selected(&self) -> Option<Match> {
        self.matches.get(self.current).copied()
    }

    /// Select the first occurrence at or after `line`, for starting a search
    /// from where the cursor already is rather than from the top of the file.
    ///
    /// Falls back to the first occurrence, so a query whose only hits are
    /// above the cursor still selects something.
    pub fn select_from(&mut self, line: usize) {
        self.current = self
            .matches
            .iter()
            .position(|m| m.line >= line)
            .unwrap_or(0);
    }

    /// Step to the next or previous occurrence, wrapping.
    ///
    /// Returns true when it wrapped, so the caller can say so -- silently
    /// jumping back to the top of a file is disorienting.
    pub fn step(&mut self, forward: bool) -> bool {
        if self.matches.is_empty() {
            return false;
        }
        if forward {
            self.current += 1;
            if self.current >= self.matches.len() {
                self.current = 0;
                return true;
            }
        } else {
            if self.current == 0 {
                self.current = self.matches.len() - 1;
                return true;
            }
            self.current -= 1;
        }
        false
    }

    /// Occurrences on one line, for the renderer.
    pub fn on_line(&self, line: usize) -> impl Iterator<Item = (Match, bool)> + '_ {
        let current = self.selected();
        self.matches
            .iter()
            .filter(move |m| m.line == line)
            .map(move |m| (*m, Some(*m) == current))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_string).collect()
    }

    #[test]
    fn finds_every_occurrence_in_order() {
        let s = Search::new(&lines("foo bar\nbaz\nfoo"), "foo");
        assert_eq!(s.len(), 2);
        assert_eq!(
            s.matches[0],
            Match {
                line: 0,
                start: 0,
                end: 3
            }
        );
        assert_eq!(s.matches[1].line, 2);
    }

    #[test]
    fn several_hits_on_one_line_are_all_found() {
        let s = Search::new(&lines("x x x"), "x");
        assert_eq!(s.len(), 3);
        assert_eq!(
            s.matches.iter().map(|m| m.start).collect::<Vec<_>>(),
            vec![0, 2, 4]
        );
    }

    #[test]
    fn overlapping_hits_are_reported_once() {
        // Stepping through `aa` in `aaa` twice, landing one character apart,
        // reads as a stuck key.
        let s = Search::new(&lines("aaa"), "aa");
        assert_eq!(s.len(), 1);
        assert_eq!(s.matches[0].end, 2);
    }

    #[test]
    fn an_empty_query_matches_nothing_rather_than_everything() {
        assert!(Search::new(&lines("anything"), "").is_empty());
    }

    #[test]
    fn a_lowercase_query_ignores_case() {
        let s = Search::new(&lines("New new NEW"), "new");
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn a_query_with_a_capital_is_taken_literally() {
        let s = Search::new(&lines("Buffer buffer"), "Buffer");
        assert_eq!(s.len(), 1);
        assert_eq!(s.matches[0].start, 0);
    }

    #[test]
    fn offsets_are_characters_not_bytes() {
        // Three two-byte characters, then the needle: a byte offset would
        // report 6 and the renderer would highlight the wrong columns.
        let s = Search::new(&lines("ααα x"), "x");
        assert_eq!(s.matches[0].start, 4);
    }

    #[test]
    fn case_folding_does_not_shift_offsets() {
        // `İ` lowercases to two characters. Folding the whole line would move
        // every offset after it; folding per character cannot.
        let s = Search::new(&lines("İx"), "x");
        assert_eq!(s.matches[0].start, 1);
    }

    #[test]
    fn stepping_forward_wraps_and_says_so() {
        let mut s = Search::new(&lines("a\na"), "a");
        assert!(!s.step(true), "first step stays inside the file");
        assert_eq!(s.current, 1);
        assert!(s.step(true), "the second wraps");
        assert_eq!(s.current, 0);
    }

    #[test]
    fn stepping_backward_wraps_to_the_last() {
        let mut s = Search::new(&lines("a\na\na"), "a");
        assert!(s.step(false));
        assert_eq!(s.current, 2);
    }

    #[test]
    fn stepping_an_empty_result_does_nothing() {
        let mut s = Search::new(&lines("abc"), "zzz");
        assert!(!s.step(true));
        assert_eq!(s.selected(), None);
    }

    #[test]
    fn a_search_starts_from_the_cursor_not_the_top() {
        let mut s = Search::new(&lines("a\nb\na\nb\na"), "a");
        s.select_from(3);
        assert_eq!(s.selected().unwrap().line, 4);
    }

    #[test]
    fn starting_below_every_hit_falls_back_to_the_first() {
        let mut s = Search::new(&lines("a\nb\nb"), "a");
        s.select_from(2);
        assert_eq!(s.selected().unwrap().line, 0, "rather than selecting none");
    }

    #[test]
    fn the_renderer_is_told_which_hit_on_a_line_is_selected() {
        let mut s = Search::new(&lines("x x"), "x");
        s.step(true);
        let row: Vec<bool> = s.on_line(0).map(|(_, cur)| cur).collect();
        assert_eq!(row, vec![false, true]);
    }

    #[test]
    fn a_line_with_no_hits_yields_nothing() {
        let s = Search::new(&lines("x\ny"), "x");
        assert_eq!(s.on_line(1).count(), 0);
    }
}
