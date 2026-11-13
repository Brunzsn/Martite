//! Lowering from the checked AST into the resolved IR.
//!
//! # Scope
//!
//! This module lowers the checked AST into the resolved IR: function, lambda
//! and slot allocation, the constant and static-string pools, expressions,
//! statements, calls, mutable-argument places, patterns, captures and program
//! entries. A checked program that uses a construct the IR cannot represent
//! receives [`LowerErrorKind::Unsupported`]; any other failure is an internal
//! compiler error.
//!
//! Lambdas are separate [`IrFn`]s named `<lambda>`. They are allocated when
//! their expression is lowered, which visits a parent lambda before any nested
//! one, regardless of `NodeId` order. Their `source` is the defining
//! function's source. `CaptureInfo` records whether the closure snapshots a
//! local slot or one of the creating lambda's own captures; see
//! [`crate::CaptureSource`].
//!
//! The checker's `expr_types`, `resolutions`, `struct_lit_fields`,
//! `pattern_resolutions`, `struct_pattern_fields` and `pattern_bindings` side
//! tables are read only here. After [`lower`] returns, the `IrProgram` is
//! self-contained.

mod context;
mod effects;
mod expr;
mod slots;
mod stmt;

use std::collections::HashMap;
use std::fmt;

use martite_base::{SourceId, SourceMap, Span};
use martite_check::{
    CheckedProgram, ConstValue as CheckConstValue, DefId, EntryKind as CheckEntryKind,
    FnKind as CheckFnKind, PreludeVariant as CheckPreludeVariant, Ty as CheckTy,
    TypeDefKind as CheckTypeDefKind,
};
use martite_syntax::{Block, LambdaBody, NodeId, PatKind, Pattern};

use crate::id::{ConstId, FnId, HostId, Slot, StrId};
use crate::program::{
    ConstValue, Entries, EntryKind, FnKind, HookSig, HostFnInfo, IrFn, IrProgram, ParamInfo,
    SlotInfo,
};
use crate::types::{Ty, TyId, TypeId, TypeTable};

pub(crate) use context::FnLowerer;
pub(crate) use slots::{BodyRoot, SlotPlan};

/// Options for [`lower_with`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LowerOptions {
    /// Run [`crate::verify`] on the result before returning it.
    pub verify: bool,
}

impl Default for LowerOptions {
    fn default() -> Self {
        Self {
            verify: cfg!(debug_assertions),
        }
    }
}

/// Lower one checked program into a self-contained IR program.
///
/// `sources` supplies the file names indexed by [`SourceId`]. The
/// `CheckedProgram` does not retain them, and the IR needs them for trap
/// provenance and dumps.
///
/// [`LowerOptions::default`] verifies the result in debug builds.
pub fn lower(program: &CheckedProgram, sources: &SourceMap) -> Result<IrProgram, LowerError> {
    lower_with(program, sources, LowerOptions::default())
}

/// Lower one checked program with explicit [`LowerOptions`].
///
/// Tests and benchmarks use this to measure lowering without the debug-build
/// self-check; production callers should keep verification on.
pub fn lower_with(
    program: &CheckedProgram,
    sources: &SourceMap,
    options: LowerOptions,
) -> Result<IrProgram, LowerError> {
    let mut lowerer = Lowerer::new(program, sources);
    lowerer.build_pools()?;
    lowerer.allocate_functions()?;
    lowerer.lower_bodies()?;
    lowerer.into_program(options)
}

/// One lowering failure.
///
/// [`LowerErrorKind::Unsupported`] covers checked constructs the IR has no
/// node for and the interpreter itself traps on. Every other kind on a program
/// that passed the checker is an internal compiler error.
#[derive(Debug, Clone, PartialEq)]
pub struct LowerError {
    pub fn_id: Option<FnId>,
    pub source: Option<SourceId>,
    pub span: Option<Span>,
    pub kind: LowerErrorKind,
}

#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum LowerErrorKind {
    /// A checked construct the IR cannot represent.
    Unsupported { construct: &'static str },
    /// A lowering invariant failed.
    Internal { message: String },
    /// The IR tree exceeds [`crate::MAX_NODE_DEPTH`].
    DepthLimit { limit: u32 },
    /// The debug-build self-check rejected the lowered program.
    Verification(Box<crate::VerifyError>),
}

impl LowerError {
    pub(crate) fn internal(message: impl Into<String>) -> Self {
        Self {
            fn_id: None,
            source: None,
            span: None,
            kind: LowerErrorKind::Internal {
                message: message.into(),
            },
        }
    }
}

impl fmt::Display for LowerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(function) = self.fn_id {
            write!(f, "fn #{}", function.0)?;
            if let Some(source) = self.source {
                write!(f, " src#{}", source.0)?;
            }
            if let Some(span) = self.span {
                write!(f, " at {}..{}", span.start, span.end)?;
            }
            f.write_str(": ")?;
        }
        match &self.kind {
            LowerErrorKind::Unsupported { construct } => {
                write!(f, "lowering for `{construct}` is not implemented yet")
            }
            LowerErrorKind::Internal { message } => write!(f, "internal lowering error: {message}"),
            LowerErrorKind::DepthLimit { limit } => {
                write!(f, "IR nesting exceeds the {limit} node depth limit")
            }
            LowerErrorKind::Verification(error) => {
                write!(f, "lowering produced invalid IR: {error}")
            }
        }
    }
}

impl std::error::Error for LowerError {}

pub(crate) struct Lowerer<'p> {
    program: &'p CheckedProgram,
    types: TypeTable,
    unit: TyId,
    consts: Vec<ConstValue>,
    const_ids: HashMap<DefId, ConstId>,
    strs: Vec<Box<str>>,
    str_ids: HashMap<Box<str>, StrId>,
    fns: Vec<IrFn>,
    fn_ids: HashMap<DefId, FnId>,
    fn_defs: Vec<DefId>,
    /// Lambda expression `NodeId` to the `IrFn` allocated for it.
    lambda_ids: HashMap<NodeId, FnId>,
    /// The inverse map, used to find the lambda body while lowering functions.
    lambda_nodes: HashMap<FnId, NodeId>,
    hosts: Vec<HostFnInfo>,
    grants: Vec<Box<str>>,
    source_names: Vec<Box<str>>,
}

impl<'p> Lowerer<'p> {
    fn new(program: &'p CheckedProgram, sources: &SourceMap) -> Self {
        let mut types = TypeTable::from_checker(program);
        let unit = types.intern_ty(Ty::Unit);
        let source_names = sources
            .files()
            .map(|(_, file)| file.name().into())
            .collect();
        let grants = program
            .resolved
            .grants
            .iter()
            .map(|grant| grant.clone().into_boxed_str())
            .collect();
        Self {
            program,
            types,
            unit,
            consts: Vec::new(),
            const_ids: HashMap::new(),
            strs: Vec::new(),
            str_ids: HashMap::new(),
            fns: Vec::new(),
            fn_ids: HashMap::new(),
            fn_defs: Vec::new(),
            lambda_ids: HashMap::new(),
            lambda_nodes: HashMap::new(),
            hosts: Vec::new(),
            grants,
            source_names,
        }
    }

    fn type_id(&mut self, ty: &CheckTy) -> Result<TyId, LowerError> {
        if ty.is_error() || ty.has_var() {
            return Err(LowerError::internal(format!(
                "unresolved checker type reached lowering: {ty:?}"
            )));
        }
        Ok(self.types.ty_from_checker(ty))
    }

    fn intern_str(&mut self, text: &str) -> StrId {
        if let Some(id) = self.str_ids.get(text) {
            return *id;
        }
        let id = StrId::new(self.strs.len() as u32);
        let stored: Box<str> = text.into();
        self.str_ids.insert(stored.clone(), id);
        self.strs.push(stored);
        id
    }

    /// Append a new pool entry without deduplicating the text.
    ///
    /// Static literal expressions are cached and accounted per source node by
    /// the interpreter, so two identical literals at different sites must get
    /// distinct pool entries. Constants, patterns and interpolation text keep
    /// using [`Self::intern_str`], where only the text matters.
    fn literal_str(&mut self, text: &str) -> StrId {
        let id = StrId::new(self.strs.len() as u32);
        self.strs.push(text.into());
        id
    }

    fn push_const(&mut self, value: ConstValue) -> ConstId {
        let id = ConstId::new(self.consts.len() as u32);
        self.consts.push(value);
        id
    }

    fn build_pools(&mut self) -> Result<(), LowerError> {
        let mut defs: Vec<(DefId, CheckConstValue)> = self
            .program
            .consts
            .iter()
            .map(|(def, info)| (*def, info.value.clone()))
            .collect();
        defs.sort_by_key(|(def, _)| def.0);
        for (def, value) in defs {
            let value = self.convert_const(&value)?;
            let id = self.push_const(value);
            self.const_ids.insert(def, id);
        }
        Ok(())
    }

    fn convert_const(&mut self, value: &CheckConstValue) -> Result<ConstValue, LowerError> {
        Ok(match value {
            CheckConstValue::Unit => ConstValue::Unit,
            CheckConstValue::Int(value) => ConstValue::Int(*value),
            CheckConstValue::Float(value) => ConstValue::Float(*value),
            CheckConstValue::Bool(value) => ConstValue::Bool(*value),
            CheckConstValue::Str(value) => ConstValue::Str(self.intern_str(value)),
            CheckConstValue::Tuple(items) => ConstValue::Tuple(
                items
                    .iter()
                    .map(|item| self.convert_const(item))
                    .collect::<Result<_, _>>()?,
            ),
            CheckConstValue::List(items) => ConstValue::List(
                items
                    .iter()
                    .map(|item| self.convert_const(item))
                    .collect::<Result<_, _>>()?,
            ),
            CheckConstValue::Map(entries) => ConstValue::Map(
                entries
                    .iter()
                    .map(|(key, value)| Ok((self.convert_const(key)?, self.convert_const(value)?)))
                    .collect::<Result<_, LowerError>>()?,
            ),
            CheckConstValue::Struct(type_id, fields) => {
                let declaration = match self.program.types.get(*type_id).map(|def| &def.kind) {
                    Some(CheckTypeDefKind::Struct { fields }) => fields,
                    _ => {
                        return Err(LowerError::internal(format!(
                            "constant struct type #{} is not a struct",
                            type_id.0
                        )));
                    }
                };
                let mut ordered = Vec::with_capacity(declaration.len());
                for field in declaration {
                    let Some((_, value)) = fields.iter().find(|(name, _)| name == &field.name)
                    else {
                        return Err(LowerError::internal(format!(
                            "constant struct is missing field `{}`",
                            field.name
                        )));
                    };
                    ordered.push(self.convert_const(value)?);
                }
                if fields.len() != ordered.len() {
                    return Err(LowerError::internal(
                        "constant struct has fields outside its declaration",
                    ));
                }
                ConstValue::Struct(TypeId::new(type_id.0), ordered)
            }
            CheckConstValue::Enum(type_id, variant, payload) => ConstValue::Enum(
                TypeId::new(type_id.0),
                u32::try_from(*variant)
                    .map_err(|_| LowerError::internal("constant enum variant exceeds u32"))?,
                payload
                    .iter()
                    .map(|item| self.convert_const(item))
                    .collect::<Result<_, _>>()?,
            ),
            CheckConstValue::Prelude(variant, payload) => ConstValue::Prelude(
                convert_prelude(*variant),
                payload
                    .iter()
                    .map(|item| self.convert_const(item))
                    .collect::<Result<_, _>>()?,
            ),
        })
    }

    fn allocate_functions(&mut self) -> Result<(), LowerError> {
        for (index, sig) in self.program.functions.iter().enumerate() {
            let Some(sig) = sig else {
                continue;
            };
            let def = DefId(index as u32);
            let id = FnId::new(self.fns.len() as u32);
            let source = self.program.resolved.module(sig.module).source;
            let ret = self.type_id(&sig.ret)?;
            let mut params = Vec::with_capacity(sig.params.len());
            let mut slot_info = Vec::with_capacity(sig.params.len());
            for (index, param) in sig.params.iter().enumerate() {
                let ty = self.type_id(&param.ty)?;
                params.push(ParamInfo {
                    slot: Slot::new(index as u32),
                    name: param.name.clone().into_boxed_str(),
                    ty,
                    mutable: param.mutable,
                    is_self: param.is_self,
                    pattern: None,
                });
                slot_info.push(SlotInfo {
                    ty,
                    mutable: param.mutable,
                    name: Some(param.name.clone().into_boxed_str()),
                });
            }
            let kind = match sig.kind {
                CheckFnKind::Free => FnKind::Free,
                CheckFnKind::Method(type_id) => FnKind::Method(TypeId::new(type_id.0)),
                CheckFnKind::Handler => FnKind::Handler,
                CheckFnKind::Test => FnKind::Test,
                CheckFnKind::Host => {
                    let host_id = HostId::new(self.hosts.len() as u32);
                    let params_ty = params.iter().map(|param| param.ty).collect();
                    let module = self.program.resolved.module(sig.module).path.clone();
                    let opaque_return = self.opaque_return(&sig.ret);
                    self.hosts.push(HostFnInfo {
                        name: sig.name.clone().into_boxed_str(),
                        module: module.into_boxed_str(),
                        params: params_ty,
                        ret,
                        opaque_return,
                    });
                    FnKind::Host(host_id)
                }
            };
            let function = IrFn {
                id,
                name: sig.name.clone().into_boxed_str(),
                source,
                kind,
                params,
                ret,
                locals: slot_info.len() as u32,
                slot_info,
                captures: Vec::new(),
                patterns: Vec::new(),
                matches: Vec::new(),
                struct_layouts: Vec::new(),
                str_parts: Vec::new(),
                nodes: Vec::new(),
                spans: Vec::new(),
                synthetic: Vec::new(),
                safepoints: Vec::new(),
                children: Vec::new(),
                body: None,
            };
            self.fn_ids.insert(def, id);
            self.fn_defs.push(def);
            self.fns.push(function);
        }
        Ok(())
    }

    /// The opaque type `HostContext::make_handle` may construct for this host.
    ///
    /// Mirrors the AST interpreter: only a direct opaque interface return, its
    /// `Option` payload, or the success payload of a `Result` is a handle.
    /// Hosts that merely mention an opaque type elsewhere (a list element, a
    /// tuple member, a function parameter or the `Err` payload) cannot create
    /// handles.
    fn opaque_return(&self, ty: &CheckTy) -> Option<TypeId> {
        match ty {
            CheckTy::Named(type_id, args) if args.is_empty() => {
                let def = self.program.types.get(*type_id)?;
                (def.is_interface && matches!(def.kind, CheckTypeDefKind::Opaque))
                    .then_some(TypeId::new(type_id.0))
            }
            CheckTy::Option(inner) => self.opaque_return(inner),
            CheckTy::Result(ok, _) => self.opaque_return(ok),
            _ => None,
        }
    }

    fn lower_bodies(&mut self) -> Result<(), LowerError> {
        // Lambdas are appended while their parent body is lowered, so a
        // dynamically growing loop visits every parent before its children.
        let mut index = 0;
        while index < self.fns.len() {
            if matches!(self.fns[index].kind, FnKind::Host(_)) {
                index += 1;
                continue;
            }
            let fn_id = FnId::new(index as u32);
            let lambda_node = self.lambda_nodes.get(&fn_id).copied();
            let (body, lambda) = match lambda_node {
                Some(node) => {
                    let info = self
                        .program
                        .lambda(node)
                        .ok_or_else(|| {
                            LowerError::internal(format!("lambda #{node:?} has no metadata"))
                        })?
                        .clone();
                    let body = info.body.clone();
                    (BodySource::Lambda(body), Some(info))
                }
                None => {
                    let def = self.fn_defs.get(index).copied().ok_or_else(|| {
                        LowerError::internal(format!("fn #{index} has no definition"))
                    })?;
                    let block = self
                        .program
                        .function_block(def)
                        .ok_or_else(|| LowerError::internal(format!("fn #{index} has no body")))?;
                    (BodySource::Block(block), None)
                }
            };
            let first_slot = self.fns[index].params.len() as u32;
            let parameter_patterns: Vec<&Pattern> = lambda
                .as_ref()
                .map(|info| {
                    info.params
                        .iter()
                        .filter(|param| !matches!(param.pattern.kind, PatKind::Bind(_)))
                        .map(|param| &param.pattern)
                        .collect()
                })
                .unwrap_or_default();
            let root = body.root();
            let (mut plan, extra_slots) = slots::plan_slots(
                self.program,
                &mut self.types,
                root,
                first_slot,
                &parameter_patterns,
            )?;
            match &lambda {
                Some(info) => {
                    for (parameter, param) in info.params.iter().enumerate() {
                        if matches!(param.pattern.kind, PatKind::Bind(_))
                            && let Some(binding) =
                                info.param_bindings.get(parameter).copied().flatten()
                        {
                            plan.add_parameter(binding, Slot::new(parameter as u32));
                        }
                    }
                }
                None => {
                    let def = self.fn_defs[index];
                    let sig = self.program.fn_sig(def).ok_or_else(|| {
                        LowerError::internal(format!("fn #{index} has no signature"))
                    })?;
                    for (parameter, param) in sig.params.iter().enumerate() {
                        plan.add_parameter(param.binding, Slot::new(parameter as u32));
                    }
                }
            }
            self.fns[index].slot_info.extend(extra_slots);
            self.fns[index].locals = self.fns[index].slot_info.len() as u32;
            let captures = lambda
                .as_ref()
                .map(|info| {
                    info.captures
                        .iter()
                        .enumerate()
                        .map(|(index, binding)| (*binding, crate::CaptureIndex::new(index as u32)))
                        .collect()
                })
                .unwrap_or_default();
            let function = self.fns[index].clone();
            let mut lowerer = FnLowerer::new(self, function, plan, captures);
            if let Some(info) = &lambda {
                lowerer.lower_lambda_params(info)?;
            }
            let function = lowerer.lower_body(body)?;
            self.fns[index] = function;
            index += 1;
        }
        Ok(())
    }

    fn build_entries(&mut self) -> Result<Entries, LowerError> {
        let main = self
            .program
            .main
            .map(|def| self.entry_fn(def, "main"))
            .transpose()?;
        let init = self
            .program
            .init
            .map(|def| self.entry_fn(def, "init"))
            .transpose()?;
        let shutdown = self
            .program
            .shutdown
            .map(|def| self.entry_fn(def, "shutdown"))
            .transpose()?;

        let mut tests = Vec::with_capacity(self.program.tests.len());
        for def in &self.program.tests {
            tests.push(self.entry_fn(*def, "test")?);
        }

        let mut handlers: Vec<_> = self.program.handlers.iter().collect();
        handlers.sort_unstable_by(|left, right| left.0.cmp(right.0));
        let mut entries_handlers = Vec::with_capacity(handlers.len());
        for (name, def) in handlers {
            entries_handlers.push((
                name.clone().into_boxed_str(),
                self.entry_fn(*def, "handler")?,
            ));
        }

        let mut hooks: Vec<_> = self.program.hooks.iter().collect();
        hooks.sort_unstable_by(|left, right| left.0.cmp(right.0));
        let mut entries_hooks = Vec::with_capacity(hooks.len());
        for (name, hook) in hooks {
            let params = hook
                .params
                .iter()
                .map(|ty| self.type_id(ty))
                .collect::<Result<Vec<_>, _>>()?;
            let ret = self.type_id(&hook.ret)?;
            entries_hooks.push((
                name.clone().into_boxed_str(),
                HookSig {
                    name: hook.name.clone().into_boxed_str(),
                    module: self
                        .program
                        .resolved
                        .module(hook.module)
                        .path
                        .clone()
                        .into_boxed_str(),
                    params,
                    ret,
                    source: hook.source,
                    span: hook.span,
                },
            ));
        }

        let state_type = match &self.program.state_type {
            Some(ty) => {
                let ty = ty.clone();
                Some(self.type_id(&ty)?)
            }
            None => None,
        };
        let json_type = self.program.json_type.map(|id| crate::TypeId::new(id.0));
        let entry_kind = match self.program.entry_kind {
            CheckEntryKind::Library => EntryKind::Library,
            CheckEntryKind::Script => EntryKind::Script,
            CheckEntryKind::Plugin => EntryKind::Plugin,
        };

        Ok(Entries {
            main,
            tests,
            handlers: entries_handlers,
            hooks: entries_hooks,
            init,
            shutdown,
            state_type,
            json_type,
            entry_kind,
        })
    }

    fn entry_fn(&self, def: DefId, context: &'static str) -> Result<FnId, LowerError> {
        self.fn_ids
            .get(&def)
            .copied()
            .ok_or_else(|| LowerError::internal(format!("{context} definition has no IR function")))
    }

    fn into_program(mut self, options: LowerOptions) -> Result<IrProgram, LowerError> {
        let entries = self.build_entries()?;
        let mut program = IrProgram {
            fns: self.fns,
            consts: self.consts,
            strs: self.strs,
            types: self.types,
            hosts: self.hosts,
            grants: self.grants,
            entries,
            source_names: self.source_names,
        };
        effects::fill(&mut program)?;
        if options.verify
            && let Err(error) = crate::verify(&program)
        {
            return Err(LowerError {
                fn_id: error.function,
                source: error.source,
                span: error.span,
                kind: LowerErrorKind::Verification(Box::new(error)),
            });
        }
        Ok(program)
    }
}

/// The AST body of one function or lambda while it is being lowered.
pub(crate) enum BodySource<'a> {
    Block(&'a Block),
    Lambda(LambdaBody),
}

impl BodySource<'_> {
    fn root(&self) -> BodyRoot<'_> {
        match self {
            Self::Block(block) => BodyRoot::Block(block),
            Self::Lambda(LambdaBody::Block(block)) => BodyRoot::Block(block),
            Self::Lambda(LambdaBody::Expr(expr)) => BodyRoot::Expr(expr),
        }
    }
}

fn convert_prelude(variant: CheckPreludeVariant) -> crate::PreludeVariant {
    match variant {
        CheckPreludeVariant::None => crate::PreludeVariant::None,
        CheckPreludeVariant::Some => crate::PreludeVariant::Some,
        CheckPreludeVariant::Ok => crate::PreludeVariant::Ok,
        CheckPreludeVariant::Err => crate::PreludeVariant::Err,
    }
}
