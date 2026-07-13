use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow};
use ruff_db::Db as SourceDb;
use ruff_db::diagnostic::Diagnostic;
use ruff_db::files::{File, Files, system_path_to_file};
use ruff_db::parsed::parsed_module;
use ruff_db::system::{OsSystem, System, SystemPathBuf};
use ruff_db::vendored::VendoredFileSystem;
use ruff_python_ast::visitor::{Visitor, walk_expr};
use ruff_python_ast::{Expr, ExprAttribute, ExprContext, PythonVersion};
use ruff_text_size::{Ranged, TextRange};
use ty_module_resolver::{Db as ModuleResolverDb, SearchPathSettings, SearchPaths};
use ty_python_core::platform::PythonPlatform;
use ty_python_core::program::{FallibleStrategy, Program, ProgramSettings};
use ty_python_semantic::lint::{LintRegistry, RuleSelection};
use ty_python_semantic::types::ide_support::{ResolvedDefinition, definitions_for_attribute};
use ty_python_semantic::{
    AnalysisSettings, Db, SemanticModel, check_file_unwrap, default_lint_registry,
};
use ty_site_packages::{PythonVersionSource, PythonVersionWithSource};

#[salsa::db]
#[derive(Clone)]
pub struct IndexerDb {
    storage: salsa::Storage<Self>,
    files: Files,
    system: OsSystem,
    vendored: VendoredFileSystem,
    rule_selection: Arc<RuleSelection>,
    analysis_settings: Arc<AnalysisSettings>,
}

#[salsa::db]
impl SourceDb for IndexerDb {
    fn vendored(&self) -> &VendoredFileSystem {
        &self.vendored
    }

    fn system(&self) -> &dyn System {
        &self.system
    }

    fn files(&self) -> &Files {
        &self.files
    }

    fn python_version(&self) -> PythonVersion {
        Program::get(self).python_version(self)
    }
}

#[salsa::db]
impl ty_python_core::Db for IndexerDb {
    fn should_check_file(&self, file: File) -> bool {
        !file.path(self).is_vendored_path()
    }
}

#[salsa::db]
impl ModuleResolverDb for IndexerDb {
    fn search_paths(&self) -> &SearchPaths {
        Program::get(self).search_paths(self)
    }
}

#[salsa::db]
impl Db for IndexerDb {
    fn check_file(&self, file: File) -> Vec<Diagnostic> {
        if !ty_python_core::Db::should_check_file(self, file) {
            return Vec::new();
        }
        check_file_unwrap(self, file)
    }

    fn rule_selection(&self, _file: File) -> &RuleSelection {
        &self.rule_selection
    }

    fn lint_registry(&self) -> &LintRegistry {
        default_lint_registry()
    }

    fn analysis_settings(&self, _file: File) -> &AnalysisSettings {
        &self.analysis_settings
    }

    fn verbose(&self) -> bool {
        false
    }

    fn dyn_clone(&self) -> Box<dyn Db> {
        Box::new(self.clone())
    }
}

#[salsa::db]
impl salsa::Database for IndexerDb {}

/// Where a ty-resolved attribute reference points.
pub enum ResolvedTarget {
    /// A definition at a byte offset in a file.
    Location { path: PathBuf, start: u32 },
    /// An entire module.
    Module(PathBuf),
}

/// An attribute reference ty resolved for us: the range of the attribute
/// name, whether it is a store, and the definitions it points at.
pub struct InferredReference {
    pub range: TextRange,
    pub is_store: bool,
    pub targets: Vec<ResolvedTarget>,
}

pub struct Inference {
    db: IndexerDb,
}

impl Inference {
    pub fn new(project_root: &Path) -> Result<Self> {
        let root = SystemPathBuf::from_path_buf(project_root.to_path_buf())
            .map_err(|p| anyhow!("project root {} is not valid UTF-8", p.display()))?;
        let db = IndexerDb {
            storage: salsa::Storage::new(None),
            files: Files::default(),
            system: OsSystem::new(&root),
            vendored: ty_vendored::file_system().clone(),
            rule_selection: Arc::new(RuleSelection::from_registry(default_lint_registry())),
            analysis_settings: Arc::new(AnalysisSettings::default()),
        };
        // Match the src-layout heuristic used for module naming: modules
        // may live either at the root or under src/.
        let mut src_roots = vec![];
        if root.join("src").as_std_path().is_dir() {
            src_roots.push(root.join("src"));
        }
        src_roots.push(root.clone());
        // TODO: derive the Python version from requires-python in
        // pyproject.toml instead of using ty's default.
        Program::from_settings(
            &db,
            ProgramSettings {
                python_version: PythonVersionWithSource {
                    version: PythonVersion::default(),
                    source: PythonVersionSource::default(),
                },
                python_platform: PythonPlatform::default(),
                search_paths: SearchPathSettings::new(src_roots)
                    .to_search_paths(db.system(), db.vendored(), &FallibleStrategy)
                    .context("invalid search path settings")?,
            },
        );
        Ok(Inference { db })
    }

    /// Resolve attribute references in a file that the syntactic pass could
    /// not, using ty's type inference. `occupied` holds the start offsets of
    /// occurrences that were already emitted.
    pub fn attribute_references(
        &self,
        abs_path: &Path,
        occupied: &HashSet<u32>,
    ) -> Result<Vec<InferredReference>> {
        let path = SystemPathBuf::from_path_buf(abs_path.to_path_buf())
            .map_err(|p| anyhow!("path {} is not valid UTF-8", p.display()))?;
        let file = system_path_to_file(&self.db, &path)
            .map_err(|e| anyhow!("cannot load {} into ty: {:?}", path, e))?;
        // Walk ty's own parse of the file so that AST node identities match
        // what its semantic index was built from. Ranges are identical to
        // our parse since it is the same parser and source.
        let parsed = parsed_module(&self.db, file).load(&self.db);
        let mut collector = AttributeCollector { attributes: vec![] };
        collector.visit_body(&parsed.syntax().body);
        let model = SemanticModel::new(&self.db, file);
        let mut references = vec![];
        for attribute in collector.attributes {
            if occupied.contains(&u32::from(attribute.attr.range().start())) {
                continue;
            }
            let mut targets = vec![];
            for definition in definitions_for_attribute(&model, attribute) {
                match &definition {
                    ResolvedDefinition::Module(file) => {
                        if let Some(path) = file.path(&self.db).as_system_path() {
                            targets.push(ResolvedTarget::Module(path.as_std_path().to_path_buf()));
                        }
                    }
                    _ => {
                        let focus = definition.focus_range(&self.db);
                        if let Some(path) = focus.file().path(&self.db).as_system_path() {
                            targets.push(ResolvedTarget::Location {
                                path: path.as_std_path().to_path_buf(),
                                start: u32::from(focus.range().start()),
                            });
                        }
                        // TODO: synthesize symbols for definitions in
                        // vendored typeshed stubs.
                    }
                }
            }
            if !targets.is_empty() {
                references.push(InferredReference {
                    range: attribute.attr.range(),
                    is_store: matches!(attribute.ctx, ExprContext::Store),
                    targets,
                });
            }
        }
        Ok(references)
    }
}

struct AttributeCollector<'a> {
    attributes: Vec<&'a ExprAttribute>,
}

impl<'a> Visitor<'a> for AttributeCollector<'a> {
    fn visit_expr(&mut self, expr: &'a Expr) {
        if let Expr::Attribute(attribute) = expr {
            self.attributes.push(attribute);
        }
        walk_expr(self, expr);
    }
}
