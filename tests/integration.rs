use std::path::PathBuf;

use scip::types::{Document, Index, SyntaxKind};
use scip_python::{Options, index_project};

fn fixture_root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn index_fixture(name: &str) -> Index {
    let result = index_project(&Options {
        project_root: fixture_root(name),
        project_name: "testpkg".to_string(),
        project_version: "1.0".to_string(),
        infer: true,
    })
    .expect("indexing failed");
    assert!(result.errors.is_empty());
    result.index
}

/// Every occurrence on `line` as (source text, syntax kind) pairs, ordered
/// by column. Reconstructing a whole line this way catches tokens that were
/// dropped, duplicated or given the wrong range.
fn highlighted_line(fixture: &str, doc: &Document, line: i32) -> Vec<(String, SyntaxKind)> {
    let source = std::fs::read_to_string(fixture_root(fixture).join(&doc.relative_path))
        .expect("cannot read fixture");
    let text = source.lines().nth(line as usize).expect("no such line");
    let mut tokens: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|o| o.range[0] == line && o.range.len() == 3)
        .filter(|o| o.syntax_kind.value() != 0)
        .map(|o| {
            let (start, end) = (o.range[1] as usize, o.range[2] as usize);
            let slice = text
                .get(start..end)
                .unwrap_or_else(|| panic!("range {start}..{end} outside line {line:?}"));
            (
                o.range[1],
                slice.to_string(),
                o.syntax_kind.enum_value_or_default(),
            )
        })
        .collect();
    tokens.sort_by_key(|(col, _, _)| *col);
    tokens
        .into_iter()
        .map(|(_, text, kind)| (text, kind))
        .collect()
}

fn kinds(pairs: &[(String, SyntaxKind)]) -> Vec<(&str, SyntaxKind)> {
    pairs.iter().map(|(t, k)| (t.as_str(), *k)).collect()
}

fn doc<'a>(index: &'a Index, rel_path: &str) -> &'a Document {
    index
        .documents
        .iter()
        .find(|d| d.relative_path == rel_path)
        .unwrap_or_else(|| panic!("no document for {rel_path}"))
}

/// All occurrences of a symbol as (range, roles) pairs, in emission order.
fn occurrences(doc: &Document, symbol: &str) -> Vec<(Vec<i32>, i32)> {
    doc.occurrences
        .iter()
        .filter(|o| o.symbol == symbol)
        .map(|o| (o.range.clone(), o.symbol_roles))
        .collect()
}

const DEF: i32 = 1;

#[test]
fn documents_present() {
    let index = index_fixture("simple");
    let mut paths: Vec<_> = index
        .documents
        .iter()
        .map(|d| d.relative_path.as_str())
        .collect();
    paths.sort();
    assert_eq!(paths, vec!["main.py", "pkg/__init__.py", "pkg/util.py"]);
}

#[test]
fn shebang_scripts_are_discovered() {
    let index = index_fixture("shebang");
    let mut paths: Vec<_> = index
        .documents
        .iter()
        .map(|d| d.relative_path.as_str())
        .collect();
    paths.sort();
    // `tool` has a Python shebang and no extension; `notpython` is a shell
    // script and must be left out.
    assert_eq!(paths, vec!["helper.py", "tool"]);
}

#[test]
fn shebang_script_references_resolve() {
    let index = index_fixture("shebang");
    let tool = doc(&index, "tool");
    // The script gets a module symbol named after the file, without an
    // extension to strip.
    assert_eq!(
        occurrences(tool, "scip-python python testpkg 1.0 tool/"),
        vec![(vec![0, 0, 0], DEF)]
    );
    // Its import resolves to the definition in helper.py at both the import
    // and the call site, so the file was really parsed and not merely listed.
    assert_eq!(
        occurrences(tool, "scip-python python testpkg 1.0 helper/greet()."),
        vec![(vec![3, 19, 24], 0), (vec![7, 10, 15], 0)]
    );
}

#[test]
fn definitions_in_module() {
    let index = index_fixture("simple");
    let util = doc(&index, "pkg/util.py");

    assert_eq!(
        occurrences(util, "scip-python python testpkg 1.0 `pkg.util`/"),
        vec![(vec![0, 0, 0], DEF)]
    );
    assert_eq!(
        occurrences(util, "scip-python python testpkg 1.0 `pkg.util`/CONSTANT."),
        vec![(vec![2, 0, 8], DEF), (vec![7, 20, 28], 0)]
    );
    assert_eq!(
        occurrences(util, "scip-python python testpkg 1.0 `pkg.util`/helper().")
            .first()
            .expect("helper definition missing"),
        &(vec![5, 4, 10], DEF)
    );
    assert_eq!(
        occurrences(
            util,
            "scip-python python testpkg 1.0 `pkg.util`/helper().(value)"
        ),
        vec![(vec![5, 11, 16], DEF), (vec![7, 12, 17], 0)]
    );
    assert_eq!(
        occurrences(util, "scip-python python testpkg 1.0 `pkg.util`/Greeter#"),
        vec![(vec![11, 6, 13], DEF)]
    );
}

#[test]
fn local_variables() {
    let index = index_fixture("simple");
    let util = doc(&index, "pkg/util.py");
    assert_eq!(
        occurrences(util, "local 0"),
        vec![(vec![7, 4, 9], DEF), (vec![8, 11, 16], 0)]
    );
}

#[test]
fn self_attributes() {
    let index = index_fixture("simple");
    let util = doc(&index, "pkg/util.py");
    // Defined via `self.name = name` in __init__, read in greet().
    assert_eq!(
        occurrences(
            util,
            "scip-python python testpkg 1.0 `pkg.util`/Greeter#name."
        ),
        vec![(vec![17, 13, 17], DEF), (vec![20, 39, 43], 0)]
    );
    // Class-level attribute, read via self in greet().
    assert_eq!(
        occurrences(
            util,
            "scip-python python testpkg 1.0 `pkg.util`/Greeter#greeting."
        ),
        vec![(vec![14, 4, 12], DEF), (vec![20, 23, 31], 0)]
    );
}

#[test]
fn cross_module_references() {
    let index = index_fixture("simple");
    let main = doc(&index, "main.py");
    // `from pkg import Greeter` resolves through the re-export in
    // pkg/__init__.py to the definition in pkg.util.
    assert_eq!(
        occurrences(main, "scip-python python testpkg 1.0 `pkg.util`/Greeter#"),
        vec![(vec![2, 16, 23], 0), (vec![6, 14, 21], 0)]
    );
    assert_eq!(
        occurrences(main, "scip-python python testpkg 1.0 `pkg.util`/helper()."),
        vec![(vec![2, 25, 31], 0), (vec![8, 11, 17], 0)]
    );
    assert_eq!(
        occurrences(main, "scip-python python testpkg 1.0 pkg/"),
        vec![(vec![2, 5, 8], 0)]
    );
    // External imports fall back to a synthesized package.
    assert_eq!(
        occurrences(main, "scip-python python os unknown os/"),
        vec![(vec![0, 7, 9], 0), (vec![11, 7, 9], 0)]
    );
}

#[test]
fn reexport_module() {
    let index = index_fixture("simple");
    let init = doc(&index, "pkg/__init__.py");
    assert_eq!(
        occurrences(init, "scip-python python testpkg 1.0 `pkg.util`/"),
        vec![(vec![0, 6, 10], 0)]
    );
    assert_eq!(
        occurrences(init, "scip-python python testpkg 1.0 `pkg.util`/Greeter#"),
        vec![(vec![0, 18, 25], 0)]
    );
}

#[test]
fn symbol_information() {
    let index = index_fixture("simple");
    let util = doc(&index, "pkg/util.py");
    let greeter = util
        .symbols
        .iter()
        .find(|s| s.symbol == "scip-python python testpkg 1.0 `pkg.util`/Greeter#")
        .expect("no symbol information for Greeter");
    assert_eq!(greeter.display_name, "Greeter");
    assert_eq!(greeter.documentation, vec!["Greets."]);
}

#[test]
fn inferred_method_call() {
    let index = index_fixture("simple");
    let main = doc(&index, "main.py");
    // greeter.greet() needs type inference: greeter is a local of
    // inferred type Greeter.
    assert_eq!(
        occurrences(
            main,
            "scip-python python testpkg 1.0 `pkg.util`/Greeter#greet()."
        ),
        vec![(vec![7, 18, 23], 0)]
    );
}

#[test]
fn inferred_builtin_name() {
    let index = index_fixture("simple");
    let main = doc(&index, "main.py");
    // print has no syntactic binding; ty resolves it into typeshed.
    assert_eq!(
        occurrences(
            main,
            "scip-python python builtins unknown builtins/print()."
        ),
        vec![(vec![7, 4, 9], 0)]
    );
}

#[test]
fn inferred_external_method() {
    let index = index_fixture("simple");
    let main = doc(&index, "main.py");
    // A method on an inferred str, synthesized from typeshed's AST.
    assert_eq!(
        occurrences(
            main,
            "scip-python python builtins unknown builtins/str#upper()."
        ),
        vec![(vec![12, 24, 29], 0)]
    );
}

#[test]
fn all_symbols_parse() {
    let index = index_fixture("simple");
    for doc in &index.documents {
        for occurrence in &doc.occurrences {
            // Syntax-only occurrences carry a highlighting kind and no symbol.
            if occurrence.symbol.is_empty() {
                assert_ne!(occurrence.syntax_kind.value(), 0);
                continue;
            }
            scip::symbol::parse_symbol(&occurrence.symbol)
                .unwrap_or_else(|e| panic!("invalid symbol {:?}: {:?}", occurrence.symbol, e));
        }
    }
}

#[test]
fn definitions_have_symbol_information() {
    let index = index_fixture("simple");
    for doc in &index.documents {
        for occurrence in &doc.occurrences {
            if occurrence.symbol_roles & DEF != 0 {
                assert!(
                    doc.symbols.iter().any(|s| s.symbol == occurrence.symbol),
                    "definition of {} in {} has no symbol information",
                    occurrence.symbol,
                    doc.relative_path
                );
            }
        }
    }
}

#[test]
fn highlights_whole_function_signature() {
    use SyntaxKind::*;
    let index = index_fixture("syntax");
    let doc = doc(&index, "tokens.py");
    // def compute(items, *args, scale=1.0, **kwargs):
    assert_eq!(
        kinds(&highlighted_line("syntax", doc, 9)),
        vec![
            ("def", IdentifierKeyword),
            ("compute", IdentifierFunctionDefinition),
            ("(", PunctuationBracket),
            ("items", IdentifierParameter),
            (",", PunctuationDelimiter),
            ("*", IdentifierOperator),
            ("args", IdentifierParameter),
            (",", PunctuationDelimiter),
            ("scale", IdentifierParameter),
            ("=", IdentifierOperator),
            ("1.0", NumericLiteral),
            (",", PunctuationDelimiter),
            ("**", IdentifierOperator),
            ("kwargs", IdentifierParameter),
            (")", PunctuationBracket),
            (":", PunctuationDelimiter),
        ]
    );
}

#[test]
fn highlights_comments_and_literals() {
    use SyntaxKind::*;
    let index = index_fixture("syntax");
    let doc = doc(&index, "tokens.py");
    assert_eq!(
        kinds(&highlighted_line("syntax", doc, 2)),
        vec![("# A leading comment.", Comment)]
    );
    // TOTAL: int = 0
    assert_eq!(
        kinds(&highlighted_line("syntax", doc, 5)),
        vec![
            ("TOTAL", IdentifierMutableGlobal),
            (":", PunctuationDelimiter),
            ("int", IdentifierBuiltinType),
            ("=", IdentifierOperator),
            ("0", NumericLiteral),
        ]
    );
}

#[test]
fn highlights_operators_and_keywords() {
    use SyntaxKind::*;
    let index = index_fixture("syntax");
    let doc = doc(&index, "tokens.py");
    // if (n := len(item)) > 2 and item is not None:
    assert_eq!(
        kinds(&highlighted_line("syntax", doc, 13)),
        vec![
            ("if", IdentifierKeyword),
            ("(", PunctuationBracket),
            ("n", IdentifierLocal),
            (":=", IdentifierOperator),
            ("len", IdentifierBuiltin),
            ("(", PunctuationBracket),
            ("item", IdentifierLocal),
            (")", PunctuationBracket),
            (")", PunctuationBracket),
            (">", IdentifierOperator),
            ("2", NumericLiteral),
            ("and", IdentifierKeyword),
            ("item", IdentifierLocal),
            ("is", IdentifierKeyword),
            ("not", IdentifierKeyword),
            ("None", IdentifierNull),
            (":", PunctuationDelimiter),
        ]
    );
}

#[test]
fn highlights_fstring_parts() {
    use SyntaxKind::*;
    let index = index_fixture("syntax");
    let doc = doc(&index, "tokens.py");
    // return f"{total:>{scale}} of {os.path.sep!r}"
    // The literal pieces stay strings; the interpolated names keep the
    // kinds they resolved to, and the nested format spec is highlighted too.
    let line = highlighted_line("syntax", doc, 21);
    assert_eq!(
        kinds(&line),
        vec![
            ("return", IdentifierKeyword),
            ("f\"", StringLiteral),
            ("{", PunctuationBracket),
            ("total", IdentifierLocal),
            (":", PunctuationDelimiter),
            (">", StringLiteral),
            ("{", PunctuationBracket),
            ("scale", IdentifierParameter),
            ("}", PunctuationBracket),
            ("}", PunctuationBracket),
            (" of ", StringLiteral),
            ("{", PunctuationBracket),
            ("os", IdentifierNamespace),
            (".", PunctuationDelimiter),
            ("path", IdentifierMutableGlobal),
            (".", PunctuationDelimiter),
            // Resolves into posixpath, so it is a global rather than an
            // unresolved plain identifier.
            ("sep", IdentifierMutableGlobal),
            ("!", IdentifierOperator),
            ("r", Identifier),
            ("}", PunctuationBracket),
            ("\"", StringLiteral),
        ]
    );
}

#[test]
fn soft_keywords_used_as_names() {
    use SyntaxKind::*;
    let index = index_fixture("syntax");
    let doc = doc(&index, "tokens.py");
    // `match` introduces a match statement here...
    assert_eq!(
        kinds(&highlighted_line("syntax", doc, 15)),
        vec![
            ("match", IdentifierKeyword),
            ("total", IdentifierLocal),
            (":", PunctuationDelimiter),
        ]
    );
    // ...but is an ordinary name here, and so is `type`.
    assert_eq!(
        kinds(&highlighted_line("syntax", doc, 35)),
        vec![
            ("match", IdentifierMutableGlobal),
            ("=", IdentifierOperator),
            ("compute", IdentifierFunction),
        ]
    );
    assert_eq!(
        kinds(&highlighted_line("syntax", doc, 36)),
        vec![
            ("type", IdentifierMutableGlobal),
            ("=", IdentifierOperator),
            ("TOTAL", IdentifierMutableGlobal),
        ]
    );
}

#[test]
fn highlights_class_and_async_method() {
    use SyntaxKind::*;
    let index = index_fixture("syntax");
    let doc = doc(&index, "tokens.py");
    // async def render(self) -> str:
    assert_eq!(
        kinds(&highlighted_line("syntax", doc, 27)),
        vec![
            ("async", IdentifierKeyword),
            ("def", IdentifierKeyword),
            ("render", IdentifierFunctionDefinition),
            ("(", PunctuationBracket),
            ("self", IdentifierParameter),
            (")", PunctuationBracket),
            ("->", IdentifierOperator),
            ("str", IdentifierBuiltinType),
            (":", PunctuationDelimiter),
        ]
    );
    // self.kind resolves to the class attribute.
    assert_eq!(
        kinds(&highlighted_line("syntax", doc, 29)),
        vec![
            ("return", IdentifierKeyword),
            ("self", IdentifierParameter),
            (".", PunctuationDelimiter),
            ("kind", IdentifierAttribute),
        ]
    );
}

/// A syntax kind must never cost a symbol: highlighting is layered onto the
/// occurrences the semantic pass produced, never emitted alongside them.
#[test]
fn every_token_occurs_once() {
    let index = index_fixture("syntax");
    for doc in &index.documents {
        let mut seen = std::collections::HashSet::new();
        for occurrence in &doc.occurrences {
            // The module definition is a zero-width marker at 0:0, not a token.
            if occurrence.range == vec![0, 0, 0] {
                continue;
            }
            assert!(
                seen.insert(occurrence.range.clone()),
                "two occurrences at {:?} in {}",
                occurrence.range,
                doc.relative_path
            );
        }
    }
}

/// Cross-module references carry a syntax kind too, even though the symbol
/// they resolve to is defined in another document.
#[test]
fn imported_symbols_are_highlighted() {
    use SyntaxKind::*;
    let index = index_fixture("simple");
    let main = doc(&index, "main.py");
    assert_eq!(
        kinds(&highlighted_line("simple", main, 2)),
        vec![
            ("from", IdentifierKeyword),
            ("pkg", IdentifierNamespace),
            ("import", IdentifierKeyword),
            ("Greeter", IdentifierType),
            (",", PunctuationDelimiter),
            ("helper", IdentifierFunction),
        ]
    );
    // greeter.greet() only resolves through type inference; it must still
    // end up as a single occurrence with both a symbol and a kind.
    assert_eq!(
        kinds(&highlighted_line("simple", main, 7)),
        vec![
            ("print", IdentifierBuiltin),
            ("(", PunctuationBracket),
            ("greeter", IdentifierLocal),
            (".", PunctuationDelimiter),
            ("greet", IdentifierFunction),
            ("(", PunctuationBracket),
            (")", PunctuationBracket),
            (")", PunctuationBracket),
        ]
    );
}

#[test]
fn write_roles() {
    let index = index_fixture("simple");
    let main = doc(&index, "main.py");
    // greeter is assigned once; the definition is a local.
    let local_defs: Vec<_> = main
        .occurrences
        .iter()
        .filter(|o| o.symbol.starts_with("local ") && o.symbol_roles == DEF)
        .map(|o| o.range.clone())
        .collect();
    assert_eq!(local_defs, vec![vec![6, 4, 11]]);
}
