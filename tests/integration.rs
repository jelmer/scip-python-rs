use std::path::PathBuf;

use scip::types::{Document, Index};
use scip_python::{Options, index_project};

fn index_fixture(name: &str) -> Index {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let result = index_project(&Options {
        project_root: root,
        project_name: "testpkg".to_string(),
        project_version: "1.0".to_string(),
        infer: true,
    })
    .expect("indexing failed");
    assert!(result.errors.is_empty());
    result.index
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
