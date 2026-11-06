//! Module graph, imports, and top-level name resolution for Martite.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use martite_base::{
    Code, Diagnostic, LoadLimits, Severity, SourceId, SourceMap, SourceProvider, Span,
};
use martite_syntax::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DefId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefKind {
    Fn,
    Handler,
    Test,
    Struct,
    Enum,
    Const,
    Method,
}

#[derive(Debug, Clone)]
pub struct DefInfo {
    pub id: DefId,
    pub module: ModuleId,
    pub name: String,
    pub kind: DefKind,
    pub is_pub: bool,
    pub node: NodeId,
    pub item_index: usize,
    /// Position inside the `impl` block for `DefKind::Method` definitions.
    pub method_index: Option<usize>,
    pub container: Option<String>,
}

impl DefInfo {
    pub fn is_value(&self) -> bool {
        matches!(self.kind, DefKind::Fn | DefKind::Handler)
    }

    pub fn is_type(&self) -> bool {
        matches!(self.kind, DefKind::Struct | DefKind::Enum)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleTarget {
    Source(ModuleId),
    Interface(ModuleId),
    Builtin(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinItem {
    pub module: &'static str,
    pub name: &'static str,
    pub kind: DefKind,
}

/// Look up a builtin module item by its canonical name.
///
/// The returned names are always `'static`, so `BuiltinItem` stays `Copy` and
/// resolution never allocates when recording a builtin lookup.
pub fn builtin_item(module: &str, name: &str) -> Option<BuiltinItem> {
    let (module, name, kind) = match (module, name) {
        ("std.math", "PI") => ("std.math", "PI", DefKind::Const),
        ("std.math", "EULER") => ("std.math", "EULER", DefKind::Const),
        ("std.math", "min") => ("std.math", "min", DefKind::Fn),
        ("std.math", "max") => ("std.math", "max", DefKind::Fn),
        ("std.math", "clamp") => ("std.math", "clamp", DefKind::Fn),
        ("std.math", "sin") => ("std.math", "sin", DefKind::Fn),
        ("std.math", "cos") => ("std.math", "cos", DefKind::Fn),
        ("std.math", "tan") => ("std.math", "tan", DefKind::Fn),
        ("std.math", "atan2") => ("std.math", "atan2", DefKind::Fn),
        ("std.math", "exp") => ("std.math", "exp", DefKind::Fn),
        ("std.math", "ln") => ("std.math", "ln", DefKind::Fn),
        ("std.math", "log10") => ("std.math", "log10", DefKind::Fn),
        ("std.json", "Json") => ("std.json", "Json", DefKind::Enum),
        ("std.json", "parse") => ("std.json", "parse", DefKind::Fn),
        ("std.json", "stringify") => ("std.json", "stringify", DefKind::Fn),
        _ => return None,
    };
    Some(BuiltinItem { module, name, kind })
}

#[derive(Debug, Clone, Default)]
pub struct Symbols {
    pub values: HashMap<String, DefId>,
    pub types: HashMap<String, DefId>,
    pub consts: HashMap<String, DefId>,
    pub modules: HashMap<String, ModuleTarget>,
    pub builtin_items: HashMap<String, BuiltinItem>,
    pub imported_values: HashMap<String, DefId>,
    pub imported_types: HashMap<String, DefId>,
    pub imported_consts: HashMap<String, DefId>,
}

impl Symbols {
    pub fn contains(&self, name: &str) -> bool {
        self.values.contains_key(name)
            || self.types.contains_key(name)
            || self.consts.contains_key(name)
            || self.modules.contains_key(name)
            || self.builtin_items.contains_key(name)
    }
}

#[derive(Debug, Clone)]
pub struct UseBinding {
    pub decl_node: NodeId,
    pub span: Span,
    pub path: Vec<String>,
    pub alias: Option<String>,
    pub target: ModuleTargetUse,
}

#[derive(Debug, Clone)]
pub enum ModuleTargetUse {
    Module(ModuleTarget),
    Item(DefId),
    BuiltinItem(BuiltinItem),
}

#[derive(Debug, Clone)]
pub struct Module {
    pub id: ModuleId,
    pub path: String,
    pub source: SourceId,
    /// Parsed module AST shared by reference so resolver and checker passes can
    /// borrow it without deep-cloning the item list.
    pub ast: Rc<Program>,
    pub is_interface: bool,
    pub is_host: bool,
    pub symbols: Symbols,
    pub uses: Vec<UseBinding>,
}

#[derive(Debug, Clone)]
pub struct ResolvedProgram {
    pub modules: Vec<Module>,
    pub entry: ModuleId,
    pub defs: Vec<DefInfo>,
    pub node_to_def: HashMap<NodeId, DefId>,
    pub grants: Vec<String>,
}

impl ResolvedProgram {
    pub fn def(&self, id: DefId) -> &DefInfo {
        &self.defs[id.0 as usize]
    }

    pub fn module(&self, id: ModuleId) -> &Module {
        &self.modules[id.0 as usize]
    }

    pub fn module_mut(&mut self, id: ModuleId) -> &mut Module {
        &mut self.modules[id.0 as usize]
    }

    pub fn entry_module(&self) -> &Module {
        self.module(self.entry)
    }

    pub fn def_by_node(&self, node: NodeId) -> Option<&DefInfo> {
        self.node_to_def.get(&node).map(|id| self.def(*id))
    }

    pub fn find_module(&self, path: &str) -> Option<ModuleId> {
        self.modules.iter().find(|m| m.path == path).map(|m| m.id)
    }

    pub fn local_def(&self, module: ModuleId, name: &str) -> Option<DefId> {
        let m = self.module(module);
        m.symbols
            .values
            .get(name)
            .or_else(|| m.symbols.types.get(name))
            .or_else(|| m.symbols.consts.get(name))
            .copied()
    }
}

#[derive(Debug, Clone)]
pub struct InterfaceSource {
    /// Module path, for example `app.editor`.
    pub path: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct ResolveOptions {
    pub entry: String,
    pub interfaces: Vec<InterfaceSource>,
    pub grants: Vec<String>,
    pub limits: LoadLimits,
    pub parse: ParseOptions,
}

#[derive(Debug, Clone, Default)]
pub struct ResolveOutput {
    pub program: Option<ResolvedProgram>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ResolveOutput {
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
    }
}

pub fn resolve(
    provider: &dyn SourceProvider,
    options: &ResolveOptions,
    sources: &mut SourceMap,
) -> ResolveOutput {
    let mut loader = Loader::new(provider, options, sources);
    loader.run();
    let diagnostics = std::mem::take(&mut loader.diagnostics);
    ResolveOutput {
        program: loader.finish(),
        diagnostics,
    }
}

struct Loader<'a> {
    provider: &'a dyn SourceProvider,
    options: &'a ResolveOptions,
    sources: &'a mut SourceMap,
    diagnostics: Vec<Diagnostic>,
    modules: Vec<Module>,
    by_path: HashMap<String, ModuleId>,
    defs: Vec<DefInfo>,
    node_to_def: HashMap<NodeId, DefId>,
    interfaces: HashMap<String, InterfaceSource>,
    file_cache: HashMap<String, Option<String>>,
    loading: HashSet<String>,
    conflicts: HashSet<String>,
    interface_hooks: HashMap<String, (String, Span, SourceId)>,
    entry: Option<ModuleId>,
}

impl<'a> Loader<'a> {
    fn new(
        provider: &'a dyn SourceProvider,
        options: &'a ResolveOptions,
        sources: &'a mut SourceMap,
    ) -> Self {
        let mut interfaces = HashMap::new();
        for i in &options.interfaces {
            interfaces.insert(i.path.clone(), i.clone());
        }
        Self {
            provider,
            options,
            sources,
            diagnostics: Vec::new(),
            modules: Vec::new(),
            by_path: HashMap::new(),
            defs: Vec::new(),
            node_to_def: HashMap::new(),
            interfaces,
            file_cache: HashMap::new(),
            loading: HashSet::new(),
            conflicts: HashSet::new(),
            interface_hooks: HashMap::new(),
            entry: None,
        }
    }

    fn run(&mut self) {
        let names: Vec<String> = self.interfaces.keys().cloned().collect();
        for path in names {
            let _ = self.ensure_module(&path, true);
        }

        let entry_path = module_path_from_file(&self.options.entry);
        let Some(entry) = self.ensure_module(&entry_path, false) else {
            self.diag(
                Code::E508,
                None,
                None,
                format!("cannot load entry file `{}`", self.options.entry),
            );
            return;
        };
        self.entry = Some(entry);

        self.build_all_locals();
        self.build_impls();
        self.validate_interfaces();
        self.resolve_all_uses();
        self.check_module_rules();
        self.check_grants();
        self.check_const_cycles();
    }

    fn finish(self) -> Option<ResolvedProgram> {
        let entry = self.entry?;
        Some(ResolvedProgram {
            modules: self.modules,
            entry,
            defs: self.defs,
            node_to_def: self.node_to_def,
            grants: self.options.grants.clone(),
        })
    }

    // ------------------------------------------------------------------
    // Loading
    // ------------------------------------------------------------------

    fn module_file(path: &str) -> String {
        format!("{}.mt", path.replace('.', "/"))
    }

    fn read_file(&mut self, relative: &str) -> Option<String> {
        if let Some(cached) = self.file_cache.get(relative) {
            return cached.clone();
        }
        let text = match self
            .provider
            .read_limited(relative, self.options.limits.file_size)
        {
            Ok(text) => Some(text),
            Err(error) => {
                if error.contains("exceeds") {
                    self.diag(Code::E002, None, None, error);
                }
                None
            }
        };
        self.file_cache.insert(relative.to_string(), text.clone());
        text
    }

    fn is_builtin_module(path: &str) -> bool {
        matches!(path, "std.math" | "std.json")
    }

    fn module_exists(&mut self, path: &str) -> bool {
        if Self::is_builtin_module(path) {
            return true;
        }
        let has_interface = self.interfaces.contains_key(path);
        let file = Self::module_file(path);
        let has_file = self.read_file(&file).is_some();
        if has_interface && has_file && !self.conflicts.contains(path) {
            self.conflicts.insert(path.to_string());
            self.diag(
                Code::E106,
                None,
                None,
                format!("module `{path}` matches both a source file and an interface"),
            );
        }
        has_interface || has_file
    }

    fn ensure_module(&mut self, path: &str, want_interface: bool) -> Option<ModuleId> {
        if self.loading.contains(path) {
            self.diag(
                Code::E106,
                None,
                None,
                format!("import cycle involving module `{path}`"),
            );
            return None;
        }
        if let Some(id) = self.by_path.get(path) {
            return Some(*id);
        }

        let interface = self.interfaces.get(path).cloned();
        let source_file = Self::module_file(path);
        let source_text = if want_interface {
            if interface.is_none() {
                return None;
            }
            None
        } else {
            self.read_file(&source_file)
        };

        if interface.is_some() && source_text.is_some() && !self.conflicts.contains(path) {
            self.conflicts.insert(path.to_string());
            self.diag(
                Code::E106,
                None,
                None,
                format!("module `{path}` matches both a source file and an interface"),
            );
        }

        let (text, is_interface, file_name) = if let Some(iface) = &interface {
            (
                iface.text.clone(),
                true,
                format!("{}.mti", path.replace('.', "/")),
            )
        } else {
            let Some(text) = source_text else {
                return None;
            };
            (text, false, source_file.clone())
        };

        if self.modules.len() >= self.options.limits.file_count {
            self.diag(
                Code::E002,
                None,
                None,
                format!(
                    "too many source files (limit {})",
                    self.options.limits.file_count
                ),
            );
            return None;
        }
        if text.len() > self.options.limits.file_size {
            let source = self.sources.add(file_name, text);
            self.diag(
                Code::E002,
                Some(source),
                Some(Span::new(0, 0)),
                format!(
                    "source file exceeds the {} byte limit",
                    self.options.limits.file_size
                ),
            );
            return None;
        }

        let source = self.sources.add(file_name, text.clone());
        let parsed = if is_interface {
            parse_interface_with(&text, self.options.limits, self.options.parse)
        } else {
            parse_source_with(&text, self.options.limits, self.options.parse)
        };
        self.push_syntax_diagnostics(parsed.diagnostics, source);

        let id = ModuleId(self.modules.len() as u32);
        self.modules.push(Module {
            id,
            path: path.to_string(),
            source,
            ast: Rc::new(parsed.program),
            is_interface,
            is_host: is_interface,
            symbols: Symbols::default(),
            uses: Vec::new(),
        });
        self.by_path.insert(path.to_string(), id);
        self.loading.insert(path.to_string());

        let use_paths: Vec<Vec<String>> = self.modules[id.0 as usize]
            .ast
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Use(u) => Some(u.path.clone()),
                _ => None,
            })
            .collect();
        for use_path in use_paths {
            self.ensure_use_dependencies(&use_path);
        }

        self.loading.remove(path);
        Some(id)
    }

    fn ensure_use_dependencies(&mut self, path: &[String]) {
        let full = path.join(".");
        if self.module_exists(&full) {
            let _ = self.ensure_module(&full, self.interfaces.contains_key(&full));
            return;
        }
        for k in (1..path.len()).rev() {
            let prefix = path[..k].join(".");
            if Self::is_builtin_module(&prefix) {
                return;
            }
            if self.module_exists(&prefix) {
                let is_iface = self.interfaces.contains_key(&prefix);
                let _ = self.ensure_module(&prefix, is_iface);
                return;
            }
        }
    }

    fn push_syntax_diagnostics(&mut self, diagnostics: Vec<Diagnostic>, source: SourceId) {
        for mut d in diagnostics {
            if d.source.is_none() {
                d.source = Some(source);
            }
            self.diagnostics.push(d);
        }
    }

    // ------------------------------------------------------------------
    // Top-level symbols
    // ------------------------------------------------------------------

    fn add_def(
        &mut self,
        module: ModuleId,
        name: String,
        kind: DefKind,
        is_pub: bool,
        node: NodeId,
        item_index: usize,
        container: Option<String>,
    ) -> DefId {
        let id = DefId(self.defs.len() as u32);
        self.defs.push(DefInfo {
            id,
            module,
            name,
            kind,
            is_pub,
            node,
            item_index,
            method_index: None,
            container,
        });
        self.node_to_def.insert(node, id);
        id
    }

    fn insert_symbol(
        &mut self,
        module: ModuleId,
        name: &str,
        def: DefId,
        kind: DefKind,
        span: Span,
    ) -> bool {
        let existing = {
            let symbols = &self.modules[module.0 as usize].symbols;
            symbols.contains(name)
        };
        if existing {
            self.diag_at(
                module,
                span,
                Code::E102,
                format!("duplicate declaration of `{name}`"),
            );
            return false;
        }
        let symbols = &mut self.modules[module.0 as usize].symbols;
        match kind {
            DefKind::Fn | DefKind::Handler => {
                symbols.values.insert(name.to_string(), def);
            }
            DefKind::Struct | DefKind::Enum => {
                symbols.types.insert(name.to_string(), def);
            }
            DefKind::Const => {
                symbols.consts.insert(name.to_string(), def);
            }
            DefKind::Test | DefKind::Method => {}
        }
        true
    }

    fn build_all_locals(&mut self) {
        for i in 0..self.modules.len() {
            self.build_local_symbols(ModuleId(i as u32));
        }
    }

    fn build_local_symbols(&mut self, module_id: ModuleId) {
        let ast = Rc::clone(&self.modules[module_id.0 as usize].ast);
        let is_interface = self.modules[module_id.0 as usize].is_interface;
        let is_root = self.entry == Some(module_id);
        let mut test_names = HashSet::new();

        for (index, item) in ast.items.iter().enumerate() {
            match item {
                Item::Use(_) => {}
                Item::Fn(f) => {
                    self.check_params(module_id, &f.params);
                    self.check_type_params(module_id, &f.generics);
                    let def = self.add_def(
                        module_id,
                        f.name.clone(),
                        DefKind::Fn,
                        is_interface || f.is_pub,
                        f.id,
                        index,
                        None,
                    );
                    self.insert_symbol(module_id, &f.name, def, DefKind::Fn, f.span);
                }
                Item::On(o) => {
                    self.check_params(module_id, &o.params);
                    let def = self.add_def(
                        module_id,
                        o.name.clone(),
                        DefKind::Handler,
                        true,
                        o.id,
                        index,
                        None,
                    );
                    self.insert_symbol(module_id, &o.name, def, DefKind::Handler, o.span);
                    if !is_interface && !is_root {
                        self.diag_at(
                            module_id,
                            o.span,
                            Code::E504,
                            format!("`on` handler `{}` is only allowed in the root file", o.name),
                        );
                    }
                }
                Item::Test(t) => {
                    if !test_names.insert(t.name.clone()) {
                        self.diag_at(
                            module_id,
                            t.span,
                            Code::E102,
                            format!("duplicate test name `{}`", t.name),
                        );
                    }
                    self.add_def(
                        module_id,
                        t.name.clone(),
                        DefKind::Test,
                        false,
                        t.id,
                        index,
                        None,
                    );
                }
                Item::Struct(s) => {
                    self.check_type_params(module_id, &s.generics);
                    if is_interface && s.opaque && !s.generics.is_empty() {
                        self.diag_at(
                            module_id,
                            s.span,
                            Code::E507,
                            "an opaque handle may not have type parameters",
                        );
                    }
                    if !is_interface {
                        for g in &s.generics {
                            if !g.bounds.is_empty() {
                                self.diag_at(
                                    module_id,
                                    g.span,
                                    Code::E209,
                                    "bounds are not allowed on struct type parameters",
                                );
                            }
                        }
                    }
                    let mut fields = HashSet::new();
                    for field in &s.fields {
                        if !fields.insert(field.name.clone()) {
                            self.diag_at(
                                module_id,
                                field.span,
                                Code::E102,
                                format!("duplicate field `{}`", field.name),
                            );
                        }
                    }
                    let def = self.add_def(
                        module_id,
                        s.name.clone(),
                        DefKind::Struct,
                        is_interface || s.is_pub,
                        s.id,
                        index,
                        None,
                    );
                    self.insert_symbol(module_id, &s.name, def, DefKind::Struct, s.span);
                }
                Item::Enum(e) => {
                    self.check_type_params(module_id, &e.generics);
                    if !is_interface {
                        for g in &e.generics {
                            if !g.bounds.is_empty() {
                                self.diag_at(
                                    module_id,
                                    g.span,
                                    Code::E209,
                                    "bounds are not allowed on enum type parameters",
                                );
                            }
                        }
                    }
                    let mut variants = HashSet::new();
                    for v in &e.variants {
                        if !variants.insert(v.name.clone()) {
                            self.diag_at(
                                module_id,
                                v.span,
                                Code::E102,
                                format!("duplicate variant `{}`", v.name),
                            );
                        }
                    }
                    let def = self.add_def(
                        module_id,
                        e.name.clone(),
                        DefKind::Enum,
                        is_interface || e.is_pub,
                        e.id,
                        index,
                        None,
                    );
                    self.insert_symbol(module_id, &e.name, def, DefKind::Enum, e.span);
                }
                Item::Const(c) => {
                    let def = self.add_def(
                        module_id,
                        c.name.clone(),
                        DefKind::Const,
                        is_interface || c.is_pub,
                        c.id,
                        index,
                        None,
                    );
                    self.insert_symbol(module_id, &c.name, def, DefKind::Const, c.span);
                }
                Item::Impl(_) => {}
            }
        }
    }

    fn check_params(&mut self, module: ModuleId, params: &[Param]) {
        let mut names = HashSet::new();
        for param in params {
            if let ParamKind::Normal { name, .. } = &param.kind {
                if !names.insert(name.clone()) {
                    self.diag_at(
                        module,
                        param.span,
                        Code::E102,
                        format!("duplicate parameter `{name}`"),
                    );
                }
            }
        }
    }

    fn check_type_params(&mut self, module: ModuleId, generics: &[GenericParam]) {
        let mut names = HashSet::new();
        for g in generics {
            if !names.insert(g.name.clone()) {
                self.diag_at(
                    module,
                    g.span,
                    Code::E102,
                    format!("duplicate type parameter `{}`", g.name),
                );
            }
        }
    }

    fn build_impls(&mut self) {
        for i in 0..self.modules.len() {
            let module_id = ModuleId(i as u32);
            let ast = Rc::clone(&self.modules[i].ast);
            let mut methods_by_type: HashMap<String, HashSet<String>> = HashMap::new();
            for (index, item) in ast.items.iter().enumerate() {
                let Item::Impl(imp) = item else {
                    continue;
                };
                self.check_type_params(module_id, &imp.generics);
                let type_def = self.modules[i].symbols.types.get(&imp.type_name).copied();
                let Some(type_def) = type_def else {
                    self.diag_at(
                        module_id,
                        imp.span,
                        Code::E209,
                        format!(
                            "`impl` target `{}` is not a type defined in this file",
                            imp.type_name
                        ),
                    );
                    continue;
                };
                let type_index = self.defs[type_def.0 as usize].item_index;
                let expected_args = match &ast.items[type_index] {
                    Item::Struct(s) => s.generics.len(),
                    Item::Enum(e) => e.generics.len(),
                    _ => 0,
                };
                if imp.type_args.len() != expected_args {
                    self.diag_at(
                        module_id,
                        imp.span,
                        Code::E209,
                        format!(
                            "`{}` expects {expected_args} type argument(s), found {}",
                            imp.type_name,
                            imp.type_args.len()
                        ),
                    );
                }
                if !imp.generics.is_empty() && imp.generics.len() != expected_args {
                    self.diag_at(
                        module_id,
                        imp.span,
                        Code::E209,
                        format!(
                            "`impl` declares {} type parameter(s) for a type with {expected_args}",
                            imp.generics.len()
                        ),
                    );
                }
                let field_names: HashSet<String> = match &ast.items[type_index] {
                    Item::Struct(s) => s.fields.iter().map(|f| f.name.clone()).collect(),
                    _ => HashSet::new(),
                };
                let methods = methods_by_type.entry(imp.type_name.clone()).or_default();
                for (method_index, method) in imp.methods.iter().enumerate() {
                    self.check_params(module_id, &method.params);
                    self.check_type_params(module_id, &method.generics);
                    if field_names.contains(&method.name) {
                        self.diag_at(
                            module_id,
                            method.span,
                            Code::E102,
                            format!(
                                "`{}` has both a field and a method named `{}`",
                                imp.type_name, method.name
                            ),
                        );
                    }
                    if !methods.insert(method.name.clone()) {
                        self.diag_at(
                            module_id,
                            method.span,
                            Code::E102,
                            format!("duplicate method `{}` on `{}`", method.name, imp.type_name),
                        );
                    }
                    let def = self.add_def(
                        module_id,
                        method.name.clone(),
                        DefKind::Method,
                        method.is_pub || self.modules[i].is_interface,
                        method.id,
                        index,
                        Some(imp.type_name.clone()),
                    );
                    self.defs[def.0 as usize].method_index = Some(method_index);
                    self.node_to_def.insert(method.id, def);
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Imports
    // ------------------------------------------------------------------

    fn resolve_all_uses(&mut self) {
        for i in 0..self.modules.len() {
            self.resolve_uses(ModuleId(i as u32));
        }
    }

    fn resolve_uses(&mut self, module_id: ModuleId) {
        let ast = Rc::clone(&self.modules[module_id.0 as usize].ast);
        let is_interface = self.modules[module_id.0 as usize].is_interface;
        for item in ast.items.iter() {
            let Item::Use(use_decl) = item else {
                continue;
            };
            let full = use_decl.path.join(".");
            let target = if Self::is_builtin_module(&full) {
                Some(UseResolved::Module(ModuleTarget::Builtin(
                    match full.as_str() {
                        "std.math" => "std.math",
                        "std.json" => "std.json",
                        _ => unreachable!(),
                    },
                )))
            } else if self.module_exists(&full) {
                let is_iface = self.interfaces.contains_key(&full);
                let mid = self.ensure_module(&full, is_iface);
                mid.map(|id| {
                    if is_iface {
                        UseResolved::Module(ModuleTarget::Interface(id))
                    } else {
                        UseResolved::Module(ModuleTarget::Source(id))
                    }
                })
            } else {
                self.resolve_item_import(use_decl)
            };

            let Some(target) = target else {
                self.diag_at(
                    module_id,
                    use_decl.span,
                    Code::E106,
                    format!("unknown module or item `{full}`"),
                );
                continue;
            };

            match target {
                UseResolved::Module(target) => {
                    let name = use_decl
                        .alias
                        .clone()
                        .unwrap_or_else(|| use_decl.path.last().cloned().unwrap_or_default());
                    if !is_snake_case(&name) {
                        self.diag_at(
                            module_id,
                            use_decl.span,
                            Code::E104,
                            format!("module alias `{name}` must be snake_case"),
                        );
                    }
                    if self.modules[module_id.0 as usize].symbols.contains(&name) {
                        self.diag_at(
                            module_id,
                            use_decl.span,
                            Code::E102,
                            format!("duplicate import binding `{name}`"),
                        );
                    } else {
                        self.modules[module_id.0 as usize]
                            .symbols
                            .modules
                            .insert(name, target);
                    }
                    if let ModuleTarget::Interface(mid) = target {
                        if !is_interface {
                            let path = self.modules[mid.0 as usize].path.clone();
                            if !self.options.grants.iter().any(|g| g == &path) {
                                self.diag_at(
                                    module_id,
                                    use_decl.span,
                                    Code::E501,
                                    format!("module `{path}` is not granted by the manifest"),
                                );
                            }
                        }
                    }
                    self.modules[module_id.0 as usize].uses.push(UseBinding {
                        decl_node: use_decl.id,
                        span: use_decl.span,
                        path: use_decl.path.clone(),
                        alias: use_decl.alias.clone(),
                        target: ModuleTargetUse::Module(target),
                    });
                }
                UseResolved::Item(target_module, item_name) => {
                    let Some(def) = self.lookup_item(target_module, &item_name) else {
                        self.diag_at(
                            module_id,
                            use_decl.span,
                            Code::E106,
                            format!(
                                "module `{}` has no item `{item_name}`",
                                self.modules[target_module.0 as usize].path
                            ),
                        );
                        continue;
                    };
                    let target_is_interface = self.modules[target_module.0 as usize].is_interface;
                    if !target_is_interface && !self.defs[def.0 as usize].is_pub {
                        self.diag_at(
                            module_id,
                            use_decl.span,
                            Code::E106,
                            format!("`{item_name}` is private"),
                        );
                        continue;
                    }
                    if target_is_interface && !is_interface {
                        let path = self.modules[target_module.0 as usize].path.clone();
                        if !self.options.grants.iter().any(|g| g == &path) {
                            self.diag_at(
                                module_id,
                                use_decl.span,
                                Code::E501,
                                format!("module `{path}` is not granted by the manifest"),
                            );
                        }
                    }
                    let alias = use_decl.alias.clone().unwrap_or(item_name.clone());
                    let kind = self.defs[def.0 as usize].kind;
                    let class_ok = match kind {
                        DefKind::Fn | DefKind::Handler | DefKind::Method => is_snake_case(&alias),
                        DefKind::Struct | DefKind::Enum => is_pascal_case(&alias),
                        DefKind::Const => is_screaming_case(&alias),
                        DefKind::Test => true,
                    };
                    if !class_ok {
                        self.diag_at(
                            module_id,
                            use_decl.span,
                            Code::E104,
                            format!("import alias `{alias}` has the wrong casing class"),
                        );
                    }
                    if self.modules[module_id.0 as usize].symbols.contains(&alias) {
                        self.diag_at(
                            module_id,
                            use_decl.span,
                            Code::E102,
                            format!("duplicate import binding `{alias}`"),
                        );
                    } else {
                        let symbols = &mut self.modules[module_id.0 as usize].symbols;
                        match kind {
                            DefKind::Fn | DefKind::Handler | DefKind::Method => {
                                symbols.values.insert(alias.clone(), def);
                                symbols.imported_values.insert(alias.clone(), def);
                            }
                            DefKind::Struct | DefKind::Enum => {
                                symbols.types.insert(alias.clone(), def);
                                symbols.imported_types.insert(alias.clone(), def);
                            }
                            DefKind::Const => {
                                symbols.consts.insert(alias.clone(), def);
                                symbols.imported_consts.insert(alias.clone(), def);
                            }
                            DefKind::Test => {}
                        }
                    }
                    self.modules[module_id.0 as usize].uses.push(UseBinding {
                        decl_node: use_decl.id,
                        span: use_decl.span,
                        path: use_decl.path.clone(),
                        alias: use_decl.alias.clone(),
                        target: ModuleTargetUse::Item(def),
                    });
                }
                UseResolved::BuiltinItem(module, item_name) => {
                    let Some(item) = builtin_item(&module, &item_name) else {
                        self.diag_at(
                            module_id,
                            use_decl.span,
                            Code::E106,
                            format!("`{module}` has no item `{item_name}`"),
                        );
                        continue;
                    };
                    let alias = use_decl
                        .alias
                        .clone()
                        .unwrap_or_else(|| item.name.to_string());
                    let class_ok = match item.kind {
                        DefKind::Fn | DefKind::Handler | DefKind::Method => is_snake_case(&alias),
                        DefKind::Struct | DefKind::Enum => is_pascal_case(&alias),
                        DefKind::Const => is_screaming_case(&alias),
                        DefKind::Test => true,
                    };
                    if !class_ok {
                        self.diag_at(
                            module_id,
                            use_decl.span,
                            Code::E104,
                            format!("import alias `{alias}` has the wrong casing class"),
                        );
                    }
                    if self.modules[module_id.0 as usize].symbols.contains(&alias) {
                        self.diag_at(
                            module_id,
                            use_decl.span,
                            Code::E102,
                            format!("duplicate import binding `{alias}`"),
                        );
                    } else {
                        self.modules[module_id.0 as usize]
                            .symbols
                            .builtin_items
                            .insert(alias, item);
                        self.modules[module_id.0 as usize].uses.push(UseBinding {
                            decl_node: use_decl.id,
                            span: use_decl.span,
                            path: use_decl.path.clone(),
                            alias: use_decl.alias.clone(),
                            target: ModuleTargetUse::BuiltinItem(item),
                        });
                    }
                }
            }
        }
    }

    fn resolve_item_import(&mut self, use_decl: &UseDecl) -> Option<UseResolved> {
        for k in (1..use_decl.path.len()).rev() {
            let prefix = use_decl.path[..k].join(".");
            if Self::is_builtin_module(&prefix) {
                if use_decl.path.len() - k != 1 {
                    return None;
                }
                return Some(UseResolved::BuiltinItem(prefix, use_decl.path[k].clone()));
            }
            if self.module_exists(&prefix) {
                let is_iface = self.interfaces.contains_key(&prefix);
                let mid = self.ensure_module(&prefix, is_iface)?;
                let remaining = &use_decl.path[k..];
                if remaining.len() != 1 {
                    return None;
                }
                return Some(UseResolved::Item(mid, remaining[0].clone()));
            }
        }
        None
    }

    fn lookup_item(&self, module: ModuleId, name: &str) -> Option<DefId> {
        let symbols = &self.modules[module.0 as usize].symbols;
        symbols
            .values
            .get(name)
            .or_else(|| symbols.types.get(name))
            .or_else(|| symbols.consts.get(name))
            .copied()
    }

    // ------------------------------------------------------------------
    // Cross-cutting rules
    // ------------------------------------------------------------------

    fn validate_interfaces(&mut self) {
        for i in 0..self.modules.len() {
            if !self.modules[i].is_interface {
                continue;
            }
            let ast = Rc::clone(&self.modules[i].ast);
            let source = self.modules[i].source;
            let path = self.modules[i].path.clone();
            for item in ast.items.iter() {
                if let Item::On(hook) = item {
                    if hook.name == "init" || hook.name == "shutdown" {
                        self.diag(
                            Code::E507,
                            Some(source),
                            Some(hook.span),
                            format!("`{}` is a reserved hook name", hook.name),
                        );
                    }
                    if let Some((other_path, _span, _)) = self.interface_hooks.get(&hook.name) {
                        self.diag(
                            Code::E507,
                            Some(source),
                            Some(hook.span),
                            format!(
                                "hook `{}` is already declared by interface `{other_path}`",
                                hook.name
                            ),
                        );
                    } else {
                        self.interface_hooks
                            .insert(hook.name.clone(), (path.clone(), hook.span, source));
                    }
                }
            }
        }
    }

    fn check_module_rules(&mut self) {
        let Some(entry) = self.entry else { return };
        let mut root_main: Option<Span> = None;
        let mut root_handlers: Option<Span> = None;
        for i in 0..self.modules.len() {
            if self.modules[i].is_interface {
                continue;
            }
            let ast = Rc::clone(&self.modules[i].ast);
            let source = self.modules[i].source;
            let is_root = self.modules[i].id == entry;
            for item in ast.items.iter() {
                match item {
                    Item::Fn(f) if f.name == "main" && is_root => {
                        root_main = Some(f.span);
                    }
                    Item::On(o) => {
                        if is_root {
                            root_handlers = Some(o.span);
                        } else {
                            self.diag(
                                Code::E504,
                                Some(source),
                                Some(o.span),
                                "`on` handlers are only allowed in the root file",
                            );
                        }
                    }
                    _ => {}
                }
            }
        }
        if let (Some(main), Some(_handler)) = (root_main, root_handlers) {
            self.diag(
                Code::E407,
                Some(self.modules[entry.0 as usize].source),
                Some(main),
                "a root file may not be both a script (`fn main`) and a plugin (`on` handlers)",
            );
        }
    }

    fn check_grants(&mut self) {
        for grant in &self.options.grants {
            if !self.interfaces.contains_key(grant) && !Self::is_builtin_module(grant) {
                self.diag(
                    Code::E508,
                    None,
                    None,
                    format!("granted module `{grant}` does not exist"),
                );
            }
        }
    }

    fn check_const_cycles(&mut self) {
        for i in 0..self.modules.len() {
            let ast = Rc::clone(&self.modules[i].ast);
            // Collect the value references once per constant so cycle checking
            // never has to touch (or clone) the module AST while recursing.
            let mut consts: HashMap<String, (Span, Vec<String>)> = HashMap::new();
            for item in ast.items.iter() {
                if let Item::Const(c) = item {
                    let mut refs = Vec::new();
                    collect_const_refs(&c.value, &mut refs);
                    consts.insert(c.name.clone(), (c.span, refs));
                }
            }
            let mut state: HashMap<String, u8> = HashMap::new();
            let mut stack = Vec::new();
            let mut names: Vec<String> = consts.keys().cloned().collect();
            names.sort();
            for name in names {
                self.visit_const(&name, &consts, &mut state, &mut stack, ModuleId(i as u32));
            }
        }
    }

    fn visit_const(
        &mut self,
        name: &str,
        consts: &HashMap<String, (Span, Vec<String>)>,
        state: &mut HashMap<String, u8>,
        stack: &mut Vec<String>,
        module: ModuleId,
    ) {
        match state.get(name).copied() {
            Some(2) => return,
            Some(1) => {
                if let Some((span, _)) = consts.get(name) {
                    self.diag_at(
                        module,
                        *span,
                        Code::E105,
                        format!("constant cycle involving `{name}`"),
                    );
                }
                return;
            }
            _ => {}
        }
        state.insert(name.to_string(), 1);
        stack.push(name.to_string());
        if let Some((_, refs)) = consts.get(name) {
            for r in refs {
                if consts.contains_key(r) {
                    self.visit_const(r, consts, state, stack, module);
                }
            }
        }
        stack.pop();
        state.insert(name.to_string(), 2);
    }

    // ------------------------------------------------------------------
    // Diagnostic helpers
    // ------------------------------------------------------------------

    fn diag(
        &mut self,
        code: Code,
        source: Option<SourceId>,
        span: Option<Span>,
        message: impl Into<String>,
    ) {
        let mut d = Diagnostic::error(code, message);
        d.source = source;
        d.span = span;
        self.diagnostics.push(d);
    }

    fn diag_at(&mut self, module: ModuleId, span: Span, code: Code, message: impl Into<String>) {
        let source = self.modules[module.0 as usize].source;
        self.diag(code, Some(source), Some(span), message);
    }
}

enum UseResolved {
    Module(ModuleTarget),
    Item(ModuleId, String),
    BuiltinItem(String, String),
}

fn module_path_from_file(path: &str) -> String {
    let path = path.replace('\\', "/");
    let without_ext = path
        .strip_suffix(".mt")
        .or_else(|| path.strip_suffix(".mti"))
        .unwrap_or(&path);
    without_ext.replace('/', ".")
}

fn is_snake_case(name: &str) -> bool {
    let body = name.strip_prefix('_').unwrap_or(name);
    let mut chars = body.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_pascal_case(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    let rest: Vec<char> = chars.collect();
    rest.iter().all(|c| c.is_ascii_alphanumeric())
        && (name.len() == 1 || rest.iter().any(|c| c.is_ascii_lowercase()))
}

fn is_screaming_case(name: &str) -> bool {
    name.len() >= 2
        && name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn collect_const_refs(expr: &Expr, out: &mut Vec<String>) {
    match &expr.kind {
        ExprKind::Var(name) => out.push(name.clone()),
        ExprKind::Tuple(items) | ExprKind::List(items) => {
            for item in items {
                collect_const_refs(item, out);
            }
        }
        ExprKind::Map(entries) => {
            for (k, v) in entries {
                collect_const_refs(k, out);
                collect_const_refs(v, out);
            }
        }
        ExprKind::StructLit { fields, base, .. } => {
            if let Some(base) = base {
                collect_const_refs(base, out);
            }
            for f in fields {
                if let Some(v) = &f.value {
                    collect_const_refs(v, out);
                }
            }
        }
        ExprKind::Unary { expr, .. } => collect_const_refs(expr, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_const_refs(lhs, out);
            collect_const_refs(rhs, out);
        }
        ExprKind::Range { start, end, .. } => {
            collect_const_refs(start, out);
            collect_const_refs(end, out);
        }
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_const_refs(cond, out);
            collect_block_refs(then_branch, out);
            if let Some(e) = else_branch {
                collect_const_refs(e, out);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_const_refs(scrutinee, out);
            for arm in arms {
                if let Some(g) = &arm.guard {
                    collect_const_refs(g, out);
                }
                match &arm.body {
                    MatchBody::Block(b) => collect_block_refs(b, out),
                    MatchBody::Expr(e) => collect_const_refs(e, out),
                }
            }
        }
        ExprKind::Lambda { body, .. } => match body {
            LambdaBody::Block(b) => collect_block_refs(b, out),
            LambdaBody::Expr(e) => collect_const_refs(e, out),
        },
        ExprKind::Block(b) => collect_block_refs(b, out),
        ExprKind::Field { base, .. } => collect_const_refs(base, out),
        ExprKind::Call { callee, args } => {
            collect_const_refs(callee, out);
            for a in args {
                collect_const_refs(&a.expr, out);
            }
        }
        ExprKind::Index { base, index } => {
            collect_const_refs(base, out);
            collect_const_refs(index, out);
        }
        ExprKind::Try(e) => collect_const_refs(e, out),
        ExprKind::Convert { expr, .. } => collect_const_refs(expr, out),
        ExprKind::Str(parts) => {
            for p in parts {
                if let StrExprPart::Expr(e) = p {
                    collect_const_refs(e, out);
                }
            }
        }
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Unit => {}
    }
}

fn collect_block_refs(block: &Block, out: &mut Vec<String>) {
    for stmt in &block.stmts {
        match &stmt.kind {
            StmtKind::Let { value, .. } => collect_const_refs(value, out),
            StmtKind::While { cond, body } => {
                collect_const_refs(cond, out);
                collect_block_refs(body, out);
            }
            StmtKind::For { iter, body, .. } => {
                collect_const_refs(iter, out);
                collect_block_refs(body, out);
            }
            StmtKind::Return(Some(e)) => collect_const_refs(e, out),
            StmtKind::Expr { expr, assign } => {
                collect_const_refs(expr, out);
                if let Some((_, rhs)) = assign {
                    collect_const_refs(rhs, out);
                }
            }
            StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use martite_base::MemoryProvider;

    fn resolve_text(files: &[(&str, &str)], entry: &str) -> (SourceMap, ResolveOutput) {
        let mut provider = MemoryProvider::new();
        for (path, text) in files {
            provider.insert(*path, *text);
        }
        let mut sources = SourceMap::new();
        let output = resolve(
            &provider,
            &ResolveOptions {
                entry: entry.to_string(),
                interfaces: Vec::new(),
                grants: Vec::new(),
                limits: LoadLimits::default(),
                parse: ParseOptions::default(),
            },
            &mut sources,
        );
        (sources, output)
    }

    #[test]
    fn loads_modules_and_items() {
        let (sources, out) = resolve_text(
            &[
                (
                    "main.mt",
                    "use geometry.shapes\nuse geometry.shapes.Circle\nuse config.MAX_SIZE\n\nfn main() { let c: Circle = shapes.make() }\n",
                ),
                (
                    "geometry/shapes.mt",
                    "pub struct Circle { r: int }\npub fn make() -> Circle { Circle { r: 1 } }\n",
                ),
                ("config.mt", "pub const MAX_SIZE: int = 10\n"),
            ],
            "main.mt",
        );
        if !out.diagnostics.is_empty() {
            for d in &out.diagnostics {
                eprintln!("{}", d.render(&sources));
            }
        }
        assert!(!out.has_errors(), "unexpected errors");
        let program = out.program.expect("program");
        let main = program.entry_module();
        assert!(main.symbols.modules.contains_key("shapes"));
        assert!(main.symbols.types.contains_key("Circle"));
        assert!(main.symbols.consts.contains_key("MAX_SIZE"));
    }

    #[test]
    fn builtin_items_are_canonical_and_static() {
        let (_sources, out) = resolve_text(
            &[(
                "main.mt",
                "use std.math.EULER\nuse std.json.parse\nfn main() {}\n",
            )],
            "main.mt",
        );
        assert!(!out.has_errors(), "unexpected errors");
        let program = out.program.expect("program");
        let symbols = &program.entry_module().symbols;

        let euler = symbols.builtin_items.get("EULER").copied().expect("EULER");
        assert_eq!(euler.module, "std.math");
        assert_eq!(euler.name, "EULER");
        assert_eq!(euler.kind, DefKind::Const);

        let parse = symbols.builtin_items.get("parse").copied().expect("parse");
        assert_eq!(parse.module, "std.json");
        assert_eq!(parse.name, "parse");
        assert_eq!(parse.kind, DefKind::Fn);
    }

    #[test]
    fn unknown_builtin_item_is_e106() {
        let (_sources, out) = resolve_text(
            &[("main.mt", "use std.math.nope\nfn main() {}\n")],
            "main.mt",
        );
        assert!(out.diagnostics.iter().any(|d| d.code == Code::E106));
    }

    #[test]
    fn duplicate_import_is_e102() {
        let (_sources, out) = resolve_text(
            &[
                ("main.mt", "use a.util\nuse b.util\nfn main() {}\n"),
                ("a/util.mt", "pub fn f() {}\n"),
                ("b/util.mt", "pub fn f() {}\n"),
            ],
            "main.mt",
        );
        assert!(out.diagnostics.iter().any(|d| d.code == Code::E102));
    }
}
