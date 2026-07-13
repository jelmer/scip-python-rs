use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use ruff_db::files::{File, system_path_to_file};
use ruff_db::parsed::parsed_module;
use ruff_db::source::source_text;
use ruff_db::system::{OsSystem, SystemPath};
use ruff_python_ast::visitor::{Visitor, walk_expr};
use ruff_python_ast::{AnyNodeRef, Expr, ExprAttribute, ExprContext, ExprName, Stmt};
use ruff_text_size::{Ranged, TextRange};
use scip::types::descriptor::Suffix;
use ty_module_resolver::file_to_module;
use ty_project::{ProjectDatabase, ProjectMetadata};
use ty_python_semantic::{
    ImportAliasResolution, ResolvedDefinition, SemanticModel, definitions_for_attribute,
    definitions_for_name,
};

/// Where a ty-resolved reference points.
pub enum ResolvedTarget {
    /// A definition at a byte offset in a project file.
    Location { path: PathBuf, start: u32 },
    /// A module, by dotted name.
    Module(String),
    /// A definition outside the project (typeshed, site-packages): the
    /// dotted module name plus the descriptor path within the module.
    External {
        module: String,
        descriptors: Vec<(String, Suffix)>,
    },
}

/// A reference ty resolved for us: the range of the name, whether it is a
/// store, and the definitions it points at.
pub struct InferredReference {
    pub range: TextRange,
    pub is_store: bool,
    pub targets: Vec<ResolvedTarget>,
}

pub struct Inference {
    db: ProjectDatabase,
    root: PathBuf,
}

impl Inference {
    pub fn new(project_root: &Path) -> Result<Self> {
        let root = SystemPath::from_std_path(project_root)
            .ok_or_else(|| anyhow!("project root {} is not valid UTF-8", project_root.display()))?
            .to_path_buf();
        let system = OsSystem::new(&root);
        let metadata = ProjectMetadata::discover(&root, &system)
            .map_err(|e| anyhow!("cannot discover project at {root}: {e}"))?;
        let db = ProjectDatabase::fallible(metadata, system)?;
        Ok(Inference {
            db,
            root: project_root.to_path_buf(),
        })
    }

    /// Resolve references in a file that the syntactic pass could not:
    /// attribute accesses on inferred types and names without a syntactic
    /// binding (builtins, star imports). `occupied` holds the start offsets
    /// of occurrences that were already emitted.
    pub fn references(
        &self,
        abs_path: &Path,
        occupied: &HashSet<u32>,
    ) -> Result<Vec<InferredReference>> {
        let path = SystemPath::from_std_path(abs_path)
            .ok_or_else(|| anyhow!("path {} is not valid UTF-8", abs_path.display()))?;
        let file = system_path_to_file(&self.db, path)
            .map_err(|e| anyhow!("cannot load {} into ty: {:?}", path, e))?;
        // Walk ty's own parse of the file so that AST node identities match
        // what its semantic index was built from. Ranges are identical to
        // our parse since it is the same parser and source.
        let parsed = parsed_module(&self.db, file).load(&self.db);
        let mut collector = RefCollector {
            attributes: vec![],
            names: vec![],
        };
        collector.visit_body(&parsed.syntax().body);
        let model = SemanticModel::new(&self.db, file);
        let mut references = vec![];
        for attribute in collector.attributes {
            if occupied.contains(&u32::from(attribute.attr.range().start())) {
                continue;
            }
            let targets = self.targets(definitions_for_attribute(&model, attribute));
            if !targets.is_empty() {
                references.push(InferredReference {
                    range: attribute.attr.range(),
                    is_store: matches!(attribute.ctx, ExprContext::Store),
                    targets,
                });
            }
        }
        for name in collector.names {
            if occupied.contains(&u32::from(name.range().start())) {
                continue;
            }
            let targets = self.targets(definitions_for_name(
                &model,
                name.id.as_str(),
                AnyNodeRef::from(name),
                ImportAliasResolution::ResolveAliases,
            ));
            if !targets.is_empty() {
                references.push(InferredReference {
                    range: name.range(),
                    is_store: matches!(name.ctx, ExprContext::Store),
                    targets,
                });
            }
        }
        Ok(references)
    }

    fn targets(&self, definitions: Vec<ResolvedDefinition<'_>>) -> Vec<ResolvedTarget> {
        let mut targets = vec![];
        for definition in definitions {
            match &definition {
                ResolvedDefinition::Module(file) => {
                    if let Some(module) = file_to_module(&self.db, *file) {
                        targets.push(ResolvedTarget::Module(module.name(&self.db).to_string()));
                    }
                }
                _ => {
                    let focus = definition.focus_range(&self.db);
                    let project_path = focus
                        .file()
                        .path(&self.db)
                        .as_system_path()
                        .map(|p| p.as_std_path())
                        .filter(|p| p.starts_with(&self.root));
                    match project_path {
                        Some(path) => targets.push(ResolvedTarget::Location {
                            path: path.to_path_buf(),
                            start: u32::from(focus.range().start()),
                        }),
                        None => {
                            if let Some(target) = self.external_target(focus.file(), focus.range())
                            {
                                targets.push(target);
                            }
                        }
                    }
                }
            }
        }
        targets
    }

    /// Synthesize a symbol for a definition outside the project by locating
    /// it within its module's AST: the enclosing class/function definitions
    /// become the descriptor path.
    fn external_target(&self, file: File, focus: TextRange) -> Option<ResolvedTarget> {
        let module = file_to_module(&self.db, file)?.name(&self.db).to_string();
        let parsed = parsed_module(&self.db, file).load(&self.db);
        let mut descriptors = vec![];
        match descend(&parsed.syntax().body, focus, &mut descriptors) {
            Outcome::DefLeaf => {}
            Outcome::TextLeaf => {
                let text = source_text(&self.db, file);
                let name = text
                    .as_str()
                    .get(usize::from(focus.start())..usize::from(focus.end()))?;
                if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    return None;
                }
                descriptors.push((name.to_string(), Suffix::Term));
            }
            Outcome::Bail => return None,
        }
        Some(ResolvedTarget::External {
            module,
            descriptors,
        })
    }
}

enum Outcome {
    /// The focus is the name of a class or function definition; the
    /// descriptor path is complete.
    DefLeaf,
    /// The focus is some other name (a variable or attribute); the leaf
    /// descriptor comes from the source text.
    TextLeaf,
    /// The focus is inside a function body; there is no stable symbol.
    Bail,
}

fn descend(body: &[Stmt], focus: TextRange, out: &mut Vec<(String, Suffix)>) -> Outcome {
    for stmt in body {
        if !stmt.range().contains_range(focus) {
            continue;
        }
        return match stmt {
            Stmt::ClassDef(class) => {
                out.push((class.name.to_string(), Suffix::Type));
                if class.name.range() == focus {
                    Outcome::DefLeaf
                } else {
                    descend(&class.body, focus, out)
                }
            }
            Stmt::FunctionDef(function) => {
                out.push((function.name.to_string(), Suffix::Method));
                if function.name.range() == focus {
                    Outcome::DefLeaf
                } else {
                    // Anything inside a function body is local to it.
                    match descend(&function.body, focus, out) {
                        Outcome::DefLeaf => Outcome::DefLeaf,
                        _ => Outcome::Bail,
                    }
                }
            }
            // Descend through control flow without adding descriptors;
            // typeshed guards many definitions with version checks.
            Stmt::If(s) => {
                let mut blocks: Vec<&[Stmt]> = vec![&s.body];
                blocks.extend(
                    s.elif_else_clauses
                        .iter()
                        .map(|clause| clause.body.as_slice()),
                );
                descend_blocks(&blocks, focus, out)
            }
            Stmt::Try(s) => {
                let mut blocks: Vec<&[Stmt]> = vec![&s.body, &s.orelse, &s.finalbody];
                blocks.extend(s.handlers.iter().map(|handler| {
                    let ruff_python_ast::ExceptHandler::ExceptHandler(h) = handler;
                    h.body.as_slice()
                }));
                descend_blocks(&blocks, focus, out)
            }
            Stmt::While(s) => descend_blocks(&[&s.body, &s.orelse], focus, out),
            Stmt::For(s) => descend_blocks(&[&s.body, &s.orelse], focus, out),
            Stmt::With(s) => descend_blocks(&[&s.body], focus, out),
            _ => Outcome::TextLeaf,
        };
    }
    Outcome::TextLeaf
}

fn descend_blocks(
    blocks: &[&[Stmt]],
    focus: TextRange,
    out: &mut Vec<(String, Suffix)>,
) -> Outcome {
    for block in blocks {
        if block.iter().any(|stmt| stmt.range().contains_range(focus)) {
            return descend(block, focus, out);
        }
    }
    Outcome::TextLeaf
}

struct RefCollector<'a> {
    attributes: Vec<&'a ExprAttribute>,
    names: Vec<&'a ExprName>,
}

impl<'a> Visitor<'a> for RefCollector<'a> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        match expr {
            Expr::Attribute(attribute) => self.attributes.push(attribute),
            Expr::Name(name) => self.names.push(name),
            _ => {}
        }
        walk_expr(self, expr);
    }
}
