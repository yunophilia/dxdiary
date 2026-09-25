//! GCC nested-function detection.
//!
//! The workaround that makes C usable at all (DESIGN.md §2). clangd cannot
//! parse a nested function — it reports `function definition is not allowed
//! here` and the *enclosing function* degrades: calls to the nested function
//! fall back to implicit declarations, and every diagnostic in that range
//! becomes parse-recovery noise.
//!
//! tree-sitter parses them cleanly (spike 0.1), so it can say exactly which
//! byte ranges to distrust clangd about. Damage is contained to the enclosing
//! function, so the filter is scoped to that — not to end-of-file, which would
//! throw away most of the file's real diagnostics.

use tree_sitter::{Node, Parser};

use crate::language::Language;

/// A nested function, and the enclosing function whose diagnostics are suspect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NestedFn {
    pub name: String,
    /// How deeply nested; 1 is a function directly inside a top-level one.
    pub depth: usize,
    /// 0-based line of the nested definition.
    pub line: usize,
    /// Line range of the outermost enclosing function, inclusive of the start
    /// and exclusive of the end. Clangd diagnostics here are unreliable.
    pub enclosing: std::ops::Range<usize>,
}

/// Find GCC nested functions in C or C++ source.
///
/// Returns empty for other languages, which have no such construct.
pub fn nested_functions(lang: Language, source: &str) -> Vec<NestedFn> {
    if !matches!(lang, Language::C | Language::Cpp) {
        return Vec::new();
    }

    let mut parser = Parser::new();
    if parser.set_language(&lang.grammar()).is_err() {
        return Vec::new();
    }
    let Some(tree) = parser.parse(source, None) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    walk(tree.root_node(), source, &mut Vec::new(), &mut out);
    out
}

/// `ancestors` holds the enclosing `function_definition` nodes, outermost first.
fn walk<'a>(node: Node<'a>, source: &str, ancestors: &mut Vec<Node<'a>>, out: &mut Vec<NestedFn>) {
    let is_fn = node.kind() == "function_definition";

    if is_fn && !ancestors.is_empty() {
        // The outermost enclosing function is what clangd mis-parses, so the
        // suppression range is its extent, not the immediate parent's.
        let root_fn = ancestors[0];
        if let Some(name) = declarator_name(node, source) {
            out.push(NestedFn {
                name,
                depth: ancestors.len(),
                line: node.start_position().row,
                enclosing: root_fn.start_position().row..root_fn.end_position().row + 1,
            });
        }
    }

    if is_fn {
        ancestors.push(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, source, ancestors, out);
    }
    if is_fn {
        ancestors.pop();
    }
}

/// Descend through pointer, array, and function declarators to the identifier.
fn declarator_name(node: Node, source: &str) -> Option<String> {
    let mut cur = node.child_by_field_name("declarator")?;
    loop {
        match cur.kind() {
            "identifier" | "type_identifier" | "field_identifier" => {
                return Some(cur.utf8_text(source.as_bytes()).ok()?.to_string())
            }
            _ => cur = cur.child_by_field_name("declarator")?,
        }
    }
}

/// True if `line` falls inside a function containing a nested definition, and
/// clangd's diagnostics there should therefore be discarded.
pub fn is_suspect_line(nested: &[NestedFn], line: usize) -> bool {
    nested.iter().any(|n| n.enclosing.contains(&line))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NESTED: &str = r#"
int outer(int n)
{
    int shared = n * 2;

    int helper(int x)
    {
        return x + shared;
    }

    int accumulate(int count)
    {
        int total = 0;
        void bump(int v) { total += v; }
        for (int i = 0; i < count; i++) bump(helper(i));
        return total;
    }

    return accumulate(n);
}

static int after(int a) { return a; }
"#;

    #[test]
    fn finds_every_nested_function_with_its_depth() {
        let found = nested_functions(Language::C, NESTED);
        let mut names: Vec<_> = found.iter().map(|n| (n.name.as_str(), n.depth)).collect();
        names.sort();
        assert_eq!(
            names,
            vec![("accumulate", 1), ("bump", 2), ("helper", 1)],
            "{found:#?}"
        );
    }

    #[test]
    fn a_plain_file_reports_nothing() {
        let src = "int a(void) { return 1; }\nint b(void) { return 2; }\n";
        assert!(nested_functions(Language::C, src).is_empty());
    }

    #[test]
    fn the_suppression_range_is_the_enclosing_function_not_the_whole_file() {
        let found = nested_functions(Language::C, NESTED);
        let range = &found[0].enclosing;

        let after_line = NESTED
            .lines()
            .position(|l| l.contains("static int after"))
            .unwrap();
        assert!(
            !range.contains(&after_line),
            "code after the enclosing function keeps its diagnostics: {range:?}"
        );

        let helper_line = NESTED
            .lines()
            .position(|l| l.contains("int helper"))
            .unwrap();
        assert!(
            range.contains(&helper_line),
            "the nested function is inside"
        );
    }

    #[test]
    fn suspect_lines_are_exactly_the_enclosing_function() {
        let found = nested_functions(Language::C, NESTED);
        let line_of = |needle: &str| NESTED.lines().position(|l| l.contains(needle)).unwrap();

        assert!(is_suspect_line(&found, line_of("int shared")));
        assert!(!is_suspect_line(&found, line_of("static int after")));
    }

    #[test]
    fn languages_without_the_construct_are_skipped() {
        let src = "fn outer() { fn inner() {} }\n";
        assert!(
            nested_functions(Language::Rust, src).is_empty(),
            "Rust nesting is ordinary and needs no workaround"
        );
    }

    #[test]
    fn mutually_recursive_nested_functions_are_both_found() {
        // The `auto` forward-declaration form from spike 0.1's hard fixture.
        let src = "int mutual(int n)\n{\n    auto int second(int);\n    int first(int x) { return second(x); }\n    int second(int x) { return x; }\n    return first(n);\n}\n";
        let found = nested_functions(Language::C, src);
        let mut names: Vec<_> = found.iter().map(|n| n.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["first", "second"]);
    }
}
