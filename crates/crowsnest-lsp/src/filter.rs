//! Discarding clangd diagnostics that GCC nested functions make meaningless.
//!
//! DESIGN.md §2, implemented. clangd cannot parse a nested function; it emits
//! `function definition is not allowed here` and then the *enclosing function*
//! degrades — calls to the nested function become implicit declarations, and
//! every further diagnostic in that range is parse-recovery noise rather than a
//! real finding.
//!
//! Spike 0.1 measured the blast radius: damage stops at the end of the
//! enclosing function. Symbols after it compile clean. So the filter is scoped
//! there, not to end-of-file — which would throw away most of the file's
//! genuine diagnostics.

use crowsnest_syntax::{nested_functions, Language};
use lsp_types::Diagnostic;

/// Messages clangd emits *because* of the extension, which say nothing about
/// the code.
const NOISE: &[&str] = &[
    "function definition is not allowed here",
    "expected ';' after top level declarator",
];

/// The result of filtering, so the UI can say what it hid rather than silently
/// dropping findings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filtered {
    pub kept: Vec<Diagnostic>,
    /// How many were discarded as parse-recovery noise.
    pub suppressed: usize,
    /// Names of the nested functions that caused it.
    pub because_of: Vec<String>,
}

impl Filtered {
    /// A note for the status bar, when anything was hidden.
    pub fn note(&self) -> Option<String> {
        if self.suppressed == 0 {
            return None;
        }
        Some(format!(
            "{} clangd diagnostic(s) hidden — GCC nested function{} ({}) that clangd cannot parse",
            self.suppressed,
            if self.because_of.len() == 1 { "" } else { "s" },
            self.because_of.join(", ")
        ))
    }
}

/// Filter diagnostics for one file.
///
/// A no-op for languages without the construct, and for C files that do not
/// use it — the common case must cost nothing but a parse.
pub fn filter(language: Language, source: &str, diagnostics: Vec<Diagnostic>) -> Filtered {
    if !matches!(language, Language::C | Language::Cpp) {
        return Filtered {
            kept: diagnostics,
            suppressed: 0,
            because_of: Vec::new(),
        };
    }

    let nested = nested_functions(language, source);
    if nested.is_empty() {
        return Filtered {
            kept: diagnostics,
            suppressed: 0,
            because_of: Vec::new(),
        };
    }

    let mut because_of: Vec<String> = nested.iter().map(|n| n.name.clone()).collect();
    because_of.sort();
    because_of.dedup();

    let before = diagnostics.len();
    let kept: Vec<Diagnostic> = diagnostics
        .into_iter()
        .filter(|d| {
            // Drop the tell-tale message wherever it appears...
            if NOISE.iter().any(|n| d.message.contains(n)) {
                return false;
            }
            // ...and everything inside a function whose parse it wrecked.
            let line = d.range.start.line as usize;
            !nested.iter().any(|n| n.enclosing.contains(&line))
        })
        .collect();

    Filtered {
        suppressed: before - kept.len(),
        kept,
        because_of,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{DiagnosticSeverity, Position, Range};

    /// The fixture from spike 0.1: a nested function, then unrelated code
    /// after the enclosing function closes.
    const SRC: &str = r#"int outer(int n)
{
    int shared = n * 2;
    int helper(int x)
    {
        return x + shared;
    }
    return helper(n);
}

static int after(int a)
{
    return a + undefined_thing;
}
"#;

    fn diag(line: u32, message: &str) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position { line, character: 0 },
                end: Position {
                    line,
                    character: 10,
                },
            },
            severity: Some(DiagnosticSeverity::ERROR),
            message: message.to_string(),
            ..Default::default()
        }
    }

    fn line_of(needle: &str) -> u32 {
        SRC.lines().position(|l| l.contains(needle)).unwrap() as u32
    }

    #[test]
    fn the_tell_tale_message_is_dropped() {
        let out = filter(
            Language::C,
            SRC,
            vec![diag(3, "function definition is not allowed here")],
        );
        assert!(out.kept.is_empty());
        assert_eq!(out.suppressed, 1);
    }

    #[test]
    fn noise_inside_the_wrecked_function_is_dropped_too() {
        // Not the tell-tale message, but inside `outer`, so it is recovery noise.
        let out = filter(
            Language::C,
            SRC,
            vec![diag(
                line_of("return helper(n)"),
                "implicit declaration of function 'helper'",
            )],
        );
        assert!(out.kept.is_empty(), "{:?}", out.kept);
    }

    #[test]
    fn real_diagnostics_after_the_enclosing_function_survive() {
        // This is the whole point of scoping to the function rather than to
        // end-of-file: `after` is genuinely broken and must still be reported.
        let real = diag(line_of("undefined_thing"), "use of undeclared identifier");
        let out = filter(Language::C, SRC, vec![real.clone()]);

        assert_eq!(out.kept, vec![real], "a real finding is kept");
        assert_eq!(out.suppressed, 0);
    }

    #[test]
    fn a_c_file_without_nested_functions_is_untouched() {
        let plain = "int main(void) { return bad; }\n";
        let d = vec![diag(0, "use of undeclared identifier")];
        let out = filter(Language::C, plain, d.clone());

        assert_eq!(out.kept, d);
        assert_eq!(out.suppressed, 0);
        assert!(out.because_of.is_empty());
    }

    #[test]
    fn other_languages_are_never_filtered() {
        let d = vec![diag(0, "function definition is not allowed here")];
        let out = filter(Language::Rust, "fn main() {}\n", d.clone());
        assert_eq!(out.kept, d, "the message is only meaningful for C");
    }

    #[test]
    fn the_note_names_what_caused_the_suppression() {
        let out = filter(
            Language::C,
            SRC,
            vec![diag(3, "function definition is not allowed here")],
        );
        let note = out.note().expect("something was hidden");
        assert!(note.contains("helper"), "names the nested function: {note}");
        assert!(note.contains("1 clangd diagnostic"), "{note}");
    }

    #[test]
    fn nothing_hidden_means_no_note() {
        let out = filter(Language::C, "int main(void){return 0;}\n", vec![]);
        assert!(out.note().is_none());
    }

    #[test]
    fn a_mixed_batch_keeps_exactly_the_real_findings() {
        let real = diag(line_of("undefined_thing"), "use of undeclared identifier");
        let out = filter(
            Language::C,
            SRC,
            vec![
                diag(3, "function definition is not allowed here"),
                diag(line_of("return helper(n)"), "implicit declaration"),
                real.clone(),
            ],
        );
        assert_eq!(out.kept, vec![real]);
        assert_eq!(out.suppressed, 2);
    }
}
