//! Language detection and grammar access.

use std::path::Path;

use tree_sitter::Language as TsLanguage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Language {
    C,
    Cpp,
    Go,
    Python,
    Rust,
}

impl Language {
    pub const ALL: &'static [Language] = &[
        Language::C,
        Language::Cpp,
        Language::Go,
        Language::Python,
        Language::Rust,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Go => "go",
            Language::Python => "python",
            Language::Rust => "rust",
        }
    }

    /// Detect from a file extension.
    ///
    /// `.h` is the ambiguous one — it is C in a C project and C++ in a C++ one,
    /// and nothing in the filename says which. C is the safer default: the C
    /// grammar parses the common subset, whereas the C++ grammar applied to C
    /// mis-parses ordinary identifiers that happen to be C++ keywords.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Some(match ext.as_str() {
            "c" | "h" => Language::C,
            "cc" | "cpp" | "cxx" | "c++" | "hpp" | "hh" | "hxx" => Language::Cpp,
            "go" => Language::Go,
            "py" | "pyi" => Language::Python,
            "rs" => Language::Rust,
            _ => return None,
        })
    }

    pub fn grammar(self) -> TsLanguage {
        match self {
            Language::C => tree_sitter_c::LANGUAGE.into(),
            Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
        }
    }

    /// The grammar's highlight query.
    ///
    /// Two upstream quirks are absorbed here:
    ///
    /// - The constant is spelled `HIGHLIGHT_QUERY` in the C and C++ crates and
    ///   `HIGHLIGHTS_QUERY` in the others.
    /// - **C++'s query only covers C++-specific nodes.** The C++ grammar is a
    ///   superset of C, and its query assumes C's is prepended — without that,
    ///   `int main() { return 0; }` highlights nothing at all. Other editors do
    ///   the same concatenation.
    pub fn highlight_query(self) -> String {
        match self {
            Language::C => tree_sitter_c::HIGHLIGHT_QUERY.to_string(),
            Language::Cpp => {
                format!(
                    "{}\n{}",
                    tree_sitter_c::HIGHLIGHT_QUERY,
                    tree_sitter_cpp::HIGHLIGHT_QUERY
                )
            }
            Language::Go => tree_sitter_go::HIGHLIGHTS_QUERY.to_string(),
            Language::Python => tree_sitter_python::HIGHLIGHTS_QUERY.to_string(),
            Language::Rust => tree_sitter_rust::HIGHLIGHTS_QUERY.to_string(),
        }
    }

    /// Injected sub-languages, where the grammar defines any.
    pub fn injection_query(self) -> &'static str {
        match self {
            Language::Rust => tree_sitter_rust::INJECTIONS_QUERY,
            _ => "",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_each_supported_language() {
        let cases = [
            ("main.c", Language::C),
            ("lib.h", Language::C),
            ("app.cpp", Language::Cpp),
            ("app.hpp", Language::Cpp),
            ("server.go", Language::Go),
            ("script.py", Language::Python),
            ("lib.rs", Language::Rust),
        ];
        for (name, want) in cases {
            assert_eq!(Language::from_path(Path::new(name)), Some(want), "{name}");
        }
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert_eq!(Language::from_path(Path::new("MAIN.C")), Some(Language::C));
    }

    #[test]
    fn unknown_extensions_and_bare_names_are_not_guessed() {
        assert_eq!(Language::from_path(Path::new("README.md")), None);
        assert_eq!(Language::from_path(Path::new("Makefile")), None);
        assert_eq!(Language::from_path(Path::new("noext")), None);
    }

    #[test]
    fn every_grammar_loads_and_has_a_highlight_query() {
        // A version mismatch between tree-sitter and a grammar shows up here
        // rather than as a mystery at runtime.
        for lang in Language::ALL {
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&lang.grammar())
                .unwrap_or_else(|e| panic!("{}: {e}", lang.name()));
            assert!(
                !lang.highlight_query().is_empty(),
                "{} has no highlight query",
                lang.name()
            );
        }
    }
}
