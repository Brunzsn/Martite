//! Resolved, self-contained tree IR for Martite.
//!
//! This crate is the data model between the checker and the runtime. Lowering
//! from `CheckedProgram` starts in #103; this crate currently owns the type,
//! builtin and id vocabulary that lowering and the runtime share. There is no
//! evaluator, optimizer, printer or verifier here yet.
//!
//! Dependency direction: `martite-ir` may depend on `martite-base`,
//! `martite-fuel`, `martite-syntax` and `martite-check` for lowering inputs,
//! but never on `martite-runtime` or `martite-bench`.
//!
//! # Layout
//!
//! [`Node`] is exactly 32 bytes:
//!
//! | Field | Size |
//! | --- | --- |
//! | `kind` | 12 |
//! | `ty` | 4 |
//! | `charge` | 12 |
//! | `effects` | 4 |
//!
//! [`NodeKind`] is a compact opcode enum. Its variant is the operator or node
//! kind, fixed-arity children are inline (at most two [`IrId`]s or one
//! [`ChildRange`]), and variable-arity children live in [`IrFn::children`].
//! 64-bit scalar literals live in the const pool so `NodeKind` stays 4-byte
//! aligned. The exact sizes are pinned by `tests/layout.rs`.
//!
//! Per-function side vectors carry spans, synthetic provenance and the
//! arenas whose payloads do not fit on a node. They are indexed by the ids in
//! [`id`], and the verifier will check that they stay parallel to `nodes`.

pub mod builtin;
pub mod charge;
pub mod effects;
pub mod id;
pub mod node;
pub mod pattern;
pub mod program;
pub mod types;

pub use builtin::{BuiltinFnDef, BuiltinItemDef, BuiltinItemKind};
pub use charge::Charge;
pub use effects::Effects;
pub use id::{
    BuiltinFnId, BuiltinItemId, BuiltinMethodId, CaptureIndex, ConstId, FnId, HostId, IrId,
    MatchSetId, PatternId, Slot, StrId, StructLayoutId,
};
pub use martite_base::{SourceId, Span};
pub use node::{
    AccessMode, BinaryOp, ChildRange, CompoundOp, Node, NodeKind, OperandClass, StrPart, UnaryOp,
};
pub use pattern::{MatchArm, MatchSet, Pattern, StructLayout};
pub use program::{
    CaptureInfo, ConstValue, Entries, EntryKind, FnKind, HookSig, HostFnInfo, IrFn, IrProgram,
    ParamInfo, PreludeVariant, SlotInfo,
};
pub use types::{FieldDef, Ty, TyId, TypeDef, TypeDefKind, TypeId, TypeTable, VariantDef};
