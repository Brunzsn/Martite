//! Resolved, self-contained tree IR for Martite.
//!
//! This crate is the data model between the checker and the runtime. It owns
//! the type, builtin and id vocabulary, the deterministic text printer
//! ([`Printer`]) for dumps and golden tests, the verifier ([`verify`]) that
//! enforces the contract between lowering and every pass, the on-demand slot
//! analyses in [`analysis`], and the first half of lowering ([`lower`]). Calls,
//! match patterns and lambdas are lowered in #104–#106; there is no evaluator
//! or optimizer here yet.
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
//! Per-function side vectors carry spans, synthetic provenance, safepoints and
//! the arenas whose payloads do not fit on a node. They are indexed by the ids
//! in [`id`], and [`verify`] checks that they stay parallel to `nodes`.

pub mod analysis;
pub mod builtin;
pub mod charge;
pub mod effects;
pub mod id;
pub mod lower;
pub mod node;
pub mod pattern;
pub mod print;
pub mod program;
pub mod types;
pub mod verify;

/// Maximum IR tree depth per function, enforced by lowering and the verifier.
///
/// The parser's `nesting_depth` limit does not bound operator, postfix or
/// `else if` chains, so the IR owns this limit.
pub const MAX_NODE_DEPTH: u32 = 1024;

pub use builtin::{
    BuiltinFnDef, BuiltinItemDef, BuiltinItemKind, BuiltinMethodDef, BuiltinReceiver,
};
pub use charge::Charge;
pub use effects::Effects;
pub use id::{
    BuiltinFnId, BuiltinItemId, BuiltinMethodId, CaptureIndex, ConstId, FnId, HostId, IrId,
    MatchSetId, PatternId, Slot, StrId, StructLayoutId,
};
pub use lower::{LowerError, LowerErrorKind, lower};
pub use martite_base::{SourceId, Span};
pub use node::{
    AccessMode, BinaryOp, ChildRange, CompoundOp, Node, NodeKind, OperandClass, StrPart, UnaryOp,
};
pub use pattern::{MatchArm, MatchSet, Pattern, StructLayout};
pub use print::{FORMAT_VERSION, Printer};
pub use program::{
    CaptureInfo, ConstValue, Entries, EntryKind, FnKind, HookSig, HostFnInfo, IrFn, IrProgram,
    ParamInfo, PreludeVariant, SlotInfo,
};
pub use types::{FieldDef, Ty, TyId, TypeDef, TypeDefKind, TypeId, TypeTable, VariantDef};
pub use verify::{VerifyError, VerifyErrorKind, verify, verify_after_pass};
