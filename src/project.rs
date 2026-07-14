use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read as _};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use ruff_python_ast::{Expr, ModModule, Stmt};
use ruff_python_parser::{Parsed, parse_module};
use scip::types::descriptor::Suffix;
use scip::types::{Index, Metadata, ProtocolVersion, TextEncoding, ToolInfo};
use walkdir::WalkDir;

use crate::document::{DocIndexer, kind_from_suffix};
use crate::infer::{Inference, ResolvedTarget};
use crate::line_index::LineIndex;
use crate::symbols::{PackageInfo, descriptor, format_global};

pub struct Options {
    pub project_root: PathBuf,
    pub project_name: String,
    pub project_version: String,
    /// Resolve attribute references through ty's type inference in
    /// addition to syntactic scope resolution.
    pub infer: bool,
}

pub struct ParseFailure {
    pub path: PathBuf,
    pub message: String,
}

pub struct IndexResult {
    pub index: Index,
    pub errors: Vec<ParseFailure>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BindingKind {
    /// The name is bound to a module; the payload is the dotted module path.
    Module(String),
    Other,
}

#[derive(Clone, Debug)]
pub struct Binding {
    pub symbol: String,
    pub kind: BindingKind,
}

pub enum Export {
    Binding(Binding),
    Module(String),
    Reexport { module: String, name: String },
}

pub struct ProjectContext {
    pub package: PackageInfo,
    /// Dotted names of all modules in the project.
    pub modules: HashSet<String>,
    /// Top-level module/package names in the project.
    pub top_levels: HashSet<String>,
    /// Per-module map of top-level names to what they resolve to.
    pub exports: HashMap<String, HashMap<String, Export>>,
}

impl ProjectContext {
    pub fn is_project_module(&self, module: &str) -> bool {
        let top = module.split('.').next().unwrap_or(module);
        self.modules.contains(module) || self.top_levels.contains(top)
    }

    fn module_package(&self, module: &str) -> PackageInfo {
        if self.is_project_module(module) {
            self.package.clone()
        } else {
            PackageInfo {
                name: module.split('.').next().unwrap_or(module).to_string(),
                version: "unknown".to_string(),
            }
        }
    }

    pub fn module_symbol(&self, module: &str) -> String {
        format_global(
            &self.module_package(module),
            vec![descriptor(module, Suffix::Namespace)],
        )
    }

    pub fn module_binding(&self, module: &str) -> Binding {
        Binding {
            symbol: self.module_symbol(module),
            kind: BindingKind::Module(module.to_string()),
        }
    }

    pub fn member_symbol(&self, module: &str, name: &str, suffix: Suffix) -> String {
        format_global(
            &self.module_package(module),
            vec![
                descriptor(module, Suffix::Namespace),
                descriptor(name, suffix),
            ],
        )
    }

    /// A symbol for a definition addressed by module plus descriptor path,
    /// as synthesized for definitions outside the project.
    pub fn symbol_with_descriptors(&self, module: &str, path: &[(String, Suffix)]) -> String {
        let mut descriptors = vec![descriptor(module, Suffix::Namespace)];
        for (name, suffix) in path {
            descriptors.push(descriptor(name, *suffix));
        }
        format_global(&self.module_package(module), descriptors)
    }

    /// Resolve `module.name` to a binding: a submodule, an exported
    /// definition, or (as a fallback) a synthesized term under the module.
    pub fn resolve_member(&self, module: &str, name: &str, depth: u8) -> Binding {
        let submodule = format!("{module}.{name}");
        if self.modules.contains(&submodule) {
            return self.module_binding(&submodule);
        }
        if depth > 0 {
            match self.exports.get(module).and_then(|m| m.get(name)) {
                Some(Export::Binding(binding)) => return binding.clone(),
                Some(Export::Module(module)) => return self.module_binding(module),
                Some(Export::Reexport { module, name }) => {
                    return self.resolve_member(module, name, depth - 1);
                }
                None => {}
            }
        }
        Binding {
            symbol: self.member_symbol(module, name, Suffix::Term),
            kind: BindingKind::Other,
        }
    }
}

/// Resolve the base module of an import statement, taking relative import
/// levels into account. `module` is the dotted name of the importing module.
pub fn resolve_import_base(
    module: &str,
    is_package: bool,
    level: u32,
    target: Option<&str>,
) -> String {
    if level == 0 {
        return target.unwrap_or("").to_string();
    }
    let parts: Vec<&str> = if module.is_empty() {
        vec![]
    } else {
        module.split('.').collect()
    };
    let mut drop = level as usize;
    if is_package {
        drop -= 1;
    }
    let keep = parts.len().saturating_sub(drop);
    let mut base = parts[..keep].join(".");
    if let Some(target) = target {
        if base.is_empty() {
            base = target.to_string();
        } else {
            base = format!("{base}.{target}");
        }
    }
    base
}

/// Compute the dotted module name for a file path relative to the project
/// root. Returns the name and whether the file is a package `__init__`.
pub fn module_name(rel: &Path) -> (String, bool) {
    let mut parts: Vec<String> = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if let Some(last) = parts.last_mut()
        && let Some(stem) = last
            .strip_suffix(".py")
            .or_else(|| last.strip_suffix(".pyi"))
    {
        *last = stem.to_string();
    }
    // Heuristic for src layouts: modules live under src/, not in a package
    // named "src".
    if parts.len() > 1 && parts[0] == "src" {
        parts.remove(0);
    }
    let is_package = parts.last().is_some_and(|p| p == "__init__");
    if is_package && parts.len() > 1 {
        parts.pop();
        (parts.join("."), true)
    } else {
        // A bare __init__.py at the project root keeps its literal name.
        (parts.join("."), is_package)
    }
}

struct ParsedModule {
    rel_path: String,
    module: String,
    is_package: bool,
    source: String,
    parsed: Parsed<ModModule>,
}

impl ParsedModule {
    fn body(&self) -> &[Stmt] {
        &self.parsed.syntax().body
    }
}

const SKIP_DIRS: &[&str] = &["__pycache__", "venv", "node_modules", "build", "dist"];

/// Whether a `#!` line names a Python interpreter. The interpreter is the
/// last path component of the first word, or of the second word when the
/// first is `env`; it counts as Python when it is `python` or `pypy` with an
/// optional version suffix (`python3`, `python3.12`).
fn shebang_is_python(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("#!") else {
        return false;
    };
    let mut words = rest.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    let interpreter = match Path::new(first).file_name().and_then(|n| n.to_str()) {
        // `#!/usr/bin/env python3`, possibly with env's own options
        // (`env -S python3 -u`), which we skip past to the first word that
        // is not a flag.
        Some("env") => {
            let Some(word) = words.find(|w| !w.starts_with('-')) else {
                return false;
            };
            match Path::new(word).file_name().and_then(|n| n.to_str()) {
                Some(name) => name,
                None => return false,
            }
        }
        Some(name) => name,
        None => return false,
    };
    let version = interpreter
        .strip_prefix("python")
        .or_else(|| interpreter.strip_prefix("pypy"));
    version.is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// Read the first line of `path` and report whether it is a Python shebang.
/// Unreadable and non-UTF-8 files simply do not match.
fn has_python_shebang(path: &Path) -> bool {
    let Ok(file) = File::open(path) else {
        return false;
    };
    let mut line = String::new();
    // Cap the read so a binary blob without newlines is not slurped whole.
    match BufReader::new(file.take(256)).read_line(&mut line) {
        Ok(_) => shebang_is_python(&line),
        // Invalid UTF-8 in the first bytes: not a text file, so not Python.
        Err(_) => false,
    }
}

fn discover_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = vec![];
    let walker = WalkDir::new(root).into_iter().filter_entry(|entry| {
        let name = entry.file_name().to_string_lossy();
        if entry.file_type().is_dir() {
            !name.starts_with('.')
                && !SKIP_DIRS.contains(&name.as_ref())
                && !name.ends_with(".egg-info")
        } else {
            true
        }
    });
    for entry in walker {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        match path.extension().and_then(|e| e.to_str()) {
            Some("py") | Some("pyi") => {
                files.push(path.strip_prefix(root)?.to_path_buf());
            }
            // Scripts are commonly extension-less and identify themselves
            // with a shebang. Files carrying some other extension are left
            // alone rather than sniffed, so that indexing a project does not
            // mean opening every asset in it.
            None if has_python_shebang(path) => {
                files.push(path.strip_prefix(root)?.to_path_buf());
            }
            _ => {}
        }
    }
    files.sort();
    // Prefer .py over .pyi when both exist for the same module.
    let py_stems: HashSet<PathBuf> = files
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "py"))
        .map(|p| p.with_extension(""))
        .collect();
    files.retain(|p| {
        p.extension().is_some_and(|e| e == "py") || !py_stems.contains(&p.with_extension(""))
    });
    Ok(files)
}

/// Collect the names a module exposes at its top level. Recurses into
/// control-flow statements since conditional definitions are common.
fn collect_exports(
    context_module: &str,
    is_package: bool,
    stmts: &[Stmt],
    module_symbol: impl Fn(&str, Suffix) -> String + Copy,
    exports: &mut HashMap<String, Export>,
) {
    let add = |name: &str, export: Export, exports: &mut HashMap<String, Export>| {
        exports.entry(name.to_string()).or_insert(export);
    };
    let add_targets = |expr: &Expr, exports: &mut HashMap<String, Export>| {
        collect_target_names(expr, &mut |name| {
            exports.entry(name.to_string()).or_insert_with(|| {
                Export::Binding(Binding {
                    symbol: module_symbol(name, Suffix::Term),
                    kind: BindingKind::Other,
                })
            });
        });
    };
    for stmt in stmts {
        match stmt {
            Stmt::FunctionDef(def) => add(
                def.name.as_str(),
                Export::Binding(Binding {
                    symbol: module_symbol(def.name.as_str(), Suffix::Method),
                    kind: BindingKind::Other,
                }),
                exports,
            ),
            Stmt::ClassDef(def) => add(
                def.name.as_str(),
                Export::Binding(Binding {
                    symbol: module_symbol(def.name.as_str(), Suffix::Type),
                    kind: BindingKind::Other,
                }),
                exports,
            ),
            Stmt::Assign(assign) => {
                for target in &assign.targets {
                    add_targets(target, exports);
                }
            }
            Stmt::AnnAssign(assign) => add_targets(&assign.target, exports),
            Stmt::AugAssign(assign) => add_targets(&assign.target, exports),
            Stmt::Import(import) => {
                for alias in &import.names {
                    match &alias.asname {
                        Some(asname) => add(
                            asname.as_str(),
                            Export::Module(alias.name.to_string()),
                            exports,
                        ),
                        None => {
                            let top = alias.name.split('.').next().unwrap_or(&alias.name);
                            add(top, Export::Module(top.to_string()), exports);
                        }
                    }
                }
            }
            Stmt::ImportFrom(import) => {
                let base = resolve_import_base(
                    context_module,
                    is_package,
                    import.level,
                    import.module.as_ref().map(|m| m.as_str()),
                );
                for alias in &import.names {
                    if alias.name.as_str() == "*" {
                        // TODO: expand star re-exports from the source module.
                        continue;
                    }
                    let bound = alias.asname.as_ref().unwrap_or(&alias.name);
                    add(
                        bound.as_str(),
                        Export::Reexport {
                            module: base.clone(),
                            name: alias.name.to_string(),
                        },
                        exports,
                    );
                }
            }
            Stmt::If(s) => {
                collect_exports(context_module, is_package, &s.body, module_symbol, exports);
                for clause in &s.elif_else_clauses {
                    collect_exports(
                        context_module,
                        is_package,
                        &clause.body,
                        module_symbol,
                        exports,
                    );
                }
            }
            Stmt::While(s) => {
                collect_exports(context_module, is_package, &s.body, module_symbol, exports);
                collect_exports(
                    context_module,
                    is_package,
                    &s.orelse,
                    module_symbol,
                    exports,
                );
            }
            Stmt::For(s) => {
                add_targets(&s.target, exports);
                collect_exports(context_module, is_package, &s.body, module_symbol, exports);
                collect_exports(
                    context_module,
                    is_package,
                    &s.orelse,
                    module_symbol,
                    exports,
                );
            }
            Stmt::With(s) => {
                for item in &s.items {
                    if let Some(vars) = &item.optional_vars {
                        add_targets(vars, exports);
                    }
                }
                collect_exports(context_module, is_package, &s.body, module_symbol, exports);
            }
            Stmt::Try(s) => {
                collect_exports(context_module, is_package, &s.body, module_symbol, exports);
                collect_exports(
                    context_module,
                    is_package,
                    &s.orelse,
                    module_symbol,
                    exports,
                );
                collect_exports(
                    context_module,
                    is_package,
                    &s.finalbody,
                    module_symbol,
                    exports,
                );
            }
            _ => {}
        }
    }
}

fn collect_target_names(expr: &Expr, add: &mut impl FnMut(&str)) {
    match expr {
        Expr::Name(name) => add(name.id.as_str()),
        Expr::Tuple(tuple) => {
            for elt in &tuple.elts {
                collect_target_names(elt, add);
            }
        }
        Expr::List(list) => {
            for elt in &list.elts {
                collect_target_names(elt, add);
            }
        }
        Expr::Starred(starred) => collect_target_names(&starred.value, add),
        _ => {}
    }
}

pub fn index_project(options: &Options) -> Result<IndexResult> {
    let root = options
        .project_root
        .canonicalize()
        .with_context(|| format!("cannot resolve project root {:?}", options.project_root))?;
    let package = PackageInfo {
        name: options.project_name.clone(),
        version: options.project_version.clone(),
    };

    let mut parsed = vec![];
    let mut errors = vec![];
    for rel in discover_files(&root)? {
        let path = root.join(&rel);
        let source = std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read {}", path.display()))?;
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let (module, is_package) = module_name(&rel);
        match parse_module(&source) {
            Ok(module_ast) => {
                parsed.push(ParsedModule {
                    rel_path: rel_str,
                    module,
                    is_package,
                    source,
                    parsed: module_ast,
                });
            }
            Err(err) => {
                errors.push(ParseFailure {
                    path: rel,
                    message: err.to_string(),
                });
            }
        }
    }

    let modules: HashSet<String> = parsed.iter().map(|p| p.module.clone()).collect();
    let top_levels: HashSet<String> = modules
        .iter()
        .map(|m| m.split('.').next().unwrap_or(m).to_string())
        .collect();

    let mut exports = HashMap::new();
    for module in &parsed {
        let mut module_exports = HashMap::new();
        let package_ref = &package;
        let module_dotted = module.module.as_str();
        collect_exports(
            module_dotted,
            module.is_package,
            module.body(),
            |name, suffix| {
                format_global(
                    package_ref,
                    vec![
                        descriptor(module_dotted, Suffix::Namespace),
                        descriptor(name, suffix),
                    ],
                )
            },
            &mut module_exports,
        );
        exports.insert(module.module.clone(), module_exports);
    }

    let context = ProjectContext {
        package,
        modules,
        top_levels,
        exports,
    };

    let mut index = Index {
        metadata: Some(Metadata {
            version: ProtocolVersion::UnspecifiedProtocolVersion.into(),
            tool_info: Some(ToolInfo {
                name: "scip-python".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                ..Default::default()
            })
            .into(),
            project_root: format!("file://{}", root.display()),
            text_document_encoding: TextEncoding::UTF8.into(),
            ..Default::default()
        })
        .into(),
        ..Default::default()
    };

    let mut indexed = vec![];
    let mut def_table: HashMap<(String, u32), String> = HashMap::new();
    for module in &parsed {
        let indexer = DocIndexer::new(
            &module.source,
            &module.rel_path,
            &module.module,
            module.is_package,
            &context,
        );
        let result = indexer.index(module.body(), module.parsed.tokens());
        for (start, symbol) in &result.definitions {
            def_table.insert((module.rel_path.clone(), *start), symbol.clone());
        }
        indexed.push(result);
    }

    if options.infer {
        let inference = Inference::new(&root)?;
        for (module, result) in parsed.iter().zip(&mut indexed) {
            let references =
                inference.references(&root.join(&module.rel_path), &result.occupied)?;
            if references.is_empty() {
                continue;
            }
            let lines = LineIndex::new(&module.source);
            for reference in references {
                let symbol = reference.targets.iter().find_map(|target| match target {
                    ResolvedTarget::Location { path, start } => {
                        let rel = path.strip_prefix(&root).ok()?;
                        let rel_str = rel.to_string_lossy().replace('\\', "/");
                        let symbol = def_table.get(&(rel_str.clone(), *start))?;
                        // Local symbols are only meaningful within their
                        // own document.
                        if symbol.starts_with("local ") && rel_str != module.rel_path {
                            return None;
                        }
                        Some(symbol.clone())
                    }
                    ResolvedTarget::Module(name) => Some(context.module_symbol(name)),
                    ResolvedTarget::External {
                        module,
                        descriptors,
                    } => Some(context.symbol_with_descriptors(module, descriptors)),
                });
                if let Some(symbol) = symbol {
                    let range = lines.range_vec(reference.range);
                    let roles = if reference.is_store {
                        scip::types::SymbolRole::WriteAccess as i32
                    } else {
                        0
                    };
                    let syntax_kind = kind_from_suffix(&symbol, false);
                    // The syntactic pass already emitted a syntax-only
                    // occurrence for this token, since it could not resolve
                    // it to a symbol. Attach the symbol to it, and refine
                    // its kind now that we know what it refers to, rather
                    // than emitting a second occurrence over the same range.
                    match result
                        .document
                        .occurrences
                        .iter_mut()
                        .find(|o| o.symbol.is_empty() && o.range == range)
                    {
                        Some(occurrence) => {
                            occurrence.symbol = symbol;
                            occurrence.symbol_roles = roles;
                            occurrence.syntax_kind = syntax_kind.into();
                        }
                        None => result.document.occurrences.push(scip::types::Occurrence {
                            range,
                            symbol,
                            symbol_roles: roles,
                            syntax_kind: syntax_kind.into(),
                            ..Default::default()
                        }),
                    }
                }
            }
            result
                .document
                .occurrences
                .sort_by(|a, b| a.range.cmp(&b.range));
        }
    }

    index.documents = indexed.into_iter().map(|r| r.document).collect();

    Ok(IndexResult { index, errors })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn module_names() {
        assert_eq!(
            module_name(Path::new("foo/bar.py")),
            ("foo.bar".into(), false)
        );
        assert_eq!(
            module_name(Path::new("foo/__init__.py")),
            ("foo".into(), true)
        );
        assert_eq!(
            module_name(Path::new("src/foo/baz.py")),
            ("foo.baz".into(), false)
        );
        assert_eq!(module_name(Path::new("main.py")), ("main".into(), false));
        assert_eq!(
            module_name(Path::new("foo/bar.pyi")),
            ("foo.bar".into(), false)
        );
    }

    #[test]
    fn python_shebangs() {
        assert!(shebang_is_python("#!/usr/bin/python"));
        assert!(shebang_is_python("#!/usr/bin/python3"));
        assert!(shebang_is_python("#!/usr/local/bin/python3.12"));
        assert!(shebang_is_python("#!/usr/bin/env python"));
        assert!(shebang_is_python("#!/usr/bin/env python3"));
        assert!(shebang_is_python("#!/usr/bin/env python3.13\n"));
        assert!(shebang_is_python("#!/usr/bin/env pypy3"));
        assert!(shebang_is_python("#!/usr/bin/python3 -u"));
        assert!(shebang_is_python("#!/usr/bin/env -S python3 -u"));
        assert!(shebang_is_python("#! /usr/bin/env python3"));
    }

    #[test]
    fn non_python_shebangs() {
        assert!(!shebang_is_python("#!/bin/sh"));
        assert!(!shebang_is_python("#!/usr/bin/env bash"));
        assert!(!shebang_is_python("#!/usr/bin/env ruby"));
        assert!(!shebang_is_python("#!/usr/bin/env pythonista"));
        assert!(!shebang_is_python("#!/usr/bin/pythonx"));
        assert!(!shebang_is_python("#!/usr/bin/env"));
        assert!(!shebang_is_python("#!"));
        // Not a shebang at all.
        assert!(!shebang_is_python("import os"));
        assert!(!shebang_is_python("# !/usr/bin/env python3"));
        assert!(!shebang_is_python(""));
    }

    #[test]
    fn import_base_resolution() {
        assert_eq!(
            resolve_import_base("pkg.mod", false, 0, Some("os.path")),
            "os.path"
        );
        assert_eq!(resolve_import_base("pkg.mod", false, 1, None), "pkg");
        assert_eq!(
            resolve_import_base("pkg.mod", false, 1, Some("other")),
            "pkg.other"
        );
        assert_eq!(resolve_import_base("pkg", true, 1, Some("sub")), "pkg.sub");
        assert_eq!(resolve_import_base("pkg.sub.mod", false, 2, None), "pkg");
    }
}
