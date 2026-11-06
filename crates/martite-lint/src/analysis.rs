//! Shared immutable analysis over a checked program.
//!
//! Every rule reads from this structure instead of walking the AST repeatedly.
//! The analysis records binding reads/writes, mutation facts, call edges, and
//! the module-local uses that the unused-import and reachability rules need.

use std::collections::{HashMap, HashSet};

use martite_base::{SourceId, SourceMap, Span};
use martite_check::{
    BindingId, BuiltinItem, BuiltinReceiver, CheckedProgram, DefId, DefKind, FnSig, ModuleId,
    ModuleTarget, ParamSig, Resolution, Ty,
};
use martite_syntax::{
    Arg, Block, Expr, ExprKind, Item, MatchBody, PatKind, Pattern, Program, Stmt, StmtKind, TyExpr,
    TyKind,
};

#[derive(Clone, Copy)]
pub struct ModuleRef<'a> {
    pub id: ModuleId,
    pub source: SourceId,
    pub text: &'a str,
    pub ast: &'a Program,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclKind {
    Var,
    Let,
    Param,
    SelfParam,
    For,
    Match,
    Lambda,
}

#[derive(Debug, Clone)]
pub struct Decl {
    pub kind: DeclKind,
    pub name: String,
    pub span: Span,
    pub source: SourceId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    ReadWrite,
}

#[derive(Debug, Clone, Copy)]
pub struct UseSite {
    pub source: SourceId,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct CallSite {
    pub caller: Option<DefId>,
    pub callee: DefId,
    pub source: SourceId,
    /// Argument spans in call order, with their `mut` marker.
    pub args: Vec<(Span, bool)>,
}

#[derive(Debug, Clone)]
pub struct FunctionBody<'a> {
    pub def: DefId,
    pub module: ModuleId,
    pub block: &'a Block,
    pub sig: &'a FnSig,
}

pub struct Analysis<'a> {
    pub program: &'a CheckedProgram,
    pub modules: Vec<ModuleRef<'a>>,
    pub reads: HashMap<BindingId, Vec<UseSite>>,
    pub writes: HashMap<BindingId, Vec<UseSite>>,
    pub mutated: HashSet<BindingId>,
    pub decls: HashMap<BindingId, Decl>,
    pub calls: Vec<CallSite>,
    pub function_value_uses: HashSet<DefId>,
    pub def_uses: HashMap<ModuleId, HashSet<DefId>>,
    pub module_uses: HashMap<ModuleId, Vec<ModuleTarget>>,
    pub module_name_uses: HashMap<ModuleId, HashSet<String>>,
    pub builtin_uses: HashMap<ModuleId, Vec<BuiltinItem>>,
    pub type_names: HashMap<ModuleId, HashSet<String>>,
    pub shorthand_reads: HashMap<ModuleId, HashSet<String>>,
    pub shorthand_sites: HashMap<ModuleId, Vec<(String, Span)>>,
    pub bodies: Vec<FunctionBody<'a>>,
    pub reachable: HashSet<DefId>,
    pub roots: HashSet<DefId>,
}

#[derive(Clone, Copy)]
struct Walk<'a> {
    module: ModuleRef<'a>,
    owner: Option<DefId>,
    loop_depth: u32,
    access: Option<Access>,
}

impl<'a> Walk<'a> {
    fn new(module: ModuleRef<'a>) -> Self {
        Self {
            module,
            owner: None,
            loop_depth: 0,
            access: None,
        }
    }

    fn with_owner(self, owner: DefId) -> Self {
        Self {
            owner: Some(owner),
            ..self
        }
    }

    fn with_access(self, access: Access) -> Self {
        Self {
            access: Some(access),
            ..self
        }
    }

    fn read(self) -> Self {
        self.with_access(Access::Read)
    }

    fn in_loop(self) -> Self {
        Self {
            loop_depth: self.loop_depth + 1,
            ..self
        }
    }
}

impl<'a> Analysis<'a> {
    pub fn new(program: &'a CheckedProgram, sources: &'a SourceMap) -> Self {
        let modules: Vec<ModuleRef<'a>> = program
            .resolved
            .modules
            .iter()
            .filter_map(|module| {
                let file = sources.get(module.source)?;
                Some(ModuleRef {
                    id: module.id,
                    source: module.source,
                    text: file.text(),
                    ast: &module.ast,
                })
            })
            .collect();
        let mut analysis = Self {
            program,
            modules,
            reads: HashMap::new(),
            writes: HashMap::new(),
            mutated: HashSet::new(),
            decls: HashMap::new(),
            calls: Vec::new(),
            function_value_uses: HashSet::new(),
            def_uses: HashMap::new(),
            module_uses: HashMap::new(),
            module_name_uses: HashMap::new(),
            builtin_uses: HashMap::new(),
            type_names: HashMap::new(),
            shorthand_reads: HashMap::new(),
            shorthand_sites: HashMap::new(),
            bodies: Vec::new(),
            reachable: HashSet::new(),
            roots: HashSet::new(),
        };
        analysis.collect_signatures();
        analysis.collect_items();
        analysis.collect_bodies();
        analysis.collect_reachability();
        analysis
    }

    pub fn module(&self, id: ModuleId) -> Option<ModuleRef<'a>> {
        self.modules.iter().find(|module| module.id == id).copied()
    }

    pub fn source_text(&self, source: SourceId) -> Option<&'a str> {
        self.modules
            .iter()
            .find(|module| module.source == source)
            .map(|module| module.text)
    }

    pub fn is_mutated(&self, binding: BindingId) -> bool {
        self.mutated.contains(&binding)
    }

    pub fn reads_of(&self, binding: BindingId) -> &[UseSite] {
        self.reads
            .get(&binding)
            .map(|sites| sites.as_slice())
            .unwrap_or(&[])
    }

    pub fn writes_of(&self, binding: BindingId) -> &[UseSite] {
        self.writes
            .get(&binding)
            .map(|sites| sites.as_slice())
            .unwrap_or(&[])
    }

    pub fn body(&self, def: DefId) -> Option<&FunctionBody<'a>> {
        self.bodies.iter().find(|body| body.def == def)
    }

    pub fn calls_from(&self, def: DefId) -> impl Iterator<Item = &CallSite> {
        self.calls
            .iter()
            .filter(move |call| call.caller == Some(def))
    }

    pub fn is_reachable(&self, def: DefId) -> bool {
        self.reachable.contains(&def)
    }

    pub fn type_names_in(&self, module: ModuleId) -> impl Iterator<Item = &String> {
        self.type_names.get(&module).into_iter().flatten()
    }

    fn collect_signatures(&mut self) {
        for sig in self.program.functions.values() {
            let Some(module) = self.module(sig.module) else {
                continue;
            };
            for param in &sig.params {
                self.record_param(module, sig, param);
            }
        }
    }

    fn record_param(&mut self, module: ModuleRef<'a>, _sig: &FnSig, param: &ParamSig) {
        let kind = if param.is_self {
            DeclKind::SelfParam
        } else {
            DeclKind::Param
        };
        self.decls.insert(
            param.binding,
            Decl {
                kind,
                name: param.name.clone(),
                span: param.span,
                source: module.source,
            },
        );
    }

    fn collect_items(&mut self) {
        for index in 0..self.modules.len() {
            let module = self.modules[index];
            for item in &module.ast.items {
                self.collect_item(module, item);
            }
        }
    }

    fn collect_item(&mut self, module: ModuleRef<'a>, item: &'a Item) {
        match item {
            Item::Use(_) => {}
            Item::Const(constant) => {
                self.collect_ty_names(module.id, &constant.ty);
                let mut walk = Walk::new(module);
                walk = walk.read();
                self.walk_expr(&constant.value, walk);
            }
            Item::Struct(decl) => {
                for field in &decl.fields {
                    self.collect_ty_names(module.id, &field.ty);
                }
            }
            Item::Enum(decl) => {
                for variant in &decl.variants {
                    for ty in &variant.payload {
                        self.collect_ty_names(module.id, ty);
                    }
                }
            }
            Item::Fn(decl) => {
                for param in &decl.params {
                    if let martite_syntax::ParamKind::Normal { ty, .. } = &param.kind {
                        self.collect_ty_names(module.id, ty);
                    }
                }
                if let Some(ret) = &decl.ret {
                    self.collect_ty_names(module.id, ret);
                }
            }
            Item::On(decl) => {
                for param in &decl.params {
                    if let martite_syntax::ParamKind::Normal { ty, .. } = &param.kind {
                        self.collect_ty_names(module.id, ty);
                    }
                }
                if let Some(ret) = &decl.ret {
                    self.collect_ty_names(module.id, ret);
                }
            }
            Item::Test(_) => {}
            Item::Impl(decl) => {
                self.type_names
                    .entry(module.id)
                    .or_default()
                    .insert(decl.type_name.clone());
                for ty in &decl.type_args {
                    self.collect_ty_names(module.id, ty);
                }
                for method in &decl.methods {
                    for param in &method.params {
                        if let martite_syntax::ParamKind::Normal { ty, .. } = &param.kind {
                            self.collect_ty_names(module.id, ty);
                        }
                    }
                    if let Some(ret) = &method.ret {
                        self.collect_ty_names(module.id, ret);
                    }
                }
            }
        }
    }

    fn collect_bodies(&mut self) {
        let mut defs: Vec<DefId> = self
            .program
            .functions
            .keys()
            .copied()
            .filter(|def| self.program.function_block(*def).is_some())
            .collect();
        defs.sort();
        for def in defs {
            let Some(sig) = self.program.fn_sig(def) else {
                continue;
            };
            let Some(module) = self.module(sig.module) else {
                continue;
            };
            let Some(block) = self.program.function_block(def) else {
                continue;
            };
            self.bodies.push(FunctionBody {
                def,
                module: module.id,
                block,
                sig,
            });
        }
        for index in 0..self.bodies.len() {
            let body = &self.bodies[index];
            let walk = Walk::new(
                self.module(body.module)
                    .expect("body module must be present"),
            )
            .with_owner(body.def);
            self.walk_block(body.block, walk);
        }
    }

    fn collect_reachability(&mut self) {
        for def in &self.program.resolved.defs {
            let is_root = def.is_pub
                || def.kind == DefKind::Handler
                || def.kind == DefKind::Test
                || self.program.main == Some(def.id);
            if is_root {
                self.roots.insert(def.id);
            }
        }
        for (_, def) in &self.program.handlers {
            self.roots.insert(*def);
        }
        self.roots.extend(self.program.tests.iter().copied());
        if let Some(main) = self.program.main {
            self.roots.insert(main);
        }
        let mut stack: Vec<DefId> = self.roots.iter().copied().collect();
        while let Some(def) = stack.pop() {
            if !self.reachable.insert(def) {
                continue;
            }
            for call in self.calls.iter().filter(|call| call.caller == Some(def)) {
                if !self.reachable.contains(&call.callee) {
                    stack.push(call.callee);
                }
            }
        }
        // A function value is callable once its containing function is
        // reachable. The edge is conservative, as the specification requires.
        for call in &self.calls {
            if call.caller.is_some() && !self.reachable.contains(&call.callee) {
                if let Some(owner) = call.caller {
                    if self.reachable.contains(&owner) {
                        self.reachable.insert(call.callee);
                    }
                }
            }
        }
    }

    // ----- declarations -------------------------------------------------

    fn declare_pattern(
        &mut self,
        pattern: &Pattern,
        kind: DeclKind,
        mutable: bool,
        walk: Walk<'a>,
    ) {
        let _ = mutable;
        self.collect_pattern_type_names(walk.module.id, pattern);
        match &pattern.kind {
            PatKind::Bind(_) => {
                if let Some(binding) = self.program.pattern_bindings.get(&pattern.id) {
                    self.decls.entry(*binding).or_insert_with(|| Decl {
                        kind,
                        name: self
                            .program
                            .bindings
                            .get(binding)
                            .map(|info| info.name.clone())
                            .unwrap_or_default(),
                        span: pattern.span,
                        source: walk.module.source,
                    });
                }
            }
            PatKind::Tuple(items) | PatKind::Or(items) => {
                for item in items {
                    self.declare_pattern(item, kind, mutable, walk);
                }
            }
            PatKind::Variant { args, .. } => {
                for item in args {
                    self.declare_pattern(item, kind, mutable, walk);
                }
            }
            PatKind::Struct { fields, .. } => {
                for field in fields {
                    self.declare_pattern(&field.pattern, kind, mutable, walk);
                }
            }
            _ => {}
        }
    }

    fn collect_pattern_type_names(&mut self, module: ModuleId, pattern: &Pattern) {
        match &pattern.kind {
            PatKind::Struct { name, fields, .. } => {
                self.type_names
                    .entry(module)
                    .or_default()
                    .insert(name.clone());
                for field in fields {
                    self.collect_pattern_type_names(module, &field.pattern);
                }
            }
            PatKind::Variant {
                qualifier, args, ..
            } => {
                if let Some(name) = qualifier {
                    self.type_names
                        .entry(module)
                        .or_default()
                        .insert(name.clone());
                }
                if let Some(martite_check::PatResolution::Variant(type_id, _)) =
                    self.program.pattern_resolutions.get(&pattern.id)
                {
                    if let Some(def) = self.program.types.get(*type_id) {
                        self.type_names
                            .entry(module)
                            .or_default()
                            .insert(def.name.clone());
                    }
                }
                for arg in args {
                    self.collect_pattern_type_names(module, arg);
                }
            }
            PatKind::Tuple(items) | PatKind::Or(items) => {
                for item in items {
                    self.collect_pattern_type_names(module, item);
                }
            }
            _ => {}
        }
    }

    fn collect_ty_names(&mut self, module: ModuleId, ty: &TyExpr) {
        match &ty.kind {
            TyKind::Named { name, args } => {
                self.type_names
                    .entry(module)
                    .or_default()
                    .insert(name.clone());
                for arg in args {
                    self.collect_ty_names(module, arg);
                }
            }
            TyKind::Optional(inner) => self.collect_ty_names(module, inner),
            TyKind::Tuple(items) => {
                for item in items {
                    self.collect_ty_names(module, item);
                }
            }
            TyKind::Fn { params, ret } => {
                for param in params {
                    self.collect_ty_names(module, param);
                }
                if let Some(ret) = ret {
                    self.collect_ty_names(module, ret);
                }
            }
            TyKind::Prim(_) | TyKind::Unit => {}
        }
    }

    // ----- walking ------------------------------------------------------

    fn walk_block(&mut self, block: &Block, walk: Walk<'a>) {
        for stmt in &block.stmts {
            self.walk_stmt(stmt, walk);
        }
    }

    fn walk_stmt(&mut self, stmt: &Stmt, walk: Walk<'a>) {
        match &stmt.kind {
            StmtKind::Let {
                mutable,
                pattern,
                ty,
                value,
            } => {
                if let Some(ty) = ty {
                    self.collect_ty_names(walk.module.id, ty);
                }
                self.declare_pattern(
                    pattern,
                    if *mutable {
                        DeclKind::Var
                    } else {
                        DeclKind::Let
                    },
                    *mutable,
                    walk,
                );
                self.walk_expr(value, walk.read());
            }
            StmtKind::While { cond, body } => {
                self.walk_expr(cond, walk.read());
                self.walk_block(body, walk.in_loop());
            }
            StmtKind::For {
                pattern,
                iter,
                body,
            } => {
                self.declare_pattern(pattern, DeclKind::For, false, walk);
                self.walk_expr(iter, walk.read());
                self.walk_block(body, walk.in_loop());
            }
            StmtKind::Return(value) => {
                if let Some(value) = value {
                    self.walk_expr(value, walk.read());
                }
            }
            StmtKind::Break | StmtKind::Continue => {}
            StmtKind::Expr { expr, assign } => {
                if let Some((op, rhs)) = assign {
                    self.walk_expr(
                        expr,
                        walk.with_access(if *op == martite_syntax::AssignOp::Assign {
                            Access::Write
                        } else {
                            Access::ReadWrite
                        }),
                    );
                    self.walk_expr(rhs, walk.read());
                } else {
                    self.walk_expr(expr, walk.read());
                }
            }
        }
    }

    fn walk_expr(&mut self, expr: &Expr, walk: Walk<'a>) {
        match &expr.kind {
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Unit => {}
            ExprKind::Str(parts) => {
                for part in parts {
                    if let martite_syntax::StrExprPart::Expr(expr) = part {
                        self.walk_expr(expr, walk.read());
                    }
                }
            }
            ExprKind::Var(_) => self.resolve_var(expr, walk),
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                for item in items {
                    self.walk_expr(item, walk.read());
                }
            }
            ExprKind::Map(entries) => {
                for (key, value) in entries {
                    self.walk_expr(key, walk.read());
                    self.walk_expr(value, walk.read());
                }
            }
            ExprKind::StructLit { name, fields, base } => {
                self.type_names
                    .entry(walk.module.id)
                    .or_default()
                    .insert(name.clone());
                for field in fields {
                    if let Some(value) = &field.value {
                        self.walk_expr(value, walk.read());
                    } else {
                        self.shorthand_reads
                            .entry(walk.module.id)
                            .or_default()
                            .insert(field.name.clone());
                        self.shorthand_sites
                            .entry(walk.module.id)
                            .or_default()
                            .push((field.name.clone(), field.span));
                    }
                }
                if let Some(base) = base {
                    self.walk_expr(base, walk.read());
                }
            }
            ExprKind::Unary { expr, .. } => self.walk_expr(expr, walk.read()),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.walk_expr(lhs, walk.read());
                self.walk_expr(rhs, walk.read());
            }
            ExprKind::Range { start, end, .. } => {
                self.walk_expr(start, walk.read());
                self.walk_expr(end, walk.read());
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.walk_expr(cond, walk.read());
                self.walk_block(then_branch, walk);
                if let Some(else_branch) = else_branch {
                    self.walk_expr(else_branch, walk.read());
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee, walk.read());
                for arm in arms {
                    self.declare_pattern(&arm.pattern, DeclKind::Match, false, walk);
                    if let Some(guard) = &arm.guard {
                        self.walk_expr(guard, walk.read());
                    }
                    match &arm.body {
                        MatchBody::Block(block) => self.walk_block(block, walk),
                        MatchBody::Expr(expr) => self.walk_expr(expr, walk.read()),
                    }
                }
            }
            ExprKind::Lambda { params, body, .. } => {
                for param in params {
                    if let Some(ty) = &param.ty {
                        self.collect_ty_names(walk.module.id, ty);
                    }
                    self.declare_pattern(&param.pattern, DeclKind::Lambda, false, walk);
                }
                match body {
                    martite_syntax::LambdaBody::Block(block) => self.walk_block(block, walk),
                    martite_syntax::LambdaBody::Expr(expr) => self.walk_expr(expr, walk.read()),
                }
            }
            ExprKind::Block(block) => self.walk_block(block, walk),
            ExprKind::Field { base, .. } => {
                if let ExprKind::Var(name) = &base.kind {
                    if !matches!(self.program.resolution(base.id), Some(Resolution::Local(_))) {
                        self.module_name_uses
                            .entry(walk.module.id)
                            .or_default()
                            .insert(name.clone());
                    }
                }
                if let Some(Resolution::ModuleItem(def)) = self.program.resolution(expr.id) {
                    self.def_uses
                        .entry(walk.module.id)
                        .or_default()
                        .insert(*def);
                }
                if let Some(Resolution::Method(def)) = self.program.resolution(expr.id) {
                    self.def_uses
                        .entry(walk.module.id)
                        .or_default()
                        .insert(*def);
                }
                if let Some(Resolution::Variant(type_id, _)) = self.program.resolution(expr.id) {
                    if let Some(def) = self.program.types.get(*type_id) {
                        self.type_names
                            .entry(walk.module.id)
                            .or_default()
                            .insert(def.name.clone());
                    }
                }
                self.walk_expr(base, walk)
            }
            ExprKind::Call { callee, args } => self.walk_call(callee, args, walk),
            ExprKind::Index { base, index } => {
                self.walk_expr(base, walk);
                self.walk_expr(index, walk.read());
            }
            ExprKind::Try(inner) => self.walk_expr(inner, walk.read()),
            ExprKind::Convert { expr, .. } => self.walk_expr(expr, walk.read()),
        }
    }

    fn walk_call(&mut self, callee: &Expr, args: &[Arg], walk: Walk<'a>) {
        let resolution = self.program.resolution(callee.id).cloned();
        match (&callee.kind, resolution) {
            (ExprKind::Var(_), Some(Resolution::Fn(def)))
            | (ExprKind::Var(_), Some(Resolution::Handler(def)))
            | (ExprKind::Var(_), Some(Resolution::Test(def))) => {
                self.record_call(walk, def, args);
                for arg in args {
                    self.walk_arg(arg, walk);
                }
            }
            (ExprKind::Field { base, .. }, Some(Resolution::Method(def))) => {
                let mutable = self
                    .program
                    .fn_sig(def)
                    .map(|sig| sig.self_mutable())
                    .unwrap_or(false);
                self.record_call(walk, def, args);
                self.walk_expr(
                    base,
                    if mutable {
                        walk.with_access(Access::Write)
                    } else {
                        walk.read()
                    },
                );
                for arg in args {
                    self.walk_arg(arg, walk);
                }
            }
            (ExprKind::Field { .. }, Some(Resolution::ModuleItem(def)))
                if self.program.fn_sig(def).is_some() =>
            {
                self.record_call(walk, def, args);
                self.walk_expr(callee, walk.read());
                for arg in args {
                    self.walk_arg(arg, walk);
                }
            }
            (ExprKind::Field { base, .. }, Some(Resolution::BuiltinMethod { receiver, name }))
                if builtin_mutates(receiver, name) =>
            {
                self.walk_expr(base, walk.with_access(Access::Write));
                for arg in args {
                    self.walk_arg(arg, walk);
                }
            }
            _ => {
                self.walk_expr(callee, walk.read());
                for arg in args {
                    self.walk_arg(arg, walk);
                }
            }
        }
    }

    fn walk_arg(&mut self, arg: &Arg, walk: Walk<'a>) {
        if arg.mutable {
            self.walk_expr(&arg.expr, walk.with_access(Access::Write));
        } else {
            self.walk_expr(&arg.expr, walk.read());
        }
    }

    fn record_call(&mut self, walk: Walk<'a>, callee: DefId, args: &[Arg]) {
        self.def_uses
            .entry(walk.module.id)
            .or_default()
            .insert(callee);
        self.calls.push(CallSite {
            caller: walk.owner,
            callee,
            source: walk.module.source,
            args: args.iter().map(|arg| (arg.span, arg.mutable)).collect(),
        });
    }

    fn resolve_var(&mut self, expr: &Expr, walk: Walk<'a>) {
        let Some(resolution) = self.program.resolution(expr.id) else {
            // Type names and other non-value identifiers.
            return;
        };
        match resolution {
            Resolution::Local(binding) => {
                let site = UseSite {
                    source: walk.module.source,
                    span: expr.span,
                };
                match walk.access.unwrap_or(Access::Read) {
                    Access::Read => {
                        self.reads.entry(*binding).or_default().push(site);
                    }
                    Access::Write => {
                        self.writes.entry(*binding).or_default().push(site);
                        self.mutated.insert(*binding);
                    }
                    Access::ReadWrite => {
                        self.reads.entry(*binding).or_default().push(site);
                        self.writes.entry(*binding).or_default().push(site);
                        self.mutated.insert(*binding);
                    }
                }
            }
            Resolution::Fn(def)
            | Resolution::Handler(def)
            | Resolution::Test(def)
            | Resolution::Const(def)
            | Resolution::ModuleItem(def) => {
                self.def_uses
                    .entry(walk.module.id)
                    .or_default()
                    .insert(*def);
                if let Some(owner) = walk.owner {
                    if self.program.fn_sig(*def).is_some() {
                        self.function_value_uses.insert(*def);
                        if owner != *def {
                            self.calls.push(CallSite {
                                caller: Some(owner),
                                callee: *def,
                                source: walk.module.source,
                                args: Vec::new(),
                            });
                        }
                    }
                }
            }
            Resolution::Module(target) => {
                let uses = self.module_uses.entry(walk.module.id).or_default();
                if !uses.contains(target) {
                    uses.push(*target);
                }
            }
            Resolution::BuiltinItem(item) => {
                let uses = self.builtin_uses.entry(walk.module.id).or_default();
                if !uses.contains(item) {
                    uses.push(*item);
                }
            }
            _ => {}
        }
    }
}

/// Whether an expression is a side-effect-free place rooted at a binding.
pub fn place_root(program: &CheckedProgram, expr: &Expr) -> Option<BindingId> {
    match &expr.kind {
        ExprKind::Var(_) => match program.resolution(expr.id) {
            Some(Resolution::Local(binding)) => Some(*binding),
            _ => None,
        },
        ExprKind::Field { base, .. } => place_root(program, base),
        ExprKind::Index { base, index } => {
            if side_effect_free(program, index) {
                place_root(program, base)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Side-effect-free expression as defined in section 12.1.
pub fn side_effect_free(program: &CheckedProgram, expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Unit => true,
        ExprKind::Str(parts) => parts
            .iter()
            .all(|part| matches!(part, martite_syntax::StrExprPart::Text(_))),
        ExprKind::Var(_) => {
            place_root(program, expr).is_some()
                || matches!(program.resolution(expr.id), Some(Resolution::Const(_)))
        }
        ExprKind::Tuple(items) | ExprKind::List(items) => {
            items.iter().all(|item| side_effect_free(program, item))
        }
        ExprKind::Map(entries) => entries
            .iter()
            .all(|(key, value)| side_effect_free(program, key) && side_effect_free(program, value)),
        ExprKind::Unary {
            op: martite_syntax::UnaryOp::Neg,
            expr: inner,
        } => matches!(inner.kind, ExprKind::Int(_) | ExprKind::Float(_)),
        ExprKind::Field { base, .. } => side_effect_free(program, base),
        ExprKind::Convert { to, expr } => {
            *to == martite_syntax::PrimTy::Float
                && matches!(program.type_of(expr.id), Some(Ty::Int))
                && side_effect_free(program, expr)
        }
        ExprKind::Index { .. } => place_root(program, expr).is_some(),
        _ => false,
    }
}

/// A stable expression: side-effect-free or a chain of non-panicking builtin
/// methods that take no function arguments.
pub fn stable(program: &CheckedProgram, expr: &Expr) -> bool {
    if matches!(expr.kind, ExprKind::Index { .. }) {
        return false;
    }
    if side_effect_free(program, expr) {
        return true;
    }
    match &expr.kind {
        ExprKind::Field { base, .. } => match program.resolution(expr.id) {
            Some(Resolution::BuiltinMethod { name, .. }) => {
                builtin_is_stable(*name) && stable(program, base)
            }
            Some(Resolution::Method(_)) => false,
            _ => stable(program, base),
        },
        ExprKind::Call { callee, args } => {
            let Some(Resolution::BuiltinMethod { name, .. }) = program.resolution(callee.id) else {
                return false;
            };
            if !builtin_is_stable(*name)
                || !args.iter().all(|arg| side_effect_free(program, &arg.expr))
            {
                return false;
            }
            if let ExprKind::Field { base, .. } = &callee.kind {
                stable(program, base)
            } else {
                false
            }
        }
        _ => false,
    }
}

fn builtin_is_stable(name: &str) -> bool {
    !matches!(
        name,
        "expect"
            | "abs"
            | "pow"
            | "round"
            | "replace"
            | "push"
            | "pop"
            | "sort"
            | "insert"
            | "remove_at"
            | "extend"
            | "clear"
            | "remove"
    )
}

/// Structural equality for places and simple expressions, ignoring NodeIds.
pub fn same_place(program: &CheckedProgram, left: &Expr, right: &Expr) -> bool {
    match (&left.kind, &right.kind) {
        (ExprKind::Var(a), ExprKind::Var(b)) => {
            match (program.resolution(left.id), program.resolution(right.id)) {
                (Some(Resolution::Local(a_id)), Some(Resolution::Local(b_id))) => a_id == b_id,
                _ => a == b,
            }
        }
        (
            ExprKind::Field {
                base: a_base,
                name: a_name,
            },
            ExprKind::Field {
                base: b_base,
                name: b_name,
            },
        ) => a_name == b_name && same_place(program, a_base, b_base),
        (
            ExprKind::Index {
                base: a_base,
                index: a_index,
            },
            ExprKind::Index {
                base: b_base,
                index: b_index,
            },
        ) => {
            same_place(program, a_base, b_base)
                && side_effect_free(program, a_index)
                && side_effect_free(program, b_index)
                && same_expr(a_index, b_index)
        }
        _ => false,
    }
}

pub fn same_expr(left: &Expr, right: &Expr) -> bool {
    match (&left.kind, &right.kind) {
        (ExprKind::Int(a), ExprKind::Int(b)) => a == b,
        (ExprKind::Float(a), ExprKind::Float(b)) => a.to_bits() == b.to_bits(),
        (ExprKind::Bool(a), ExprKind::Bool(b)) => a == b,
        (ExprKind::Unit, ExprKind::Unit) => true,
        (ExprKind::Var(a), ExprKind::Var(b)) => a == b,
        (
            ExprKind::Field {
                base: a_base,
                name: a_name,
            },
            ExprKind::Field {
                base: b_base,
                name: b_name,
            },
        ) => a_name == b_name && same_expr(a_base, b_base),
        (
            ExprKind::Index {
                base: a_base,
                index: a_index,
            },
            ExprKind::Index {
                base: b_base,
                index: b_index,
            },
        ) => same_expr(a_base, b_base) && same_expr(a_index, b_index),
        (ExprKind::Tuple(a), ExprKind::Tuple(b)) | (ExprKind::List(a), ExprKind::List(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_expr(a, b))
        }
        (
            ExprKind::Unary {
                op: a_op,
                expr: a_expr,
            },
            ExprKind::Unary {
                op: b_op,
                expr: b_expr,
            },
        ) => a_op == b_op && same_expr(a_expr, b_expr),
        (
            ExprKind::Binary {
                op: a_op,
                lhs: a_lhs,
                rhs: a_rhs,
            },
            ExprKind::Binary {
                op: b_op,
                lhs: b_lhs,
                rhs: b_rhs,
            },
        ) => a_op == b_op && same_expr(a_lhs, b_lhs) && same_expr(a_rhs, b_rhs),
        (
            ExprKind::Call {
                callee: a_callee,
                args: a_args,
            },
            ExprKind::Call {
                callee: b_callee,
                args: b_args,
            },
        ) => {
            same_expr(a_callee, b_callee)
                && a_args.len() == b_args.len()
                && a_args
                    .iter()
                    .zip(b_args)
                    .all(|(a, b)| a.mutable == b.mutable && same_expr(&a.expr, &b.expr))
        }
        (ExprKind::Try(a), ExprKind::Try(b)) => same_expr(a, b),
        (
            ExprKind::Convert {
                to: a,
                expr: a_expr,
            },
            ExprKind::Convert {
                to: b,
                expr: b_expr,
            },
        ) => a == b && same_expr(a_expr, b_expr),
        _ => false,
    }
}

pub fn ty_has_float(program: &CheckedProgram, expr: &Expr) -> bool {
    matches!(program.type_of(expr.id), Some(Ty::Float))
}

pub fn expr_is_result(program: &CheckedProgram, expr: &Expr) -> bool {
    matches!(program.type_of(expr.id), Some(Ty::Result(_, _)))
}

/// Builtin methods that follow `mut self`.
pub fn builtin_mutates(receiver: BuiltinReceiver, name: &str) -> bool {
    match receiver {
        BuiltinReceiver::List => matches!(
            name,
            "push" | "pop" | "sort" | "insert" | "remove_at" | "extend" | "clear"
        ),
        BuiltinReceiver::Map => matches!(name, "remove" | "clear"),
        _ => false,
    }
}
