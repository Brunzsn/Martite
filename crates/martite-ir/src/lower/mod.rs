//! Lowering from the checked AST into the resolved IR.
//!
//! # Scope
//!
//! This module is the first half of lowering: function and slot allocation,
//! the constant and static-string pools, and the expression and statement node
//! kinds listed in #103. Calls (#104), match patterns (#105) and lambdas plus
//! `Entries` (#106) are deliberately not lowered yet. A checked program that
//! uses one of those constructs receives [`LowerErrorKind::Unsupported`]; any
//! other failure is an internal compiler error.
//!
//! The checker's `expr_types`, `resolutions`, `struct_lit_fields` and
//! `pattern_bindings` side tables are read only here. After [`lower`] returns,
//! the `IrProgram` is self-contained.

mod context;
mod effects;
mod expr;
mod slots;
mod stmt;

use std::collections::HashMap;
use std::fmt;

use martite_base::{SourceId, SourceMap, Span};
use martite_check::{
    CheckedProgram, ConstValue as CheckConstValue, DefId, FnKind as CheckFnKind,
    PreludeVariant as CheckPreludeVariant, Ty as CheckTy, TypeDefKind as CheckTypeDefKind,
};

use crate::id::{ConstId, FnId, HostId, Slot, StrId};
use crate::program::{
    ConstValue, Entries, FnKind, HostFnInfo, IrFn, IrProgram, ParamInfo, SlotInfo,
};
use crate::types::{Ty, TyId, TypeId, TypeTable};

pub(crate) use context::FnLowerer;
pub(crate) use slots::SlotPlan;

/// Lower one checked program into a self-contained IR program.
///
/// `sources` supplies the file names indexed by [`SourceId`]. The
/// `CheckedProgram` does not retain them, and the IR needs them for trap
/// provenance and dumps.
///
/// In debug builds the result is verified before it is returned.
pub fn lower(program: &CheckedProgram, sources: &SourceMap) -> Result<IrProgram, LowerError> {
    let mut lowerer = Lowerer::new(program, sources);
    lowerer.build_pools()?;
    lowerer.allocate_functions()?;
    lowerer.lower_bodies()?;
    lowerer.into_program()
}

/// One lowering failure.
///
/// [`LowerErrorKind::Unsupported`] is expected until #104–#106 land. Every
/// other kind on a program that passed the checker is an internal compiler
/// error.
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
    /// A construct owned by #104, #105 or #106.
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

    fn opaque_return(&self, ty: &CheckTy) -> Option<TypeId> {
        match ty {
            CheckTy::Named(type_id, args) => {
                let def = self.program.types.get(*type_id)?;
                if def.is_interface && matches!(def.kind, CheckTypeDefKind::Opaque) {
                    return Some(TypeId::new(type_id.0));
                }
                args.iter().find_map(|arg| self.opaque_return(arg))
            }
            CheckTy::Tuple(items) => items.iter().find_map(|item| self.opaque_return(item)),
            CheckTy::List(item) | CheckTy::Option(item) => self.opaque_return(item),
            CheckTy::Map(key, value) | CheckTy::Result(key, value) => self
                .opaque_return(key)
                .or_else(|| self.opaque_return(value)),
            CheckTy::Fn(params, ret) => params
                .iter()
                .find_map(|param| self.opaque_return(param))
                .or_else(|| self.opaque_return(ret)),
            _ => None,
        }
    }

    fn lower_bodies(&mut self) -> Result<(), LowerError> {
        for index in 0..self.fns.len() {
            if matches!(self.fns[index].kind, FnKind::Host(_)) {
                continue;
            }
            let def = self.fn_defs[index];
            let block = self
                .program
                .function_block(def)
                .ok_or_else(|| LowerError::internal(format!("fn #{} has no body", index)))?;
            let first_slot = self.fns[index].params.len() as u32;
            let (mut plan, extra_slots) =
                slots::plan_slots(self.program, &mut self.types, block, first_slot)?;
            let sig = self
                .program
                .fn_sig(def)
                .ok_or_else(|| LowerError::internal(format!("fn #{} has no signature", index)))?;
            for (parameter, param) in sig.params.iter().enumerate() {
                plan.add_parameter(param.binding, Slot::new(parameter as u32));
            }
            self.fns[index].slot_info.extend(extra_slots);
            self.fns[index].locals = self.fns[index].slot_info.len() as u32;
            let function = self.fns[index].clone();
            let function = FnLowerer::new(self, function, plan).lower_function(block)?;
            self.fns[index] = function;
        }
        Ok(())
    }

    fn into_program(self) -> Result<IrProgram, LowerError> {
        let mut program = IrProgram {
            fns: self.fns,
            consts: self.consts,
            strs: self.strs,
            types: self.types,
            hosts: self.hosts,
            grants: self.grants,
            entries: Entries::default(),
            source_names: self.source_names,
        };
        effects::fill(&mut program)?;
        #[cfg(debug_assertions)]
        if let Err(error) = crate::verify(&program) {
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

fn convert_prelude(variant: CheckPreludeVariant) -> crate::PreludeVariant {
    match variant {
        CheckPreludeVariant::None => crate::PreludeVariant::None,
        CheckPreludeVariant::Some => crate::PreludeVariant::Some,
        CheckPreludeVariant::Ok => crate::PreludeVariant::Ok,
        CheckPreludeVariant::Err => crate::PreludeVariant::Err,
    }
}
