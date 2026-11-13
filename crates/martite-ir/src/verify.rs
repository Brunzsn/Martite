//! Verification of [`IrProgram`].
//!
//! The verifier is the contract enforcement point for lowering and every pass.
//! It checks ids, ownership, the tree property, node roles and types, calls,
//! control flow, charge schedules, safepoints and stored effects. Definite
//! assignment, one-initialization-site-per-slot and the on-demand slot
//! analyses are provided by [`crate::analysis`].
//!
//! [`verify`] returns the first error. [`verify_after_pass`] is the hook for the
//! pass manager: it verifies in tests and debug builds and is a no-op in
//! release builds.

use std::fmt;

use martite_base::{SourceId, Span};

use crate::analysis::{DefiniteAssignment, check_slot_initialization};
use crate::builtin::{BuiltinMethodDef, BuiltinReceiver};
use crate::charge::Charge;
use crate::effects::Effects;
use crate::id::{
    BuiltinFnId, BuiltinItemId, BuiltinMethodId, CaptureIndex, ConstId, FnId, HostId, IrId,
    MatchSetId, PatternId, Slot, StrId, StructLayoutId,
};
use crate::node::{AccessMode, ChildRange, Node, NodeKind, StrPart};
use crate::pattern::{MatchSet, Pattern, StructLayout};
use crate::program::{ConstValue, FnKind, IrFn, IrProgram, ParamInfo, PreludeVariant};
use crate::types::{Ty, TyId, TypeDefKind, TypeId};

/// Verify one lowered program.
pub fn verify(program: &IrProgram) -> Result<(), VerifyError> {
    Verifier { program }.run()
}

/// Verify after a pass mutation in tests and debug builds.
///
/// The pass manager in `martite-opt` calls this after every rewrite. Release
/// builds do not pay for verification here.
#[track_caller]
pub fn verify_after_pass(program: &IrProgram) {
    if cfg!(debug_assertions)
        && let Err(error) = verify(program)
    {
        panic!("IR verification failed after pass: {error}");
    }
}

impl IrProgram {
    /// Convenience wrapper for [`verify`].
    pub fn verify(&self) -> Result<(), VerifyError> {
        verify(self)
    }
}

/// One arena addressed by the verifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Arena {
    Function,
    Node,
    Slot,
    Const,
    Str,
    Host,
    Type,
    TypeDef,
    Pattern,
    MatchSet,
    StructLayout,
    Capture,
    BuiltinFn,
    BuiltinItem,
    BuiltinMethod,
}

/// The role a child plays in its parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeRole {
    Value,
    Place,
    Statement,
    Body,
    Condition,
    Receiver,
    Argument,
    Scrutinee,
    Guard,
    Tail,
}

/// A structured verification failure.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum VerifyErrorKind {
    OutOfRange {
        arena: Arena,
        id: u32,
    },
    SideVectorLength {
        nodes: usize,
        spans: usize,
        synthetic: usize,
        safepoints: usize,
    },
    FunctionIdMismatch {
        expected: FnId,
        found: FnId,
    },
    SourceOutOfRange {
        source: SourceId,
    },
    BodyPresence {
        expected: bool,
        found: bool,
    },
    HostOutOfRange {
        host: HostId,
    },
    HostSignature {
        host: HostId,
        context: &'static str,
    },
    LocalsMismatch {
        locals: u32,
        slots: usize,
    },
    SlotInfo {
        slot: Slot,
        context: &'static str,
    },
    /// A slot read that is not definitely assigned.
    UnboundSlot {
        slot: Slot,
        context: &'static str,
    },
    /// A slot with no initialization site or more than one.
    SlotBinding {
        slot: Slot,
        context: &'static str,
    },
    Param {
        index: u32,
        context: &'static str,
    },
    Capture {
        index: CaptureIndex,
        context: &'static str,
    },
    Pattern {
        pattern: PatternId,
        context: &'static str,
    },
    MatchSet {
        match_set: MatchSetId,
    },
    StructLayout {
        layout: StructLayoutId,
    },
    StrPart {
        part: u32,
    },
    ChildRange {
        range: ChildRange,
        len: u32,
    },
    Tree {
        node: IrId,
        context: &'static str,
        other: Option<IrId>,
    },
    WrongRole {
        expected: NodeRole,
        found: NodeKind,
    },
    Arity {
        expected: u32,
        found: u32,
        context: &'static str,
    },
    TypeMismatch {
        expected: Box<Ty>,
        found: Box<Ty>,
        context: &'static str,
    },
    NodeType {
        expected: TyId,
        found: TyId,
        context: &'static str,
    },
    TypeTable {
        context: &'static str,
        ty: Option<TyId>,
        def: Option<TypeId>,
    },
    Const {
        id: ConstId,
        context: &'static str,
    },
    Str {
        id: StrId,
    },
    Path {
        index: u32,
        context: &'static str,
    },
    Builtin {
        id: u32,
        context: &'static str,
    },
    BuiltinReceiver {
        expected: &'static str,
        found: Box<str>,
    },
    Call {
        context: &'static str,
        index: u32,
    },
    MutArg {
        index: u32,
        expected: bool,
    },
    DuplicateMutRoot {
        root: Slot,
    },
    Place {
        context: &'static str,
    },
    ControlFlow {
        context: &'static str,
    },
    Safepoint {
        node: IrId,
        expected: bool,
    },
    Charge {
        expected: Charge,
        found: Charge,
    },
    Effects {
        missing: Effects,
    },
    Entry {
        entry: FnId,
        context: &'static str,
    },
    HostGrant {
        host: HostId,
    },
    PatternShape {
        pattern: PatternId,
        context: &'static str,
    },
    Span {
        node: IrId,
    },
    SpanRange {
        span: Span,
    },
}

/// One verification error with its source location.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifyError {
    pub function: Option<FnId>,
    pub source: Option<SourceId>,
    pub node: Option<IrId>,
    pub span: Option<Span>,
    pub kind: VerifyErrorKind,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(function) = self.function {
            write!(f, "fn #{}", function.0)?;
            if let Some(source) = self.source {
                write!(f, " src#{}", source.0)?;
            }
            if let Some(node) = self.node {
                write!(f, " node #{}", node.0)?;
            }
            if let Some(span) = self.span {
                write!(f, " at {}..{}", span.start, span.end)?;
            }
            f.write_str(": ")?;
        }
        fmt::Display::fmt(&self.kind, f)
    }
}

impl std::error::Error for VerifyError {}

impl fmt::Display for VerifyErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfRange { arena, id } => write!(f, "{arena:?} id {id} is out of range"),
            Self::SideVectorLength {
                nodes,
                spans,
                synthetic,
                safepoints,
            } => write!(
                f,
                "side vectors disagree: nodes={nodes} spans={spans} synthetic={synthetic} safepoints={safepoints}"
            ),
            Self::FunctionIdMismatch { expected, found } => {
                write!(
                    f,
                    "function id mismatch: expected #{}, found #{}",
                    expected.0, found.0
                )
            }
            Self::SourceOutOfRange { source } => {
                write!(f, "source #{} is out of range", source.0)
            }
            Self::BodyPresence { expected, found } => {
                write!(
                    f,
                    "body presence mismatch: expected {expected}, found {found}"
                )
            }
            Self::HostOutOfRange { host } => write!(f, "host #{} is out of range", host.0),
            Self::HostSignature { host, context } => {
                write!(f, "host #{} has an invalid signature ({context})", host.0)
            }
            Self::LocalsMismatch { locals, slots } => {
                write!(f, "locals={locals} but slot_info has {slots} entries")
            }
            Self::SlotInfo { slot, context } => {
                write!(f, "slot #{} is invalid ({context})", slot.0)
            }
            Self::UnboundSlot { slot, context } => {
                write!(f, "read of unassigned slot #{} ({context})", slot.0)
            }
            Self::SlotBinding { slot, context } => {
                write!(
                    f,
                    "slot #{} has an invalid initialization ({context})",
                    slot.0
                )
            }
            Self::Param { index, context } => write!(f, "parameter {index} is invalid ({context})"),
            Self::Capture { index, context } => {
                write!(f, "capture #{} is invalid ({context})", index.0)
            }
            Self::Pattern { pattern, context } => {
                write!(f, "pattern #{} is invalid ({context})", pattern.0)
            }
            Self::MatchSet { match_set } => {
                write!(f, "match set #{} is out of range", match_set.0)
            }
            Self::StructLayout { layout } => {
                write!(f, "struct layout #{} is out of range", layout.0)
            }
            Self::StrPart { part } => write!(f, "string part {part} is out of range"),
            Self::ChildRange { range, len } => write!(
                f,
                "child range {}..{} is out of range for {len} children",
                range.start,
                range.end()
            ),
            Self::Tree {
                node,
                context,
                other,
            } => match other {
                Some(other) => write!(f, "node #{} {context} with #{}", node.0, other.0),
                None => write!(f, "node #{} {context}", node.0),
            },
            Self::WrongRole { expected, found } => {
                write!(f, "expected {expected:?}, found {found:?}")
            }
            Self::Arity {
                expected,
                found,
                context,
            } => write!(f, "{context}: expected {expected} children, found {found}"),
            Self::TypeMismatch {
                expected,
                found,
                context,
            } => write!(f, "{context}: expected {expected:?}, found {found:?}"),
            Self::NodeType {
                expected,
                found,
                context,
            } => write!(
                f,
                "{context}: expected ty#{}, found ty#{}",
                expected.0, found.0
            ),
            Self::TypeTable { context, ty, def } => {
                if let Some(ty) = ty {
                    write!(f, "type table is invalid at ty#{} ({context})", ty.0)
                } else if let Some(def) = def {
                    write!(f, "type table is invalid at type#{} ({context})", def.0)
                } else {
                    write!(f, "type table is invalid ({context})")
                }
            }
            Self::Const { id, context } => write!(f, "constant #{} is invalid ({context})", id.0),
            Self::Str { id } => write!(f, "string #{} is out of range", id.0),
            Self::Path { index, context } => write!(f, "path index {index} is invalid ({context})"),
            Self::Builtin { id, context } => write!(f, "builtin #{id} is invalid ({context})"),
            Self::BuiltinReceiver { expected, found } => {
                write!(f, "expected {expected} receiver, found {found}")
            }
            Self::Call { context, index } => {
                write!(f, "call argument {index} is invalid ({context})")
            }
            Self::MutArg { index, expected } => {
                if *expected {
                    write!(f, "call argument {index} must be a `mut` place")
                } else {
                    write!(f, "call argument {index} must not be `mut`")
                }
            }
            Self::DuplicateMutRoot { root } => {
                write!(
                    f,
                    "the same root slot #{} is passed more than once as `mut`",
                    root.0
                )
            }
            Self::Place { context } => write!(f, "not a valid place ({context})"),
            Self::ControlFlow { context } => f.write_str(context),
            Self::Safepoint { node, expected } => {
                write!(f, "node #{} safepoint flag should be {expected}", node.0)
            }
            Self::Charge { expected, found } => {
                write!(f, "charge mismatch: expected {expected:?}, found {found:?}")
            }
            Self::Effects { missing } => write!(f, "stored effects are missing {missing:?}"),
            Self::Entry { entry, context } => {
                write!(f, "entry function #{} is invalid ({context})", entry.0)
            }
            Self::HostGrant { host } => {
                write!(f, "host #{} is called without its module grant", host.0)
            }
            Self::PatternShape { pattern, context } => {
                write!(f, "pattern #{} has an invalid shape ({context})", pattern.0)
            }
            Self::Span { node } => write!(f, "node #{} has no span", node.0),
            Self::SpanRange { span } => {
                write!(f, "span {}..{} is inverted", span.start, span.end)
            }
        }
    }
}

struct Verifier<'a> {
    program: &'a IrProgram,
}

impl<'a> Verifier<'a> {
    fn run(&self) -> Result<(), VerifyError> {
        self.check_type_table()?;
        self.check_consts()?;
        self.check_hosts()?;
        self.check_entries()?;
        for (index, function) in self.program.fns.iter().enumerate() {
            self.check_function_shape(function, FnId::new(index as u32))?;
        }
        for function in &self.program.fns {
            if function.body.is_some() {
                FnCheck::new(self.program, function).run()?;
            }
        }
        EffectsCheck::new(self.program).run()
    }

    fn error(&self, kind: VerifyErrorKind) -> VerifyError {
        VerifyError {
            function: None,
            source: None,
            node: None,
            span: None,
            kind,
        }
    }

    fn ty(&self, id: TyId) -> Result<&Ty, VerifyError> {
        self.program.types.get_ty(id).ok_or_else(|| {
            self.error(VerifyErrorKind::OutOfRange {
                arena: Arena::Type,
                id: id.0,
            })
        })
    }

    fn ty_id(&self, id: TyId, context: &'static str) -> Result<(), VerifyError> {
        if self.program.types.get_ty(id).is_some() {
            Ok(())
        } else {
            Err(self.error(VerifyErrorKind::TypeTable {
                context,
                ty: Some(id),
                def: None,
            }))
        }
    }

    fn type_def(&self, id: TypeId, context: &'static str) -> Result<(), VerifyError> {
        if self.program.types.get(id).is_some() {
            Ok(())
        } else {
            Err(self.error(VerifyErrorKind::TypeTable {
                context,
                ty: None,
                def: Some(id),
            }))
        }
    }

    fn check_type_table(&self) -> Result<(), VerifyError> {
        let mut states = vec![TypeState::Unvisited; self.program.types.tys.len()];
        for index in 0..self.program.types.tys.len() {
            self.check_ty_id(TyId::new(index as u32), &mut states)?;
        }
        for (index, def) in self.program.types.defs.iter().enumerate() {
            let type_id = TypeId::new(index as u32);
            match &def.kind {
                TypeDefKind::Struct { fields } => {
                    for field in fields {
                        self.check_ty_id(field.ty, &mut states)?;
                    }
                }
                TypeDefKind::Enum { variants } => {
                    for variant in variants {
                        for ty in &variant.payload {
                            self.check_ty_id(*ty, &mut states)?;
                        }
                    }
                }
                TypeDefKind::Opaque => {}
            }
            let _ = type_id;
        }
        Ok(())
    }

    fn check_ty_id(&self, id: TyId, states: &mut [TypeState]) -> Result<(), VerifyError> {
        let Some(state) = states.get_mut(id.index()) else {
            return Err(self.error(VerifyErrorKind::OutOfRange {
                arena: Arena::Type,
                id: id.0,
            }));
        };
        match state {
            TypeState::Done => return Ok(()),
            TypeState::Visiting => {
                return Err(self.error(VerifyErrorKind::TypeTable {
                    context: "cyclic type",
                    ty: Some(id),
                    def: None,
                }));
            }
            TypeState::Unvisited => *state = TypeState::Visiting,
        }

        let ty = self.ty(id)?.clone();
        match ty {
            Ty::Int
            | Ty::Float
            | Ty::Bool
            | Ty::Str
            | Ty::Unit
            | Ty::Range
            | Ty::Never
            | Ty::Param(_) => {}
            Ty::Tuple(items) => {
                for item in items {
                    self.check_ty_id(item, states)?;
                }
            }
            Ty::List(item) | Ty::Option(item) => self.check_ty_id(item, states)?,
            Ty::Map(key, value) | Ty::Result(key, value) => {
                self.check_ty_id(key, states)?;
                self.check_ty_id(value, states)?;
            }
            Ty::Fn(params, ret) => {
                for param in params {
                    self.check_ty_id(param, states)?;
                }
                self.check_ty_id(ret, states)?;
            }
            Ty::Named(type_id, args) => {
                let Some(def) = self.program.types.get(type_id) else {
                    return Err(self.error(VerifyErrorKind::TypeTable {
                        context: "named type is missing",
                        ty: Some(id),
                        def: Some(type_id),
                    }));
                };
                if def.params as usize != args.len() {
                    return Err(self.error(VerifyErrorKind::TypeTable {
                        context: "named type argument count",
                        ty: Some(id),
                        def: Some(type_id),
                    }));
                }
                for arg in args {
                    self.check_ty_id(arg, states)?;
                }
            }
        }

        states[id.index()] = TypeState::Done;
        Ok(())
    }

    fn check_consts(&self) -> Result<(), VerifyError> {
        for (index, value) in self.program.consts.iter().enumerate() {
            self.check_const_value(ConstId::new(index as u32), value, "const pool")?;
        }
        Ok(())
    }

    #[allow(clippy::only_used_in_recursion)]
    fn check_const_value(
        &self,
        id: ConstId,
        value: &ConstValue,
        context: &'static str,
    ) -> Result<(), VerifyError> {
        match value {
            ConstValue::Unit | ConstValue::Int(_) | ConstValue::Float(_) | ConstValue::Bool(_) => {
                Ok(())
            }
            ConstValue::Str(str) => self.check_str(*str),
            ConstValue::Tuple(items) | ConstValue::List(items) => {
                for item in items {
                    self.check_const_value(id, item, context)?;
                }
                Ok(())
            }
            ConstValue::Map(entries) => {
                for (key, value) in entries {
                    self.check_const_value(id, key, context)?;
                    self.check_const_value(id, value, context)?;
                }
                Ok(())
            }
            ConstValue::Struct(type_id, fields) => {
                let Some(def) = self.program.types.get(*type_id) else {
                    return Err(self.error(VerifyErrorKind::Const {
                        id,
                        context: "struct type is out of range",
                    }));
                };
                let TypeDefKind::Struct { fields: def_fields } = &def.kind else {
                    return Err(self.error(VerifyErrorKind::Const {
                        id,
                        context: "constant struct refers to a non-struct",
                    }));
                };
                if fields.len() != def_fields.len() {
                    return Err(self.error(VerifyErrorKind::Const {
                        id,
                        context: "constant struct field count",
                    }));
                }
                for value in fields {
                    self.check_const_value(id, value, context)?;
                }
                Ok(())
            }
            ConstValue::Enum(type_id, variant, payload) => {
                let Some(def) = self.program.types.get(*type_id) else {
                    return Err(self.error(VerifyErrorKind::Const {
                        id,
                        context: "enum type is out of range",
                    }));
                };
                let TypeDefKind::Enum { variants } = &def.kind else {
                    return Err(self.error(VerifyErrorKind::Const {
                        id,
                        context: "constant enum refers to a non-enum",
                    }));
                };
                let Some(def_variant) = variants.get(*variant as usize) else {
                    return Err(self.error(VerifyErrorKind::Const {
                        id,
                        context: "constant variant is out of range",
                    }));
                };
                if payload.len() != def_variant.payload.len() {
                    return Err(self.error(VerifyErrorKind::Const {
                        id,
                        context: "constant payload count",
                    }));
                }
                for value in payload {
                    self.check_const_value(id, value, context)?;
                }
                Ok(())
            }
            ConstValue::Prelude(variant, payload) => {
                let expected = match variant {
                    PreludeVariant::None => 0,
                    PreludeVariant::Some | PreludeVariant::Ok | PreludeVariant::Err => 1,
                };
                if payload.len() != expected {
                    return Err(self.error(VerifyErrorKind::Const {
                        id,
                        context: "prelude payload count",
                    }));
                }
                for value in payload {
                    self.check_const_value(id, value, context)?;
                }
                Ok(())
            }
        }
    }

    fn check_str(&self, id: StrId) -> Result<(), VerifyError> {
        if self.program.string(id).is_some() {
            Ok(())
        } else {
            Err(self.error(VerifyErrorKind::Str { id }))
        }
    }

    fn check_hosts(&self) -> Result<(), VerifyError> {
        for (index, host) in self.program.hosts.iter().enumerate() {
            for ty in &host.params {
                self.ty_id(*ty, "host parameter")?;
            }
            self.ty_id(host.ret, "host return")?;
            if let Some(opaque) = host.opaque_return {
                let Some(def) = self.program.types.get(opaque) else {
                    return Err(self.error(VerifyErrorKind::HostSignature {
                        host: HostId::new(index as u32),
                        context: "opaque return type is out of range",
                    }));
                };
                if !(def.is_interface && def.is_opaque()) {
                    return Err(self.error(VerifyErrorKind::HostSignature {
                        host: HostId::new(index as u32),
                        context: "opaque return is not an opaque interface",
                    }));
                }
                if !self.ty_contains_type(host.ret, opaque)? {
                    return Err(self.error(VerifyErrorKind::HostSignature {
                        host: HostId::new(index as u32),
                        context: "opaque return is not part of the host return type",
                    }));
                }
            }
        }
        Ok(())
    }

    fn ty_contains_type(&self, ty: TyId, type_id: TypeId) -> Result<bool, VerifyError> {
        let ty = self.ty(ty)?.clone();
        Ok(match ty {
            Ty::Named(id, _) => id == type_id,
            Ty::Tuple(items) => {
                for item in items {
                    if self.ty_contains_type(item, type_id)? {
                        return Ok(true);
                    }
                }
                false
            }
            Ty::List(item) | Ty::Option(item) => self.ty_contains_type(item, type_id)?,
            Ty::Map(key, value) | Ty::Result(key, value) => {
                self.ty_contains_type(key, type_id)? || self.ty_contains_type(value, type_id)?
            }
            Ty::Fn(params, ret) => {
                for param in params {
                    if self.ty_contains_type(param, type_id)? {
                        return Ok(true);
                    }
                }
                self.ty_contains_type(ret, type_id)?
            }
            _ => false,
        })
    }

    fn check_entries(&self) -> Result<(), VerifyError> {
        if let Some(entry) = self.program.entries.main {
            self.check_entry_fn(entry, "main")?;
        }
        for entry in &self.program.entries.tests {
            self.check_entry_fn(*entry, "test")?;
            if !matches!(
                self.program.function(*entry).map(|function| function.kind),
                Some(FnKind::Test)
            ) {
                return Err(self.error(VerifyErrorKind::Entry {
                    entry: *entry,
                    context: "test entry is not a test function",
                }));
            }
        }
        for (name, entry) in &self.program.entries.handlers {
            self.check_entry_fn(*entry, "handler")?;
            if !matches!(
                self.program.function(*entry).map(|function| function.kind),
                Some(FnKind::Handler)
            ) {
                return Err(self.error(VerifyErrorKind::Entry {
                    entry: *entry,
                    context: "handler entry is not a handler function",
                }));
            }
            if name.is_empty() {
                return Err(self.error(VerifyErrorKind::Entry {
                    entry: *entry,
                    context: "handler name is empty",
                }));
            }
        }
        if let Some(entry) = self.program.entries.init {
            self.check_entry_fn(entry, "init")?;
        }
        if let Some(entry) = self.program.entries.shutdown {
            self.check_entry_fn(entry, "shutdown")?;
        }
        for (name, hook) in &self.program.entries.hooks {
            if name.is_empty() {
                return Err(self.error(VerifyErrorKind::Entry {
                    entry: FnId(0),
                    context: "hook name is empty",
                }));
            }
            if hook.source.0 as usize >= self.program.source_names.len() {
                return Err(self.error(VerifyErrorKind::SourceOutOfRange {
                    source: hook.source,
                }));
            }
            if hook.span.start > hook.span.end {
                return Err(self.error(VerifyErrorKind::SpanRange { span: hook.span }));
            }
            for ty in &hook.params {
                self.ty_id(*ty, "hook parameter")?;
            }
            self.ty_id(hook.ret, "hook return")?;
        }
        if let Some(ty) = self.program.entries.state_type {
            self.ty_id(ty, "entry state type")?;
        }
        if let Some(type_id) = self.program.entries.json_type {
            self.type_def(type_id, "entry json type")?;
        }
        Ok(())
    }

    fn check_entry_fn(&self, entry: FnId, context: &'static str) -> Result<(), VerifyError> {
        if self.program.function(entry).is_none() {
            return Err(self.error(VerifyErrorKind::Entry { entry, context }));
        }
        Ok(())
    }

    fn check_function_shape(&self, function: &IrFn, expected_id: FnId) -> Result<(), VerifyError> {
        let error = |kind| verify_error(function, kind);
        if function.id != expected_id {
            return Err(error(VerifyErrorKind::FunctionIdMismatch {
                expected: expected_id,
                found: function.id,
            }));
        }
        if function.source.0 as usize >= self.program.source_names.len() {
            return Err(error(VerifyErrorKind::SourceOutOfRange {
                source: function.source,
            }));
        }
        if function.nodes.len() != function.spans.len()
            || function.nodes.len() != function.synthetic.len()
            || function.nodes.len() != function.safepoints.len()
        {
            return Err(error(VerifyErrorKind::SideVectorLength {
                nodes: function.nodes.len(),
                spans: function.spans.len(),
                synthetic: function.synthetic.len(),
                safepoints: function.safepoints.len(),
            }));
        }
        for span in &function.spans {
            if span.start > span.end {
                return Err(error(VerifyErrorKind::SpanRange { span: *span }));
            }
        }
        for (node, origin) in function.synthetic.iter().enumerate() {
            if let Some(origin) = origin {
                if origin.index() >= function.nodes.len() {
                    return Err(error(VerifyErrorKind::OutOfRange {
                        arena: Arena::Node,
                        id: origin.0,
                    }));
                }
                if origin.index() == node {
                    return Err(error(VerifyErrorKind::Tree {
                        node: IrId::new(node as u32),
                        context: "is its own synthetic origin",
                        other: Some(*origin),
                    }));
                }
            }
        }
        if function.slot_info.len() != function.locals as usize {
            return Err(error(VerifyErrorKind::LocalsMismatch {
                locals: function.locals,
                slots: function.slot_info.len(),
            }));
        }
        self.ty_id(function.ret, "function return")?;
        for (index, slot) in function.slot_info.iter().enumerate() {
            self.ty_id(slot.ty, "slot type")?;
            let _ = index;
        }
        let mut param_slots = Vec::new();
        for (index, param) in function.params.iter().enumerate() {
            self.check_param(function, param, index as u32, &mut param_slots)?;
        }
        for (index, capture) in function.captures.iter().enumerate() {
            self.ty_id(capture.ty, "capture type")?;
            let _ = index;
        }
        for (index, pattern) in function.patterns.iter().enumerate() {
            self.check_pattern_structure(function, PatternId::new(index as u32), pattern)?;
        }
        for (index, set) in function.matches.iter().enumerate() {
            self.check_match_set(function, MatchSetId::new(index as u32), set)?;
        }
        for (index, layout) in function.struct_layouts.iter().enumerate() {
            self.check_layout(function, StructLayoutId::new(index as u32), layout)?;
        }
        for (index, part) in function.str_parts.iter().enumerate() {
            match part {
                StrPart::Text(str) => self.check_str(*str)?,
                StrPart::Expr(expr) => {
                    if expr.index() >= function.nodes.len() {
                        return Err(error(VerifyErrorKind::OutOfRange {
                            arena: Arena::Node,
                            id: expr.0,
                        }));
                    }
                }
            }
            let _ = index;
        }
        for (index, node) in function.nodes.iter().enumerate() {
            self.ty_id(node.ty, "node type")?;
            let _ = index;
        }

        let expected_body = !matches!(function.kind, FnKind::Host(_) | FnKind::Hook);
        if expected_body && function.body.is_none() {
            return Err(error(VerifyErrorKind::BodyPresence {
                expected: true,
                found: false,
            }));
        }
        if matches!(function.kind, FnKind::Host(_)) && function.body.is_some() {
            return Err(error(VerifyErrorKind::BodyPresence {
                expected: false,
                found: true,
            }));
        }
        if let Some(body) = function.body {
            if body.index() >= function.nodes.len() {
                return Err(error(VerifyErrorKind::OutOfRange {
                    arena: Arena::Node,
                    id: body.0,
                }));
            }
        } else if !function.nodes.is_empty() {
            return Err(error(VerifyErrorKind::Tree {
                node: IrId::new(0),
                context: "body is absent but the function owns nodes",
                other: None,
            }));
        }
        if let FnKind::Host(host_id) = function.kind {
            let Some(host) = self.program.host(host_id) else {
                return Err(error(VerifyErrorKind::HostOutOfRange { host: host_id }));
            };
            if host.params.len() != function.params.len() {
                return Err(error(VerifyErrorKind::HostSignature {
                    host: host_id,
                    context: "parameter count",
                }));
            }
            for (param, ty) in function.params.iter().zip(&host.params) {
                if !type_accepts(self.program, param.ty, *ty)? {
                    return Err(error(VerifyErrorKind::HostSignature {
                        host: host_id,
                        context: "parameter type",
                    }));
                }
            }
            if !type_accepts(self.program, function.ret, host.ret)? {
                return Err(error(VerifyErrorKind::HostSignature {
                    host: host_id,
                    context: "return type",
                }));
            }
        }
        if let FnKind::Method(type_id) = function.kind {
            self.type_def(type_id, "method owner")?;
            if let Some(self_param) = function.params.first().filter(|param| param.is_self) {
                let actual = self.ty(self_param.ty)?.clone();
                let valid = match actual {
                    Ty::Named(id, _) => id == type_id,
                    Ty::Param(_) => true,
                    _ => false,
                };
                if !valid {
                    return Err(error(VerifyErrorKind::Param {
                        index: 0,
                        context: "method self type",
                    }));
                }
            }
        }
        Ok(())
    }

    fn check_param(
        &self,
        function: &IrFn,
        param: &ParamInfo,
        index: u32,
        seen: &mut Vec<Slot>,
    ) -> Result<(), VerifyError> {
        let error = |kind| verify_error(function, kind);
        if param.slot.index() >= function.locals as usize {
            return Err(error(VerifyErrorKind::SlotInfo {
                slot: param.slot,
                context: "parameter slot",
            }));
        }
        if seen.contains(&param.slot) {
            return Err(error(VerifyErrorKind::Param {
                index,
                context: "duplicate parameter slot",
            }));
        }
        seen.push(param.slot);
        let slot = &function.slot_info[param.slot.index()];
        if !type_accepts(self.program, param.ty, slot.ty)? {
            return Err(error(VerifyErrorKind::Param {
                index,
                context: "parameter type does not match its slot",
            }));
        }
        if param.mutable != slot.mutable {
            return Err(error(VerifyErrorKind::Param {
                index,
                context: "parameter mutability does not match its slot",
            }));
        }
        if param.is_self && index != 0 {
            return Err(error(VerifyErrorKind::Param {
                index,
                context: "self is not the first parameter",
            }));
        }
        if let Some(pattern) = param.pattern
            && pattern.index() >= function.patterns.len()
        {
            return Err(error(VerifyErrorKind::Pattern {
                pattern,
                context: "parameter pattern",
            }));
        }
        Ok(())
    }

    fn check_match_set(
        &self,
        function: &IrFn,
        id: MatchSetId,
        set: &MatchSet,
    ) -> Result<(), VerifyError> {
        let error = |kind| verify_error(function, kind);
        for arm in &set.arms {
            if arm.pattern.index() >= function.patterns.len() {
                return Err(error(VerifyErrorKind::Pattern {
                    pattern: arm.pattern,
                    context: "match arm",
                }));
            }
            if arm.body.index() >= function.nodes.len() {
                return Err(error(VerifyErrorKind::OutOfRange {
                    arena: Arena::Node,
                    id: arm.body.0,
                }));
            }
            if let Some(guard) = arm.guard
                && guard.index() >= function.nodes.len()
            {
                return Err(error(VerifyErrorKind::OutOfRange {
                    arena: Arena::Node,
                    id: guard.0,
                }));
            }
            let _ = id;
        }
        Ok(())
    }

    fn check_layout(
        &self,
        function: &IrFn,
        id: StructLayoutId,
        layout: &StructLayout,
    ) -> Result<(), VerifyError> {
        let _ = (function, id, layout);
        Ok(())
    }

    fn check_pattern_structure(
        &self,
        function: &IrFn,
        id: PatternId,
        pattern: &Pattern,
    ) -> Result<(), VerifyError> {
        let error = |kind| verify_error(function, kind);
        match pattern {
            Pattern::Wild | Pattern::Int(_) | Pattern::Bool(_) => Ok(()),
            Pattern::Str(string) => {
                self.check_str(*string)?;
                Ok(())
            }
            Pattern::Bind { slot, ty } => {
                self.ty_id(*ty, "pattern binding type")?;
                if slot.index() >= function.locals as usize {
                    return Err(error(VerifyErrorKind::SlotInfo {
                        slot: *slot,
                        context: "pattern binding",
                    }));
                }
                if !type_accepts(self.program, *ty, function.slot_info[slot.index()].ty)? {
                    return Err(error(VerifyErrorKind::PatternShape {
                        pattern: id,
                        context: "binding slot type",
                    }));
                }
                Ok(())
            }
            Pattern::Tuple(items) => {
                for item in items {
                    self.check_pattern_structure(function, id, item)?;
                }
                Ok(())
            }
            Pattern::Variant {
                type_id,
                variant,
                args,
            } => {
                let Some(def) = self.program.types.get(*type_id) else {
                    return Err(error(VerifyErrorKind::TypeTable {
                        context: "pattern variant type",
                        ty: None,
                        def: Some(*type_id),
                    }));
                };
                let TypeDefKind::Enum { variants } = &def.kind else {
                    return Err(error(VerifyErrorKind::PatternShape {
                        pattern: id,
                        context: "variant pattern on a non-enum",
                    }));
                };
                let Some(def_variant) = variants.get(*variant as usize) else {
                    return Err(error(VerifyErrorKind::PatternShape {
                        pattern: id,
                        context: "variant index",
                    }));
                };
                if args.len() != def_variant.payload.len() {
                    return Err(error(VerifyErrorKind::PatternShape {
                        pattern: id,
                        context: "variant payload count",
                    }));
                }
                for item in args {
                    self.check_pattern_structure(function, id, item)?;
                }
                Ok(())
            }
            Pattern::Prelude { variant, args } => {
                let expected = match variant {
                    PreludeVariant::None => 0,
                    PreludeVariant::Some | PreludeVariant::Ok | PreludeVariant::Err => 1,
                };
                if args.len() != expected {
                    return Err(error(VerifyErrorKind::PatternShape {
                        pattern: id,
                        context: "prelude payload count",
                    }));
                }
                for item in args {
                    self.check_pattern_structure(function, id, item)?;
                }
                Ok(())
            }
            Pattern::Struct { type_id, fields } => {
                let Some(def) = self.program.types.get(*type_id) else {
                    return Err(error(VerifyErrorKind::TypeTable {
                        context: "pattern struct type",
                        ty: None,
                        def: Some(*type_id),
                    }));
                };
                let TypeDefKind::Struct { fields: def_fields } = &def.kind else {
                    return Err(error(VerifyErrorKind::PatternShape {
                        pattern: id,
                        context: "struct pattern on a non-struct",
                    }));
                };
                let mut seen = Vec::new();
                for (offset, item) in fields {
                    if *offset as usize >= def_fields.len() || seen.contains(offset) {
                        return Err(error(VerifyErrorKind::PatternShape {
                            pattern: id,
                            context: "struct pattern field",
                        }));
                    }
                    seen.push(*offset);
                    self.check_pattern_structure(function, id, item)?;
                }
                Ok(())
            }
            Pattern::Or(items) => {
                let mut first: Option<Vec<Slot>> = None;
                for item in items {
                    let slots = crate::analysis::pattern_binding_set(item);
                    match &first {
                        None => first = Some(slots),
                        Some(expected) if *expected != slots => {
                            return Err(error(VerifyErrorKind::PatternShape {
                                pattern: id,
                                context: "or-pattern alternatives bind different slots",
                            }));
                        }
                        Some(_) => {}
                    }
                    self.check_pattern_structure(function, id, item)?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TypeState {
    Unvisited,
    Visiting,
    Done,
}

fn verify_error(function: &IrFn, kind: VerifyErrorKind) -> VerifyError {
    VerifyError {
        function: Some(function.id),
        source: Some(function.source),
        node: None,
        span: None,
        kind,
    }
}

fn type_accepts(program: &IrProgram, actual: TyId, expected: TyId) -> Result<bool, VerifyError> {
    let actual = program
        .types
        .get_ty(actual)
        .ok_or_else(|| type_error(actual))?;
    let expected = program
        .types
        .get_ty(expected)
        .ok_or_else(|| type_error(expected))?;
    type_accepts_owned(program, actual, expected)
}

fn type_accepts_owned(
    program: &IrProgram,
    actual: &Ty,
    expected: &Ty,
) -> Result<bool, VerifyError> {
    if matches!(actual, Ty::Never) || matches!(expected, Ty::Never) {
        return Ok(true);
    }
    type_equal(program, actual, expected)
}

fn type_equal(program: &IrProgram, actual: &Ty, expected: &Ty) -> Result<bool, VerifyError> {
    Ok(match (actual, expected) {
        (Ty::Int, Ty::Int)
        | (Ty::Float, Ty::Float)
        | (Ty::Bool, Ty::Bool)
        | (Ty::Str, Ty::Str)
        | (Ty::Unit, Ty::Unit)
        | (Ty::Range, Ty::Range)
        | (Ty::Never, Ty::Never) => true,
        (Ty::Param(a), Ty::Param(b)) => a == b,
        (Ty::Tuple(a), Ty::Tuple(b)) => {
            if a.len() != b.len() {
                false
            } else {
                for (a, b) in a.iter().zip(b) {
                    if !type_equal(
                        program,
                        program.types.get_ty(*a).ok_or_else(|| type_error(*a))?,
                        program.types.get_ty(*b).ok_or_else(|| type_error(*b))?,
                    )? {
                        return Ok(false);
                    }
                }
                true
            }
        }
        (Ty::List(a), Ty::List(b)) | (Ty::Option(a), Ty::Option(b)) => type_equal(
            program,
            program.types.get_ty(*a).ok_or_else(|| type_error(*a))?,
            program.types.get_ty(*b).ok_or_else(|| type_error(*b))?,
        )?,
        (Ty::Map(ak, av), Ty::Map(bk, bv)) | (Ty::Result(ak, av), Ty::Result(bk, bv)) => {
            type_equal(
                program,
                program.types.get_ty(*ak).ok_or_else(|| type_error(*ak))?,
                program.types.get_ty(*bk).ok_or_else(|| type_error(*bk))?,
            )? && type_equal(
                program,
                program.types.get_ty(*av).ok_or_else(|| type_error(*av))?,
                program.types.get_ty(*bv).ok_or_else(|| type_error(*bv))?,
            )?
        }
        (Ty::Fn(ap, ar), Ty::Fn(bp, br)) => {
            if ap.len() != bp.len() {
                false
            } else {
                for (a, b) in ap.iter().zip(bp) {
                    if !type_equal(
                        program,
                        program.types.get_ty(*a).ok_or_else(|| type_error(*a))?,
                        program.types.get_ty(*b).ok_or_else(|| type_error(*b))?,
                    )? {
                        return Ok(false);
                    }
                }
                type_equal(
                    program,
                    program.types.get_ty(*ar).ok_or_else(|| type_error(*ar))?,
                    program.types.get_ty(*br).ok_or_else(|| type_error(*br))?,
                )?
            }
        }
        (Ty::Named(aid, aargs), Ty::Named(bid, bargs)) => {
            if aid != bid || aargs.len() != bargs.len() {
                false
            } else {
                for (a, b) in aargs.iter().zip(bargs) {
                    if !type_equal(
                        program,
                        program.types.get_ty(*a).ok_or_else(|| type_error(*a))?,
                        program.types.get_ty(*b).ok_or_else(|| type_error(*b))?,
                    )? {
                        return Ok(false);
                    }
                }
                true
            }
        }
        _ => false,
    })
}

fn type_error(id: TyId) -> VerifyError {
    VerifyError {
        function: None,
        source: None,
        node: None,
        span: None,
        kind: VerifyErrorKind::OutOfRange {
            arena: Arena::Type,
            id: id.0,
        },
    }
}

// The type/tree/charge and effects checks are below.

const MAX_VERIFY_DEPTH: u32 = 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum VisitState {
    Unvisited,
    OnStack,
    Done,
}

struct FnCheck<'p> {
    program: &'p IrProgram,
    function: &'p IrFn,
    states: Vec<VisitState>,
    stack: Vec<IrId>,
    loop_depth: u32,
    depth: u32,
}

impl<'p> FnCheck<'p> {
    fn new(program: &'p IrProgram, function: &'p IrFn) -> Self {
        Self {
            program,
            function,
            states: vec![VisitState::Unvisited; function.nodes.len()],
            stack: Vec::new(),
            loop_depth: 0,
            depth: 0,
        }
    }

    fn run(&mut self) -> Result<(), VerifyError> {
        let body = self.function.body.expect("shape check requires a body");
        if !matches!(self.function.kind, FnKind::Lambda)
            && !matches!(
                self.function.node(body).map(|node| node.kind),
                Some(NodeKind::Block(_))
            )
        {
            let found = self
                .function
                .node(body)
                .map(|node| node.kind)
                .unwrap_or(NodeKind::Unit);
            return Err(self.error(
                Some(body),
                VerifyErrorKind::WrongRole {
                    expected: NodeRole::Body,
                    found,
                },
            ));
        }

        let body_ty = self.check_node(body, NodeRole::Body)?;
        if !self.function.is_safepoint(body) {
            return Err(self.error(
                Some(body),
                VerifyErrorKind::Safepoint {
                    node: body,
                    expected: true,
                },
            ));
        }
        self.expect_ty_id(body_ty, self.function.ret, "function body type")?;

        for (index, state) in self.states.iter().enumerate() {
            if *state == VisitState::Unvisited {
                return Err(self.error(
                    Some(IrId::new(index as u32)),
                    VerifyErrorKind::Tree {
                        node: IrId::new(index as u32),
                        context: "is not reachable from the body",
                        other: Some(body),
                    },
                ));
            }
        }

        if let Err(error) = DefiniteAssignment::compute(self.program, self.function) {
            return Err(self.error(
                Some(error.node),
                VerifyErrorKind::UnboundSlot {
                    slot: error.slot,
                    context: error.context,
                },
            ));
        }
        if let Err(error) = check_slot_initialization(self.program, self.function) {
            return Err(VerifyError {
                function: Some(self.function.id),
                source: Some(self.function.source),
                node: error.node,
                span: error.node.and_then(|node| self.function.span(node)),
                kind: VerifyErrorKind::SlotBinding {
                    slot: error.slot,
                    context: error.context,
                },
            });
        }
        Ok(())
    }

    fn error(&self, node: Option<IrId>, kind: VerifyErrorKind) -> VerifyError {
        let span = node.and_then(|id| self.function.span(id));
        VerifyError {
            function: Some(self.function.id),
            source: Some(self.function.source),
            node,
            span,
            kind,
        }
    }

    fn node(&self, id: IrId) -> Result<Node, VerifyError> {
        self.function.node(id).copied().ok_or_else(|| {
            self.error(
                Some(id),
                VerifyErrorKind::OutOfRange {
                    arena: Arena::Node,
                    id: id.0,
                },
            )
        })
    }

    fn ty(&self, id: TyId) -> Result<Ty, VerifyError> {
        self.program.types.get_ty(id).cloned().ok_or_else(|| {
            self.error(
                None,
                VerifyErrorKind::OutOfRange {
                    arena: Arena::Type,
                    id: id.0,
                },
            )
        })
    }

    fn check_node(&mut self, id: IrId, role: NodeRole) -> Result<TyId, VerifyError> {
        let node = self.node(id)?;
        let index = id.index();
        match self.states[index] {
            VisitState::OnStack => {
                return Err(self.error(
                    Some(id),
                    VerifyErrorKind::Tree {
                        node: id,
                        context: "is part of a cycle",
                        other: self.stack.last().copied(),
                    },
                ));
            }
            VisitState::Done => {
                return Err(self.error(
                    Some(id),
                    VerifyErrorKind::Tree {
                        node: id,
                        context: "has more than one parent",
                        other: self.stack.last().copied(),
                    },
                ));
            }
            VisitState::Unvisited => {}
        }

        self.states[index] = VisitState::OnStack;
        self.stack.push(id);
        self.depth += 1;
        if self.depth > MAX_VERIFY_DEPTH {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::ControlFlow {
                    context: "IR nesting exceeds the verifier depth limit",
                },
            ));
        }

        let result = self.check_node_inner(id, role, node);
        self.depth -= 1;
        self.stack.pop();
        if result.is_ok() {
            self.states[index] = VisitState::Done;
        }
        result
    }

    fn check_node_inner(
        &mut self,
        id: IrId,
        role: NodeRole,
        node: Node,
    ) -> Result<TyId, VerifyError> {
        let role = match role {
            NodeRole::Condition
            | NodeRole::Receiver
            | NodeRole::Scrutinee
            | NodeRole::Guard
            | NodeRole::Tail => NodeRole::Value,
            role => role,
        };
        self.check_charge(id, role, &node)?;

        let computed = match role {
            NodeRole::Value => self.check_value_node(id, node)?,
            NodeRole::Place => self.check_place_node(id, node)?,
            NodeRole::Statement => self.check_statement_node(id, node)?,
            NodeRole::Argument => self.check_argument_node(id, node)?,
            NodeRole::Body => match node.kind {
                NodeKind::Block(_) | NodeKind::ExprBlock(_) => self.check_block_node(id, node)?,
                _ if matches!(self.function.kind, FnKind::Lambda) => {
                    self.check_value_node(id, node)?
                }
                _ => {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::WrongRole {
                            expected: NodeRole::Body,
                            found: node.kind,
                        },
                    ));
                }
            },
            NodeRole::Condition
            | NodeRole::Receiver
            | NodeRole::Scrutinee
            | NodeRole::Guard
            | NodeRole::Tail => unreachable!(),
        };
        self.expect_ty_value(node.ty, &computed, "node type")?;
        Ok(node.ty)
    }

    fn check_argument_node(&mut self, id: IrId, node: Node) -> Result<Ty, VerifyError> {
        let NodeKind::MutArg(place) = node.kind else {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::WrongRole {
                    expected: NodeRole::Argument,
                    found: node.kind,
                },
            ));
        };
        let place_ty = self.check_node(place, NodeRole::Place)?;
        self.ty(place_ty)
    }

    fn check_charge(&self, id: IrId, role: NodeRole, node: &Node) -> Result<(), VerifyError> {
        let expected = self.expected_charge(node, role)?;
        if node.charge != expected {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::Charge {
                    expected,
                    found: node.charge,
                },
            ));
        }
        let required = id == self.function.body.expect("body")
            || matches!(
                node.kind,
                NodeKind::Let { .. }
                    | NodeKind::Assign { .. }
                    | NodeKind::AddAssign { .. }
                    | NodeKind::SubAssign { .. }
                    | NodeKind::MulAssign { .. }
                    | NodeKind::DivAssign { .. }
                    | NodeKind::RemAssign { .. }
                    | NodeKind::ExprStmt(_)
                    | NodeKind::Return(_)
            );
        if self.function.is_safepoint(id) != required {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::Safepoint {
                    node: id,
                    expected: required,
                },
            ));
        }
        Ok(())
    }

    fn expected_charge(&self, node: &Node, role: NodeRole) -> Result<Charge, VerifyError> {
        let pre = match role {
            NodeRole::Value => 1,
            NodeRole::Place | NodeRole::Statement | NodeRole::Argument => 0,
            NodeRole::Body => match node.kind {
                NodeKind::ExprBlock(_) => 1,
                NodeKind::Block(_) => 0,
                _ => 1,
            },
            NodeRole::Condition
            | NodeRole::Receiver
            | NodeRole::Scrutinee
            | NodeRole::Guard
            | NodeRole::Tail => 1,
        };
        let per_iter = match node.kind {
            NodeKind::While { .. } | NodeKind::For { .. } => 1,
            _ => 0,
        };
        let post = match node.kind {
            NodeKind::Tuple(range) | NodeKind::List(range) => range.len,
            NodeKind::Map(range) => range.len,
            NodeKind::Struct { type_id, .. } | NodeKind::StructUpdate { type_id, .. } => self
                .program
                .types
                .get(type_id)
                .and_then(|def| match &def.kind {
                    TypeDefKind::Struct { fields } => Some(fields.len() as u32),
                    _ => None,
                })
                .unwrap_or(0),
            NodeKind::EnumVariant { variant, .. } => {
                let ty = self.ty(node.ty)?;
                match ty {
                    Ty::Named(type_id, _) => self
                        .program
                        .types
                        .get(type_id)
                        .and_then(|def| match &def.kind {
                            TypeDefKind::Enum { variants } => variants
                                .get(variant as usize)
                                .map(|variant| variant.payload.len() as u32),
                            _ => None,
                        })
                        .unwrap_or(0),
                    _ => 0,
                }
            }
            NodeKind::PreludeVariant { variant, .. } => match variant {
                PreludeVariant::None => 0,
                PreludeVariant::Some | PreludeVariant::Ok | PreludeVariant::Err => 1,
            },
            _ => 0,
        };
        Ok(Charge::new(pre, post, per_iter))
    }

    fn expect_ty_id(
        &self,
        actual: TyId,
        expected: TyId,
        context: &'static str,
    ) -> Result<(), VerifyError> {
        let actual_ty = self.ty(actual)?;
        let expected_ty = self.ty(expected)?;
        if type_accepts_owned(self.program, &actual_ty, &expected_ty)? {
            Ok(())
        } else {
            Err(self.error(
                None,
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(expected_ty),
                    found: Box::new(actual_ty),
                    context,
                },
            ))
        }
    }

    fn expect_ty_value(
        &self,
        actual_id: TyId,
        expected: &Ty,
        context: &'static str,
    ) -> Result<(), VerifyError> {
        let actual = self.ty(actual_id)?;
        if type_accepts_owned(self.program, &actual, expected)? {
            Ok(())
        } else {
            Err(self.error(
                None,
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(expected.clone()),
                    found: Box::new(actual),
                    context,
                },
            ))
        }
    }

    fn expect_ty_id_value(
        &self,
        actual_id: TyId,
        expected_id: TyId,
        context: &'static str,
    ) -> Result<(), VerifyError> {
        let expected = self.ty(expected_id)?;
        self.expect_ty_value(actual_id, &expected, context)
    }

    fn check_block_node(&mut self, id: IrId, node: Node) -> Result<Ty, VerifyError> {
        let range = match node.kind {
            NodeKind::Block(range) | NodeKind::ExprBlock(range) => range,
            _ => {
                return Err(self.error(
                    Some(id),
                    VerifyErrorKind::WrongRole {
                        expected: NodeRole::Body,
                        found: node.kind,
                    },
                ));
            }
        };
        let children = self.range(range)?;
        let mut last_value = None;
        for (index, child) in children.iter().enumerate() {
            self.check_node(*child, NodeRole::Statement)?;
            if index + 1 == children.len() {
                let child_node = self.node(*child)?;
                if let NodeKind::ExprStmt(inner) = child_node.kind {
                    last_value = Some(self.node(inner)?.ty);
                }
            }
        }
        match last_value {
            Some(ty) => self.ty(ty),
            None => Ok(Ty::Unit),
        }
    }

    fn range(&self, range: ChildRange) -> Result<Vec<IrId>, VerifyError> {
        let end = range.start as usize + range.len as usize;
        if end > self.function.children.len() {
            return Err(self.error(
                None,
                VerifyErrorKind::ChildRange {
                    range,
                    len: self.function.children.len() as u32,
                },
            ));
        }
        Ok(self.function.children[range.start as usize..end].to_vec())
    }

    fn str_parts(&self, range: ChildRange) -> Result<Vec<StrPart>, VerifyError> {
        let end = range.start as usize + range.len as usize;
        if end > self.function.str_parts.len() {
            return Err(self.error(
                None,
                VerifyErrorKind::ChildRange {
                    range,
                    len: self.function.str_parts.len() as u32,
                },
            ));
        }
        Ok(self.function.str_parts[range.start as usize..end].to_vec())
    }

    fn check_place_node(&mut self, id: IrId, node: Node) -> Result<Ty, VerifyError> {
        match node.kind {
            NodeKind::SlotPlace(slot) => self.slot_type(slot),
            NodeKind::Field { base, index } => {
                let base_ty = self.check_node(base, NodeRole::Place)?;
                self.field_type(base_ty, index, false)
            }
            NodeKind::TupleField { base, index } => {
                let base_ty = self.check_node(base, NodeRole::Place)?;
                self.field_type(base_ty, index, true)
            }
            NodeKind::Index { base, index } => {
                let base_ty = self.check_node(base, NodeRole::Place)?;
                let index_ty = self.check_node(index, NodeRole::Value)?;
                self.place_index_type(base_ty, index_ty)
            }
            _ => Err(self.error(
                Some(id),
                VerifyErrorKind::WrongRole {
                    expected: NodeRole::Place,
                    found: node.kind,
                },
            )),
        }
    }

    fn slot_type(&self, slot: Slot) -> Result<Ty, VerifyError> {
        let Some(info) = self.function.slot_info.get(slot.index()) else {
            return Err(self.error(
                None,
                VerifyErrorKind::SlotInfo {
                    slot,
                    context: "slot read",
                },
            ));
        };
        self.ty(info.ty)
    }

    fn field_type(&self, base: TyId, index: u32, tuple: bool) -> Result<Ty, VerifyError> {
        let base_ty = self.ty(base)?;
        if tuple {
            let Ty::Tuple(items) = base_ty else {
                return Err(self.error(
                    None,
                    VerifyErrorKind::Place {
                        context: "tuple field on a non-tuple",
                    },
                ));
            };
            let Some(item) = items.get(index as usize) else {
                return Err(self.error(
                    None,
                    VerifyErrorKind::Path {
                        index,
                        context: "tuple field",
                    },
                ));
            };
            return self.ty(*item);
        }
        let Ty::Named(type_id, args) = base_ty else {
            return Err(self.error(
                None,
                VerifyErrorKind::Place {
                    context: "field on a non-struct",
                },
            ));
        };
        let Some(def) = self.program.types.get(type_id) else {
            return Err(self.error(
                None,
                VerifyErrorKind::TypeTable {
                    context: "field owner",
                    ty: None,
                    def: Some(type_id),
                },
            ));
        };
        let TypeDefKind::Struct { fields } = &def.kind else {
            return Err(self.error(
                None,
                VerifyErrorKind::Place {
                    context: "field on a non-struct",
                },
            ));
        };
        let Some(field) = fields.get(index as usize) else {
            return Err(self.error(
                None,
                VerifyErrorKind::Path {
                    index,
                    context: "struct field",
                },
            ));
        };
        Ok(self.substitute(field.ty, &args))
    }

    fn place_index_type(&self, base: TyId, index: TyId) -> Result<Ty, VerifyError> {
        let base_ty = self.ty(base)?;
        let index_ty = self.ty(index)?;
        match base_ty {
            Ty::List(item) => {
                if type_accepts_owned(self.program, &index_ty, &Ty::Int)? {
                    self.ty(item)
                } else {
                    Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Int),
                            found: Box::new(index_ty),
                            context: "list index",
                        },
                    ))
                }
            }
            Ty::Map(key, value) => {
                if type_accepts_owned(self.program, &index_ty, &self.ty(key)?)? {
                    self.ty(value)
                } else {
                    Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(self.ty(key)?),
                            found: Box::new(index_ty),
                            context: "map key",
                        },
                    ))
                }
            }
            _ => Err(self.error(
                None,
                VerifyErrorKind::Place {
                    context: "index place on a non-container",
                },
            )),
        }
    }

    fn value_index_type(&self, base: TyId, index: TyId) -> Result<Ty, VerifyError> {
        let base_ty = self.ty(base)?;
        let index_ty = self.ty(index)?;
        match base_ty {
            Ty::List(item) => {
                if type_accepts_owned(self.program, &index_ty, &Ty::Int)? {
                    self.ty(item)
                } else if type_accepts_owned(self.program, &index_ty, &Ty::Range)? {
                    Ok(Ty::List(item))
                } else {
                    Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Int),
                            found: Box::new(index_ty),
                            context: "list index",
                        },
                    ))
                }
            }
            Ty::Map(key, value) => {
                if type_accepts_owned(self.program, &index_ty, &self.ty(key)?)? {
                    self.ty(value)
                } else {
                    Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(self.ty(key)?),
                            found: Box::new(index_ty),
                            context: "map key",
                        },
                    ))
                }
            }
            Ty::Str => {
                if type_accepts_owned(self.program, &index_ty, &Ty::Range)? {
                    Ok(Ty::Str)
                } else {
                    Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Range),
                            found: Box::new(index_ty),
                            context: "string index",
                        },
                    ))
                }
            }
            _ => Err(self.error(
                None,
                VerifyErrorKind::Place {
                    context: "index on a non-container",
                },
            )),
        }
    }

    fn substitute(&self, ty: TyId, args: &[TyId]) -> Ty {
        let Some(value) = self.program.types.get_ty(ty) else {
            return Ty::Never;
        };
        match value {
            Ty::Param(index) => args
                .get(*index as usize)
                .and_then(|id| self.program.types.get_ty(*id))
                .cloned()
                .unwrap_or(Ty::Never),
            Ty::Tuple(items) => Ty::Tuple(
                items
                    .iter()
                    .map(|item| self.substitute_id(*item, args))
                    .collect(),
            ),
            Ty::List(item) => Ty::List(self.substitute_id(*item, args)),
            Ty::Map(key, value) => Ty::Map(
                self.substitute_id(*key, args),
                self.substitute_id(*value, args),
            ),
            Ty::Option(item) => Ty::Option(self.substitute_id(*item, args)),
            Ty::Result(ok, err) => Ty::Result(
                self.substitute_id(*ok, args),
                self.substitute_id(*err, args),
            ),
            Ty::Fn(params, ret) => Ty::Fn(
                params
                    .iter()
                    .map(|param| self.substitute_id(*param, args))
                    .collect(),
                self.substitute_id(*ret, args),
            ),
            Ty::Named(type_id, type_args) => Ty::Named(
                *type_id,
                type_args
                    .iter()
                    .map(|arg| self.substitute_id(*arg, args))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    fn substitute_id(&self, ty: TyId, args: &[TyId]) -> TyId {
        match self.program.types.get_ty(ty) {
            Some(Ty::Param(index)) => args.get(*index as usize).copied().unwrap_or(ty),
            _ => ty,
        }
    }

    fn check_value_node(&mut self, id: IrId, node: Node) -> Result<Ty, VerifyError> {
        match node.kind {
            NodeKind::Unit => Ok(Ty::Unit),
            NodeKind::Int(constant) => {
                self.check_const_kind(constant, "int motif", |value| {
                    matches!(value, ConstValue::Int(_))
                })?;
                Ok(Ty::Int)
            }
            NodeKind::Float(constant) => {
                self.check_const_kind(constant, "float motif", |value| {
                    matches!(value, ConstValue::Float(_))
                })?;
                Ok(Ty::Float)
            }
            NodeKind::Bool(_) => Ok(Ty::Bool),
            NodeKind::Str(string) => {
                self.check_str_id(string)?;
                Ok(Ty::Str)
            }
            NodeKind::ConstRef(constant) => {
                let value = self.const_value(constant)?;
                let ty = self.ty(node.ty)?;
                if self.const_matches(&ty, value)? {
                    Ok(ty)
                } else {
                    Err(self.error(
                        Some(id),
                        VerifyErrorKind::Const {
                            id: constant,
                            context: "constant does not match the node type",
                        },
                    ))
                }
            }
            NodeKind::FnRef(target) => self.fn_ref_type(target, node.ty),
            NodeKind::BuiltinFnRef(builtin) => self.builtin_fn_type(builtin, node.ty),
            NodeKind::BuiltinModuleRef(item) => self.builtin_item_type(item, node.ty),
            NodeKind::SlotRead { slot, .. } => self.slot_type(slot),
            NodeKind::CaptureRead(index) => {
                let Some(capture) = self.function.captures.get(index.index()) else {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::Capture {
                            index,
                            context: "capture read",
                        },
                    ));
                };
                self.ty(capture.ty)
            }
            NodeKind::Field { base, index } => {
                let base_ty = self.check_node(base, NodeRole::Value)?;
                self.field_type(base_ty, index, false)
            }
            NodeKind::TupleField { base, index } => {
                let base_ty = self.check_node(base, NodeRole::Value)?;
                self.field_type(base_ty, index, true)
            }
            NodeKind::Index { base, index } => {
                let base_ty = self.check_node(base, NodeRole::Value)?;
                let index_ty = self.check_node(index, NodeRole::Value)?;
                self.value_index_type(base_ty, index_ty)
            }
            NodeKind::Neg(value) => {
                let ty = self.check_node(value, NodeRole::Value)?;
                let ty = self.ty(ty)?;
                match ty {
                    Ty::Int | Ty::Float | Ty::Param(_) | Ty::Never => Ok(ty),
                    other => Err(self.error(
                        Some(id),
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Int),
                            found: Box::new(other),
                            context: "unary negation",
                        },
                    )),
                }
            }
            NodeKind::Not(value) => {
                self.expect_child_type(value, &Ty::Bool, "logical negation")?;
                Ok(Ty::Bool)
            }
            NodeKind::Add { lhs, rhs }
            | NodeKind::Sub { lhs, rhs }
            | NodeKind::Mul { lhs, rhs }
            | NodeKind::Div { lhs, rhs }
            | NodeKind::Rem { lhs, rhs }
            | NodeKind::Eq { lhs, rhs }
            | NodeKind::Ne { lhs, rhs }
            | NodeKind::Lt { lhs, rhs }
            | NodeKind::Le { lhs, rhs }
            | NodeKind::Gt { lhs, rhs }
            | NodeKind::Ge { lhs, rhs }
            | NodeKind::And { lhs, rhs }
            | NodeKind::Or { lhs, rhs }
            | NodeKind::Coalesce { lhs, rhs } => {
                let op = node.kind.binary_op().expect("binary node");
                self.check_binary(op, lhs, rhs)
            }
            NodeKind::CallDirect { target, first_arg } => {
                self.check_direct_call(target, first_arg, node.ty)
            }
            NodeKind::CallMethod {
                target,
                first_child,
            } => self.check_method_call(target, first_child, node.ty),
            NodeKind::CallValue { first_child, argc } => {
                self.check_value_call(first_child, argc, node.ty)
            }
            NodeKind::CallBuiltinFn { builtin, first_arg } => {
                self.check_builtin_fn_call(builtin, first_arg, node.ty)
            }
            NodeKind::CallBuiltinMethod {
                method,
                first_child,
            } => self.check_builtin_method_call(method, first_child, node.ty),
            NodeKind::CallBuiltinModule { item, first_arg } => {
                self.check_builtin_item_call(item, first_arg, node.ty)
            }
            NodeKind::CallHost { host, first_arg } => {
                self.check_host_call(host, first_arg, node.ty)
            }
            NodeKind::Struct {
                type_id,
                first_field,
            } => self.check_struct_value(type_id, first_field, node.ty, id),
            NodeKind::StructUpdate { type_id, layout } => {
                self.check_struct_update(type_id, layout, node.ty, id)
            }
            NodeKind::EnumVariant {
                variant,
                first_payload,
            } => self.check_enum_variant(variant, first_payload, node.ty, id),
            NodeKind::PreludeVariant {
                variant,
                first_payload,
            } => self.check_prelude_variant(variant, first_payload, node.ty, id),
            NodeKind::Tuple(range) => {
                let ty = self.ty(node.ty)?;
                let Ty::Tuple(items) = ty else {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Tuple(Vec::new())),
                            found: Box::new(ty),
                            context: "tuple literal",
                        },
                    ));
                };
                let children = self.range(range)?;
                if children.len() != items.len() {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::Arity {
                            expected: items.len() as u32,
                            found: children.len() as u32,
                            context: "tuple literal",
                        },
                    ));
                }
                for (child, expected) in children.iter().zip(&items) {
                    self.expect_child_type(*child, &self.ty(*expected)?, "tuple element")?;
                }
                Ok(Ty::Tuple(items))
            }
            NodeKind::List(range) => {
                let ty = self.ty(node.ty)?;
                let Ty::List(item) = ty else {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::List(node.ty)),
                            found: Box::new(ty),
                            context: "list literal",
                        },
                    ));
                };
                let expected = self.ty(item)?;
                for child in self.range(range)? {
                    self.expect_child_type(child, &expected, "list element")?;
                }
                Ok(Ty::List(item))
            }
            NodeKind::Map(range) => {
                let ty = self.ty(node.ty)?;
                let Ty::Map(key, value) = ty else {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Map(node.ty, node.ty)),
                            found: Box::new(ty),
                            context: "map literal",
                        },
                    ));
                };
                let children = self.range(range)?;
                if children.len() % 2 != 0 {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::Arity {
                            expected: children.len() as u32 + 1,
                            found: children.len() as u32,
                            context: "map literal key/value pairs",
                        },
                    ));
                }
                let key_ty = self.ty(key)?;
                let value_ty = self.ty(value)?;
                for pair in children.chunks(2) {
                    self.expect_child_type(pair[0], &key_ty, "map key")?;
                    self.expect_child_type(pair[1], &value_ty, "map value")?;
                }
                Ok(Ty::Map(key, value))
            }
            NodeKind::Range { start, end } | NodeKind::InclusiveRange { start, end } => {
                self.expect_child_type(start, &Ty::Int, "range start")?;
                self.expect_child_type(end, &Ty::Int, "range end")?;
                Ok(Ty::Range)
            }
            NodeKind::Block(_) => Err(self.error(
                Some(id),
                VerifyErrorKind::WrongRole {
                    expected: NodeRole::Value,
                    found: node.kind,
                },
            )),
            NodeKind::ExprBlock(_) => self.check_block_node(id, node),
            NodeKind::If(range) => self.check_if(id, range),
            NodeKind::Match {
                match_set,
                scrutinee,
            } => self.check_match(id, match_set, scrutinee),
            NodeKind::Try(inner) => self.check_try(id, inner),
            NodeKind::ConvertInt(inner) => {
                self.expect_child_type(inner, &Ty::Float, "int conversion")?;
                Ok(Ty::Int)
            }
            NodeKind::ConvertFloat(inner) => {
                self.expect_child_type(inner, &Ty::Int, "float conversion")?;
                Ok(Ty::Float)
            }
            NodeKind::ConvertStr(inner) => {
                self.check_node(inner, NodeRole::Value)?;
                Ok(Ty::Str)
            }
            NodeKind::MakeClosure(target) => self.make_closure_type(target, node.ty),
            NodeKind::StrInterp(range) => {
                let parts = self.str_parts(range)?;
                for part in parts {
                    if let StrPart::Expr(inner) = part {
                        self.check_node(inner, NodeRole::Value)?;
                    }
                }
                Ok(Ty::Str)
            }
            NodeKind::Assert(range) => {
                let children = self.range(range)?;
                if children.len() != 2 {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::Arity {
                            expected: 2,
                            found: children.len() as u32,
                            context: "assert",
                        },
                    ));
                }
                self.expect_child_type(children[0], &Ty::Bool, "assert condition")?;
                self.expect_child_type(children[1], &Ty::Str, "assert message")?;
                Ok(Ty::Unit)
            }
            NodeKind::Panic(range) => {
                let children = self.range(range)?;
                if children.len() != 1 {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::Arity {
                            expected: 1,
                            found: children.len() as u32,
                            context: "panic",
                        },
                    ));
                }
                self.expect_child_type(children[0], &Ty::Str, "panic message")?;
                Ok(Ty::Never)
            }
            NodeKind::SlotPlace(_)
            | NodeKind::SlotWrite { .. }
            | NodeKind::SlotInit { .. }
            | NodeKind::While { .. }
            | NodeKind::For { .. }
            | NodeKind::Break
            | NodeKind::Continue
            | NodeKind::Return(_)
            | NodeKind::Let { .. }
            | NodeKind::Assign { .. }
            | NodeKind::AddAssign { .. }
            | NodeKind::SubAssign { .. }
            | NodeKind::MulAssign { .. }
            | NodeKind::DivAssign { .. }
            | NodeKind::RemAssign { .. }
            | NodeKind::ExprStmt(_)
            | NodeKind::MutArg(_)
            | NodeKind::Charge(_) => Err(self.error(
                Some(id),
                VerifyErrorKind::WrongRole {
                    expected: NodeRole::Value,
                    found: node.kind,
                },
            )),
        }
    }

    fn check_statement_node(&mut self, id: IrId, node: Node) -> Result<Ty, VerifyError> {
        match node.kind {
            NodeKind::Block(_) => self.check_block_node(id, node),
            NodeKind::Let { pattern, value } => {
                let value_ty = self.check_node(value, NodeRole::Value)?;
                let value_ty = self.ty(value_ty)?;
                self.check_pattern_against(pattern, &value_ty)?;
                Ok(Ty::Unit)
            }
            NodeKind::Assign { place, value } => {
                let place_ty = self.check_node(place, NodeRole::Place)?;
                let (root, mutable) = self.place_root(place)?;
                let _ = root;
                if !mutable {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::Place {
                            context: "assignment to an immutable root",
                        },
                    ));
                }
                let value_ty = self.check_node(value, NodeRole::Value)?;
                self.expect_ty_id_value(value_ty, place_ty, "assignment value")?;
                Ok(Ty::Unit)
            }
            NodeKind::AddAssign { place, value }
            | NodeKind::SubAssign { place, value }
            | NodeKind::MulAssign { place, value }
            | NodeKind::DivAssign { place, value }
            | NodeKind::RemAssign { place, value } => {
                let op = node.kind.compound_op().expect("compound assignment");
                let place_ty = self.check_node(place, NodeRole::Place)?;
                let (root, mutable) = self.place_root(place)?;
                let _ = root;
                if !mutable {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::Place {
                            context: "compound assignment to an immutable root",
                        },
                    ));
                }
                let value_ty = self.check_node(value, NodeRole::Value)?;
                let lhs = self.ty(place_ty)?;
                let rhs = self.ty(value_ty)?;
                self.check_compound_op(op, &lhs, &rhs)?;
                Ok(Ty::Unit)
            }
            NodeKind::ExprStmt(inner) => {
                self.check_node(inner, NodeRole::Value)?;
                Ok(Ty::Unit)
            }
            NodeKind::While { cond, body } => {
                self.expect_child_type(cond, &Ty::Bool, "while condition")?;
                self.loop_depth += 1;
                let body_ty = self.check_node(body, NodeRole::Body);
                self.loop_depth -= 1;
                self.expect_ty_value(body_ty?, &Ty::Unit, "while body")?;
                Ok(Ty::Unit)
            }
            NodeKind::For {
                pattern,
                first_child,
            } => {
                self.check_child_count(first_child, 2, "for")?;
                let iterable = self.function.children[first_child as usize];
                let body = self.function.children[first_child as usize + 1];
                let iterable_ty = self.check_node(iterable, NodeRole::Value)?;
                let element_ty = self.for_element_type(iterable_ty)?;
                self.check_pattern_against(pattern, &element_ty)?;
                self.loop_depth += 1;
                let body_ty = self.check_node(body, NodeRole::Body);
                self.loop_depth -= 1;
                self.expect_ty_value(body_ty?, &Ty::Unit, "for body")?;
                Ok(Ty::Unit)
            }
            NodeKind::Return(value) => {
                let ret = self.ty(self.function.ret)?;
                match value {
                    Some(value) => {
                        let value_ty = self.check_node(value, NodeRole::Value)?;
                        self.expect_ty_id_value(value_ty, self.function.ret, "return value")?;
                    }
                    None => self.expect_ty_value(self.function.ret, &Ty::Unit, "return value")?,
                }
                let _ = ret;
                Ok(Ty::Unit)
            }
            NodeKind::Break => {
                if self.loop_depth == 0 {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::ControlFlow {
                            context: "`break` outside a loop",
                        },
                    ));
                }
                Ok(Ty::Unit)
            }
            NodeKind::Continue => {
                if self.loop_depth == 0 {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::ControlFlow {
                            context: "`continue` outside a loop",
                        },
                    ));
                }
                Ok(Ty::Unit)
            }
            NodeKind::SlotWrite { slot, value } | NodeKind::SlotInit { slot, value } => {
                let slot_ty = self.slot_type(slot)?;
                if !matches!(node.kind, NodeKind::SlotInit { .. })
                    && !self.function.slot_info[slot.index()].mutable
                {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::SlotInfo {
                            slot,
                            context: "write to an immutable slot",
                        },
                    ));
                }
                let value_ty = self.check_node(value, NodeRole::Value)?;
                self.expect_ty_value(value_ty, &slot_ty, "slot write")?;
                Ok(Ty::Unit)
            }
            NodeKind::Charge(_) => Ok(Ty::Unit),
            _ => Err(self.error(
                Some(id),
                VerifyErrorKind::WrongRole {
                    expected: NodeRole::Statement,
                    found: node.kind,
                },
            )),
        }
    }

    fn check_const_kind(
        &self,
        id: ConstId,
        context: &'static str,
        predicate: impl FnOnce(&ConstValue) -> bool,
    ) -> Result<(), VerifyError> {
        let value = self.const_value(id)?;
        if predicate(value) {
            Ok(())
        } else {
            Err(self.error(None, VerifyErrorKind::Const { id, context }))
        }
    }

    fn const_value(&self, id: ConstId) -> Result<&ConstValue, VerifyError> {
        self.program.constant(id).ok_or_else(|| {
            self.error(
                None,
                VerifyErrorKind::OutOfRange {
                    arena: Arena::Const,
                    id: id.0,
                },
            )
        })
    }

    fn check_str_id(&self, id: StrId) -> Result<(), VerifyError> {
        if self.program.string(id).is_some() {
            Ok(())
        } else {
            Err(self.error(None, VerifyErrorKind::Str { id }))
        }
    }

    fn const_matches(&self, ty: &Ty, value: &ConstValue) -> Result<bool, VerifyError> {
        if matches!(ty, Ty::Never | Ty::Param(_)) {
            return Ok(true);
        }
        Ok(match (ty, value) {
            (Ty::Unit, ConstValue::Unit) => true,
            (Ty::Int, ConstValue::Int(_)) => true,
            (Ty::Float, ConstValue::Float(_)) => true,
            (Ty::Bool, ConstValue::Bool(_)) => true,
            (Ty::Str, ConstValue::Str(string)) => {
                self.check_str_id(*string)?;
                true
            }
            (Ty::Tuple(items), ConstValue::Tuple(values)) => {
                if items.len() != values.len() {
                    false
                } else {
                    let mut matches = true;
                    for (ty, value) in items.iter().zip(values) {
                        if !self.const_matches(&self.ty(*ty)?, value)? {
                            matches = false;
                            break;
                        }
                    }
                    matches
                }
            }
            (Ty::List(item), ConstValue::List(values)) => {
                let item = self.ty(*item)?;
                let mut matches = true;
                for value in values {
                    if !self.const_matches(&item, value)? {
                        matches = false;
                        break;
                    }
                }
                matches
            }
            (Ty::Map(key, value), ConstValue::Map(entries)) => {
                let key = self.ty(*key)?;
                let value = self.ty(*value)?;
                let mut matches = true;
                for (entry_key, entry_value) in entries {
                    if !self.const_matches(&key, entry_key)?
                        || !self.const_matches(&value, entry_value)?
                    {
                        matches = false;
                        break;
                    }
                }
                matches
            }
            (Ty::Named(type_id, args), ConstValue::Struct(value_type, fields)) => {
                if type_id != value_type {
                    false
                } else if let Some(def) = self.program.types.get(*type_id) {
                    if let TypeDefKind::Struct { fields: def_fields } = &def.kind {
                        if def_fields.len() != fields.len() {
                            false
                        } else {
                            let mut matches = true;
                            for (field, value) in def_fields.iter().zip(fields) {
                                let expected = self.substitute(field.ty, args);
                                if !self.const_matches(&expected, value)? {
                                    matches = false;
                                    break;
                                }
                            }
                            matches
                        }
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            (Ty::Named(type_id, args), ConstValue::Enum(value_type, variant, payload)) => {
                if type_id != value_type {
                    false
                } else if let Some(def) = self.program.types.get(*type_id) {
                    if let TypeDefKind::Enum { variants } = &def.kind {
                        if let Some(def_variant) = variants.get(*variant as usize) {
                            if def_variant.payload.len() != payload.len() {
                                false
                            } else {
                                let mut matches = true;
                                for (field, value) in def_variant.payload.iter().zip(payload) {
                                    let expected = self.substitute(*field, args);
                                    if !self.const_matches(&expected, value)? {
                                        matches = false;
                                        break;
                                    }
                                }
                                matches
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            (Ty::Option(item), ConstValue::Prelude(variant, payload)) => {
                let expected = match variant {
                    PreludeVariant::None => 0,
                    PreludeVariant::Some => 1,
                    _ => return Ok(false),
                };
                if payload.len() != expected {
                    false
                } else {
                    payload
                        .first()
                        .map(|value| self.const_matches(&self.ty(*item)?, value))
                        .transpose()?
                        .unwrap_or(true)
                }
            }
            (Ty::Result(ok, err), ConstValue::Prelude(variant, payload)) => {
                let expected = match variant {
                    PreludeVariant::Ok | PreludeVariant::Err => 1,
                    _ => return Ok(false),
                };
                if payload.len() != expected {
                    false
                } else {
                    let item = match variant {
                        PreludeVariant::Ok => *ok,
                        PreludeVariant::Err => *err,
                        _ => return Ok(false),
                    };
                    payload
                        .first()
                        .map(|value| self.const_matches(&self.ty(item)?, value))
                        .transpose()?
                        .unwrap_or(true)
                }
            }
            _ => false,
        })
    }

    fn check_pattern_against(
        &mut self,
        pattern: PatternId,
        expected: &Ty,
    ) -> Result<(), VerifyError> {
        let Some(value) = self.function.pattern(pattern).cloned() else {
            return Err(self.error(
                None,
                VerifyErrorKind::Pattern {
                    pattern,
                    context: "use",
                },
            ));
        };
        self.check_pattern_against_pattern(pattern, &value, expected)
    }
    fn check_pattern_against_pattern(
        &mut self,
        id: PatternId,
        pattern: &Pattern,
        expected: &Ty,
    ) -> Result<(), VerifyError> {
        let _ = id;
        match pattern {
            Pattern::Wild => Ok(()),
            Pattern::Bind { ty, .. } => {
                let pattern_ty = self.ty(*ty)?;
                if type_accepts_owned(self.program, expected, &pattern_ty)? {
                    Ok(())
                } else {
                    Err(self.error(
                        None,
                        VerifyErrorKind::PatternShape {
                            pattern: id,
                            context: "binding type",
                        },
                    ))
                }
            }
            Pattern::Int(_) => self.expect_pattern_ty(expected, &Ty::Int, id),
            Pattern::Bool(_) => self.expect_pattern_ty(expected, &Ty::Bool, id),
            Pattern::Str(string) => {
                self.check_str_id(*string)?;
                self.expect_pattern_ty(expected, &Ty::Str, id)
            }
            Pattern::Tuple(items) => {
                let Ty::Tuple(expected_items) = expected else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::PatternShape {
                            pattern: id,
                            context: "tuple pattern",
                        },
                    ));
                };
                if items.len() != expected_items.len() {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::PatternShape {
                            pattern: id,
                            context: "tuple pattern arity",
                        },
                    ));
                }
                for (item, expected) in items.iter().zip(expected_items) {
                    let expected = self.ty(*expected)?;
                    self.check_pattern_against_pattern(id, item, &expected)?;
                }
                Ok(())
            }
            Pattern::Variant {
                type_id,
                variant,
                args,
            } => {
                let Ty::Named(expected_type, type_args) = expected else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::PatternShape {
                            pattern: id,
                            context: "variant pattern",
                        },
                    ));
                };
                if *type_id != *expected_type {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::PatternShape {
                            pattern: id,
                            context: "variant type",
                        },
                    ));
                }
                let Some(def) = self.program.types.get(*type_id) else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeTable {
                            context: "variant pattern",
                            ty: None,
                            def: Some(*type_id),
                        },
                    ));
                };
                let TypeDefKind::Enum { variants } = &def.kind else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::PatternShape {
                            pattern: id,
                            context: "variant pattern on a non-enum",
                        },
                    ));
                };
                let Some(def_variant) = variants.get(*variant as usize) else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::PatternShape {
                            pattern: id,
                            context: "variant index",
                        },
                    ));
                };
                if args.len() != def_variant.payload.len() {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::PatternShape {
                            pattern: id,
                            context: "variant payload count",
                        },
                    ));
                }
                for (item, expected) in args.iter().zip(&def_variant.payload) {
                    let expected = self.substitute(*expected, type_args);
                    self.check_pattern_against_pattern(id, item, &expected)?;
                }
                Ok(())
            }
            Pattern::Prelude { variant, args } => {
                let expected = match variant {
                    PreludeVariant::None | PreludeVariant::Some => match expected {
                        Ty::Option(item) => {
                            if matches!(variant, PreludeVariant::None) {
                                None
                            } else {
                                Some(self.ty(*item)?)
                            }
                        }
                        Ty::Never => None,
                        _ => {
                            return Err(self.error(
                                None,
                                VerifyErrorKind::PatternShape {
                                    pattern: id,
                                    context: "Option pattern",
                                },
                            ));
                        }
                    },
                    PreludeVariant::Ok => match expected {
                        Ty::Result(ok, _) => Some(self.ty(*ok)?),
                        Ty::Never => None,
                        _ => {
                            return Err(self.error(
                                None,
                                VerifyErrorKind::PatternShape {
                                    pattern: id,
                                    context: "Result pattern",
                                },
                            ));
                        }
                    },
                    PreludeVariant::Err => match expected {
                        Ty::Result(_, err) => Some(self.ty(*err)?),
                        Ty::Never => None,
                        _ => {
                            return Err(self.error(
                                None,
                                VerifyErrorKind::PatternShape {
                                    pattern: id,
                                    context: "Result pattern",
                                },
                            ));
                        }
                    },
                };
                let expected_count = if matches!(variant, PreludeVariant::None) {
                    0
                } else {
                    1
                };
                if args.len() != expected_count {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::PatternShape {
                            pattern: id,
                            context: "prelude pattern payload count",
                        },
                    ));
                }
                if let (Some(item), Some(expected)) = (args.first(), expected) {
                    self.check_pattern_against_pattern(id, item, &expected)?;
                }
                Ok(())
            }
            Pattern::Struct { type_id, fields } => {
                let Ty::Named(expected_type, type_args) = expected else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::PatternShape {
                            pattern: id,
                            context: "struct pattern",
                        },
                    ));
                };
                if *type_id != *expected_type {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::PatternShape {
                            pattern: id,
                            context: "struct type",
                        },
                    ));
                }
                let Some(def) = self.program.types.get(*type_id) else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeTable {
                            context: "struct pattern",
                            ty: None,
                            def: Some(*type_id),
                        },
                    ));
                };
                let TypeDefKind::Struct { fields: def_fields } = &def.kind else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::PatternShape {
                            pattern: id,
                            context: "struct pattern on a non-struct",
                        },
                    ));
                };
                for (offset, item) in fields {
                    let Some(field) = def_fields.get(*offset as usize) else {
                        return Err(self.error(
                            None,
                            VerifyErrorKind::PatternShape {
                                pattern: id,
                                context: "struct pattern field",
                            },
                        ));
                    };
                    let expected = self.substitute(field.ty, type_args);
                    self.check_pattern_against_pattern(id, item, &expected)?;
                }
                Ok(())
            }
            Pattern::Or(items) => {
                for item in items {
                    self.check_pattern_against_pattern(id, item, expected)?;
                }
                Ok(())
            }
        }
    }

    fn expect_pattern_ty(
        &self,
        actual: &Ty,
        expected: &Ty,
        pattern: PatternId,
    ) -> Result<(), VerifyError> {
        if type_accepts_owned(self.program, actual, expected)? {
            Ok(())
        } else {
            Err(self.error(
                None,
                VerifyErrorKind::PatternShape {
                    pattern,
                    context: "pattern literal type",
                },
            ))
        }
    }

    fn for_element_type(&self, iterable: TyId) -> Result<Ty, VerifyError> {
        match self.ty(iterable)? {
            Ty::List(item) => self.ty(item),
            Ty::Map(key, value) => Ok(Ty::Tuple(vec![key, value])),
            Ty::Str => Ok(Ty::Str),
            Ty::Range => Ok(Ty::Int),
            Ty::Never => Ok(Ty::Never),
            other => Err(self.error(
                None,
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(Ty::List(iterable)),
                    found: Box::new(other),
                    context: "for iterable",
                },
            )),
        }
    }

    fn check_binary(
        &mut self,
        op: crate::node::BinaryOp,
        lhs: IrId,
        rhs: IrId,
    ) -> Result<Ty, VerifyError> {
        let lhs_ty = self.check_node(lhs, NodeRole::Value)?;
        let lhs_ty = self.ty(lhs_ty)?;
        let rhs_ty = self.check_node(rhs, NodeRole::Value)?;
        let rhs_ty = self.ty(rhs_ty)?;
        match op {
            crate::node::BinaryOp::And | crate::node::BinaryOp::Or => {
                if !type_accepts_owned(self.program, &lhs_ty, &Ty::Bool)?
                    || !type_accepts_owned(self.program, &rhs_ty, &Ty::Bool)?
                {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Bool),
                            found: Box::new(lhs_ty),
                            context: "short-circuit operand",
                        },
                    ));
                }
                Ok(Ty::Bool)
            }
            crate::node::BinaryOp::Coalesce => {
                let Ty::Option(inner) = lhs_ty else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Option(TyId(0))),
                            found: Box::new(lhs_ty),
                            context: "coalesce left operand",
                        },
                    ));
                };
                let inner_ty = self.ty(inner)?;
                if !type_accepts_owned(self.program, &rhs_ty, &inner_ty)? {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(inner_ty),
                            found: Box::new(rhs_ty),
                            context: "coalesce right operand",
                        },
                    ));
                }
                Ok(inner_ty)
            }
            crate::node::BinaryOp::Eq | crate::node::BinaryOp::Ne => {
                if !type_accepts_owned(self.program, &rhs_ty, &lhs_ty)? {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(lhs_ty),
                            found: Box::new(rhs_ty),
                            context: "equality operands",
                        },
                    ));
                }
                Ok(Ty::Bool)
            }
            crate::node::BinaryOp::Lt
            | crate::node::BinaryOp::Le
            | crate::node::BinaryOp::Gt
            | crate::node::BinaryOp::Ge => {
                if !type_accepts_owned(self.program, &rhs_ty, &lhs_ty)? {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(lhs_ty),
                            found: Box::new(rhs_ty),
                            context: "comparison operands",
                        },
                    ));
                }
                Ok(Ty::Bool)
            }
            crate::node::BinaryOp::Add
            | crate::node::BinaryOp::Sub
            | crate::node::BinaryOp::Mul
            | crate::node::BinaryOp::Div
            | crate::node::BinaryOp::Rem => {
                let expected = match (&lhs_ty, op) {
                    (Ty::Never, _) | (Ty::Param(_), _) => lhs_ty.clone(),
                    (Ty::Int, _) => Ty::Int,
                    (Ty::Float, crate::node::BinaryOp::Rem) => {
                        return Err(self.error(
                            None,
                            VerifyErrorKind::TypeMismatch {
                                expected: Box::new(Ty::Int),
                                found: Box::new(Ty::Float),
                                context: "remainder on a float",
                            },
                        ));
                    }
                    (Ty::Float, _) => Ty::Float,
                    (Ty::Str, crate::node::BinaryOp::Add) => Ty::Str,
                    _ => {
                        return Err(self.error(
                            None,
                            VerifyErrorKind::TypeMismatch {
                                expected: Box::new(Ty::Int),
                                found: Box::new(lhs_ty),
                                context: "arithmetic operand",
                            },
                        ));
                    }
                };
                if !type_accepts_owned(self.program, &rhs_ty, &expected)? {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(expected),
                            found: Box::new(rhs_ty),
                            context: "arithmetic operands",
                        },
                    ));
                }
                Ok(expected)
            }
        }
    }

    fn check_compound_op(
        &self,
        op: crate::node::CompoundOp,
        lhs: &Ty,
        rhs: &Ty,
    ) -> Result<(), VerifyError> {
        let expected = match (lhs, op) {
            (Ty::Never, _) | (Ty::Param(_), _) => lhs.clone(),
            (Ty::Int, _) => Ty::Int,
            (Ty::Float, crate::node::CompoundOp::Rem) => {
                return Err(self.error(
                    None,
                    VerifyErrorKind::TypeMismatch {
                        expected: Box::new(Ty::Int),
                        found: Box::new(Ty::Float),
                        context: "compound remainder on a float",
                    },
                ));
            }
            (Ty::Float, _) => Ty::Float,
            (Ty::Str, crate::node::CompoundOp::Add) => Ty::Str,
            _ => {
                return Err(self.error(
                    None,
                    VerifyErrorKind::TypeMismatch {
                        expected: Box::new(Ty::Int),
                        found: Box::new(lhs.clone()),
                        context: "compound assignment operand",
                    },
                ));
            }
        };
        if type_accepts_owned(self.program, rhs, &expected)? {
            Ok(())
        } else {
            Err(self.error(
                None,
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(expected),
                    found: Box::new(rhs.clone()),
                    context: "compound assignment operands",
                },
            ))
        }
    }

    fn expect_child_type(
        &mut self,
        id: IrId,
        expected: &Ty,
        context: &'static str,
    ) -> Result<(), VerifyError> {
        let actual = self.check_node(id, NodeRole::Value)?;
        self.expect_ty_value(actual, expected, context)
    }

    fn check_child_count(
        &self,
        first: u32,
        count: u32,
        context: &'static str,
    ) -> Result<(), VerifyError> {
        let end = first as usize + count as usize;
        if end > self.function.children.len() {
            return Err(self.error(
                None,
                VerifyErrorKind::ChildRange {
                    range: ChildRange::new(first, count),
                    len: self.function.children.len() as u32,
                },
            ));
        }
        let _ = context;
        Ok(())
    }

    fn check_if(&mut self, id: IrId, range: ChildRange) -> Result<Ty, VerifyError> {
        let children = self.range(range)?;
        if !(2..=3).contains(&children.len()) {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::Arity {
                    expected: 2,
                    found: children.len() as u32,
                    context: "if",
                },
            ));
        }
        let cond = children[0];
        self.expect_child_type(cond, &Ty::Bool, "if condition")?;
        let then_ty = self.check_node(children[1], NodeRole::Body)?;
        let then_ty = self.ty(then_ty)?;
        match children.get(2) {
            Some(else_branch) => {
                let else_ty = self.check_node(*else_branch, NodeRole::Value)?;
                let else_ty = self.ty(else_ty)?;
                self.join_types(&then_ty, &else_ty, "if branches")
            }
            None => {
                if type_accepts_owned(self.program, &then_ty, &Ty::Unit)? {
                    Ok(Ty::Unit)
                } else {
                    Err(self.error(
                        Some(id),
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Unit),
                            found: Box::new(then_ty),
                            context: "if without else",
                        },
                    ))
                }
            }
        }
    }

    fn join_types(&self, left: &Ty, right: &Ty, context: &'static str) -> Result<Ty, VerifyError> {
        if matches!(left, Ty::Never) {
            return Ok(right.clone());
        }
        if matches!(right, Ty::Never) {
            return Ok(left.clone());
        }
        if type_equal(self.program, left, right)? {
            Ok(left.clone())
        } else {
            Err(self.error(
                None,
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(right.clone()),
                    found: Box::new(left.clone()),
                    context,
                },
            ))
        }
    }

    fn check_match(
        &mut self,
        id: IrId,
        match_set: MatchSetId,
        scrutinee: IrId,
    ) -> Result<Ty, VerifyError> {
        let Some(set) = self.function.match_set(match_set) else {
            return Err(self.error(Some(id), VerifyErrorKind::MatchSet { match_set }));
        };
        if set.arms.is_empty() {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::ControlFlow {
                    context: "match has no arms",
                },
            ));
        }
        let scrutinee_ty = self.check_node(scrutinee, NodeRole::Value)?;
        let scrutinee_ty = self.ty(scrutinee_ty)?;
        let mut result = None;
        for arm in &set.arms {
            self.check_pattern_against(arm.pattern, &scrutinee_ty)?;
            if let Some(guard) = arm.guard {
                self.expect_child_type(guard, &Ty::Bool, "match guard")?;
            }
            let body_kind = self.node(arm.body)?.kind;
            let body_ty = match body_kind {
                NodeKind::Block(_) | NodeKind::ExprBlock(_) => {
                    self.check_node(arm.body, NodeRole::Body)?
                }
                _ => self.check_node(arm.body, NodeRole::Value)?,
            };
            let body_ty = self.ty(body_ty)?;
            result = Some(match result {
                None => body_ty,
                Some(previous) => self.join_types(&previous, &body_ty, "match arms")?,
            });
        }
        Ok(result.unwrap_or(Ty::Unit))
    }

    fn check_try(&mut self, id: IrId, inner: IrId) -> Result<Ty, VerifyError> {
        let inner_ty = self.check_node(inner, NodeRole::Value)?;
        let inner_ty = self.ty(inner_ty)?;
        let ret = self.ty(self.function.ret)?;
        match inner_ty {
            Ty::Option(item) => {
                if !matches!(ret, Ty::Option(_) | Ty::Never) {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Option(item)),
                            found: Box::new(ret),
                            context: "`?` on Option",
                        },
                    ));
                }
                self.ty(item)
            }
            Ty::Result(ok, err) => {
                let Ty::Result(_, ret_err) = ret else {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Result(ok, err)),
                            found: Box::new(ret),
                            context: "`?` on Result",
                        },
                    ));
                };
                let err_ty = self.ty(err)?;
                let ret_err_ty = self.ty(ret_err)?;
                if !type_accepts_owned(self.program, &err_ty, &ret_err_ty)? {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(ret_err_ty),
                            found: Box::new(err_ty),
                            context: "`?` error type",
                        },
                    ));
                }
                self.ty(ok)
            }
            Ty::Never => Ok(Ty::Never),
            other => Err(self.error(
                Some(id),
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(Ty::Option(self.function.ret)),
                    found: Box::new(other),
                    context: "`?` operand",
                },
            )),
        }
    }

    fn place_root(&self, id: IrId) -> Result<(Slot, bool), VerifyError> {
        let node = self.node(id)?;
        match node.kind {
            NodeKind::SlotPlace(slot) => {
                let Some(info) = self.function.slot_info.get(slot.index()) else {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::SlotInfo {
                            slot,
                            context: "place root",
                        },
                    ));
                };
                Ok((slot, info.mutable))
            }
            NodeKind::Field { base, .. }
            | NodeKind::TupleField { base, .. }
            | NodeKind::Index { base, .. } => self.place_root(base),
            _ => Err(self.error(
                Some(id),
                VerifyErrorKind::Place {
                    context: "place root",
                },
            )),
        }
    }

    fn check_struct_value(
        &mut self,
        type_id: TypeId,
        first_field: u32,
        node_ty: TyId,
        id: IrId,
    ) -> Result<Ty, VerifyError> {
        let ty = self.ty(node_ty)?;
        let Ty::Named(found, args) = &ty else {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(Ty::Named(type_id, Vec::new())),
                    found: Box::new(ty),
                    context: "struct literal",
                },
            ));
        };
        if *found != type_id {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(Ty::Named(type_id, Vec::new())),
                    found: Box::new(ty),
                    context: "struct literal",
                },
            ));
        }
        let Some(def) = self.program.types.get(type_id) else {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::TypeTable {
                    context: "struct literal",
                    ty: None,
                    def: Some(type_id),
                },
            ));
        };
        let TypeDefKind::Struct { fields } = &def.kind else {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(Ty::Named(type_id, Vec::new())),
                    found: Box::new(ty),
                    context: "struct literal on a non-struct",
                },
            ));
        };
        let field_types: Vec<TyId> = fields.iter().map(|field| field.ty).collect();
        let args = args.clone();
        self.check_child_count(first_field, field_types.len() as u32, "struct literal")?;
        for (index, field_ty) in field_types.iter().enumerate() {
            let child = self.function.children[first_field as usize + index];
            let expected = self.substitute(*field_ty, &args);
            self.expect_child_type(child, &expected, "struct field")?;
        }
        Ok(ty)
    }

    fn check_struct_update(
        &mut self,
        type_id: TypeId,
        layout: StructLayoutId,
        node_ty: TyId,
        id: IrId,
    ) -> Result<Ty, VerifyError> {
        let ty = self.ty(node_ty)?;
        let Ty::Named(found, args) = &ty else {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(Ty::Named(type_id, Vec::new())),
                    found: Box::new(ty),
                    context: "struct update",
                },
            ));
        };
        if *found != type_id {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(Ty::Named(type_id, Vec::new())),
                    found: Box::new(ty),
                    context: "struct update",
                },
            ));
        }
        let Some(def) = self.program.types.get(type_id) else {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::TypeTable {
                    context: "struct update",
                    ty: None,
                    def: Some(type_id),
                },
            ));
        };
        let TypeDefKind::Struct { fields } = &def.kind else {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(Ty::Named(type_id, Vec::new())),
                    found: Box::new(ty),
                    context: "struct update on a non-struct",
                },
            ));
        };
        let Some(layout) = self.function.struct_layout(layout) else {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::StructLayout {
                    layout: StructLayoutId(0),
                },
            ));
        };
        let field_types: Vec<TyId> = fields.iter().map(|field| field.ty).collect();
        let args = args.clone();
        let first_child = layout.first_child;
        let layout_fields = layout.fields.clone();
        self.check_child_count(first_child, layout_fields.len() as u32 + 1, "struct update")?;
        let base = self.function.children[first_child as usize];
        let base_ty = self.check_node(base, NodeRole::Value)?;
        self.expect_ty_value(base_ty, &ty, "struct update base")?;
        let mut seen = Vec::new();
        for (index, field) in layout_fields.iter().enumerate() {
            if *field as usize >= field_types.len() || seen.contains(field) {
                return Err(self.error(
                    Some(id),
                    VerifyErrorKind::Path {
                        index: *field,
                        context: "struct update field",
                    },
                ));
            }
            seen.push(*field);
            let child = self.function.children[first_child as usize + 1 + index];
            let expected = self.substitute(field_types[*field as usize], &args);
            self.expect_child_type(child, &expected, "struct update field")?;
        }
        Ok(ty)
    }

    fn check_enum_variant(
        &mut self,
        variant: u32,
        first_payload: u32,
        node_ty: TyId,
        id: IrId,
    ) -> Result<Ty, VerifyError> {
        let ty = self.ty(node_ty)?;
        let Ty::Named(type_id, args) = &ty else {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(Ty::Named(TypeId(0), Vec::new())),
                    found: Box::new(ty),
                    context: "enum variant",
                },
            ));
        };
        let Some(def) = self.program.types.get(*type_id) else {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::TypeTable {
                    context: "enum variant",
                    ty: None,
                    def: Some(*type_id),
                },
            ));
        };
        let TypeDefKind::Enum { variants } = &def.kind else {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(Ty::Named(*type_id, Vec::new())),
                    found: Box::new(ty),
                    context: "enum variant on a non-enum",
                },
            ));
        };
        let Some(def_variant) = variants.get(variant as usize) else {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::Path {
                    index: variant,
                    context: "enum variant",
                },
            ));
        };
        let payload: Vec<TyId> = def_variant.payload.clone();
        let args = args.clone();
        self.check_child_count(first_payload, payload.len() as u32, "enum variant")?;
        for (index, payload_ty) in payload.iter().enumerate() {
            let child = self.function.children[first_payload as usize + index];
            let expected = self.substitute(*payload_ty, &args);
            self.expect_child_type(child, &expected, "enum payload")?;
        }
        Ok(ty)
    }

    fn check_prelude_variant(
        &mut self,
        variant: PreludeVariant,
        first_payload: u32,
        node_ty: TyId,
        id: IrId,
    ) -> Result<Ty, VerifyError> {
        let ty = self.ty(node_ty)?;
        let (count, expected) = match &ty {
            Ty::Option(item) => match variant {
                PreludeVariant::None => (0, None),
                PreludeVariant::Some => (1, Some(self.ty(*item)?)),
                _ => {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Option(*item)),
                            found: Box::new(ty),
                            context: "prelude variant",
                        },
                    ));
                }
            },
            Ty::Result(ok, err) => match variant {
                PreludeVariant::Ok => (1, Some(self.ty(*ok)?)),
                PreludeVariant::Err => (1, Some(self.ty(*err)?)),
                _ => {
                    return Err(self.error(
                        Some(id),
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Result(*ok, *ok)),
                            found: Box::new(ty),
                            context: "prelude variant",
                        },
                    ));
                }
            },
            _ => {
                return Err(self.error(
                    Some(id),
                    VerifyErrorKind::TypeMismatch {
                        expected: Box::new(Ty::Option(node_ty)),
                        found: Box::new(ty),
                        context: "prelude variant",
                    },
                ));
            }
        };
        self.check_child_count(first_payload, count, "prelude variant")?;
        if let Some(expected) = expected {
            let child = self.function.children[first_payload as usize];
            self.expect_child_type(child, &expected, "prelude payload")?;
        }
        Ok(ty)
    }

    fn check_arg(
        &mut self,
        id: IrId,
        mutable: bool,
        index: u32,
    ) -> Result<(TyId, Option<Slot>), VerifyError> {
        let node = self.node(id)?;
        if let NodeKind::MutArg(place) = node.kind {
            if !mutable {
                return Err(self.error(
                    Some(id),
                    VerifyErrorKind::MutArg {
                        index,
                        expected: false,
                    },
                ));
            }
            let ty = self.check_node(id, NodeRole::Argument)?;
            let (root, root_mutable) = self.place_root(place)?;
            if !root_mutable {
                return Err(self.error(
                    Some(place),
                    VerifyErrorKind::Place {
                        context: "mutable argument to an immutable root",
                    },
                ));
            }
            return Ok((ty, Some(root)));
        }
        if mutable {
            return Err(self.error(
                Some(id),
                VerifyErrorKind::MutArg {
                    index,
                    expected: true,
                },
            ));
        }
        let ty = self.check_node(id, NodeRole::Value)?;
        Ok((ty, None))
    }

    fn push_mut_root(
        &self,
        roots: &mut Vec<Slot>,
        root: Option<Slot>,
        node: IrId,
    ) -> Result<(), VerifyError> {
        if let Some(root) = root {
            if roots.contains(&root) {
                return Err(self.error(Some(node), VerifyErrorKind::DuplicateMutRoot { root }));
            }
            roots.push(root);
        }
        Ok(())
    }

    fn match_generic(
        &self,
        pattern: &Ty,
        actual: TyId,
        subst: &mut Vec<Option<TyId>>,
    ) -> Result<bool, VerifyError> {
        if matches!(pattern, Ty::Never) {
            return Ok(true);
        }
        let actual_ty = self.ty(actual)?;
        if matches!(actual_ty, Ty::Never) {
            return Ok(true);
        }
        match (pattern, &actual_ty) {
            (Ty::Param(index), _) => {
                let index = *index as usize;
                if subst.len() <= index {
                    subst.resize(index + 1, None);
                }
                match subst[index] {
                    Some(bound) => type_accepts_owned(self.program, &actual_ty, &self.ty(bound)?),
                    None => {
                        subst[index] = Some(actual);
                        Ok(true)
                    }
                }
            }
            (Ty::Int, Ty::Int)
            | (Ty::Float, Ty::Float)
            | (Ty::Bool, Ty::Bool)
            | (Ty::Str, Ty::Str)
            | (Ty::Unit, Ty::Unit)
            | (Ty::Range, Ty::Range)
            | (Ty::Never, _) => Ok(true),
            (Ty::Tuple(items), Ty::Tuple(actual_items)) => {
                if items.len() != actual_items.len() {
                    return Ok(false);
                }
                for (pattern, actual) in items.iter().zip(actual_items) {
                    if !self.match_generic(&self.ty(*pattern)?, *actual, subst)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            (Ty::List(item), Ty::List(actual_item))
            | (Ty::Option(item), Ty::Option(actual_item)) => {
                self.match_generic(&self.ty(*item)?, *actual_item, subst)
            }
            (Ty::Map(key, value), Ty::Map(actual_key, actual_value))
            | (Ty::Result(key, value), Ty::Result(actual_key, actual_value)) => Ok(self
                .match_generic(&self.ty(*key)?, *actual_key, subst)?
                && self.match_generic(&self.ty(*value)?, *actual_value, subst)?),
            (Ty::Fn(params, ret), Ty::Fn(actual_params, actual_ret)) => {
                if params.len() != actual_params.len() {
                    return Ok(false);
                }
                for (pattern, actual) in params.iter().zip(actual_params) {
                    if !self.match_generic(&self.ty(*pattern)?, *actual, subst)? {
                        return Ok(false);
                    }
                }
                self.match_generic(&self.ty(*ret)?, *actual_ret, subst)
            }
            (Ty::Named(type_id, args), Ty::Named(actual_type, actual_args)) => {
                if type_id != actual_type || args.len() != actual_args.len() {
                    return Ok(false);
                }
                for (pattern, actual) in args.iter().zip(actual_args) {
                    if !self.match_generic(&self.ty(*pattern)?, *actual, subst)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn check_direct_call(
        &mut self,
        target: FnId,
        first_arg: u32,
        node_ty: TyId,
    ) -> Result<Ty, VerifyError> {
        let Some(target_fn) = self.program.function(target) else {
            return Err(self.error(
                None,
                VerifyErrorKind::OutOfRange {
                    arena: Arena::Function,
                    id: target.0,
                },
            ));
        };
        if matches!(target_fn.kind, FnKind::Host(_)) {
            return Err(self.error(
                None,
                VerifyErrorKind::Call {
                    context: "direct call to a host function",
                    index: 0,
                },
            ));
        }
        let params: Vec<ParamInfo> = target_fn.params.clone();
        let ret = target_fn.ret;
        let captures = target_fn.captures.len();
        let kind = target_fn.kind;
        if params.first().is_some_and(|param| param.is_self) {
            return Err(self.error(
                None,
                VerifyErrorKind::Call {
                    context: "direct call to a method",
                    index: 0,
                },
            ));
        }
        if matches!(kind, FnKind::Lambda) && captures > 0 {
            return Err(self.error(
                None,
                VerifyErrorKind::Call {
                    context: "direct call to a closure with captures",
                    index: 0,
                },
            ));
        }
        self.check_child_count(first_arg, params.len() as u32, "direct call")?;
        let mut subst = Vec::new();
        let mut roots = Vec::new();
        for (index, param) in params.iter().enumerate() {
            let child = self.function.children[first_arg as usize + index];
            let (actual, root) = self.check_arg(child, param.mutable, index as u32)?;
            self.push_mut_root(&mut roots, root, child)?;
            if !self.match_generic(&self.ty(param.ty)?, actual, &mut subst)? {
                return Err(self.error(
                    Some(child),
                    VerifyErrorKind::TypeMismatch {
                        expected: Box::new(self.ty(param.ty)?),
                        found: Box::new(self.ty(actual)?),
                        context: "call argument",
                    },
                ));
            }
        }
        if !self.match_generic(&self.ty(ret)?, node_ty, &mut subst)? {
            return Err(self.error(
                None,
                VerifyErrorKind::NodeType {
                    expected: ret,
                    found: node_ty,
                    context: "call result",
                },
            ));
        }
        self.ty(node_ty)
    }

    fn check_method_call(
        &mut self,
        target: FnId,
        first_child: u32,
        node_ty: TyId,
    ) -> Result<Ty, VerifyError> {
        let Some(target_fn) = self.program.function(target) else {
            return Err(self.error(
                None,
                VerifyErrorKind::OutOfRange {
                    arena: Arena::Function,
                    id: target.0,
                },
            ));
        };
        let params: Vec<ParamInfo> = target_fn.params.clone();
        let ret = target_fn.ret;
        let Some(self_param) = params.first().filter(|param| param.is_self) else {
            return Err(self.error(
                None,
                VerifyErrorKind::Call {
                    context: "method call to a function without self",
                    index: 0,
                },
            ));
        };
        let self_param = self_param.clone();
        self.check_child_count(first_child, params.len() as u32, "method call")?;
        let mut subst = Vec::new();
        let mut roots = Vec::new();
        let receiver = self.function.children[first_child as usize];
        let (receiver_ty, root) = self.check_arg(receiver, self_param.mutable, 0)?;
        self.push_mut_root(&mut roots, root, receiver)?;
        if !self.match_generic(&self.ty(self_param.ty)?, receiver_ty, &mut subst)? {
            return Err(self.error(
                Some(receiver),
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(self.ty(self_param.ty)?),
                    found: Box::new(self.ty(receiver_ty)?),
                    context: "method receiver",
                },
            ));
        }
        for (index, param) in params.iter().skip(1).enumerate() {
            let child = self.function.children[first_child as usize + 1 + index];
            let (actual, root) = self.check_arg(child, param.mutable, index as u32 + 1)?;
            self.push_mut_root(&mut roots, root, child)?;
            if !self.match_generic(&self.ty(param.ty)?, actual, &mut subst)? {
                return Err(self.error(
                    Some(child),
                    VerifyErrorKind::TypeMismatch {
                        expected: Box::new(self.ty(param.ty)?),
                        found: Box::new(self.ty(actual)?),
                        context: "method argument",
                    },
                ));
            }
        }
        if !self.match_generic(&self.ty(ret)?, node_ty, &mut subst)? {
            return Err(self.error(
                None,
                VerifyErrorKind::NodeType {
                    expected: ret,
                    found: node_ty,
                    context: "method result",
                },
            ));
        }
        self.ty(node_ty)
    }

    fn check_value_call(
        &mut self,
        first_child: u32,
        argc: u32,
        node_ty: TyId,
    ) -> Result<Ty, VerifyError> {
        self.check_child_count(first_child, argc + 1, "value call")?;
        let callee = self.function.children[first_child as usize];
        let callee_ty = self.check_node(callee, NodeRole::Value)?;
        let callee_ty = self.ty(callee_ty)?;
        let (params, ret) = match callee_ty {
            Ty::Fn(params, ret) => (params, ret),
            Ty::Never => return self.ty(node_ty),
            other => {
                return Err(self.error(
                    Some(callee),
                    VerifyErrorKind::TypeMismatch {
                        expected: Box::new(Ty::Fn(Vec::new(), node_ty)),
                        found: Box::new(other),
                        context: "called value",
                    },
                ));
            }
        };
        if params.len() != argc as usize {
            return Err(self.error(
                None,
                VerifyErrorKind::Arity {
                    expected: params.len() as u32,
                    found: argc,
                    context: "value call",
                },
            ));
        }
        let mut subst = Vec::new();
        for (index, param) in params.iter().enumerate() {
            let child = self.function.children[first_child as usize + 1 + index];
            let node = self.node(child)?;
            if matches!(node.kind, NodeKind::MutArg(_)) {
                return Err(self.error(
                    Some(child),
                    VerifyErrorKind::MutArg {
                        index: index as u32,
                        expected: false,
                    },
                ));
            }
            let actual = self.check_node(child, NodeRole::Value)?;
            if !self.match_generic(&self.ty(*param)?, actual, &mut subst)? {
                return Err(self.error(
                    Some(child),
                    VerifyErrorKind::TypeMismatch {
                        expected: Box::new(self.ty(*param)?),
                        found: Box::new(self.ty(actual)?),
                        context: "value call argument",
                    },
                ));
            }
        }
        if !self.match_generic(&self.ty(ret)?, node_ty, &mut subst)? {
            return Err(self.error(
                None,
                VerifyErrorKind::NodeType {
                    expected: ret,
                    found: node_ty,
                    context: "value call result",
                },
            ));
        }
        self.ty(node_ty)
    }

    fn check_plain_args(
        &mut self,
        first_arg: u32,
        expected: &[Ty],
        context: &'static str,
    ) -> Result<Vec<TyId>, VerifyError> {
        self.check_child_count(first_arg, expected.len() as u32, context)?;
        let mut actuals = Vec::with_capacity(expected.len());
        for (index, expected) in expected.iter().enumerate() {
            let child = self.function.children[first_arg as usize + index];
            let (actual, _) = self.check_arg(child, false, index as u32)?;
            self.expect_ty_value(actual, expected, context)?;
            actuals.push(actual);
        }
        Ok(actuals)
    }

    fn check_builtin_fn_call(
        &mut self,
        builtin: BuiltinFnId,
        first_arg: u32,
        node_ty: TyId,
    ) -> Result<Ty, VerifyError> {
        let Some(def) = builtin.def() else {
            return Err(self.error(
                None,
                VerifyErrorKind::Builtin {
                    id: builtin.0,
                    context: "function call",
                },
            ));
        };
        let result = match def.name {
            "print" => {
                self.check_child_count(first_arg, 1, "print")?;
                let child = self.function.children[first_arg as usize];
                self.check_arg(child, false, 0)?;
                Ty::Unit
            }
            "panic" => {
                self.check_plain_args(first_arg, &[Ty::Str], "panic")?;
                Ty::Never
            }
            "assert" => {
                self.check_plain_args(first_arg, &[Ty::Bool, Ty::Str], "assert")?;
                Ty::Unit
            }
            _ => {
                return Err(self.error(
                    None,
                    VerifyErrorKind::Builtin {
                        id: builtin.0,
                        context: "unknown builtin function",
                    },
                ));
            }
        };
        self.expect_ty_value(node_ty, &result, "builtin function result")?;
        Ok(result)
    }

    fn check_builtin_item_call(
        &mut self,
        item: BuiltinItemId,
        first_arg: u32,
        node_ty: TyId,
    ) -> Result<Ty, VerifyError> {
        let Some(def) = item.item() else {
            return Err(self.error(
                None,
                VerifyErrorKind::Builtin {
                    id: item.0,
                    context: "module call",
                },
            ));
        };
        match (def.module, def.name) {
            ("std.math", "min" | "max") => {
                let args = self.check_any_args(first_arg, 2, "math min/max")?;
                let first = self.ty(args[0])?;
                let second = self.ty(args[1])?;
                if !type_accepts_owned(self.program, &second, &first)? {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(first.clone()),
                            found: Box::new(second),
                            context: "math min/max arguments",
                        },
                    ));
                }
                self.expect_ty_value(node_ty, &first, "math min/max result")?;
                Ok(first)
            }
            ("std.math", "clamp") => {
                let args = self.check_any_args(first_arg, 3, "math clamp")?;
                let first = self.ty(args[0])?;
                for arg in &args[1..] {
                    if !type_accepts_owned(self.program, &self.ty(*arg)?, &first)? {
                        return Err(self.error(
                            None,
                            VerifyErrorKind::TypeMismatch {
                                expected: Box::new(first.clone()),
                                found: Box::new(self.ty(*arg)?),
                                context: "math clamp arguments",
                            },
                        ));
                    }
                }
                self.expect_ty_value(node_ty, &first, "math clamp result")?;
                Ok(first)
            }
            ("std.math", "atan2") => {
                self.check_plain_args(first_arg, &[Ty::Float, Ty::Float], "math atan2")?;
                self.expect_ty_value(node_ty, &Ty::Float, "math atan2 result")?;
                Ok(Ty::Float)
            }
            ("std.math", "sin" | "cos" | "tan" | "exp" | "ln" | "log10") => {
                self.check_plain_args(first_arg, &[Ty::Float], "math function")?;
                self.expect_ty_value(node_ty, &Ty::Float, "math result")?;
                Ok(Ty::Float)
            }
            ("std.json", "parse") => {
                self.check_plain_args(first_arg, &[Ty::Str], "json parse")?;
                let ty = self.ty(node_ty)?;
                let Ty::Result(ok, err) = &ty else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Result(node_ty, node_ty)),
                            found: Box::new(ty),
                            context: "json parse result",
                        },
                    ));
                };
                if !type_accepts_owned(self.program, &self.ty(*ok)?, &self.json_ty())?
                    || !type_accepts_owned(self.program, &self.ty(*err)?, &Ty::Str)?
                {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Result(node_ty, node_ty)),
                            found: Box::new(ty),
                            context: "json parse result",
                        },
                    ));
                }
                Ok(ty)
            }
            ("std.json", "stringify") => {
                self.check_plain_args(first_arg, &[self.json_ty()], "json stringify")?;
                self.expect_ty_value(node_ty, &Ty::Str, "json stringify result")?;
                Ok(Ty::Str)
            }
            ("std.json", "Json") | ("std.math", "PI" | "EULER") => Err(self.error(
                None,
                VerifyErrorKind::Builtin {
                    id: item.0,
                    context: "item is not callable",
                },
            )),
            _ => Err(self.error(
                None,
                VerifyErrorKind::Builtin {
                    id: item.0,
                    context: "unknown builtin module item",
                },
            )),
        }
    }

    fn check_host_call(
        &mut self,
        host: HostId,
        first_arg: u32,
        node_ty: TyId,
    ) -> Result<Ty, VerifyError> {
        let Some(info) = self.program.host(host) else {
            return Err(self.error(None, VerifyErrorKind::HostOutOfRange { host }));
        };
        if !self.program.has_grant(&info.module) {
            return Err(self.error(None, VerifyErrorKind::HostGrant { host }));
        }
        let params: Vec<Ty> = info
            .params
            .iter()
            .map(|id| self.ty(*id))
            .collect::<Result<_, _>>()?;
        let ret = self.ty(info.ret)?;
        self.check_plain_args(first_arg, &params, "host call")?;
        self.expect_ty_value(node_ty, &ret, "host call result")?;
        Ok(ret)
    }

    fn json_ty(&self) -> Ty {
        Ty::Named(
            self.program.entries.json_type.unwrap_or(TypeId(0)),
            Vec::new(),
        )
    }

    fn fn_ref_type(&self, target: FnId, node_ty: TyId) -> Result<Ty, VerifyError> {
        let Some(target_fn) = self.program.function(target) else {
            return Err(self.error(
                None,
                VerifyErrorKind::OutOfRange {
                    arena: Arena::Function,
                    id: target.0,
                },
            ));
        };
        if matches!(
            target_fn.kind,
            FnKind::Host(_) | FnKind::Method(_) | FnKind::Lambda
        ) {
            return Err(self.error(
                None,
                VerifyErrorKind::Call {
                    context: "function value kind",
                    index: target.0,
                },
            ));
        }
        let ty = self.ty(node_ty)?;
        let Ty::Fn(params, ret) = &ty else {
            return Err(self.error(
                None,
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(Ty::Fn(Vec::new(), node_ty)),
                    found: Box::new(ty),
                    context: "function reference",
                },
            ));
        };
        if params.len() != target_fn.params.len() {
            return Err(self.error(
                None,
                VerifyErrorKind::Arity {
                    expected: target_fn.params.len() as u32,
                    found: params.len() as u32,
                    context: "function reference",
                },
            ));
        }
        for (param, expected) in params.iter().zip(&target_fn.params) {
            if !type_accepts_owned(self.program, &self.ty(*param)?, &self.ty(expected.ty)?)? {
                return Err(self.error(
                    None,
                    VerifyErrorKind::TypeMismatch {
                        expected: Box::new(self.ty(expected.ty)?),
                        found: Box::new(self.ty(*param)?),
                        context: "function reference parameter",
                    },
                ));
            }
        }
        if !type_accepts_owned(self.program, &self.ty(*ret)?, &self.ty(target_fn.ret)?)? {
            return Err(self.error(
                None,
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(self.ty(target_fn.ret)?),
                    found: Box::new(self.ty(*ret)?),
                    context: "function reference return",
                },
            ));
        }
        Ok(ty)
    }

    fn make_closure_type(&self, target: FnId, node_ty: TyId) -> Result<Ty, VerifyError> {
        let Some(target_fn) = self.program.function(target) else {
            return Err(self.error(
                None,
                VerifyErrorKind::OutOfRange {
                    arena: Arena::Function,
                    id: target.0,
                },
            ));
        };
        if !matches!(target_fn.kind, FnKind::Lambda) {
            return Err(self.error(
                None,
                VerifyErrorKind::Call {
                    context: "closure target is not a lambda",
                    index: target.0,
                },
            ));
        }
        let ty = self.ty(node_ty)?;
        let Ty::Fn(params, ret) = &ty else {
            return Err(self.error(
                None,
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(Ty::Fn(Vec::new(), node_ty)),
                    found: Box::new(ty),
                    context: "closure type",
                },
            ));
        };
        if params.len() != target_fn.params.len() {
            return Err(self.error(
                None,
                VerifyErrorKind::Arity {
                    expected: target_fn.params.len() as u32,
                    found: params.len() as u32,
                    context: "closure parameters",
                },
            ));
        }
        for (param, expected) in params.iter().zip(&target_fn.params) {
            if !type_accepts_owned(self.program, &self.ty(*param)?, &self.ty(expected.ty)?)? {
                return Err(self.error(
                    None,
                    VerifyErrorKind::TypeMismatch {
                        expected: Box::new(self.ty(expected.ty)?),
                        found: Box::new(self.ty(*param)?),
                        context: "closure parameter",
                    },
                ));
            }
        }
        if !type_accepts_owned(self.program, &self.ty(*ret)?, &self.ty(target_fn.ret)?)? {
            return Err(self.error(
                None,
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(self.ty(target_fn.ret)?),
                    found: Box::new(self.ty(*ret)?),
                    context: "closure return",
                },
            ));
        }
        for capture in &target_fn.captures {
            if capture.outer.index() >= self.function.locals as usize {
                return Err(self.error(
                    None,
                    VerifyErrorKind::Capture {
                        index: CaptureIndex(target.0),
                        context: "capture outer slot",
                    },
                ));
            }
            let outer = &self.function.slot_info[capture.outer.index()];
            if !type_accepts_owned(self.program, &self.ty(capture.ty)?, &self.ty(outer.ty)?)? {
                return Err(self.error(
                    None,
                    VerifyErrorKind::Capture {
                        index: CaptureIndex(target.0),
                        context: "capture type",
                    },
                ));
            }
        }
        Ok(ty)
    }

    fn builtin_fn_type(&self, builtin: BuiltinFnId, node_ty: TyId) -> Result<Ty, VerifyError> {
        let Some(def) = builtin.def() else {
            return Err(self.error(
                None,
                VerifyErrorKind::Builtin {
                    id: builtin.0,
                    context: "function value",
                },
            ));
        };
        let ty = self.ty(node_ty)?;
        let Ty::Fn(params, ret) = &ty else {
            return Err(self.error(
                None,
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(Ty::Fn(Vec::new(), node_ty)),
                    found: Box::new(ty),
                    context: "builtin function value",
                },
            ));
        };
        match def.name {
            "print" => {
                if params.len() != 1
                    || !type_accepts_owned(self.program, &self.ty(*ret)?, &Ty::Unit)?
                {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Fn(vec![node_ty], node_ty)),
                            found: Box::new(ty),
                            context: "print function value",
                        },
                    ));
                }
            }
            "panic" => {
                if params.len() != 1
                    || !type_accepts_owned(self.program, &self.ty(params[0])?, &Ty::Str)?
                    || !type_accepts_owned(self.program, &self.ty(*ret)?, &Ty::Never)?
                {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Fn(vec![node_ty], node_ty)),
                            found: Box::new(ty),
                            context: "panic function value",
                        },
                    ));
                }
            }
            "assert" => {
                if params.len() != 2
                    || !type_accepts_owned(self.program, &self.ty(params[0])?, &Ty::Bool)?
                    || !type_accepts_owned(self.program, &self.ty(params[1])?, &Ty::Str)?
                    || !type_accepts_owned(self.program, &self.ty(*ret)?, &Ty::Unit)?
                {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Fn(vec![node_ty], node_ty)),
                            found: Box::new(ty),
                            context: "assert function value",
                        },
                    ));
                }
            }
            _ => {
                return Err(self.error(
                    None,
                    VerifyErrorKind::Builtin {
                        id: builtin.0,
                        context: "unknown builtin function",
                    },
                ));
            }
        }
        Ok(ty)
    }

    fn builtin_item_type(&self, item: BuiltinItemId, node_ty: TyId) -> Result<Ty, VerifyError> {
        let Some(def) = item.item() else {
            return Err(self.error(
                None,
                VerifyErrorKind::Builtin {
                    id: item.0,
                    context: "module item value",
                },
            ));
        };
        let ty = self.ty(node_ty)?;
        match (def.module, def.name) {
            ("std.math", "PI" | "EULER") => {
                if !type_accepts_owned(self.program, &ty, &Ty::Float)? {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Float),
                            found: Box::new(ty),
                            context: "math constant",
                        },
                    ));
                }
                Ok(ty)
            }
            ("std.math", "min" | "max") => {
                let Ty::Fn(params, ret) = &ty else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Fn(Vec::new(), node_ty)),
                            found: Box::new(ty),
                            context: "math function value",
                        },
                    ));
                };
                if params.len() != 2
                    || !type_accepts_owned(
                        self.program,
                        &self.ty(params[0])?,
                        &self.ty(params[1])?,
                    )?
                    || !type_accepts_owned(self.program, &self.ty(*ret)?, &self.ty(params[0])?)?
                {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Fn(Vec::new(), node_ty)),
                            found: Box::new(ty),
                            context: "math function value",
                        },
                    ));
                }
                Ok(ty)
            }
            ("std.math", "clamp") => {
                let Ty::Fn(params, ret) = &ty else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Fn(Vec::new(), node_ty)),
                            found: Box::new(ty),
                            context: "math clamp value",
                        },
                    ));
                };
                if params.len() != 2
                    || !type_accepts_owned(
                        self.program,
                        &self.ty(params[0])?,
                        &self.ty(params[1])?,
                    )?
                    || !type_accepts_owned(self.program, &self.ty(*ret)?, &self.ty(params[0])?)?
                {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Fn(Vec::new(), node_ty)),
                            found: Box::new(ty),
                            context: "math clamp value",
                        },
                    ));
                }
                Ok(ty)
            }
            ("std.math", "sin" | "cos" | "tan" | "exp" | "ln" | "log10") => {
                let Ty::Fn(params, ret) = &ty else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Fn(Vec::new(), node_ty)),
                            found: Box::new(ty),
                            context: "math function value",
                        },
                    ));
                };
                if params.len() != 1
                    || !type_accepts_owned(self.program, &self.ty(params[0])?, &Ty::Float)?
                    || !type_accepts_owned(self.program, &self.ty(*ret)?, &Ty::Float)?
                {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Fn(Vec::new(), node_ty)),
                            found: Box::new(ty),
                            context: "math function value",
                        },
                    ));
                }
                Ok(ty)
            }
            ("std.math", "atan2") => {
                let Ty::Fn(params, ret) = &ty else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Fn(Vec::new(), node_ty)),
                            found: Box::new(ty),
                            context: "math atan2 value",
                        },
                    ));
                };
                if params.len() != 2
                    || !type_accepts_owned(self.program, &self.ty(params[0])?, &Ty::Float)?
                    || !type_accepts_owned(self.program, &self.ty(params[1])?, &Ty::Float)?
                    || !type_accepts_owned(self.program, &self.ty(*ret)?, &Ty::Float)?
                {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Fn(Vec::new(), node_ty)),
                            found: Box::new(ty),
                            context: "math atan2 value",
                        },
                    ));
                }
                Ok(ty)
            }
            ("std.json", "Json") => {
                if !type_accepts_owned(self.program, &ty, &self.json_ty())? {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(self.json_ty()),
                            found: Box::new(ty),
                            context: "json type value",
                        },
                    ));
                }
                Ok(ty)
            }
            ("std.json", "parse") => {
                let Ty::Fn(params, ret) = &ty else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Fn(Vec::new(), node_ty)),
                            found: Box::new(ty),
                            context: "json parse value",
                        },
                    ));
                };
                let Ty::Result(ok, err) = self.ty(*ret)? else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Result(node_ty, node_ty)),
                            found: Box::new(self.ty(*ret)?),
                            context: "json parse value",
                        },
                    ));
                };
                if params.len() != 1
                    || !type_accepts_owned(self.program, &self.ty(params[0])?, &Ty::Str)?
                    || !type_accepts_owned(self.program, &self.ty(ok)?, &self.json_ty())?
                    || !type_accepts_owned(self.program, &self.ty(err)?, &Ty::Str)?
                {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Fn(Vec::new(), node_ty)),
                            found: Box::new(ty),
                            context: "json parse value",
                        },
                    ));
                }
                Ok(ty)
            }
            ("std.json", "stringify") => {
                let Ty::Fn(params, ret) = &ty else {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Fn(Vec::new(), node_ty)),
                            found: Box::new(ty),
                            context: "json stringify value",
                        },
                    ));
                };
                if params.len() != 1
                    || !type_accepts_owned(self.program, &self.ty(params[0])?, &self.json_ty())?
                    || !type_accepts_owned(self.program, &self.ty(*ret)?, &Ty::Str)?
                {
                    return Err(self.error(
                        None,
                        VerifyErrorKind::TypeMismatch {
                            expected: Box::new(Ty::Fn(Vec::new(), node_ty)),
                            found: Box::new(ty),
                            context: "json stringify value",
                        },
                    ));
                }
                Ok(ty)
            }
            _ => Err(self.error(
                None,
                VerifyErrorKind::Builtin {
                    id: item.0,
                    context: "unknown builtin module item",
                },
            )),
        }
    }

    fn check_any_args(
        &mut self,
        first_arg: u32,
        count: u32,
        context: &'static str,
    ) -> Result<Vec<TyId>, VerifyError> {
        self.check_child_count(first_arg, count, context)?;
        let mut values = Vec::with_capacity(count as usize);
        for index in 0..count {
            let child = self.function.children[first_arg as usize + index as usize];
            let (actual, _) = self.check_arg(child, false, index)?;
            values.push(actual);
        }
        Ok(values)
    }

    fn check_builtin_method_call(
        &mut self,
        method: BuiltinMethodId,
        first_child: u32,
        node_ty: TyId,
    ) -> Result<Ty, VerifyError> {
        let Some(def) = method.def() else {
            return Err(self.error(
                None,
                VerifyErrorKind::Builtin {
                    id: method.0,
                    context: "method call",
                },
            ));
        };
        self.check_child_count(first_child, 1, "builtin method receiver")?;
        let receiver = self.function.children[first_child as usize];
        let (receiver_ty, _) = self.check_arg(receiver, def.mut_self, 0)?;
        let arity = builtin_method_arity(def.receiver, def.name).ok_or_else(|| {
            self.error(
                None,
                VerifyErrorKind::Builtin {
                    id: method.0,
                    context: "unknown builtin method",
                },
            )
        })?;
        self.check_child_count(first_child + 1, arity, "builtin method arguments")?;
        let mut args = Vec::with_capacity(arity as usize);
        for index in 0..arity {
            let child = self.function.children[(first_child + 1 + index) as usize];
            let (actual, _) = self.check_arg(child, false, index + 1)?;
            args.push(actual);
        }
        self.check_builtin_method(def, receiver_ty, &args, node_ty)?;
        self.ty(node_ty)
    }

    fn check_builtin_method(
        &self,
        def: &BuiltinMethodDef,
        receiver: TyId,
        args: &[TyId],
        node_ty: TyId,
    ) -> Result<(), VerifyError> {
        let receiver_ty = self.ty(receiver)?;
        let mut result = match def.receiver {
            BuiltinReceiver::List => {
                let Ty::List(item) = receiver_ty else {
                    return Err(self.bad_builtin_receiver(def, &Ty::List(receiver)));
                };
                let t = self.ty(item)?;
                match def.name {
                    "len" => {
                        self.require_arity(args.len(), 0, "list.len")?;
                        Ty::Int
                    }
                    "get" => {
                        self.require_arity(args.len(), 1, "list.get")?;
                        self.expect_arg_ty(args[0], &Ty::Int, "list.get")?;
                        Ty::Option(item)
                    }
                    "first" | "last" | "pop" => {
                        self.require_arity(args.len(), 0, "list element")?;
                        Ty::Option(item)
                    }
                    "contains" => {
                        self.require_arity(args.len(), 1, "list.contains")?;
                        self.expect_arg_ty(args[0], &t, "list.contains")?;
                        Ty::Bool
                    }
                    "map" => {
                        self.require_arity(args.len(), 1, "list.map")?;
                        let u = self.expect_fn_arg(
                            args[0],
                            std::slice::from_ref(&t),
                            None,
                            "list.map callback",
                        )?;
                        Ty::List(u)
                    }
                    "filter" => {
                        self.require_arity(args.len(), 1, "list.filter")?;
                        self.expect_fn_arg(
                            args[0],
                            std::slice::from_ref(&t),
                            Some(&Ty::Bool),
                            "list.filter callback",
                        )?;
                        Ty::List(item)
                    }
                    "fold" => {
                        self.require_arity(args.len(), 2, "list.fold")?;
                        let u = args[0];
                        let u_ty = self.ty(u)?;
                        self.expect_fn_arg(
                            args[1],
                            &[u_ty, t.clone()],
                            Some(&self.ty(u)?),
                            "list.fold callback",
                        )?;
                        self.ty(u)?
                    }
                    "any" | "all" => {
                        self.require_arity(args.len(), 1, "list predicate")?;
                        self.expect_fn_arg(
                            args[0],
                            std::slice::from_ref(&t),
                            Some(&Ty::Bool),
                            "list predicate",
                        )?;
                        Ty::Bool
                    }
                    "enumerate" => {
                        self.require_arity(args.len(), 0, "list.enumerate")?;
                        let int = self.find_ty(&Ty::Int)?;
                        Ty::List(self.find_ty(&Ty::Tuple(vec![int, item]))?)
                    }
                    "reversed" => {
                        self.require_arity(args.len(), 0, "list.reversed")?;
                        Ty::List(item)
                    }
                    "sorted" | "sort" => {
                        self.require_arity(args.len(), 0, "list.sort")?;
                        if def.name == "sorted" {
                            Ty::List(item)
                        } else {
                            Ty::Unit
                        }
                    }
                    "sorted_by" => {
                        self.require_arity(args.len(), 1, "list.sorted_by")?;
                        self.expect_fn_arg(
                            args[0],
                            std::slice::from_ref(&t),
                            None,
                            "list.sorted_by",
                        )?;
                        Ty::List(item)
                    }
                    "join" => {
                        self.require_arity(args.len(), 1, "list.join")?;
                        if !type_accepts_owned(self.program, &t, &Ty::Str)? {
                            return Err(self.error(
                                None,
                                VerifyErrorKind::TypeMismatch {
                                    expected: Box::new(Ty::Str),
                                    found: Box::new(t),
                                    context: "list.join receiver element",
                                },
                            ));
                        }
                        self.expect_arg_ty(args[0], &Ty::Str, "list.join")?;
                        Ty::Str
                    }
                    "push" => {
                        self.require_arity(args.len(), 1, "list.push")?;
                        self.expect_arg_ty(args[0], &t, "list.push")?;
                        Ty::Unit
                    }
                    "insert" => {
                        self.require_arity(args.len(), 2, "list.insert")?;
                        self.expect_arg_ty(args[0], &Ty::Int, "list.insert index")?;
                        self.expect_arg_ty(args[1], &t, "list.insert value")?;
                        Ty::Unit
                    }
                    "remove_at" => {
                        self.require_arity(args.len(), 1, "list.remove_at")?;
                        self.expect_arg_ty(args[0], &Ty::Int, "list.remove_at")?;
                        t
                    }
                    "extend" => {
                        self.require_arity(args.len(), 1, "list.extend")?;
                        self.expect_arg_ty(args[0], &Ty::List(item), "list.extend")?;
                        Ty::Unit
                    }
                    "clear" => {
                        self.require_arity(args.len(), 0, "list.clear")?;
                        Ty::Unit
                    }
                    _ => return Err(self.unknown_method(def)),
                }
            }
            BuiltinReceiver::Map => {
                let Ty::Map(key, value) = receiver_ty else {
                    return Err(self.bad_builtin_receiver(def, &Ty::Map(receiver, receiver)));
                };
                let key_ty = self.ty(key)?;
                match def.name {
                    "len" => {
                        self.require_arity(args.len(), 0, "map.len")?;
                        Ty::Int
                    }
                    "get" | "has" | "remove" => {
                        self.require_arity(args.len(), 1, "map lookup")?;
                        self.expect_arg_ty(args[0], &key_ty, "map lookup")?;
                        if def.name == "has" {
                            Ty::Bool
                        } else {
                            Ty::Option(value)
                        }
                    }
                    "keys" => {
                        self.require_arity(args.len(), 0, "map.keys")?;
                        Ty::List(key)
                    }
                    "values" => {
                        self.require_arity(args.len(), 0, "map.values")?;
                        Ty::List(value)
                    }
                    "entries" => {
                        self.require_arity(args.len(), 0, "map.entries")?;
                        let pair = self.find_ty(&Ty::Tuple(vec![key, value]))?;
                        Ty::List(pair)
                    }
                    "clear" => {
                        self.require_arity(args.len(), 0, "map.clear")?;
                        Ty::Unit
                    }
                    _ => return Err(self.unknown_method(def)),
                }
            }
            BuiltinReceiver::Str => {
                if !type_accepts_owned(self.program, &receiver_ty, &Ty::Str)? {
                    return Err(self.bad_builtin_receiver(def, &Ty::Str));
                }
                match def.name {
                    "len" => {
                        self.require_arity(args.len(), 0, "str.len")?;
                        Ty::Int
                    }
                    "chars" => {
                        self.require_arity(args.len(), 0, "str.chars")?;
                        let str_ty = self.find_ty(&Ty::Str)?;
                        Ty::List(str_ty)
                    }
                    "split" => {
                        self.require_arity(args.len(), 1, "str.split")?;
                        self.expect_arg_ty(args[0], &Ty::Str, "str.split")?;
                        let str_ty = self.find_ty(&Ty::Str)?;
                        Ty::List(str_ty)
                    }
                    "trim" | "to_upper" | "to_lower" => {
                        self.require_arity(args.len(), 0, "str transformation")?;
                        Ty::Str
                    }
                    "contains" | "starts_with" | "ends_with" => {
                        self.require_arity(args.len(), 1, "str predicate")?;
                        self.expect_arg_ty(args[0], &Ty::Str, "str predicate")?;
                        Ty::Bool
                    }
                    "find" => {
                        self.require_arity(args.len(), 1, "str.find")?;
                        self.expect_arg_ty(args[0], &Ty::Str, "str.find")?;
                        let int = self.find_ty(&Ty::Int)?;
                        Ty::Option(int)
                    }
                    "replace" => {
                        self.require_arity(args.len(), 2, "str.replace")?;
                        self.expect_arg_ty(args[0], &Ty::Str, "str.replace")?;
                        self.expect_arg_ty(args[1], &Ty::Str, "str.replace")?;
                        Ty::Str
                    }
                    "parse_int" => {
                        self.require_arity(args.len(), 0, "str.parse_int")?;
                        let int = self.find_ty(&Ty::Int)?;
                        Ty::Option(int)
                    }
                    "parse_float" => {
                        self.require_arity(args.len(), 0, "str.parse_float")?;
                        let float = self.find_ty(&Ty::Float)?;
                        Ty::Option(float)
                    }
                    _ => return Err(self.unknown_method(def)),
                }
            }
            BuiltinReceiver::Int => {
                if !type_accepts_owned(self.program, &receiver_ty, &Ty::Int)? {
                    return Err(self.bad_builtin_receiver(def, &Ty::Int));
                }
                match def.name {
                    "abs" => {
                        self.require_arity(args.len(), 0, "int.abs")?;
                        Ty::Int
                    }
                    "pow" => {
                        self.require_arity(args.len(), 1, "int.pow")?;
                        self.expect_arg_ty(args[0], &Ty::Int, "int.pow")?;
                        Ty::Int
                    }
                    _ => return Err(self.unknown_method(def)),
                }
            }
            BuiltinReceiver::Float => {
                if !type_accepts_owned(self.program, &receiver_ty, &Ty::Float)? {
                    return Err(self.bad_builtin_receiver(def, &Ty::Float));
                }
                match def.name {
                    "abs" | "floor" | "ceil" | "trunc" | "sqrt" => {
                        self.require_arity(args.len(), 0, "float method")?;
                        Ty::Float
                    }
                    "pow" => {
                        self.require_arity(args.len(), 1, "float.pow")?;
                        self.expect_arg_ty(args[0], &Ty::Float, "float.pow")?;
                        Ty::Float
                    }
                    "round" => {
                        self.require_arity(args.len(), 1, "float.round")?;
                        self.expect_arg_ty(args[0], &Ty::Int, "float.round")?;
                        Ty::Float
                    }
                    "is_nan" | "is_finite" => {
                        self.require_arity(args.len(), 0, "float predicate")?;
                        Ty::Bool
                    }
                    _ => return Err(self.unknown_method(def)),
                }
            }
            BuiltinReceiver::Range => {
                if !type_accepts_owned(self.program, &receiver_ty, &Ty::Range)? {
                    return Err(self.bad_builtin_receiver(def, &Ty::Range));
                }
                match def.name {
                    "len" => {
                        self.require_arity(args.len(), 0, "range.len")?;
                        Ty::Int
                    }
                    "contains" => {
                        self.require_arity(args.len(), 1, "range.contains")?;
                        self.expect_arg_ty(args[0], &Ty::Int, "range.contains")?;
                        Ty::Bool
                    }
                    "to_list" => {
                        self.require_arity(args.len(), 0, "range.to_list")?;
                        let int = self.find_ty(&Ty::Int)?;
                        Ty::List(int)
                    }
                    _ => return Err(self.unknown_method(def)),
                }
            }
            BuiltinReceiver::Option => {
                let Ty::Option(item) = receiver_ty else {
                    return Err(self.bad_builtin_receiver(def, &Ty::Option(receiver)));
                };
                let t = self.ty(item)?;
                match def.name {
                    "is_some" | "is_none" => {
                        self.require_arity(args.len(), 0, "option predicate")?;
                        Ty::Bool
                    }
                    "map" => {
                        self.require_arity(args.len(), 1, "option.map")?;
                        let u = self.expect_fn_arg(args[0], &[t], None, "option.map")?;
                        Ty::Option(u)
                    }
                    "and_then" => {
                        self.require_arity(args.len(), 1, "option.and_then")?;
                        let ret = self.expect_fn_arg(args[0], &[t], None, "option.and_then")?;
                        let ret_ty = self.ty(ret)?;
                        if !matches!(ret_ty, Ty::Option(_) | Ty::Never) {
                            return Err(self.error(
                                None,
                                VerifyErrorKind::TypeMismatch {
                                    expected: Box::new(Ty::Option(ret)),
                                    found: Box::new(ret_ty),
                                    context: "option.and_then callback",
                                },
                            ));
                        }
                        ret_ty
                    }
                    "unwrap_or" => {
                        self.require_arity(args.len(), 1, "option.unwrap_or")?;
                        self.expect_arg_ty(args[0], &t, "option.unwrap_or")?;
                        t
                    }
                    "ok_or" => {
                        self.require_arity(args.len(), 1, "option.ok_or")?;
                        let err = self.ty(args[0])?;
                        let _ = err;
                        Ty::Result(item, args[0])
                    }
                    "expect" => {
                        self.require_arity(args.len(), 1, "option.expect")?;
                        self.expect_arg_ty(args[0], &Ty::Str, "option.expect")?;
                        t
                    }
                    _ => return Err(self.unknown_method(def)),
                }
            }
            BuiltinReceiver::Result => {
                let Ty::Result(ok, err) = receiver_ty else {
                    return Err(self.bad_builtin_receiver(def, &Ty::Result(receiver, receiver)));
                };
                let ok_ty = self.ty(ok)?;
                let err_ty = self.ty(err)?;
                match def.name {
                    "is_ok" | "is_err" => {
                        self.require_arity(args.len(), 0, "result predicate")?;
                        Ty::Bool
                    }
                    "map" => {
                        self.require_arity(args.len(), 1, "result.map")?;
                        let u = self.expect_fn_arg(args[0], &[ok_ty], None, "result.map")?;
                        Ty::Result(u, err)
                    }
                    "map_err" => {
                        self.require_arity(args.len(), 1, "result.map_err")?;
                        let f = self.expect_fn_arg(args[0], &[err_ty], None, "result.map_err")?;
                        Ty::Result(ok, f)
                    }
                    "and_then" => {
                        self.require_arity(args.len(), 1, "result.and_then")?;
                        let ret = self.expect_fn_arg(args[0], &[ok_ty], None, "result.and_then")?;
                        let ret_ty = self.ty(ret)?;
                        if !matches!(ret_ty, Ty::Result(_, _) | Ty::Never) {
                            return Err(self.error(
                                None,
                                VerifyErrorKind::TypeMismatch {
                                    expected: Box::new(Ty::Result(ret, err)),
                                    found: Box::new(ret_ty),
                                    context: "result.and_then callback",
                                },
                            ));
                        }
                        ret_ty
                    }
                    "unwrap_or" => {
                        self.require_arity(args.len(), 1, "result.unwrap_or")?;
                        self.expect_arg_ty(args[0], &ok_ty, "result.unwrap_or")?;
                        ok_ty
                    }
                    "ok" => {
                        self.require_arity(args.len(), 0, "result.ok")?;
                        Ty::Option(ok)
                    }
                    "expect" => {
                        self.require_arity(args.len(), 1, "result.expect")?;
                        self.expect_arg_ty(args[0], &Ty::Str, "result.expect")?;
                        ok_ty
                    }
                    _ => return Err(self.unknown_method(def)),
                }
            }
        };
        let _ = &mut result;
        self.expect_ty_value(node_ty, &result, "builtin method result")
    }

    fn require_arity(
        &self,
        found: usize,
        expected: u32,
        context: &'static str,
    ) -> Result<(), VerifyError> {
        if found == expected as usize {
            Ok(())
        } else {
            Err(self.error(
                None,
                VerifyErrorKind::Arity {
                    expected,
                    found: found as u32,
                    context,
                },
            ))
        }
    }

    fn expect_arg_ty(
        &self,
        actual: TyId,
        expected: &Ty,
        context: &'static str,
    ) -> Result<(), VerifyError> {
        self.expect_ty_value(actual, expected, context)
    }

    fn expect_fn_arg(
        &self,
        actual: TyId,
        params: &[Ty],
        ret: Option<&Ty>,
        context: &'static str,
    ) -> Result<TyId, VerifyError> {
        let actual_ty = self.ty(actual)?;
        let Ty::Fn(actual_params, actual_ret) = actual_ty else {
            return Err(self.error(
                None,
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(Ty::Fn(Vec::new(), actual)),
                    found: Box::new(self.ty(actual)?),
                    context,
                },
            ));
        };
        if actual_params.len() != params.len() {
            return Err(self.error(
                None,
                VerifyErrorKind::Arity {
                    expected: params.len() as u32,
                    found: actual_params.len() as u32,
                    context,
                },
            ));
        }
        for (actual, expected) in actual_params.iter().zip(params) {
            if !type_accepts_owned(self.program, &self.ty(*actual)?, expected)? {
                return Err(self.error(
                    None,
                    VerifyErrorKind::TypeMismatch {
                        expected: Box::new(expected.clone()),
                        found: Box::new(self.ty(*actual)?),
                        context,
                    },
                ));
            }
        }
        if let Some(expected) = ret
            && !type_accepts_owned(self.program, &self.ty(actual_ret)?, expected)?
        {
            return Err(self.error(
                None,
                VerifyErrorKind::TypeMismatch {
                    expected: Box::new(expected.clone()),
                    found: Box::new(self.ty(actual_ret)?),
                    context,
                },
            ));
        }
        Ok(actual_ret)
    }

    fn find_ty(&self, ty: &Ty) -> Result<TyId, VerifyError> {
        self.program
            .types
            .tys
            .iter()
            .position(|candidate| type_equal(self.program, candidate, ty).unwrap_or(false))
            .map(|index| TyId::new(index as u32))
            .ok_or_else(|| {
                self.error(
                    None,
                    VerifyErrorKind::TypeTable {
                        context: "type is not interned",
                        ty: None,
                        def: None,
                    },
                )
            })
    }

    fn bad_builtin_receiver(&self, def: &BuiltinMethodDef, found: &Ty) -> VerifyError {
        self.error(
            None,
            VerifyErrorKind::BuiltinReceiver {
                expected: def.receiver.as_str(),
                found: format!("{found:?}").into_boxed_str(),
            },
        )
    }

    fn unknown_method(&self, def: &BuiltinMethodDef) -> VerifyError {
        self.error(
            None,
            VerifyErrorKind::Builtin {
                id: 0,
                context: def.name,
            },
        )
    }
}

fn builtin_method_arity(receiver: BuiltinReceiver, name: &str) -> Option<u32> {
    use BuiltinReceiver::*;
    Some(match (receiver, name) {
        (
            List,
            "len" | "first" | "last" | "pop" | "enumerate" | "reversed" | "sorted" | "sort"
            | "clear",
        ) => 0,
        (
            List,
            "get" | "contains" | "map" | "filter" | "any" | "all" | "sorted_by" | "join" | "push"
            | "remove_at" | "extend",
        ) => 1,
        (List, "fold" | "insert") => 2,
        (Map, "len" | "keys" | "values" | "entries" | "clear") => 0,
        (Map, "get" | "has" | "remove") => 1,
        (Str, "len" | "chars" | "trim" | "to_upper" | "to_lower" | "parse_int" | "parse_float") => {
            0
        }
        (Str, "split" | "contains" | "find" | "starts_with" | "ends_with") => 1,
        (Str, "replace") => 2,
        (Int, "abs") => 0,
        (Int, "pow") => 1,
        (Float, "abs" | "floor" | "ceil" | "trunc" | "sqrt" | "is_nan" | "is_finite") => 0,
        (Float, "pow" | "round") => 1,
        (Range, "len" | "to_list") => 0,
        (Range, "contains") => 1,
        (Option, "is_some" | "is_none") => 0,
        (Option, "map" | "and_then" | "unwrap_or" | "ok_or" | "expect") => 1,
        (Result, "is_ok" | "is_err" | "ok") => 0,
        (Result, "map" | "map_err" | "and_then" | "unwrap_or" | "expect") => 1,
        _ => return None,
    })
}

struct EffectsCheck<'a> {
    program: &'a IrProgram,
    summaries: Vec<Effects>,
}

impl<'a> EffectsCheck<'a> {
    fn new(program: &'a IrProgram) -> Self {
        Self {
            program,
            summaries: vec![Effects::NONE; program.fns.len()],
        }
    }

    fn run(&mut self) -> Result<(), VerifyError> {
        loop {
            let snapshot = self.summaries.clone();
            let mut changed = false;
            for function in &self.program.fns {
                if let Some(body) = function.body {
                    let effects = self.summarize_node(function, body, &snapshot);
                    if effects != self.summaries[function.id.index()] {
                        self.summaries[function.id.index()] = effects;
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }
        for function in &self.program.fns {
            if let Some(body) = function.body {
                self.verify_function(function, body)?;
            }
        }
        Ok(())
    }

    fn summarize_node(&self, function: &IrFn, id: IrId, summaries: &[Effects]) -> Effects {
        let Some(node) = function.node(id).copied() else {
            return Effects::NONE;
        };
        let mut effects = self.own_effects(function, node, summaries);
        for child in children_of(self.program, function, node) {
            effects |= self.summarize_node(function, child, summaries);
        }
        if function.is_safepoint(id) {
            effects |= Effects::SAFEPOINT;
        }
        effects
    }

    fn verify_function(&self, function: &IrFn, body: IrId) -> Result<(), VerifyError> {
        self.verify_node(function, body).map(|_| ())
    }

    fn verify_node(&self, function: &IrFn, id: IrId) -> Result<Effects, VerifyError> {
        let node = *function.node(id).expect("shape checked");
        let mut computed = self.own_effects(function, node, &self.summaries);
        for child in children_of(self.program, function, node) {
            computed |= self.verify_node(function, child)?;
        }
        if function.is_safepoint(id) {
            computed |= Effects::SAFEPOINT;
        }
        if !node.effects.contains(computed) {
            let missing = Effects::from_bits_truncate(computed.bits() & !node.effects.bits());
            return Err(VerifyError {
                function: Some(function.id),
                source: Some(function.source),
                node: Some(id),
                span: function.span(id),
                kind: VerifyErrorKind::Effects { missing },
            });
        }
        Ok(computed)
    }

    fn own_effects(&self, function: &IrFn, node: Node, summaries: &[Effects]) -> Effects {
        use Effects as E;
        let mut effects = E::NONE;
        match node.kind {
            NodeKind::Unit
            | NodeKind::Int(_)
            | NodeKind::Float(_)
            | NodeKind::Bool(_)
            | NodeKind::FnRef(_)
            | NodeKind::BuiltinFnRef(_)
            | NodeKind::BuiltinModuleRef(_)
            | NodeKind::SlotPlace(_)
            | NodeKind::Range { .. }
            | NodeKind::InclusiveRange { .. }
            | NodeKind::Block(_)
            | NodeKind::ExprBlock(_)
            | NodeKind::If(_)
            | NodeKind::While { .. }
            | NodeKind::Break
            | NodeKind::Continue
            | NodeKind::ExprStmt(_)
            | NodeKind::Charge(_)
            | NodeKind::MutArg(_) => {}
            NodeKind::Str(_) | NodeKind::StrInterp(_) => {
                effects |= E::DYN_COST | E::ALLOCATES;
            }
            NodeKind::ConstRef(_) => {
                if self.is_heap_ty(node.ty) {
                    effects |= E::DYN_COST | E::ALLOCATES | E::SHARES;
                }
            }
            NodeKind::SlotRead { slot, mode } => {
                effects |= E::READS_LOCAL;
                if mode == AccessMode::Share && self.is_heap_slot(function, slot) {
                    effects |= E::SHARES;
                }
                if mode == AccessMode::Take {
                    effects |= E::WRITES_LOCAL;
                }
            }
            NodeKind::CaptureRead(_) => {
                effects |= E::READS_LOCAL;
                if self.is_heap_ty(node.ty) {
                    effects |= E::SHARES;
                }
            }
            NodeKind::SlotWrite { .. } | NodeKind::SlotInit { .. } => {
                effects |= E::WRITES_LOCAL;
            }
            NodeKind::Field { .. } | NodeKind::TupleField { .. } => {
                effects |= E::READS_HEAP;
                if self.is_heap_ty(node.ty) {
                    effects |= E::SHARES;
                }
            }
            NodeKind::Index { .. } => {
                effects |= E::READS_HEAP | E::MAY_TRAP;
                if self.is_heap_ty(node.ty) {
                    effects |= E::SHARES;
                }
                effects |= E::DYN_COST;
            }
            NodeKind::Neg(_) => {
                if !matches!(self.ty(node.ty), Ok(Ty::Float)) {
                    effects |= E::MAY_TRAP;
                }
            }
            NodeKind::Not(_) => {}
            NodeKind::Add { .. }
            | NodeKind::Sub { .. }
            | NodeKind::Mul { .. }
            | NodeKind::Div { .. }
            | NodeKind::Rem { .. } => {
                let ty = self.ty(node.ty);
                match ty {
                    Ok(Ty::Int) => effects |= E::MAY_TRAP,
                    Ok(Ty::Str) => effects |= E::ALLOCATES | E::DYN_COST,
                    Ok(Ty::Float) => {}
                    _ => effects |= E::MAY_TRAP,
                }
            }
            NodeKind::Eq { .. }
            | NodeKind::Ne { .. }
            | NodeKind::Lt { .. }
            | NodeKind::Le { .. }
            | NodeKind::Gt { .. }
            | NodeKind::Ge { .. } => effects |= E::DYN_COST,
            NodeKind::And { .. } | NodeKind::Or { .. } | NodeKind::Coalesce { .. } => {}
            NodeKind::CallDirect { target, .. } | NodeKind::CallMethod { target, .. } => {
                effects |= E::CALLS;
                effects |= self.callee_effects(target, summaries);
            }
            NodeKind::CallValue { first_child, .. } => {
                effects |= E::CALLS;
                let callee = function
                    .children
                    .get(first_child as usize)
                    .and_then(|id| function.node(*id));
                match callee.map(|callee| callee.kind) {
                    Some(NodeKind::FnRef(target)) | Some(NodeKind::MakeClosure(target)) => {
                        effects |= self.callee_effects(target, summaries);
                    }
                    Some(NodeKind::BuiltinFnRef(builtin)) => {
                        effects |= builtin_fn_effects(builtin);
                    }
                    Some(NodeKind::BuiltinModuleRef(item)) => {
                        effects |= builtin_item_effects(item);
                    }
                    _ => {
                        effects |= E::MAY_TRAP | E::ALLOCATES | E::DYN_COST;
                        for summary in summaries {
                            effects |= mask_callee(*summary);
                        }
                        effects |= E::LOGS | E::PANICS | E::HOST;
                    }
                }
            }
            NodeKind::CallBuiltinFn { builtin, .. } => {
                effects |= builtin_fn_effects(builtin);
            }
            NodeKind::CallBuiltinMethod { method, .. } => {
                let mut call_effects = E::DYN_COST | E::READS_HEAP;
                if method.def().is_some_and(|def| def.mut_self) {
                    call_effects |= E::WRITES_HEAP | E::ALLOCATES;
                }
                if method
                    .def()
                    .is_some_and(|def| method_calls_closure(def.name))
                {
                    call_effects |= E::CALLS;
                    for summary in summaries {
                        call_effects |= mask_callee(*summary);
                    }
                }
                effects |= call_effects;
            }
            NodeKind::CallBuiltinModule { item, .. } => {
                effects |= builtin_item_effects(item);
            }
            NodeKind::CallHost { host, .. } => {
                effects |= E::HOST | E::DYN_COST;
                if self
                    .program
                    .host(host)
                    .is_some_and(|info| self.is_heap_ty(info.ret))
                {
                    effects |= E::ALLOCATES | E::SHARES;
                }
            }
            NodeKind::Struct { .. } | NodeKind::Tuple(_) | NodeKind::List(_) | NodeKind::Map(_) => {
                effects |= E::ALLOCATES
            }
            NodeKind::StructUpdate { .. } => effects |= E::ALLOCATES | E::SHARES,
            NodeKind::EnumVariant { first_payload, .. }
            | NodeKind::PreludeVariant { first_payload, .. } => {
                if !self.prelude_or_variant_empty(node, first_payload) {
                    effects |= E::ALLOCATES;
                }
            }
            NodeKind::Let { pattern, .. } => {
                effects |= self.pattern_effects(function, pattern);
            }
            NodeKind::Assign { .. } | NodeKind::AddAssign { .. } => {
                effects |= E::WRITES_LOCAL | E::WRITES_HEAP | E::ALLOCATES | E::DYN_COST;
                if matches!(node.kind, NodeKind::AddAssign { .. }) {
                    effects |= E::MAY_TRAP;
                }
            }
            NodeKind::SubAssign { .. }
            | NodeKind::MulAssign { .. }
            | NodeKind::DivAssign { .. }
            | NodeKind::RemAssign { .. } => {
                effects |=
                    E::WRITES_LOCAL | E::WRITES_HEAP | E::ALLOCATES | E::DYN_COST | E::MAY_TRAP;
            }
            NodeKind::For { .. } => {
                effects |= E::ALLOCATES;
                if let NodeKind::For { pattern, .. } = node.kind {
                    effects |= self.pattern_effects(function, pattern);
                }
            }
            NodeKind::Return(_) => effects |= E::NONLOCAL_EXIT,
            NodeKind::Try(_) => effects |= E::NONLOCAL_EXIT | E::MAY_TRAP | E::ALLOCATES,
            NodeKind::Match { .. } => {
                effects |= E::MAY_TRAP | E::READS_HEAP | E::WRITES_LOCAL;
            }
            NodeKind::ConvertInt(_) => effects |= E::MAY_TRAP,
            NodeKind::ConvertFloat(_) => {}
            NodeKind::ConvertStr(_) => effects |= E::DYN_COST | E::ALLOCATES,
            NodeKind::MakeClosure(target) => {
                effects |= E::ALLOCATES;
                if let Some(lambda) = self.program.function(target) {
                    if !lambda.captures.is_empty() {
                        effects |= E::READS_LOCAL;
                    }
                    if lambda
                        .captures
                        .iter()
                        .any(|capture| self.is_heap_ty(capture.ty))
                    {
                        effects |= E::SHARES;
                    }
                }
            }
            NodeKind::Assert(_) | NodeKind::Panic(_) => effects |= E::MAY_TRAP | E::PANICS,
        }
        effects
    }

    fn is_heap_slot(&self, function: &IrFn, slot: Slot) -> bool {
        function
            .slot_info
            .get(slot.index())
            .is_some_and(|info| self.is_heap_ty(info.ty))
    }

    fn is_heap_ty(&self, ty: TyId) -> bool {
        matches!(
            self.program.types.get_ty(ty),
            Some(
                Ty::Str
                    | Ty::Tuple(_)
                    | Ty::List(_)
                    | Ty::Map(_, _)
                    | Ty::Option(_)
                    | Ty::Result(_, _)
                    | Ty::Fn(_, _)
                    | Ty::Named(_, _)
                    | Ty::Param(_)
            )
        )
    }

    fn ty(&self, id: TyId) -> Result<Ty, VerifyError> {
        self.program
            .types
            .get_ty(id)
            .cloned()
            .ok_or_else(|| type_error(id))
    }

    fn callee_effects(&self, target: FnId, summaries: &[Effects]) -> Effects {
        summaries
            .get(target.index())
            .copied()
            .map(mask_callee)
            .unwrap_or(Effects::NONE)
    }

    fn pattern_effects(&self, function: &IrFn, pattern: PatternId) -> Effects {
        let Some(pattern) = function.pattern(pattern) else {
            return Effects::NONE;
        };
        let mut effects = Effects::NONE;
        self.collect_pattern_effects(function, pattern, &mut effects);
        effects
    }

    fn collect_pattern_effects(&self, function: &IrFn, pattern: &Pattern, effects: &mut Effects) {
        match pattern {
            Pattern::Wild => {}
            Pattern::Bind { slot, .. } => {
                *effects |= Effects::WRITES_LOCAL;
                if self.is_heap_slot(function, *slot) {
                    *effects |= Effects::SHARES;
                }
            }
            Pattern::Int(_) | Pattern::Bool(_) | Pattern::Str(_) => {
                *effects |= Effects::READS_HEAP;
            }
            Pattern::Tuple(items) | Pattern::Or(items) => {
                *effects |= Effects::READS_HEAP;
                for item in items {
                    self.collect_pattern_effects(function, item, effects);
                }
            }
            Pattern::Variant { args, .. } => {
                *effects |= Effects::READS_HEAP;
                for item in args {
                    self.collect_pattern_effects(function, item, effects);
                }
            }
            Pattern::Prelude { args, .. } => {
                *effects |= Effects::READS_HEAP;
                for item in args {
                    self.collect_pattern_effects(function, item, effects);
                }
            }
            Pattern::Struct { fields, .. } => {
                *effects |= Effects::READS_HEAP;
                for (_, item) in fields {
                    self.collect_pattern_effects(function, item, effects);
                }
            }
        }
    }

    fn prelude_or_variant_empty(&self, node: Node, first_payload: u32) -> bool {
        match node.kind {
            NodeKind::PreludeVariant { .. } => first_payload == 0,
            _ => false,
        }
    }
}

fn mask_callee(effects: Effects) -> Effects {
    let mask = !(Effects::READS_LOCAL.bits()
        | Effects::WRITES_LOCAL.bits()
        | Effects::NONLOCAL_EXIT.bits());
    Effects::from_bits_truncate(effects.bits() & mask)
}

fn builtin_fn_effects(builtin: BuiltinFnId) -> Effects {
    match builtin.def().map(|def| def.name) {
        Some("print") => Effects::LOGS | Effects::DYN_COST | Effects::ALLOCATES,
        Some("panic") | Some("assert") => Effects::PANICS | Effects::MAY_TRAP,
        _ => Effects::NONE,
    }
}

fn builtin_item_effects(item: BuiltinItemId) -> Effects {
    match item.item().map(|def| (def.module, def.name)) {
        Some(("std.json", "parse" | "stringify")) => {
            Effects::DYN_COST | Effects::ALLOCATES | Effects::READS_HEAP
        }
        Some(("std.math", _)) => Effects::DYN_COST,
        _ => Effects::NONE,
    }
}

fn method_calls_closure(name: &str) -> bool {
    matches!(
        name,
        "map" | "filter" | "fold" | "any" | "all" | "sorted_by" | "and_then" | "map_err"
    )
}

pub(crate) fn children_of(program: &IrProgram, function: &IrFn, node: Node) -> Vec<IrId> {
    let mut children = Vec::new();
    match node.kind {
        NodeKind::Field { base, .. }
        | NodeKind::TupleField { base, .. }
        | NodeKind::Neg(base)
        | NodeKind::Not(base)
        | NodeKind::ConvertInt(base)
        | NodeKind::ConvertFloat(base)
        | NodeKind::ConvertStr(base)
        | NodeKind::SlotWrite { value: base, .. }
        | NodeKind::SlotInit { value: base, .. }
        | NodeKind::Return(Some(base))
        | NodeKind::Try(base)
        | NodeKind::ExprStmt(base)
        | NodeKind::MutArg(base) => children.push(base),
        NodeKind::Index { base, index } => {
            children.push(base);
            children.push(index);
        }
        NodeKind::Add { lhs, rhs }
        | NodeKind::Sub { lhs, rhs }
        | NodeKind::Mul { lhs, rhs }
        | NodeKind::Div { lhs, rhs }
        | NodeKind::Rem { lhs, rhs }
        | NodeKind::Eq { lhs, rhs }
        | NodeKind::Ne { lhs, rhs }
        | NodeKind::Lt { lhs, rhs }
        | NodeKind::Le { lhs, rhs }
        | NodeKind::Gt { lhs, rhs }
        | NodeKind::Ge { lhs, rhs }
        | NodeKind::And { lhs, rhs }
        | NodeKind::Or { lhs, rhs }
        | NodeKind::Coalesce { lhs, rhs }
        | NodeKind::Assign {
            place: lhs,
            value: rhs,
        }
        | NodeKind::AddAssign {
            place: lhs,
            value: rhs,
        }
        | NodeKind::SubAssign {
            place: lhs,
            value: rhs,
        }
        | NodeKind::MulAssign {
            place: lhs,
            value: rhs,
        }
        | NodeKind::DivAssign {
            place: lhs,
            value: rhs,
        }
        | NodeKind::RemAssign {
            place: lhs,
            value: rhs,
        } => {
            children.push(lhs);
            children.push(rhs);
        }
        NodeKind::Let { value, .. } => children.push(value),
        NodeKind::While { cond, body } => {
            children.push(cond);
            children.push(body);
        }
        NodeKind::CallDirect { target, first_arg } => {
            if let Some(target) = program.function(target) {
                for index in 0..target.params.len() {
                    if let Some(child) = function.children.get(first_arg as usize + index) {
                        children.push(*child);
                    }
                }
            }
        }
        NodeKind::CallMethod {
            target,
            first_child,
        } => {
            if let Some(target) = program.function(target) {
                for index in 0..target.params.len() {
                    if let Some(child) = function.children.get(first_child as usize + index) {
                        children.push(*child);
                    }
                }
            }
        }
        NodeKind::CallValue { first_child, argc } => {
            for index in 0..=argc {
                if let Some(child) = function.children.get(first_child as usize + index as usize) {
                    children.push(*child);
                }
            }
        }
        NodeKind::CallBuiltinFn { builtin, first_arg } => {
            if let Some(arity) = builtin.def().map(|def| builtin_fn_arity(def.name)) {
                for index in 0..arity {
                    if let Some(child) = function.children.get(first_arg as usize + index as usize)
                    {
                        children.push(*child);
                    }
                }
            }
        }
        NodeKind::CallBuiltinMethod {
            method,
            first_child,
        } => {
            if let Some(arity) = method
                .def()
                .and_then(|def| builtin_method_arity(def.receiver, def.name))
            {
                for index in 0..=arity {
                    if let Some(child) =
                        function.children.get(first_child as usize + index as usize)
                    {
                        children.push(*child);
                    }
                }
            }
        }
        NodeKind::CallBuiltinModule { item, first_arg } => {
            if let Some(arity) = item.item().map(|def| builtin_item_arity(def.name)) {
                for index in 0..arity {
                    if let Some(child) = function.children.get(first_arg as usize + index as usize)
                    {
                        children.push(*child);
                    }
                }
            }
        }
        NodeKind::CallHost { host, first_arg } => {
            if let Some(host) = program.host(host) {
                for index in 0..host.params.len() {
                    if let Some(child) = function.children.get(first_arg as usize + index) {
                        children.push(*child);
                    }
                }
            }
        }
        NodeKind::Struct {
            type_id,
            first_field,
        } => {
            if let Some(def) = program.types.get(type_id)
                && let TypeDefKind::Struct { fields } = &def.kind
            {
                for index in 0..fields.len() {
                    if let Some(child) = function.children.get(first_field as usize + index) {
                        children.push(*child);
                    }
                }
            }
        }
        NodeKind::StructUpdate { type_id, layout } => {
            let layout = function.struct_layout(layout);
            let first_child = layout.map(|layout| layout.first_child).unwrap_or(0);
            let fields = layout.map(|layout| layout.fields.len()).unwrap_or(0);
            let _ = type_id;
            for index in 0..=fields {
                if let Some(child) = function.children.get(first_child as usize + index) {
                    children.push(*child);
                }
            }
        }
        NodeKind::EnumVariant {
            variant,
            first_payload,
        } => {
            let count = program
                .types
                .get_ty(node.ty)
                .and_then(|ty| match ty {
                    Ty::Named(type_id, _) => program.types.get(*type_id),
                    _ => None,
                })
                .and_then(|def| match &def.kind {
                    TypeDefKind::Enum { variants } => variants
                        .get(variant as usize)
                        .map(|variant| variant.payload.len()),
                    _ => None,
                })
                .unwrap_or(0);
            for index in 0..count {
                if let Some(child) = function.children.get(first_payload as usize + index) {
                    children.push(*child);
                }
            }
        }
        NodeKind::PreludeVariant {
            variant,
            first_payload,
        } => {
            let count = match variant {
                PreludeVariant::None => 0,
                PreludeVariant::Some | PreludeVariant::Ok | PreludeVariant::Err => 1,
            };
            for index in 0..count {
                if let Some(child) = function.children.get(first_payload as usize + index) {
                    children.push(*child);
                }
            }
        }
        NodeKind::Tuple(range)
        | NodeKind::List(range)
        | NodeKind::Map(range)
        | NodeKind::Block(range)
        | NodeKind::ExprBlock(range)
        | NodeKind::If(range)
        | NodeKind::Assert(range)
        | NodeKind::Panic(range) => {
            children.extend(
                function
                    .children
                    .get(range.as_range())
                    .unwrap_or(&[])
                    .iter()
                    .copied(),
            );
        }
        NodeKind::For { first_child, .. } => {
            for index in 0..2 {
                if let Some(child) = function.children.get(first_child as usize + index) {
                    children.push(*child);
                }
            }
        }
        NodeKind::Match {
            match_set,
            scrutinee,
        } => {
            children.push(scrutinee);
            if let Some(set) = function.match_set(match_set) {
                for arm in &set.arms {
                    if let Some(guard) = arm.guard {
                        children.push(guard);
                    }
                    children.push(arm.body);
                }
            }
        }
        NodeKind::StrInterp(range) => {
            if let Some(parts) = function.str_parts.get(range.as_range()) {
                for part in parts {
                    if let StrPart::Expr(expr) = part {
                        children.push(*expr);
                    }
                }
            }
        }
        NodeKind::MakeClosure(_)
        | NodeKind::FnRef(_)
        | NodeKind::BuiltinFnRef(_)
        | NodeKind::BuiltinModuleRef(_)
        | NodeKind::Unit
        | NodeKind::Int(_)
        | NodeKind::Float(_)
        | NodeKind::Bool(_)
        | NodeKind::Str(_)
        | NodeKind::ConstRef(_)
        | NodeKind::SlotRead { .. }
        | NodeKind::CaptureRead(_)
        | NodeKind::SlotPlace(_)
        | NodeKind::Break
        | NodeKind::Continue
        | NodeKind::Return(None)
        | NodeKind::Charge(_) => {}
        NodeKind::Range { start, end } | NodeKind::InclusiveRange { start, end } => {
            children.push(start);
            children.push(end);
        }
    }
    children
}

fn builtin_fn_arity(name: &str) -> u32 {
    match name {
        "print" | "panic" => 1,
        "assert" => 2,
        _ => 0,
    }
}

fn builtin_item_arity(name: &str) -> u32 {
    match name {
        "min" | "max" | "atan2" => 2,
        "clamp" => 3,
        "sin" | "cos" | "tan" | "exp" | "ln" | "log10" | "parse" | "stringify" => 1,
        _ => 0,
    }
}
