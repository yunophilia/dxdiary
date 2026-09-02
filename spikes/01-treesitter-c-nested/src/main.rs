//! Spike 0.1 — Does tree-sitter-c parse GCC nested functions cleanly?
//!
//! This decides the whole C story in DESIGN.md §2. clangd cannot parse GCC
//! nested functions (llvm#9578, clangd#607) and its parse recovery corrupts
//! everything after them. The plan is to make tree-sitter the authority for C
//! highlighting/outline/same-file-nav. That only works if tree-sitter itself
//! handles nested functions — which this spike answers.
//!
//! Pass criteria:
//!   1. Zero ERROR / MISSING nodes in nested.c
//!   2. Nested function definitions appear as real `function_definition` nodes
//!   3. Symbols declared AFTER the nested functions are still found correctly
//!      (this is precisely where clang's recovery falls apart)

use tree_sitter::{Node, Parser, Tree, TreeCursor};

const NESTED: &str = include_str!("../fixtures/nested.c");
const NESTED_HARD: &str = include_str!("../fixtures/nested_hard.c");
const PLAIN: &str = include_str!("../fixtures/plain.c");

/// Symbols that appear after the nested functions in nested.c. If tree-sitter
/// degrades the way clang does, these go missing or come back wrong.
const POST_MARKER_SYMBOLS: &[&str] = &["after_marker", "after_nested", "after_kind", "main"];

#[derive(Debug)]
struct Symbol {
    name: String,
    kind: &'static str,
    line: usize,
    /// Depth in enclosing function_definition nodes. 0 = top level.
    nest_depth: usize,
}

fn main() {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .expect("load tree-sitter-c grammar");

    let mut all_passed = true;

    let cases: &[(&str, &str, bool, &[&str])] = &[
        ("nested.c", NESTED, true, POST_MARKER_SYMBOLS),
        (
            "nested_hard.c",
            NESTED_HARD,
            true,
            &["mutual", "nonlocal_goto", "sort_with_nested", "trailer", "main"],
        ),
        ("plain.c", PLAIN, false, POST_MARKER_SYMBOLS),
    ];

    for (label, src, expect_nested, post) in cases {
        println!("\n{:=<72}", "");
        println!("== {label}");
        println!("{:=<72}", "");

        let tree = parser.parse(src, None).expect("parse produced no tree");
        let passed = report(&tree, src, *expect_nested, post);
        all_passed &= passed;
    }

    println!("\n{:=<72}", "");
    if all_passed {
        println!("SPIKE 0.1 RESULT: PASS — tree-sitter-c handles GCC nested functions.");
        println!("  DESIGN.md §2 two-source model is viable.");
    } else {
        println!("SPIKE 0.1 RESULT: FAIL — tree-sitter-c degrades on nested functions.");
        println!("  DESIGN.md §2 needs rethinking; C support is in serious doubt.");
        std::process::exit(1);
    }
}

fn report(tree: &Tree, src: &str, expect_nested: bool, post_symbols: &[&str]) -> bool {
    let root = tree.root_node();
    let mut passed = true;

    // --- 1. Parse errors ------------------------------------------------
    let mut errors = Vec::new();
    collect_errors(&mut root.walk(), &mut errors);

    println!("\n[1] Parse integrity");
    println!("    root.has_error() = {}", root.has_error());
    if errors.is_empty() {
        println!("    ✓ no ERROR / MISSING nodes");
    } else {
        passed = false;
        println!("    ✗ {} problem node(s):", errors.len());
        for (kind, line, text) in &errors {
            println!("        {kind} at line {line}: {text:?}");
        }
    }

    // --- 2. Symbol outline ----------------------------------------------
    let mut symbols = Vec::new();
    collect_symbols(&mut root.walk(), src, 0, &mut symbols);

    println!("\n[2] Symbol outline ({} symbols)", symbols.len());
    for s in &symbols {
        let indent = "    ".repeat(s.nest_depth);
        let tag = if s.nest_depth > 0 { "  <NESTED>" } else { "" };
        println!(
            "    L{:<3} {indent}{} {}{tag}",
            s.line, s.kind, s.name
        );
    }

    let nested_fns: Vec<_> = symbols
        .iter()
        .filter(|s| s.kind == "fn" && s.nest_depth > 0)
        .collect();

    if expect_nested {
        println!("\n[3] Nested function detection");
        if nested_fns.is_empty() {
            passed = false;
            println!("    ✗ no nested function_definition nodes found");
        } else {
            println!(
                "    ✓ {} nested function(s): {}",
                nested_fns.len(),
                nested_fns
                    .iter()
                    .map(|s| format!("{}(depth {})", s.name, s.nest_depth))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    // --- 4. The clangd failure region -----------------------------------
    println!("\n[4] Symbols after the nested-function block");
    for want in post_symbols {
        match symbols.iter().find(|s| s.name == *want) {
            Some(s) => println!("    ✓ {:<14} found (L{}, {})", want, s.line, s.kind),
            None => {
                passed = false;
                println!("    ✗ {want:<14} MISSING");
            }
        }
    }

    passed
}

fn collect_errors(cursor: &mut TreeCursor, out: &mut Vec<(&'static str, usize, String)>) {
    let node = cursor.node();
    if node.is_error() {
        out.push(("ERROR", node.start_position().row + 1, node.kind().to_string()));
    } else if node.is_missing() {
        out.push(("MISSING", node.start_position().row + 1, node.kind().to_string()));
    }
    if cursor.goto_first_child() {
        loop {
            collect_errors(cursor, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn collect_symbols(cursor: &mut TreeCursor, src: &str, depth: usize, out: &mut Vec<Symbol>) {
    let node = cursor.node();
    let mut child_depth = depth;

    let entry = match node.kind() {
        "function_definition" => {
            child_depth = depth + 1;
            declarator_name(node, src).map(|name| ("fn", name))
        }
        "struct_specifier" => field_text(node, "name", src).map(|n| ("struct", n)),
        "enum_specifier" => field_text(node, "name", src).map(|n| ("enum", n)),
        "type_definition" => typedef_name(node, src).map(|n| ("typedef", n)),
        _ => None,
    };

    if let Some((kind, name)) = entry {
        out.push(Symbol {
            name,
            kind,
            line: node.start_position().row + 1,
            nest_depth: depth,
        });
    }

    if cursor.goto_first_child() {
        loop {
            collect_symbols(cursor, src, child_depth, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// Walk down through pointer/array/function declarators to the identifier.
fn declarator_name(node: Node, src: &str) -> Option<String> {
    let mut cur = node.child_by_field_name("declarator")?;
    loop {
        match cur.kind() {
            "identifier" | "type_identifier" => {
                return Some(cur.utf8_text(src.as_bytes()).ok()?.to_string())
            }
            _ => cur = cur.child_by_field_name("declarator")?,
        }
    }
}

fn field_text(node: Node, field: &str, src: &str) -> Option<String> {
    Some(
        node.child_by_field_name(field)?
            .utf8_text(src.as_bytes())
            .ok()?
            .to_string(),
    )
}

/// `typedef enum {...} after_kind;` — name is the last type_identifier child.
fn typedef_name(node: Node, src: &str) -> Option<String> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|c| c.kind() == "type_identifier")
        .last()
        .and_then(|c| c.utf8_text(src.as_bytes()).ok().map(str::to_string))
}
