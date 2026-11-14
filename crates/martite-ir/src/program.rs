//! Program and function data.
//!
//! [`IrProgram`] owns every function, pool and entry point. [`IrFn`] owns one
//! node arena plus the parallel side vectors and payload arenas described in
//! [`crate::node`]. All of it is plain data: no references into the checker,
//! and no interpreter state.

use martite_base::{SourceId, Span};

use crate::id::{
    CaptureIndex, ConstId, FnId, HostId, IrId, MatchSetId, PatternId, Slot, StrId, StructLayoutId,
};
use crate::node::{ChildRange, Node, StrPart};
use crate::pattern::{MatchSet, Pattern, StructLayout};
use crate::types::{TyId, TypeId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreludeVariant {
    None,
    Some,
    Ok,
    Err,
}

/// An IR-owned constant or literal value.
///
/// Strings reference the interned static-string pool. Struct fields are in
/// declaration order and enum payloads are in declaration order, matching the
/// IR type table rather than carrying checker names.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(StrId),
    Tuple(Vec<ConstValue>),
    List(Vec<ConstValue>),
    Map(Vec<(ConstValue, ConstValue)>),
    Struct(TypeId, Vec<ConstValue>),
    Enum(TypeId, u32, Vec<ConstValue>),
    Prelude(PreludeVariant, Vec<ConstValue>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FnKind {
    Free,
    Method(TypeId),
    Handler,
    Test,
    Hook,
    Lambda,
    /// A host/interface function. Its body is absent; signature metadata is in
    /// [`IrProgram::hosts`].
    Host(HostId),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParamInfo {
    pub slot: Slot,
    pub name: Box<str>,
    pub ty: TyId,
    pub mutable: bool,
    pub is_self: bool,
    /// `Some` when a lambda parameter is destructured; `None` for a plain
    /// parameter binding.
    pub pattern: Option<PatternId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SlotInfo {
    pub ty: TyId,
    pub mutable: bool,
    pub name: Option<Box<str>>,
}

/// Where a closure's captured value comes from in the frame that creates it.
///
/// A direct capture reads a frame slot. A nested lambda may capture a binding
/// that its enclosing lambda itself captured; that value is read from the
/// enclosing closure's capture array instead of a local slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptureSource {
    Slot(Slot),
    Capture(CaptureIndex),
}

/// One lambda capture: the value snapshotted when the closure is made.
/// The capture index is this entry's position in [`IrFn::captures`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CaptureInfo {
    pub source: CaptureSource,
    pub ty: TyId,
    pub name: Option<Box<str>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HostFnInfo {
    /// Stable name used in traces.
    pub name: Box<str>,
    /// Module path used for the grant check.
    pub module: Box<str>,
    pub params: Vec<TyId>,
    pub ret: TyId,
    /// Some hosts return an opaque handle type rather than a plain value.
    pub opaque_return: Option<TypeId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HookSig {
    pub name: Box<str>,
    pub module: Box<str>,
    pub params: Vec<TyId>,
    pub ret: TyId,
    pub source: SourceId,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum EntryKind {
    #[default]
    Library,
    Script,
    Plugin,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Entries {
    pub main: Option<FnId>,
    pub tests: Vec<FnId>,
    pub handlers: Vec<(Box<str>, FnId)>,
    pub hooks: Vec<(Box<str>, HookSig)>,
    pub init: Option<FnId>,
    pub shutdown: Option<FnId>,
    pub state_type: Option<TyId>,
    pub json_type: Option<TypeId>,
    pub entry_kind: EntryKind,
}

/// One function, method, handler, test, hook, host stub or lambda.
///
/// `nodes` is the function-local arena. `spans` and `synthetic` are parallel
/// to it. The remaining vectors are payload arenas referenced by node kinds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrFn {
    pub id: FnId,
    pub name: Box<str>,
    /// The function's defining source. Kept per function so a lambda's traps
    /// can name the defining file instead of the caller.
    ///
    /// Compatibility note: the retired AST interpreter named the caller's
    /// source for lambda traps (#118). Lowering records the correct defining
    /// source; until #118 lands, the IR evaluator must reproduce the recorded
    /// caller-source behavior to keep snapshots green.
    pub source: SourceId,
    pub kind: FnKind,
    pub params: Vec<ParamInfo>,
    /// Declared or inferred return type. The body's value type and every
    /// `Return`/`Try` in the function are checked against it.
    pub ret: TyId,
    /// Frame size in slots.
    pub locals: u32,
    pub slot_info: Vec<SlotInfo>,
    pub captures: Vec<CaptureInfo>,
    pub patterns: Vec<Pattern>,
    pub matches: Vec<MatchSet>,
    pub struct_layouts: Vec<StructLayout>,
    pub str_parts: Vec<StrPart>,
    pub nodes: Vec<Node>,
    /// Parallel to `nodes`, indexed by [`IrId`].
    pub spans: Vec<Span>,
    /// Parallel to `nodes`. `None` for lowered nodes, `Some(origin)` for a
    /// node created by an optimizer pass.
    pub synthetic: Vec<Option<IrId>>,
    /// Parallel to `nodes`. True when this exact node performs the memory and
    /// deadline check. `Effects::SAFEPOINT` remains the subtree summary; this
    /// vector records where the safepoint actually is.
    pub safepoints: Vec<bool>,
    /// Children of n-ary nodes and the inline first child of call nodes.
    pub children: Vec<IrId>,
    /// Absent for host and interface declarations.
    pub body: Option<IrId>,
}

impl IrFn {
    pub fn node(&self, id: IrId) -> Option<&Node> {
        self.nodes.get(id.index())
    }

    pub fn span(&self, id: IrId) -> Option<Span> {
        self.spans.get(id.index()).copied()
    }

    pub fn synthetic_origin(&self, id: IrId) -> Option<IrId> {
        self.synthetic.get(id.index()).copied().flatten()
    }

    pub fn is_synthetic(&self, id: IrId) -> bool {
        matches!(self.synthetic.get(id.index()), Some(Some(_)))
    }

    pub fn is_safepoint(&self, id: IrId) -> bool {
        self.safepoints.get(id.index()).copied().unwrap_or(false)
    }

    pub fn child_slice(&self, range: ChildRange) -> &[IrId] {
        self.children.get(range.as_range()).unwrap_or(&[])
    }

    pub fn pattern(&self, id: PatternId) -> Option<&Pattern> {
        self.patterns.get(id.index())
    }

    pub fn match_set(&self, id: MatchSetId) -> Option<&MatchSet> {
        self.matches.get(id.index())
    }

    pub fn struct_layout(&self, id: StructLayoutId) -> Option<&StructLayout> {
        self.struct_layouts.get(id.index())
    }

    /// `StrInterp` ranges index `str_parts` rather than `children`.
    pub fn str_part_slice(&self, range: ChildRange) -> &[StrPart] {
        self.str_parts.get(range.as_range()).unwrap_or(&[])
    }

    pub fn body_node(&self) -> Option<&Node> {
        self.body.and_then(|id| self.node(id))
    }
}

/// The complete lowered program.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IrProgram {
    pub fns: Vec<IrFn>,
    pub consts: Vec<ConstValue>,
    pub strs: Vec<Box<str>>,
    pub types: crate::types::TypeTable,
    pub hosts: Vec<HostFnInfo>,
    pub grants: Vec<Box<str>>,
    pub entries: Entries,
    /// Source names indexed by [`SourceId`]. The IR itself only needs the name
    /// for diagnostics and dumps, so the text stays in [`martite_base::SourceMap`].
    pub source_names: Vec<Box<str>>,
}

impl IrProgram {
    pub fn function(&self, id: FnId) -> Option<&IrFn> {
        self.fns.get(id.index())
    }

    pub fn constant(&self, id: ConstId) -> Option<&ConstValue> {
        self.consts.get(id.index())
    }

    pub fn string(&self, id: StrId) -> Option<&str> {
        self.strs.get(id.index()).map(AsRef::as_ref)
    }

    pub fn host(&self, id: HostId) -> Option<&HostFnInfo> {
        self.hosts.get(id.index())
    }

    pub fn source_name(&self, id: SourceId) -> Option<&str> {
        self.source_names.get(id.0 as usize).map(AsRef::as_ref)
    }

    pub fn has_grant(&self, module: &str) -> bool {
        self.grants.iter().any(|grant| grant.as_ref() == module)
    }
}
