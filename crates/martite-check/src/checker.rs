use std::collections::{HashMap, HashSet};

use martite_base::{Code, Diagnostic, Severity, Span};
use martite_resolve::{
    BuiltinItem, DefId, DefKind, ModuleId, ModuleTarget, ResolvedProgram, builtin_item,
};
use martite_syntax::*;

use crate::exhaustive::is_exhaustive;
use crate::program::*;
use crate::ty::*;

pub fn check(resolved: ResolvedProgram) -> crate::CheckOutput {
    let mut checker = Checker::new(resolved);
    checker.run();
    let has_errors = checker
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error);
    let diagnostics = checker.diagnostics.clone();
    let program = checker.into_program();
    crate::CheckOutput {
        program: if has_errors { None } else { Some(program) },
        diagnostics,
    }
}

#[derive(Clone)]
struct FnCtx {
    module: ModuleId,
    return_ty: Ty,
    type_params: Vec<TypeParam>,
    type_param_names: HashMap<String, usize>,
    self_ty: Option<Ty>,
    self_binding: Option<BindingId>,
    self_mutable: bool,
    is_test: bool,
    loop_depth: u32,
    closure_boundary: Option<usize>,
}

struct LambdaCaptureCtx {
    node: NodeId,
    boundary: usize,
    captures: HashSet<BindingId>,
}

struct Checker {
    resolved: ResolvedProgram,
    diagnostics: Vec<Diagnostic>,
    types: TypeTable,
    json_type: Option<TypeId>,
    def_to_type: HashMap<DefId, TypeId>,
    functions: HashMap<DefId, FnSig>,
    methods: HashMap<(TypeId, String), DefId>,
    const_types: HashMap<DefId, Ty>,
    consts: HashMap<DefId, ConstInfo>,
    expr_types: HashMap<NodeId, Ty>,
    resolutions: HashMap<NodeId, Resolution>,
    pattern_resolutions: HashMap<NodeId, PatResolution>,
    struct_lit_fields: HashMap<NodeId, Vec<(usize, usize)>>,
    struct_pattern_fields: HashMap<NodeId, Vec<(usize, usize)>>,
    bindings: HashMap<BindingId, BindingInfo>,
    pattern_bindings: HashMap<NodeId, BindingId>,
    param_bindings: HashMap<NodeId, BindingId>,
    lambdas: HashMap<NodeId, LambdaInfo>,
    handlers: HashMap<String, DefId>,
    hooks: HashMap<String, HookSig>,
    tests: Vec<DefId>,
    state_type: Option<Ty>,
    init: Option<DefId>,
    shutdown: Option<DefId>,
    main: Option<DefId>,
    entry_kind: EntryKind,
    next_binding: u32,
    scopes: Vec<HashMap<String, BindingId>>,
    lambda_stack: Vec<LambdaCaptureCtx>,
    ctx: Option<FnCtx>,
}

impl Checker {
    fn new(resolved: ResolvedProgram) -> Self {
        Self {
            resolved,
            diagnostics: Vec::new(),
            types: TypeTable::default(),
            json_type: None,
            def_to_type: HashMap::new(),
            functions: HashMap::new(),
            methods: HashMap::new(),
            const_types: HashMap::new(),
            consts: HashMap::new(),
            expr_types: HashMap::new(),
            resolutions: HashMap::new(),
            pattern_resolutions: HashMap::new(),
            struct_lit_fields: HashMap::new(),
            struct_pattern_fields: HashMap::new(),
            bindings: HashMap::new(),
            pattern_bindings: HashMap::new(),
            param_bindings: HashMap::new(),
            lambdas: HashMap::new(),
            handlers: HashMap::new(),
            hooks: HashMap::new(),
            tests: Vec::new(),
            state_type: None,
            init: None,
            shutdown: None,
            main: None,
            entry_kind: EntryKind::Library,
            next_binding: 0,
            scopes: Vec::new(),
            lambda_stack: Vec::new(),
            ctx: None,
        }
    }

    fn run(&mut self) {
        self.collect_types();
        self.collect_hooks();
        self.collect_signatures();
        self.decide_entry();
        self.collect_handlers();
        self.check_consts();
        self.check_bodies();
    }

    fn into_program(self) -> CheckedProgram {
        CheckedProgram {
            resolved: self.resolved,
            types: self.types,
            functions: self.functions,
            methods: self.methods,
            consts: self.consts,
            expr_types: self.expr_types,
            resolutions: self.resolutions,
            pattern_resolutions: self.pattern_resolutions,
            struct_lit_fields: self.struct_lit_fields,
            struct_pattern_fields: self.struct_pattern_fields,
            bindings: self.bindings,
            pattern_bindings: self.pattern_bindings,
            param_bindings: self.param_bindings,
            lambdas: self.lambdas,
            handlers: self.handlers,
            hooks: self.hooks,
            tests: self.tests,
            state_type: self.state_type,
            init: self.init,
            shutdown: self.shutdown,
            main: self.main,
            entry_kind: self.entry_kind,
            json_type: self.json_type,
        }
    }

    // ------------------------------------------------------------------
    // Types
    // ------------------------------------------------------------------

    fn collect_types(&mut self) {
        let json = self.types.add(TypeDef {
            id: TypeId(0),
            name: "Json".into(),
            module: None,
            kind: TypeDefKind::Enum {
                variants: Vec::new(),
            },
            params: Vec::new(),
            is_interface: false,
            is_builtin: true,
            def: None,
        });
        self.json_type = Some(json);
        self.types.get_mut(json).unwrap().kind = TypeDefKind::Enum {
            variants: vec![
                VariantDef {
                    name: "Null".into(),
                    payload: Vec::new(),
                },
                VariantDef {
                    name: "Bool".into(),
                    payload: vec![Ty::Bool],
                },
                VariantDef {
                    name: "Num".into(),
                    payload: vec![Ty::Float],
                },
                VariantDef {
                    name: "Str".into(),
                    payload: vec![Ty::Str],
                },
                VariantDef {
                    name: "Arr".into(),
                    payload: vec![Ty::list(Ty::Named(json, Vec::new()))],
                },
                VariantDef {
                    name: "Obj".into(),
                    payload: vec![Ty::Map(
                        Box::new(Ty::Str),
                        Box::new(Ty::Named(json, Vec::new())),
                    )],
                },
            ],
        };

        for i in 0..self.resolved.modules.len() {
            let module_id = ModuleId(i as u32);
            let items = self.resolved.module(module_id).ast.items.clone();
            let is_interface = self.resolved.module(module_id).is_interface;
            for item in &items {
                let (def, name, generics) = match item {
                    Item::Struct(s) => (
                        self.resolved.node_to_def.get(&s.id).copied(),
                        &s.name,
                        &s.generics,
                    ),
                    Item::Enum(e) => (
                        self.resolved.node_to_def.get(&e.id).copied(),
                        &e.name,
                        &e.generics,
                    ),
                    _ => continue,
                };
                let Some(def) = def else { continue };
                if self.def_to_type.contains_key(&def) {
                    continue;
                }
                let params = generics
                    .iter()
                    .map(|g| TypeParam {
                        name: g.name.clone(),
                        bounds: g.bounds.clone(),
                    })
                    .collect::<Vec<_>>();
                let id = self.types.add(TypeDef {
                    id: TypeId(0),
                    name: name.clone(),
                    module: Some(module_id),
                    kind: TypeDefKind::Struct { fields: Vec::new() },
                    params,
                    is_interface,
                    is_builtin: false,
                    def: Some(def),
                });
                self.def_to_type.insert(def, id);
            }
        }

        // Fill fields and variants now that every type name is mapped.
        let entries: Vec<(DefId, TypeId)> =
            self.def_to_type.iter().map(|(a, b)| (*a, *b)).collect();
        for (def, type_id) in entries {
            let info = self.resolved.def(def).clone();
            let module_id = info.module;
            let items = self.resolved.module(module_id).ast.items.clone();
            let Some(item) = items.get(info.item_index) else {
                continue;
            };
            let params = self
                .types
                .get(type_id)
                .map(|d| d.params.clone())
                .unwrap_or_default();
            match item {
                Item::Struct(s) => {
                    if s.opaque {
                        if let Some(t) = self.types.get_mut(type_id) {
                            t.kind = TypeDefKind::Opaque;
                        }
                    } else {
                        let mut fields = Vec::new();
                        for field in &s.fields {
                            let ty = match self.resolve_ty_in(module_id, &field.ty, &params) {
                                Ok(t) => t,
                                Err(msg) => {
                                    self.report_type_error(module_id, field.ty.span, msg);
                                    Ty::Error
                                }
                            };
                            fields.push(FieldDef {
                                name: field.name.clone(),
                                ty,
                            });
                        }
                        if let Some(t) = self.types.get_mut(type_id) {
                            t.kind = TypeDefKind::Struct { fields };
                        }
                    }
                }
                Item::Enum(e) => {
                    let mut variants = Vec::new();
                    for variant in &e.variants {
                        let mut payload = Vec::new();
                        for ty in &variant.payload {
                            match self.resolve_ty_in(module_id, ty, &params) {
                                Ok(t) => payload.push(t),
                                Err(msg) => {
                                    self.report_type_error(module_id, ty.span, msg);
                                    payload.push(Ty::Error);
                                }
                            }
                        }
                        variants.push(VariantDef {
                            name: variant.name.clone(),
                            payload,
                        });
                    }
                    if let Some(t) = self.types.get_mut(type_id) {
                        t.kind = TypeDefKind::Enum { variants };
                    }
                }
                _ => {}
            }
        }
    }

    fn resolve_ty_in(
        &self,
        module: ModuleId,
        ty: &TyExpr,
        params: &[TypeParam],
    ) -> Result<Ty, String> {
        let symbols = &self.resolved.module(module).symbols;
        let names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
        let imported = HashMap::new();
        TypeResolver {
            types: &self.types,
            def_to_type: &self.def_to_type,
            builtin_json: self.json_type,
            type_param_names: &names,
            imported_builtin_types: &imported,
        }
        .resolve(ty, symbols)
    }

    // ------------------------------------------------------------------
    // Signatures and hooks
    // ------------------------------------------------------------------

    fn collect_hooks(&mut self) {
        let interface_modules: Vec<ModuleId> = self
            .resolved
            .modules
            .iter()
            .filter(|m| m.is_interface)
            .map(|m| m.id)
            .collect();
        for module_id in interface_modules {
            let items = self.resolved.module(module_id).ast.items.clone();
            let source = self.resolved.module(module_id).source;
            for item in &items {
                let Item::On(hook) = item else { continue };
                let mut params = Vec::new();
                for param in &hook.params {
                    let ParamKind::Normal { ty, .. } = &param.kind else {
                        continue;
                    };
                    match self.resolve_ty_in(module_id, ty, &[]) {
                        Ok(t) => params.push(t),
                        Err(msg) => {
                            self.report_type_error(module_id, ty.span, msg);
                            params.push(Ty::Error);
                        }
                    }
                }
                let ret = match &hook.ret {
                    Some(t) => match self.resolve_ty_in(module_id, t, &[]) {
                        Ok(t) => t,
                        Err(msg) => {
                            self.report_type_error(module_id, t.span, msg);
                            Ty::Error
                        }
                    },
                    None => Ty::Unit,
                };
                self.hooks.insert(
                    hook.name.clone(),
                    HookSig {
                        name: hook.name.clone(),
                        params,
                        ret,
                        module: module_id,
                        span: hook.span,
                        source,
                    },
                );
            }
        }
    }

    fn collect_signatures(&mut self) {
        for i in 0..self.resolved.modules.len() {
            let module_id = ModuleId(i as u32);
            let items = self.resolved.module(module_id).ast.items.clone();
            let is_interface = self.resolved.module(module_id).is_interface;
            for item in &items {
                match item {
                    Item::Fn(f) => {
                        if f.name == "main" && module_id == self.resolved.entry {
                            // Checked later by decide_entry.
                        }
                        let Some(def) = self.resolved.node_to_def.get(&f.id).copied() else {
                            continue;
                        };
                        let type_params: Vec<TypeParam> = f
                            .generics
                            .iter()
                            .map(|g| TypeParam {
                                name: g.name.clone(),
                                bounds: g.bounds.clone(),
                            })
                            .collect();
                        let mut params = Vec::new();
                        for param in &f.params {
                            let ParamKind::Normal { name, mutable, ty } = &param.kind else {
                                continue;
                            };
                            let ty = match self.resolve_ty_in(module_id, ty, &type_params) {
                                Ok(t) => t,
                                Err(msg) => {
                                    self.report_type_error(module_id, ty.span, msg);
                                    Ty::Error
                                }
                            };
                            let binding =
                                self.alloc_binding(name, ty.clone(), *mutable, param.id, false, 0);
                            params.push(ParamSig {
                                node: param.id,
                                binding,
                                span: param.span,
                                name: name.clone(),
                                ty,
                                mutable: *mutable,
                                is_self: false,
                            });
                        }
                        let ret = match &f.ret {
                            Some(t) => match self.resolve_ty_in(module_id, t, &type_params) {
                                Ok(t) => t,
                                Err(msg) => {
                                    self.report_type_error(module_id, t.span, msg);
                                    Ty::Error
                                }
                            },
                            None => Ty::Unit,
                        };
                        self.functions.insert(
                            def,
                            FnSig {
                                def,
                                node: f.id,
                                module: module_id,
                                name: f.name.clone(),
                                params,
                                ret,
                                generics: f.generics.clone(),
                                type_params,
                                kind: if f.body.is_none() && is_interface {
                                    FnKind::Host
                                } else {
                                    FnKind::Free
                                },
                                span: f.span,
                                is_pub: f.is_pub || is_interface,
                            },
                        );
                    }
                    Item::Test(t) => {
                        let Some(def) = self.resolved.node_to_def.get(&t.id).copied() else {
                            continue;
                        };
                        self.functions.insert(
                            def,
                            FnSig {
                                def,
                                node: t.id,
                                module: module_id,
                                name: t.name.clone(),
                                params: Vec::new(),
                                ret: Ty::Unit,
                                generics: Vec::new(),
                                type_params: Vec::new(),
                                kind: FnKind::Test,
                                span: t.span,
                                is_pub: false,
                            },
                        );
                        self.tests.push(def);
                    }
                    Item::Impl(imp) => {
                        self.collect_impl_signatures(module_id, imp);
                    }
                    _ => {}
                }
            }
        }
    }

    fn collect_impl_signatures(&mut self, module_id: ModuleId, imp: &ImplDecl) {
        let Some(type_def) = self
            .resolved
            .module(module_id)
            .symbols
            .types
            .get(&imp.type_name)
            .copied()
        else {
            return;
        };
        let Some(type_id) = self.def_to_type.get(&type_def).copied() else {
            return;
        };
        let target_params = self
            .types
            .get(type_id)
            .map(|d| d.params.clone())
            .unwrap_or_default();
        let impl_params: Vec<TypeParam> = imp
            .generics
            .iter()
            .map(|g| TypeParam {
                name: g.name.clone(),
                bounds: g.bounds.clone(),
            })
            .collect();
        let self_args: Vec<Ty> = if impl_params.is_empty() {
            imp.type_args
                .iter()
                .map(|t| {
                    self.resolve_ty_in(module_id, t, &target_params)
                        .unwrap_or(Ty::Error)
                })
                .collect()
        } else {
            (0..impl_params.len())
                .map(|i| Ty::Param(i as u32))
                .collect()
        };
        let self_ty = Ty::Named(type_id, self_args);
        for method in &imp.methods {
            let Some(def) = self.resolved.node_to_def.get(&method.id).copied() else {
                continue;
            };
            let mut type_params = impl_params.clone();
            let offset = type_params.len() as u32;
            for g in &method.generics {
                type_params.push(TypeParam {
                    name: g.name.clone(),
                    bounds: g.bounds.clone(),
                });
            }
            let mut params = Vec::new();
            for (index, param) in method.params.iter().enumerate() {
                match &param.kind {
                    ParamKind::SelfParam { mutable } => {
                        let binding = self.alloc_binding(
                            "self",
                            self_ty.clone(),
                            *mutable,
                            param.id,
                            true,
                            0,
                        );
                        params.push(ParamSig {
                            node: param.id,
                            binding,
                            span: param.span,
                            name: "self".into(),
                            ty: self_ty.clone(),
                            mutable: *mutable,
                            is_self: true,
                        });
                    }
                    ParamKind::Normal { name, mutable, ty } => {
                        let ty = match self.resolve_ty_in(module_id, ty, &type_params) {
                            Ok(t) => t,
                            Err(msg) => {
                                self.report_type_error(module_id, ty.span, msg);
                                Ty::Error
                            }
                        };
                        let binding =
                            self.alloc_binding(name, ty.clone(), *mutable, param.id, false, 0);
                        params.push(ParamSig {
                            node: param.id,
                            binding,
                            span: param.span,
                            name: name.clone(),
                            ty,
                            mutable: *mutable,
                            is_self: false,
                        });
                        let _ = (index, offset);
                    }
                }
            }
            let ret = match &method.ret {
                Some(t) => match self.resolve_ty_in(module_id, t, &type_params) {
                    Ok(t) => t,
                    Err(msg) => {
                        self.report_type_error(module_id, t.span, msg);
                        Ty::Error
                    }
                },
                None => Ty::Unit,
            };
            self.functions.insert(
                def,
                FnSig {
                    def,
                    node: method.id,
                    module: module_id,
                    name: method.name.clone(),
                    params,
                    ret,
                    generics: method.generics.clone(),
                    type_params,
                    kind: FnKind::Method(type_id),
                    span: method.span,
                    is_pub: method.is_pub,
                },
            );
            self.methods.insert((type_id, method.name.clone()), def);
        }
    }

    fn decide_entry(&mut self) {
        if let Some(def) = self
            .resolved
            .entry_module()
            .symbols
            .values
            .get("main")
            .copied()
            .filter(|def| self.resolved.def(*def).kind == DefKind::Fn)
        {
            self.main = Some(def);
            self.entry_kind = EntryKind::Script;
            let sig = self.functions.get(&def).cloned();
            if let Some(sig) = sig {
                let ok_ret = sig.ret == Ty::Unit
                    || matches!(&sig.ret, Ty::Result(t, e) if **t == Ty::Unit && is_show(e, &self.types));
                if !sig.params.is_empty() || !ok_ret {
                    self.err_at(
                        sig.module,
                        sig.span,
                        Code::E407,
                        "`main` must be `fn main()` or `fn main() -> Result[(), E]` where `E: Show`",
                    );
                }
            }
        }
        let root = self.resolved.entry;
        let has_handlers = self
            .resolved
            .module(root)
            .ast
            .items
            .iter()
            .any(|item| matches!(item, Item::On(_)));
        if has_handlers {
            self.entry_kind = EntryKind::Plugin;
        }
    }

    fn collect_handlers(&mut self) {
        let root = self.resolved.entry;
        let items = self.resolved.module(root).ast.items.clone();

        let init_item = items.iter().find_map(|item| match item {
            Item::On(o) if o.name == "init" => Some(o.clone()),
            _ => None,
        });

        let mut state = None;
        if let Some(init) = &init_item {
            let Some(def) = self.resolved.node_to_def.get(&init.id).copied() else {
                return;
            };
            self.init = Some(def);
            let mut params = Vec::new();
            for param in &init.params {
                match &param.kind {
                    ParamKind::SelfParam { .. } => {
                        self.err_at(root, param.span, Code::E504, "`init` may not take `self`");
                    }
                    ParamKind::Normal { name, mutable, ty } => {
                        let ty = match self.resolve_ty_in(root, ty, &[]) {
                            Ok(t) => t,
                            Err(msg) => {
                                self.report_type_error(root, ty.span, msg);
                                Ty::Error
                            }
                        };
                        if !self.is_json(&ty) {
                            self.err_at(
                                root,
                                param.span,
                                Code::E504,
                                "`init` may take zero parameters or one `Json` parameter",
                            );
                        }
                        let binding =
                            self.alloc_binding(name, ty.clone(), *mutable, param.id, false, 0);
                        params.push(ParamSig {
                            node: param.id,
                            binding,
                            span: param.span,
                            name: name.clone(),
                            ty,
                            mutable: *mutable,
                            is_self: false,
                        });
                    }
                }
            }
            if params.len() > 1 {
                self.err_at(
                    root,
                    init.span,
                    Code::E504,
                    "`init` may take at most one parameter",
                );
            }
            let ret = match &init.ret {
                Some(t) => self.resolve_ty_in(root, t, &[]).unwrap_or_else(|msg| {
                    self.report_type_error(root, t.span, msg);
                    Ty::Error
                }),
                None => Ty::Unit,
            };
            if !self.is_data_type(&ret) {
                self.err_at(
                    root,
                    init.span,
                    Code::E506,
                    "plugin state must be a data type (it may not contain functions)",
                );
            }
            state = Some(ret.clone());
            self.functions.insert(
                def,
                FnSig {
                    def,
                    node: init.id,
                    module: root,
                    name: "init".into(),
                    params,
                    ret,
                    generics: Vec::new(),
                    type_params: Vec::new(),
                    kind: FnKind::Handler,
                    span: init.span,
                    is_pub: false,
                },
            );
            self.handlers.insert("init".into(), def);
        }
        self.state_type = state.clone();

        for item in &items {
            let Item::On(handler) = item else { continue };
            if handler.name == "init" {
                continue;
            }
            let Some(def) = self.resolved.node_to_def.get(&handler.id).copied() else {
                continue;
            };
            let is_shutdown = handler.name == "shutdown";
            let mut params = Vec::new();
            let mut self_seen = false;
            for param in &handler.params {
                match &param.kind {
                    ParamKind::SelfParam { mutable } => {
                        self_seen = true;
                        let ty = state.clone().unwrap_or_else(|| {
                            self.err_at(
                                root,
                                param.span,
                                Code::E504,
                                "a handler with `self` requires an `init` handler",
                            );
                            Ty::Error
                        });
                        let binding =
                            self.alloc_binding("self", ty.clone(), *mutable, param.id, true, 0);
                        params.push(ParamSig {
                            node: param.id,
                            binding,
                            span: param.span,
                            name: "self".into(),
                            ty,
                            mutable: *mutable,
                            is_self: true,
                        });
                    }
                    ParamKind::Normal { name, mutable, ty } => {
                        let ty = match self.resolve_ty_in(root, ty, &[]) {
                            Ok(t) => t,
                            Err(msg) => {
                                self.report_type_error(root, ty.span, msg);
                                Ty::Error
                            }
                        };
                        let binding =
                            self.alloc_binding(name, ty.clone(), *mutable, param.id, false, 0);
                        params.push(ParamSig {
                            node: param.id,
                            binding,
                            span: param.span,
                            name: name.clone(),
                            ty,
                            mutable: *mutable,
                            is_self: false,
                        });
                    }
                }
            }
            let ret = match &handler.ret {
                Some(t) => match self.resolve_ty_in(root, t, &[]) {
                    Ok(t) => t,
                    Err(msg) => {
                        self.report_type_error(root, t.span, msg);
                        Ty::Error
                    }
                },
                None => Ty::Unit,
            };

            if is_shutdown {
                if !params.iter().all(|p| p.is_self) || ret != Ty::Unit {
                    self.err_at(
                        root,
                        handler.span,
                        Code::E504,
                        "`shutdown` takes only an optional `self` and returns `()`",
                    );
                }
                self.shutdown = Some(def);
            } else {
                let hooks = match self.hooks.get(&handler.name).cloned() {
                    Some(h) => h,
                    None => {
                        self.err_at(
                            root,
                            handler.span,
                            Code::E502,
                            format!(
                                "`{}` does not name a hook in a granted interface",
                                handler.name
                            ),
                        );
                        HookSig {
                            name: handler.name.clone(),
                            params: Vec::new(),
                            ret: Ty::Unit,
                            module: root,
                            span: handler.span,
                            source: self.resolved.module(root).source,
                        }
                    }
                };
                if self.hooks.contains_key(&handler.name) {
                    let hook_module = self.resolved.module(hooks.module);
                    if !self.resolved.grants.iter().any(|g| g == &hook_module.path) {
                        self.err_at(
                            root,
                            handler.span,
                            Code::E502,
                            format!(
                                "hook `{}` belongs to ungranted module `{}`",
                                handler.name, hook_module.path
                            ),
                        );
                    }
                }
                let call_params: Vec<Ty> = params
                    .iter()
                    .filter(|p| !p.is_self)
                    .map(|p| p.ty.clone())
                    .collect();
                if call_params != hooks.params || ret != hooks.ret {
                    self.err_at(
                        root,
                        handler.span,
                        Code::E503,
                        format!(
                            "handler `{}` signature does not match its hook",
                            handler.name
                        ),
                    );
                }
                self.handlers.insert(handler.name.clone(), def);
            }
            if self_seen && state.is_none() {
                // Already reported above; keep the signature for checking.
            }
            self.functions.insert(
                def,
                FnSig {
                    def,
                    node: handler.id,
                    module: root,
                    name: handler.name.clone(),
                    params,
                    ret,
                    generics: Vec::new(),
                    type_params: Vec::new(),
                    kind: FnKind::Handler,
                    span: handler.span,
                    is_pub: false,
                },
            );
        }
    }

    fn is_json(&self, ty: &Ty) -> bool {
        matches!((ty, self.json_type), (Ty::Named(id, args), Some(json)) if *id == json && args.is_empty())
    }

    // ------------------------------------------------------------------
    // Scopes and bindings
    // ------------------------------------------------------------------

    fn alloc_binding(
        &mut self,
        name: &str,
        ty: Ty,
        mutable: bool,
        node: NodeId,
        is_self: bool,
        depth: usize,
    ) -> BindingId {
        let id = BindingId(self.next_binding);
        self.next_binding += 1;
        self.bindings.insert(
            id,
            BindingInfo {
                name: name.to_string(),
                ty,
                mutable,
                declaration: node,
                scope_depth: depth,
                is_self,
            },
        );
        id
    }

    fn lookup_local(&self, name: &str) -> Option<BindingId> {
        for scope in self.scopes.iter().rev() {
            if let Some(id) = scope.get(name) {
                return Some(*id);
            }
        }
        None
    }

    fn note_local_use(&mut self, id: BindingId) {
        let Some(depth) = self.bindings.get(&id).map(|binding| binding.scope_depth) else {
            return;
        };
        for lambda in &mut self.lambda_stack {
            if depth < lambda.boundary {
                lambda.captures.insert(id);
            }
        }
    }

    fn is_visible_name(&self, name: &str) -> bool {
        if self.ctx.as_ref().is_some_and(|c| {
            c.type_param_names.contains_key(name) || (name == "self" && c.self_binding.is_some())
        }) {
            return true;
        }
        if self.lookup_local(name).is_some() {
            return true;
        }
        if let Some(ctx) = &self.ctx {
            if self.resolved.module(ctx.module).symbols.contains(name) {
                return true;
            }
        }
        prelude_names().contains(&name)
    }

    fn bind_existing(&mut self, name: &str, id: BindingId, span: Span) {
        let module = self
            .ctx
            .as_ref()
            .map(|c| c.module)
            .unwrap_or(self.resolved.entry);
        if name != "self" && name != "_" && self.scope_contains_visible(name) {
            // Duplicate pattern names and shadowing are reported by their
            // dedicated rules; avoid double-reporting here.
            let _ = id;
            let _ = span;
            let _ = module;
        }
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), id);
        }
    }

    fn scope_contains_visible(&self, name: &str) -> bool {
        self.scopes.iter().any(|s| s.contains_key(name))
    }

    // ------------------------------------------------------------------
    // Constants
    // ------------------------------------------------------------------

    fn check_consts(&mut self) {
        for i in 0..self.resolved.modules.len() {
            let module_id = ModuleId(i as u32);
            let items = self.resolved.module(module_id).ast.items.clone();
            for item in &items {
                let Item::Const(c) = item else { continue };
                let Some(def) = self.resolved.node_to_def.get(&c.id).copied() else {
                    continue;
                };
                let ty = match self.resolve_ty_in(module_id, &c.ty, &[]) {
                    Ok(t) => t,
                    Err(msg) => {
                        self.report_type_error(module_id, c.ty.span, msg);
                        Ty::Error
                    }
                };
                self.const_types.insert(def, ty);
            }
        }

        let mut defs = Vec::new();
        for i in 0..self.resolved.modules.len() {
            let module_id = ModuleId(i as u32);
            let items = self.resolved.module(module_id).ast.items.clone();
            for item in &items {
                if let Item::Const(c) = item {
                    if let Some(def) = self.resolved.node_to_def.get(&c.id).copied() {
                        defs.push((def, module_id, c.clone()));
                    }
                }
            }
        }

        for (def, module_id, c) in defs {
            let ty = self.const_types.get(&def).cloned().unwrap_or(Ty::Error);
            let saved_ctx = self.ctx.take();
            let saved_scopes = std::mem::take(&mut self.scopes);
            self.ctx = Some(FnCtx {
                module: module_id,
                return_ty: Ty::Unit,
                type_params: Vec::new(),
                type_param_names: HashMap::new(),
                self_ty: None,
                self_binding: None,
                self_mutable: false,
                is_test: false,
                loop_depth: 0,
                closure_boundary: None,
            });
            self.scopes = vec![HashMap::new()];
            let mut infer = Infer::default();
            self.check_expr(&c.value, Some(&ty), &mut infer);
            match self.eval_const(def, &mut HashSet::new()) {
                Ok(value) => {
                    self.consts.insert(
                        def,
                        ConstInfo {
                            def,
                            ty,
                            value,
                            node: c.id,
                        },
                    );
                }
                Err(msg) => {
                    self.err_at(module_id, c.value.span, Code::E406, msg);
                }
            }
            self.ctx = saved_ctx;
            self.scopes = saved_scopes;
        }
    }

    fn eval_const(
        &mut self,
        def: DefId,
        visiting: &mut HashSet<DefId>,
    ) -> Result<ConstValue, String> {
        if let Some(info) = self.consts.get(&def) {
            return Ok(info.value.clone());
        }
        if !visiting.insert(def) {
            return Err("constant cycle".into());
        }
        let info = self.resolved.def(def).clone();
        let value_expr = {
            let module = self.resolved.module(info.module);
            let Some(Item::Const(c)) = module.ast.items.get(info.item_index) else {
                return Err("not a constant".into());
            };
            c.value.clone()
        };
        let value = self.eval_const_expr(&value_expr, visiting)?;
        visiting.remove(&def);
        Ok(value)
    }

    fn eval_const_expr(
        &mut self,
        expr: &Expr,
        visiting: &mut HashSet<DefId>,
    ) -> Result<ConstValue, String> {
        match &expr.kind {
            ExprKind::Int(v) => Ok(ConstValue::Int(*v)),
            ExprKind::Float(v) => Ok(ConstValue::Float(*v)),
            ExprKind::Bool(v) => Ok(ConstValue::Bool(*v)),
            ExprKind::Unit => Ok(ConstValue::Unit),
            ExprKind::Str(parts) => {
                let mut text = String::new();
                for part in parts {
                    match part {
                        StrExprPart::Text(t) => text.push_str(t),
                        StrExprPart::Expr(_) => {
                            return Err("interpolation is not a constant expression".into());
                        }
                    }
                }
                Ok(ConstValue::Str(text))
            }
            ExprKind::Tuple(items) => Ok(ConstValue::Tuple(
                items
                    .iter()
                    .map(|e| self.eval_const_expr(e, visiting))
                    .collect::<Result<_, _>>()?,
            )),
            ExprKind::List(items) => Ok(ConstValue::List(
                items
                    .iter()
                    .map(|e| self.eval_const_expr(e, visiting))
                    .collect::<Result<_, _>>()?,
            )),
            ExprKind::Map(entries) => {
                let mut out = Vec::new();
                for (k, v) in entries {
                    out.push((
                        self.eval_const_expr(k, visiting)?,
                        self.eval_const_expr(v, visiting)?,
                    ));
                }
                Ok(ConstValue::Map(out))
            }
            ExprKind::StructLit { fields, base, .. } => {
                let type_id = match self.resolutions.get(&expr.id) {
                    Some(Resolution::TypeName(id)) => *id,
                    _ => return Err("struct literal is not constant".into()),
                };
                let mut values: Vec<(String, ConstValue)> = Vec::new();
                if let Some(base) = base {
                    match self.eval_const_expr(base, visiting)? {
                        ConstValue::Struct(_, fields) => values = fields,
                        _ => return Err("invalid struct update".into()),
                    }
                }
                for field in fields {
                    let value = field
                        .value
                        .as_ref()
                        .ok_or_else(|| "missing field value".to_string())?;
                    let value = self.eval_const_expr(value, visiting)?;
                    if let Some(existing) = values.iter_mut().find(|(n, _)| n == &field.name) {
                        existing.1 = value;
                    } else {
                        values.push((field.name.clone(), value));
                    }
                }
                Ok(ConstValue::Struct(type_id, values))
            }
            ExprKind::Unary { op, expr } => {
                let value = self.eval_const_expr(expr, visiting)?;
                match (op, value) {
                    (UnaryOp::Neg, ConstValue::Int(v)) => v
                        .checked_neg()
                        .map(ConstValue::Int)
                        .ok_or_else(|| "integer overflow in constant".to_string()),
                    (UnaryOp::Neg, ConstValue::Float(v)) => Ok(ConstValue::Float(-v)),
                    (UnaryOp::Not, ConstValue::Bool(v)) => Ok(ConstValue::Bool(!v)),
                    _ => Err("operator is not constant".into()),
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let a = self.eval_const_expr(lhs, visiting)?;
                let b = self.eval_const_expr(rhs, visiting)?;
                eval_const_binary(*op, a, b)
            }
            ExprKind::Field { .. } | ExprKind::Index { .. } => {
                Err("field and index access are not constant expressions".into())
            }
            ExprKind::Call { callee, args } => {
                let mut values = Vec::new();
                for arg in args {
                    values.push(self.eval_const_expr(&arg.expr, visiting)?);
                }
                match self.resolutions.get(&callee.id) {
                    Some(Resolution::Variant(id, index)) => {
                        Ok(ConstValue::Enum(*id, *index, values))
                    }
                    Some(Resolution::PreludeVariant(v)) => Ok(ConstValue::Prelude(*v, values)),
                    _ => Err("function calls are not constant expressions".into()),
                }
            }
            ExprKind::Var(_) => match self.resolutions.get(&expr.id) {
                Some(Resolution::Const(def)) => self.eval_const(*def, visiting),
                Some(Resolution::PreludeVariant(v)) => Ok(ConstValue::Prelude(*v, Vec::new())),
                _ => Err("name is not a constant".into()),
            },
            _ => Err("not a constant expression".into()),
        }
    }

    // ------------------------------------------------------------------
    // Bodies
    // ------------------------------------------------------------------

    fn check_bodies(&mut self) {
        let mut defs: Vec<DefId> = self.functions.keys().copied().collect();
        defs.sort_by_key(|d| d.0);
        for def in defs {
            let sig = match self.functions.get(&def).cloned() {
                Some(s) => s,
                None => continue,
            };
            if sig.kind == FnKind::Host {
                continue;
            }
            let body = match self.function_block(def) {
                Some(b) => b,
                None => continue,
            };
            self.check_function_body(&sig, &body);
        }
    }

    fn function_block(&self, def: DefId) -> Option<Block> {
        let info = self.resolved.def(def);
        let module = self.resolved.module(info.module);
        let item = module.ast.items.get(info.item_index)?;
        match item {
            Item::Fn(f) if f.id == info.node => f.body.clone(),
            Item::On(o) if o.id == info.node => Some(o.body.clone()),
            Item::Test(t) if t.id == info.node => Some(t.body.clone()),
            Item::Impl(i) => i
                .methods
                .iter()
                .find(|m| m.id == info.node)
                .and_then(|m| m.body.clone()),
            _ => None,
        }
    }

    fn check_function_body(&mut self, sig: &FnSig, body: &Block) {
        let saved_ctx = self.ctx.take();
        let saved_scopes = std::mem::take(&mut self.scopes);
        let mut names = HashMap::new();
        for (i, p) in sig.type_params.iter().enumerate() {
            names.insert(p.name.clone(), i);
        }
        let self_param = sig.self_param().cloned();
        self.ctx = Some(FnCtx {
            module: sig.module,
            return_ty: sig.ret.clone(),
            type_params: sig.type_params.clone(),
            type_param_names: names,
            self_ty: self_param.as_ref().map(|p| p.ty.clone()),
            self_binding: self_param.as_ref().map(|p| p.binding),
            self_mutable: self_param.as_ref().map(|p| p.mutable).unwrap_or(false),
            is_test: sig.kind == FnKind::Test,
            loop_depth: 0,
            closure_boundary: None,
        });
        self.scopes = vec![HashMap::new()];
        for param in &sig.params {
            if param.is_self {
                if let Some(scope) = self.scopes.last_mut() {
                    scope.insert("self".into(), param.binding);
                }
            } else {
                if let Some(scope) = self.scopes.last_mut() {
                    scope.insert(param.name.clone(), param.binding);
                }
                self.check_param_shadow(sig.module, param);
            }
        }
        let mut infer = Infer::default();
        let ty = self.check_block(body, Some(&sig.ret), &mut infer);
        if !infer.matches(&ty, &sig.ret) {
            self.err_at(
                sig.module,
                body.span,
                Code::E201,
                format!(
                    "function body has type {}, expected {}",
                    ty.display(&self.types),
                    sig.ret.display(&self.types)
                ),
            );
        }
        self.ctx = saved_ctx;
        self.scopes = saved_scopes;
    }

    fn check_param_shadow(&mut self, module: ModuleId, param: &ParamSig) {
        if self.is_top_level_visible(param.name.as_str()) {
            self.err_at(
                module,
                param.span,
                Code::E103,
                format!("parameter `{}` shadows a visible name", param.name),
            );
        }
    }

    fn is_top_level_visible(&self, name: &str) -> bool {
        let module = self
            .ctx
            .as_ref()
            .map(|c| c.module)
            .unwrap_or(self.resolved.entry);
        self.resolved.module(module).symbols.contains(name)
            || prelude_names().contains(&name)
            || self
                .ctx
                .as_ref()
                .is_some_and(|c| c.type_param_names.contains_key(name))
    }
}

fn prelude_names() -> &'static [&'static str] {
    &[
        "Option", "Result", "Some", "None", "Ok", "Err", "print", "panic", "assert", "int",
        "float", "bool", "str", "list", "map", "range", "Eq", "Ord", "Show",
    ]
}

fn is_show(ty: &Ty, types: &TypeTable) -> bool {
    is_data_type(ty, types)
}

pub(crate) fn is_data_type(ty: &Ty, types: &TypeTable) -> bool {
    match ty {
        Ty::Fn(_, _) => false,
        Ty::Tuple(items) => items.iter().all(|t| is_data_type(t, types)),
        Ty::List(t) | Ty::Option(t) => is_data_type(t, types),
        Ty::Map(k, v) | Ty::Result(k, v) => is_data_type(k, types) && is_data_type(v, types),
        Ty::Named(id, args) => {
            let Some(def) = types.get(*id) else {
                return true;
            };
            match &def.kind {
                TypeDefKind::Opaque => true,
                TypeDefKind::Struct { fields } => fields
                    .iter()
                    .all(|f| is_data_type(&instantiate_params(&f.ty, args), types)),
                TypeDefKind::Enum { variants } => variants.iter().all(|v| {
                    v.payload
                        .iter()
                        .all(|t| is_data_type(&instantiate_params(t, args), types))
                }),
            }
        }
        Ty::Error | Ty::Param(_) | Ty::Var(_) => true,
        _ => true,
    }
}

fn eval_const_binary(op: BinaryOp, a: ConstValue, b: ConstValue) -> Result<ConstValue, String> {
    use ConstValue::*;
    let overflow = || "integer overflow in constant".to_string();
    match op {
        BinaryOp::Add => match (a, b) {
            (Int(a), Int(b)) => a.checked_add(b).map(Int).ok_or_else(overflow),
            (Float(a), Float(b)) => Ok(Float(a + b)),
            (Str(a), Str(b)) => Ok(Str(a + &b)),
            _ => Err("invalid operands for + in constant".into()),
        },
        BinaryOp::Sub => match (a, b) {
            (Int(a), Int(b)) => a.checked_sub(b).map(Int).ok_or_else(overflow),
            (Float(a), Float(b)) => Ok(Float(a - b)),
            _ => Err("invalid operands for - in constant".into()),
        },
        BinaryOp::Mul => match (a, b) {
            (Int(a), Int(b)) => a.checked_mul(b).map(Int).ok_or_else(overflow),
            (Float(a), Float(b)) => Ok(Float(a * b)),
            _ => Err("invalid operands for * in constant".into()),
        },
        BinaryOp::Div => match (a, b) {
            (Int(_), Int(0)) => Err("division by zero in constant".into()),
            (Int(a), Int(b)) => a.checked_div(b).map(Int).ok_or_else(overflow),
            (Float(a), Float(b)) => Ok(Float(a / b)),
            _ => Err("invalid operands for / in constant".into()),
        },
        BinaryOp::Rem => match (a, b) {
            (Int(_), Int(0)) => Err("division by zero in constant".into()),
            (Int(a), Int(b)) => a.checked_rem(b).map(Int).ok_or_else(overflow),
            _ => Err("invalid operands for % in constant".into()),
        },
        BinaryOp::Eq => Ok(Bool(a == b)),
        BinaryOp::Ne => Ok(Bool(a != b)),
        BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
            let ord = match (&a, &b) {
                (Int(a), Int(b)) => a.partial_cmp(b),
                (Float(a), Float(b)) => a.partial_cmp(b),
                (Str(a), Str(b)) => a.partial_cmp(b),
                (Bool(a), Bool(b)) => a.partial_cmp(b),
                _ => None,
            }
            .ok_or_else(|| "operands are not orderable in constant".to_string())?;
            Ok(Bool(match op {
                BinaryOp::Lt => ord.is_lt(),
                BinaryOp::Le => ord.is_le(),
                BinaryOp::Gt => ord.is_gt(),
                BinaryOp::Ge => ord.is_ge(),
                _ => unreachable!(),
            }))
        }
        BinaryOp::And => match (a, b) {
            (Bool(a), Bool(b)) => Ok(Bool(a && b)),
            _ => Err("invalid operands for and in constant".into()),
        },
        BinaryOp::Or => match (a, b) {
            (Bool(a), Bool(b)) => Ok(Bool(a || b)),
            _ => Err("invalid operands for or in constant".into()),
        },
        BinaryOp::Coalesce => match a {
            Prelude(PreludeVariant::Some, values) | Prelude(PreludeVariant::Ok, values) => {
                Ok(values.into_iter().next().unwrap_or(Unit))
            }
            Prelude(PreludeVariant::None, _) | Prelude(PreludeVariant::Err, _) => Ok(b),
            _ => Err("invalid operands for ?? in constant".into()),
        },
    }
}

// The rest of the checker (expressions and patterns) lives in the next impl
// block to keep this file navigable.
include!("checker_exprs.rs");
include!("checker_patterns.rs");
