use std::collections::{HashMap, HashSet};

use ruff_python_ast::{
    Comprehension, ExceptHandler, Expr, ExprAttribute, ExprLambda, InterpolatedStringElement,
    Parameter, Parameters, Pattern, Stmt, StmtClassDef, StmtFunctionDef, StmtImport,
    StmtImportFrom,
};
use ruff_text_size::{Ranged, TextRange, TextSize};
use scip::types::descriptor::Suffix;
use scip::types::symbol_information::Kind;
use scip::types::{
    Descriptor, Document, Occurrence, PositionEncoding, SymbolInformation, SymbolRole,
};

use crate::line_index::LineIndex;
use crate::project::{Binding, BindingKind, ProjectContext, resolve_import_base};
use crate::symbols::{descriptor, format_global, local_symbol};

enum ScopeType {
    Module,
    Class,
    Function { receiver: Option<String> },
    Comprehension,
}

struct Scope {
    kind: ScopeType,
    /// Descriptor path of this container for global symbols; None when
    /// definitions inside it become SCIP local symbols.
    prefix: Option<Vec<Descriptor>>,
    bindings: HashMap<String, Binding>,
}

/// The result of indexing one file: the SCIP document plus the byte-offset
/// information the type-inference pass needs to add further references.
pub struct IndexedDocument {
    pub document: Document,
    /// Start offset of each definition's name, with its symbol.
    pub definitions: Vec<(u32, String)>,
    /// Start offsets of all emitted occurrences.
    pub occupied: HashSet<u32>,
}

pub struct DocIndexer<'a> {
    source: &'a str,
    module: &'a str,
    is_package: bool,
    context: &'a ProjectContext,
    lines: LineIndex,
    scopes: Vec<Scope>,
    local_counter: usize,
    doc: Document,
    definitions: Vec<(u32, String)>,
    occupied: HashSet<u32>,
    documented: HashSet<String>,
}

impl<'a> DocIndexer<'a> {
    pub fn new(
        source: &'a str,
        rel_path: &str,
        module: &'a str,
        is_package: bool,
        context: &'a ProjectContext,
    ) -> Self {
        let doc = Document {
            language: "python".to_string(),
            relative_path: rel_path.to_string(),
            position_encoding: PositionEncoding::UTF8CodeUnitOffsetFromLineStart.into(),
            ..Default::default()
        };
        DocIndexer {
            source,
            module,
            is_package,
            context,
            lines: LineIndex::new(source),
            scopes: vec![],
            local_counter: 0,
            doc,
            definitions: vec![],
            occupied: HashSet::new(),
            documented: HashSet::new(),
        }
    }

    pub fn index(mut self, body: &[Stmt]) -> IndexedDocument {
        let module_symbol = self.context.module_symbol(self.module);
        self.doc.occurrences.push(Occurrence {
            range: vec![0, 0, 0],
            symbol: module_symbol.clone(),
            symbol_roles: SymbolRole::Definition as i32,
            ..Default::default()
        });
        self.symbol_info(&module_symbol, Kind::Module, self.module, docstring(body));
        self.scopes.push(Scope {
            kind: ScopeType::Module,
            prefix: Some(vec![descriptor(self.module, Suffix::Namespace)]),
            bindings: HashMap::new(),
        });
        self.pre_bind(body);
        self.visit_stmts(body);
        self.doc.occurrences.sort_by(|a, b| a.range.cmp(&b.range));
        IndexedDocument {
            document: self.doc,
            definitions: self.definitions,
            occupied: self.occupied,
        }
    }

    fn occurrence(&mut self, range: TextRange, symbol: &str, roles: i32) {
        self.occupied.insert(range.start().into());
        self.doc.occurrences.push(Occurrence {
            range: self.lines.range_vec(range),
            symbol: symbol.to_string(),
            symbol_roles: roles,
            ..Default::default()
        });
    }

    fn definition(&mut self, range: TextRange, symbol: &str, enclosing: Option<TextRange>) {
        self.occupied.insert(range.start().into());
        self.definitions
            .push((range.start().into(), symbol.to_string()));
        self.doc.occurrences.push(Occurrence {
            range: self.lines.range_vec(range),
            symbol: symbol.to_string(),
            symbol_roles: SymbolRole::Definition as i32,
            enclosing_range: enclosing
                .map(|r| self.lines.range_vec(r))
                .unwrap_or_default(),
            ..Default::default()
        });
    }

    fn symbol_info(&mut self, symbol: &str, kind: Kind, display_name: &str, docs: Option<String>) {
        if self.documented.insert(symbol.to_string()) {
            self.doc.symbols.push(SymbolInformation {
                symbol: symbol.to_string(),
                kind: kind.into(),
                display_name: display_name.to_string(),
                documentation: docs.into_iter().collect(),
                ..Default::default()
            });
        }
    }

    fn current_scope(&mut self) -> &mut Scope {
        self.scopes.last_mut().expect("scope stack is never empty")
    }

    fn next_local(&mut self) -> String {
        let id = self.local_counter;
        self.local_counter += 1;
        local_symbol(id)
    }

    fn symbol_for(
        &mut self,
        prefix: Option<Vec<Descriptor>>,
        name: &str,
        suffix: Suffix,
    ) -> String {
        match prefix {
            Some(mut descriptors) => {
                descriptors.push(descriptor(name, suffix));
                format_global(&self.context.package, descriptors)
            }
            None => self.next_local(),
        }
    }

    /// Compute the symbol for a new definition in the current scope.
    /// Anything defined inside a function body is a SCIP local; only
    /// module and class scopes produce global symbols.
    fn new_symbol(&mut self, name: &str, suffix: Suffix) -> String {
        let scope = self.scopes.last().expect("scope stack is never empty");
        let prefix = match scope.kind {
            ScopeType::Module | ScopeType::Class => scope.prefix.clone(),
            ScopeType::Function { .. } | ScopeType::Comprehension => None,
        };
        self.symbol_for(prefix, name, suffix)
    }

    /// Parameters hang off their function's descriptor path even though
    /// other names in the function body are locals.
    fn param_symbol(&mut self, name: &str) -> String {
        let prefix = self
            .scopes
            .last()
            .expect("scope stack is never empty")
            .prefix
            .clone();
        self.symbol_for(prefix, name, Suffix::Parameter)
    }

    fn child_prefix(&self, name: &str, suffix: Suffix) -> Option<Vec<Descriptor>> {
        let scope = self.scopes.last()?;
        if matches!(
            scope.kind,
            ScopeType::Function { .. } | ScopeType::Comprehension
        ) {
            return None;
        }
        let prefix = scope.prefix.as_ref()?;
        let mut descriptors = prefix.clone();
        descriptors.push(descriptor(name, suffix));
        Some(descriptors)
    }

    fn define(
        &mut self,
        name: &str,
        suffix: Suffix,
        kind: Kind,
        name_range: Option<TextRange>,
        enclosing: Option<TextRange>,
        docs: Option<String>,
    ) -> Binding {
        let symbol = self.new_symbol(name, suffix);
        self.define_with_symbol(symbol, name, kind, name_range, enclosing, docs)
    }

    fn define_with_symbol(
        &mut self,
        symbol: String,
        name: &str,
        kind: Kind,
        name_range: Option<TextRange>,
        enclosing: Option<TextRange>,
        docs: Option<String>,
    ) -> Binding {
        let binding = Binding {
            symbol: symbol.clone(),
            kind: BindingKind::Other,
        };
        self.current_scope()
            .bindings
            .insert(name.to_string(), binding.clone());
        if let Some(range) = name_range {
            self.definition(range, &symbol, enclosing);
        }
        self.symbol_info(&symbol, kind, name, docs);
        binding
    }

    fn lookup(&self, name: &str) -> Option<Binding> {
        let last = self.scopes.len() - 1;
        for (i, scope) in self.scopes.iter().enumerate().rev() {
            // Class scopes are invisible from nested scopes.
            if matches!(scope.kind, ScopeType::Class) && i != last {
                continue;
            }
            if let Some(binding) = scope.bindings.get(name) {
                return Some(binding.clone());
            }
        }
        None
    }

    fn receiver(&self) -> Option<String> {
        for scope in self.scopes.iter().rev() {
            match &scope.kind {
                ScopeType::Function { receiver } => return receiver.clone(),
                ScopeType::Comprehension => continue,
                _ => return None,
            }
        }
        None
    }

    fn nearest_class_scope(&self) -> Option<usize> {
        self.scopes
            .iter()
            .rposition(|s| matches!(s.kind, ScopeType::Class))
    }

    /// Pre-bind function and class names so references before the
    /// definition in source order still resolve. Only for scopes with
    /// global symbols; local ids must be allocated at definition sites.
    fn pre_bind(&mut self, body: &[Stmt]) {
        let bindable = self.scopes.last().is_some_and(|s| {
            matches!(s.kind, ScopeType::Module | ScopeType::Class) && s.prefix.is_some()
        });
        if !bindable {
            return;
        }
        for stmt in body {
            let (name, suffix) = match stmt {
                Stmt::FunctionDef(def) => (def.name.as_str(), Suffix::Method),
                Stmt::ClassDef(def) => (def.name.as_str(), Suffix::Type),
                _ => continue,
            };
            let symbol = self.new_symbol(name, suffix);
            self.current_scope().bindings.insert(
                name.to_string(),
                Binding {
                    symbol,
                    kind: BindingKind::Other,
                },
            );
        }
    }

    fn visit_stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.visit_stmt(stmt);
        }
    }

    fn visit_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::FunctionDef(def) => self.handle_function(def),
            Stmt::ClassDef(def) => self.handle_class(def),
            Stmt::Return(s) => {
                if let Some(value) = &s.value {
                    self.visit_expr(value);
                }
            }
            Stmt::Delete(s) => {
                for target in &s.targets {
                    self.visit_expr(target);
                }
            }
            Stmt::Assign(s) => {
                self.visit_expr(&s.value);
                for target in &s.targets {
                    self.bind_target(target);
                }
            }
            Stmt::TypeAlias(s) => {
                // TODO: bind PEP 695 type parameters.
                self.visit_expr(&s.value);
                self.bind_target(&s.name);
            }
            Stmt::AugAssign(s) => {
                self.visit_expr(&s.value);
                self.aug_target(&s.target);
            }
            Stmt::AnnAssign(s) => {
                self.visit_expr(&s.annotation);
                if let Some(value) = &s.value {
                    self.visit_expr(value);
                }
                self.bind_target(&s.target);
            }
            Stmt::For(s) => {
                self.visit_expr(&s.iter);
                self.bind_target(&s.target);
                self.visit_stmts(&s.body);
                self.visit_stmts(&s.orelse);
            }
            Stmt::While(s) => {
                self.visit_expr(&s.test);
                self.visit_stmts(&s.body);
                self.visit_stmts(&s.orelse);
            }
            Stmt::If(s) => {
                self.visit_expr(&s.test);
                self.visit_stmts(&s.body);
                for clause in &s.elif_else_clauses {
                    if let Some(test) = &clause.test {
                        self.visit_expr(test);
                    }
                    self.visit_stmts(&clause.body);
                }
            }
            Stmt::With(s) => {
                for item in &s.items {
                    self.visit_expr(&item.context_expr);
                    if let Some(vars) = &item.optional_vars {
                        self.bind_target(vars);
                    }
                }
                self.visit_stmts(&s.body);
            }
            Stmt::Match(s) => {
                self.visit_expr(&s.subject);
                for case in &s.cases {
                    self.bind_pattern(&case.pattern);
                    if let Some(guard) = &case.guard {
                        self.visit_expr(guard);
                    }
                    self.visit_stmts(&case.body);
                }
            }
            Stmt::Raise(s) => {
                if let Some(exc) = &s.exc {
                    self.visit_expr(exc);
                }
                if let Some(cause) = &s.cause {
                    self.visit_expr(cause);
                }
            }
            Stmt::Try(s) => {
                self.visit_stmts(&s.body);
                for handler in &s.handlers {
                    self.visit_except_handler(handler);
                }
                self.visit_stmts(&s.orelse);
                self.visit_stmts(&s.finalbody);
            }
            Stmt::Assert(s) => {
                self.visit_expr(&s.test);
                if let Some(msg) = &s.msg {
                    self.visit_expr(msg);
                }
            }
            Stmt::Import(s) => self.handle_import(s),
            Stmt::ImportFrom(s) => self.handle_import_from(s),
            Stmt::Global(s) => {
                for name in &s.names {
                    let binding = self.scopes[0].bindings.get(name.as_str()).cloned();
                    let binding = binding.unwrap_or_else(|| Binding {
                        symbol: format_global(
                            &self.context.package,
                            vec![
                                descriptor(self.module, Suffix::Namespace),
                                descriptor(name.as_str(), Suffix::Term),
                            ],
                        ),
                        kind: BindingKind::Other,
                    });
                    self.occurrence(name.range(), &binding.symbol, 0);
                    self.current_scope()
                        .bindings
                        .insert(name.to_string(), binding);
                }
            }
            Stmt::Nonlocal(s) => {
                for name in &s.names {
                    let found = self
                        .scopes
                        .iter()
                        .rev()
                        .skip(1)
                        .filter(|s| matches!(s.kind, ScopeType::Function { .. }))
                        .find_map(|s| s.bindings.get(name.as_str()).cloned());
                    if let Some(binding) = found {
                        self.occurrence(name.range(), &binding.symbol, 0);
                        self.current_scope()
                            .bindings
                            .insert(name.to_string(), binding);
                    }
                }
            }
            Stmt::Expr(s) => self.visit_expr(&s.value),
            Stmt::Pass(_) | Stmt::Break(_) | Stmt::Continue(_) | Stmt::IpyEscapeCommand(_) => {}
        }
    }

    fn visit_except_handler(&mut self, handler: &ExceptHandler) {
        let ExceptHandler::ExceptHandler(h) = handler;
        if let Some(type_) = &h.type_ {
            self.visit_expr(type_);
        }
        if let Some(name) = &h.name {
            self.define(
                name.as_str(),
                Suffix::Term,
                Kind::Variable,
                Some(name.range()),
                None,
                None,
            );
        }
        self.visit_stmts(&h.body);
    }

    fn handle_function(&mut self, def: &StmtFunctionDef) {
        for dec in &def.decorator_list {
            self.visit_expr(&dec.expression);
        }
        // Defaults and annotations are evaluated in the enclosing scope.
        for param in def
            .parameters
            .posonlyargs
            .iter()
            .chain(&def.parameters.args)
            .chain(&def.parameters.kwonlyargs)
        {
            if let Some(default) = &param.default {
                self.visit_expr(default);
            }
        }
        for param in all_params(&def.parameters) {
            if let Some(annotation) = &param.annotation {
                self.visit_expr(annotation);
            }
        }
        if let Some(returns) = &def.returns {
            self.visit_expr(returns);
        }

        let in_class = matches!(self.scopes.last().map(|s| &s.kind), Some(ScopeType::Class));
        let kind = if in_class {
            Kind::Method
        } else {
            Kind::Function
        };
        let name = def.name.as_str();
        self.define(
            name,
            Suffix::Method,
            kind,
            Some(def.name.range()),
            Some(def.range),
            docstring(&def.body),
        );

        let is_static = def.decorator_list.iter().any(|d| match &d.expression {
            Expr::Name(n) => n.id.as_str() == "staticmethod",
            Expr::Attribute(a) => a.attr.as_str() == "staticmethod",
            _ => false,
        });
        let receiver = if in_class && !is_static {
            def.parameters
                .posonlyargs
                .first()
                .or_else(|| def.parameters.args.first())
                .map(|p| p.parameter.name.to_string())
        } else {
            None
        };

        let prefix = self.child_prefix(name, Suffix::Method);
        self.scopes.push(Scope {
            kind: ScopeType::Function { receiver },
            prefix,
            bindings: HashMap::new(),
        });
        for param in all_params(&def.parameters) {
            self.define_parameter(param);
        }
        // TODO: bind PEP 695 type parameters.
        self.pre_bind(&def.body);
        self.visit_stmts(&def.body);
        self.scopes.pop();
    }

    fn define_parameter(&mut self, param: &Parameter) {
        let name = param.name.as_str();
        let symbol = self.param_symbol(name);
        self.define_with_symbol(
            symbol,
            name,
            Kind::Parameter,
            Some(param.name.range()),
            None,
            None,
        );
    }

    fn handle_class(&mut self, def: &StmtClassDef) {
        for dec in &def.decorator_list {
            self.visit_expr(&dec.expression);
        }
        if let Some(arguments) = &def.arguments {
            for base in &arguments.args {
                self.visit_expr(base);
            }
            for keyword in &arguments.keywords {
                self.visit_expr(&keyword.value);
            }
        }
        let name = def.name.as_str();
        self.define(
            name,
            Suffix::Type,
            Kind::Class,
            Some(def.name.range()),
            Some(def.range),
            docstring(&def.body),
        );
        // TODO: bind PEP 695 type parameters.
        let prefix = self.child_prefix(name, Suffix::Type);
        self.scopes.push(Scope {
            kind: ScopeType::Class,
            prefix,
            bindings: HashMap::new(),
        });
        self.pre_bind(&def.body);
        self.visit_stmts(&def.body);
        self.scopes.pop();
    }

    fn handle_import(&mut self, stmt: &StmtImport) {
        for alias in &stmt.names {
            let dotted = alias.name.as_str();
            let start = u32::from(alias.name.range().start()) as usize;
            // Per-segment occurrences, as long as the source really is the
            // plain dotted form (no whitespace around the dots).
            if self.source[start..].starts_with(dotted) {
                let mut offset = start;
                let mut prefix = String::new();
                for segment in dotted.split('.') {
                    if !prefix.is_empty() {
                        prefix.push('.');
                    }
                    prefix.push_str(segment);
                    let range = TextRange::new(
                        TextSize::from(offset as u32),
                        TextSize::from((offset + segment.len()) as u32),
                    );
                    self.occurrence(range, &self.context.module_symbol(&prefix), 0);
                    offset += segment.len() + 1;
                }
            } else {
                self.occurrence(alias.name.range(), &self.context.module_symbol(dotted), 0);
            }
            match &alias.asname {
                Some(asname) => {
                    let binding = self.context.module_binding(dotted);
                    self.occurrence(asname.range(), &binding.symbol, SymbolRole::Import as i32);
                    self.current_scope()
                        .bindings
                        .insert(asname.to_string(), binding);
                }
                None => {
                    let top = dotted.split('.').next().unwrap_or(dotted);
                    let binding = self.context.module_binding(top);
                    self.current_scope()
                        .bindings
                        .insert(top.to_string(), binding);
                }
            }
        }
    }

    fn handle_import_from(&mut self, stmt: &StmtImportFrom) {
        let base = resolve_import_base(
            self.module,
            self.is_package,
            stmt.level,
            stmt.module.as_ref().map(|m| m.as_str()),
        );
        if !base.is_empty()
            && let Some(module) = &stmt.module
        {
            self.occurrence(module.range(), &self.context.module_symbol(&base), 0);
        }
        for alias in &stmt.names {
            let name = alias.name.as_str();
            if name == "*" {
                self.handle_star_import(&base);
                continue;
            }
            let binding = self.context.resolve_member(&base, name, 8);
            self.occurrence(alias.name.range(), &binding.symbol, 0);
            match &alias.asname {
                Some(asname) => {
                    self.occurrence(asname.range(), &binding.symbol, SymbolRole::Import as i32);
                    self.current_scope()
                        .bindings
                        .insert(asname.to_string(), binding);
                }
                None => {
                    self.current_scope()
                        .bindings
                        .insert(name.to_string(), binding);
                }
            }
        }
    }

    fn handle_star_import(&mut self, base: &str) {
        let Some(exports) = self.context.exports.get(base) else {
            return;
        };
        let names: Vec<String> = exports.keys().cloned().collect();
        for name in names {
            let binding = self.context.resolve_member(base, &name, 8);
            self.current_scope().bindings.entry(name).or_insert(binding);
        }
    }

    fn define_or_write(&mut self, name: &str, range: TextRange) {
        if let Some(binding) = self.current_scope().bindings.get(name).cloned() {
            self.occurrence(range, &binding.symbol, SymbolRole::WriteAccess as i32);
        } else {
            self.define(name, Suffix::Term, Kind::Variable, Some(range), None, None);
        }
    }

    fn bind_target(&mut self, target: &Expr) {
        match target {
            Expr::Name(name) => self.define_or_write(name.id.as_str(), name.range),
            Expr::Tuple(tuple) => {
                for elt in &tuple.elts {
                    self.bind_target(elt);
                }
            }
            Expr::List(list) => {
                for elt in &list.elts {
                    self.bind_target(elt);
                }
            }
            Expr::Starred(starred) => self.bind_target(&starred.value),
            Expr::Attribute(attr) => {
                self.visit_expr(&attr.value);
                if self.is_receiver(&attr.value) {
                    self.self_attr_store(attr);
                } else if let Some(symbol) = self.attr_symbol(attr) {
                    self.occurrence(attr.attr.range(), &symbol, SymbolRole::WriteAccess as i32);
                }
            }
            Expr::Subscript(sub) => {
                self.visit_expr(&sub.value);
                self.visit_expr(&sub.slice);
            }
            other => self.visit_expr(other),
        }
    }

    fn aug_target(&mut self, target: &Expr) {
        match target {
            Expr::Name(name) => {
                if let Some(binding) = self.lookup(name.id.as_str()) {
                    self.occurrence(
                        name.range,
                        &binding.symbol,
                        SymbolRole::ReadAccess as i32 | SymbolRole::WriteAccess as i32,
                    );
                }
            }
            other => self.bind_target(other),
        }
    }

    fn is_receiver(&self, expr: &Expr) -> bool {
        match expr {
            Expr::Name(name) => Some(name.id.as_str()) == self.receiver().as_deref(),
            _ => false,
        }
    }

    fn self_attr_store(&mut self, attr: &ExprAttribute) {
        let Some(class_index) = self.nearest_class_scope() else {
            return;
        };
        let name = attr.attr.as_str();
        let range = attr.attr.range();
        if let Some(binding) = self.scopes[class_index].bindings.get(name).cloned() {
            self.occurrence(range, &binding.symbol, SymbolRole::WriteAccess as i32);
            return;
        }
        let prefix = self.scopes[class_index].prefix.clone();
        let symbol = self.symbol_for(prefix, name, Suffix::Term);
        self.scopes[class_index].bindings.insert(
            name.to_string(),
            Binding {
                symbol: symbol.clone(),
                kind: BindingKind::Other,
            },
        );
        self.definition(range, &symbol, None);
        self.symbol_info(&symbol, Kind::Variable, name, None);
    }

    fn self_attr_lookup(&self, name: &str) -> Option<String> {
        let class_index = self.nearest_class_scope()?;
        self.scopes[class_index]
            .bindings
            .get(name)
            .map(|b| b.symbol.clone())
    }

    /// Resolve dotted expressions like `mod.sub.name` to a binding.
    fn resolve_expr(&self, expr: &Expr) -> Option<Binding> {
        match expr {
            Expr::Name(name) => self.lookup(name.id.as_str()),
            Expr::Attribute(attr) => {
                let base = self.resolve_expr(&attr.value)?;
                match base.kind {
                    BindingKind::Module(module) => {
                        Some(self.context.resolve_member(&module, attr.attr.as_str(), 8))
                    }
                    BindingKind::Other => None,
                }
            }
            _ => None,
        }
    }

    fn attr_symbol(&self, attr: &ExprAttribute) -> Option<String> {
        if self.is_receiver(&attr.value) {
            return self.self_attr_lookup(attr.attr.as_str());
        }
        let base = self.resolve_expr(&attr.value)?;
        match base.kind {
            BindingKind::Module(module) => Some(
                self.context
                    .resolve_member(&module, attr.attr.as_str(), 8)
                    .symbol,
            ),
            BindingKind::Other => None,
        }
    }

    fn visit_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::BoolOp(e) => {
                for value in &e.values {
                    self.visit_expr(value);
                }
            }
            Expr::Named(e) => {
                self.visit_expr(&e.value);
                self.bind_target(&e.target);
            }
            Expr::BinOp(e) => {
                self.visit_expr(&e.left);
                self.visit_expr(&e.right);
            }
            Expr::UnaryOp(e) => self.visit_expr(&e.operand),
            Expr::Lambda(e) => self.handle_lambda(e),
            Expr::If(e) => {
                self.visit_expr(&e.test);
                self.visit_expr(&e.body);
                self.visit_expr(&e.orelse);
            }
            Expr::Dict(e) => {
                for item in &e.items {
                    if let Some(key) = &item.key {
                        self.visit_expr(key);
                    }
                    self.visit_expr(&item.value);
                }
            }
            Expr::Set(e) => {
                for elt in &e.elts {
                    self.visit_expr(elt);
                }
            }
            Expr::ListComp(e) => self.handle_comprehension(&e.generators, &[&e.elt]),
            Expr::SetComp(e) => self.handle_comprehension(&e.generators, &[&e.elt]),
            Expr::DictComp(e) => {
                // The key is only absent in unparseable source that the
                // parser recovered from.
                let mut exprs: Vec<&Expr> = vec![];
                if let Some(key) = &e.key {
                    exprs.push(key);
                }
                exprs.push(&e.value);
                self.handle_comprehension(&e.generators, &exprs);
            }
            Expr::Generator(e) => self.handle_comprehension(&e.generators, &[&e.elt]),
            Expr::Await(e) => self.visit_expr(&e.value),
            Expr::Yield(e) => {
                if let Some(value) = &e.value {
                    self.visit_expr(value);
                }
            }
            Expr::YieldFrom(e) => self.visit_expr(&e.value),
            Expr::Compare(e) => {
                self.visit_expr(&e.left);
                for comparator in &e.comparators {
                    self.visit_expr(comparator);
                }
            }
            Expr::Call(e) => {
                self.visit_expr(&e.func);
                for arg in &e.arguments.args {
                    self.visit_expr(arg);
                }
                for keyword in &e.arguments.keywords {
                    self.visit_expr(&keyword.value);
                }
            }
            Expr::FString(e) => {
                for element in e.value.elements() {
                    self.visit_interpolated_element(element);
                }
            }
            Expr::TString(e) => {
                for element in e.value.elements() {
                    self.visit_interpolated_element(element);
                }
            }
            Expr::StringLiteral(_)
            | Expr::BytesLiteral(_)
            | Expr::NumberLiteral(_)
            | Expr::BooleanLiteral(_)
            | Expr::NoneLiteral(_)
            | Expr::EllipsisLiteral(_)
            | Expr::IpyEscapeCommand(_) => {}
            Expr::Attribute(e) => {
                self.visit_expr(&e.value);
                if let Some(symbol) = self.attr_symbol(e) {
                    self.occurrence(e.attr.range(), &symbol, 0);
                }
            }
            Expr::Subscript(e) => {
                self.visit_expr(&e.value);
                self.visit_expr(&e.slice);
            }
            Expr::Starred(e) => self.visit_expr(&e.value),
            Expr::Name(e) => {
                if let Some(binding) = self.lookup(e.id.as_str()) {
                    self.occurrence(e.range, &binding.symbol, 0);
                }
            }
            Expr::List(e) => {
                for elt in &e.elts {
                    self.visit_expr(elt);
                }
            }
            Expr::Tuple(e) => {
                for elt in &e.elts {
                    self.visit_expr(elt);
                }
            }
            Expr::Slice(e) => {
                for part in [&e.lower, &e.upper, &e.step].into_iter().flatten() {
                    self.visit_expr(part);
                }
            }
        }
    }

    fn visit_interpolated_element(&mut self, element: &InterpolatedStringElement) {
        if let InterpolatedStringElement::Interpolation(interpolation) = element {
            self.visit_expr(&interpolation.expression);
            if let Some(spec) = &interpolation.format_spec {
                for element in &spec.elements {
                    self.visit_interpolated_element(element);
                }
            }
        }
    }

    fn handle_lambda(&mut self, e: &ExprLambda) {
        if let Some(parameters) = &e.parameters {
            for param in parameters
                .posonlyargs
                .iter()
                .chain(&parameters.args)
                .chain(&parameters.kwonlyargs)
            {
                if let Some(default) = &param.default {
                    self.visit_expr(default);
                }
            }
        }
        self.scopes.push(Scope {
            kind: ScopeType::Function { receiver: None },
            prefix: None,
            bindings: HashMap::new(),
        });
        if let Some(parameters) = &e.parameters {
            for param in all_params(parameters) {
                self.define_parameter(param);
            }
        }
        self.visit_expr(&e.body);
        self.scopes.pop();
    }

    fn handle_comprehension(&mut self, generators: &[Comprehension], exprs: &[&Expr]) {
        self.scopes.push(Scope {
            kind: ScopeType::Comprehension,
            prefix: None,
            bindings: HashMap::new(),
        });
        for generator in generators {
            self.visit_expr(&generator.iter);
            self.bind_target(&generator.target);
            for if_ in &generator.ifs {
                self.visit_expr(if_);
            }
        }
        for expr in exprs {
            self.visit_expr(expr);
        }
        self.scopes.pop();
    }

    fn bind_pattern(&mut self, pattern: &Pattern) {
        match pattern {
            Pattern::MatchValue(p) => self.visit_expr(&p.value),
            Pattern::MatchSingleton(_) => {}
            Pattern::MatchSequence(p) => {
                for pattern in &p.patterns {
                    self.bind_pattern(pattern);
                }
            }
            Pattern::MatchMapping(p) => {
                for key in &p.keys {
                    self.visit_expr(key);
                }
                for pattern in &p.patterns {
                    self.bind_pattern(pattern);
                }
                if let Some(rest) = &p.rest {
                    self.define(
                        rest.as_str(),
                        Suffix::Term,
                        Kind::Variable,
                        Some(rest.range()),
                        None,
                        None,
                    );
                }
            }
            Pattern::MatchClass(p) => {
                self.visit_expr(&p.cls);
                for pattern in &p.arguments.patterns {
                    self.bind_pattern(pattern);
                }
                for keyword in &p.arguments.keywords {
                    self.bind_pattern(&keyword.pattern);
                }
            }
            Pattern::MatchStar(p) => {
                if let Some(name) = &p.name {
                    self.define(
                        name.as_str(),
                        Suffix::Term,
                        Kind::Variable,
                        Some(name.range()),
                        None,
                        None,
                    );
                }
            }
            Pattern::MatchAs(p) => {
                if let Some(inner) = &p.pattern {
                    self.bind_pattern(inner);
                }
                if let Some(name) = &p.name {
                    self.define(
                        name.as_str(),
                        Suffix::Term,
                        Kind::Variable,
                        Some(name.range()),
                        None,
                        None,
                    );
                }
            }
            Pattern::MatchOr(p) => {
                for pattern in &p.patterns {
                    self.bind_pattern(pattern);
                }
            }
        }
    }
}

fn all_params(parameters: &Parameters) -> Vec<&Parameter> {
    let mut result = vec![];
    for param in &parameters.posonlyargs {
        result.push(&param.parameter);
    }
    for param in &parameters.args {
        result.push(&param.parameter);
    }
    if let Some(param) = &parameters.vararg {
        result.push(param.as_ref());
    }
    for param in &parameters.kwonlyargs {
        result.push(&param.parameter);
    }
    if let Some(param) = &parameters.kwarg {
        result.push(param.as_ref());
    }
    result
}

fn docstring(body: &[Stmt]) -> Option<String> {
    if let Some(Stmt::Expr(e)) = body.first()
        && let Expr::StringLiteral(s) = e.value.as_ref()
    {
        return Some(s.value.to_str().to_string());
    }
    None
}
