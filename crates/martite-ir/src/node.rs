//! Nodes and node kinds.
//!
//! A node stores only its kind, type, charge schedule and subtree effects.
//! Children are inline for fixed arity and live in [`crate::IrFn::children`]
//! for variable arity. The variant discriminant of [`NodeKind`] is also the
//! operator/kind discriminator, which keeps the kind within 12 bytes so
//! [`Node`] stays at 32 bytes.

use crate::charge::Charge;
use crate::effects::Effects;
use crate::id::{
    BuiltinFnId, BuiltinItemId, BuiltinMethodId, CaptureIndex, ConstId, FnId, HostId, IrId,
    MatchSetId, PatternId, Slot, StrId, StructLayoutId,
};
use crate::program::PreludeVariant;
use crate::types::{TyId, TypeId};

/// A half-open range into one of the per-function arenas on
/// [`crate::IrFn`].
///
/// A range names [`crate::IrFn::children`] by default. A
/// [`NodeKind::StrInterp`] range names [`crate::IrFn::str_parts`] instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ChildRange {
    pub start: u32,
    pub len: u32,
}

impl ChildRange {
    pub const fn new(start: u32, len: u32) -> Self {
        Self { start, len }
    }

    pub const fn empty() -> Self {
        Self::new(0, 0)
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub const fn end(self) -> u32 {
        self.start.saturating_add(self.len)
    }

    pub fn as_range(self) -> std::ops::Range<usize> {
        self.start as usize..self.end() as usize
    }
}

/// How a slot read treats a heap-typed value. Rule S1 makes sharing semantic:
/// `Share` adds a reference (and affects copy-on-write charges), `Take` moves
/// it out and is a `Reduce`-class change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessMode {
    Share,
    Take,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnaryOp {
    Neg,
    Not,
}

impl UnaryOp {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Neg => "-",
            Self::Not => "!",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
    Coalesce,
}

impl BinaryOp {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Rem => "%",
            Self::Eq => "==",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::And => "and",
            Self::Or => "or",
            Self::Coalesce => "??",
        }
    }

    pub const fn is_comparison(self) -> bool {
        matches!(
            self,
            Self::Eq | Self::Ne | Self::Lt | Self::Le | Self::Gt | Self::Ge
        )
    }
}

/// The operator in a compound assignment. The read of the current place value
/// is part of the node's evaluation, not a separate binary node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompoundOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}

impl CompoundOp {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Add => "+=",
            Self::Sub => "-=",
            Self::Mul => "*=",
            Self::Div => "/=",
            Self::Rem => "%=",
        }
    }
}

/// How a binary operator is evaluated. `Dyn` covers generic `Eq`/`Ord` and any
/// operand whose type is a type parameter.
///
/// The class is a verifier fact derived from the node and child types; storing
/// it on every binary node would not fit the 12-byte `NodeKind` budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OperandClass {
    Int,
    Float,
    Str,
    Dyn,
}

/// One part of an interpolated string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StrPart {
    Text(StrId),
    Expr(IrId),
}

/// The operation or shape of one IR node.
///
/// The variant is the discriminator. Fixed-arity children are inline; n-ary
/// children are in [`crate::IrFn::children`]. Payloads are deliberately kept
/// to two ids or one [`ChildRange`] (8 bytes) so the enum stays 12 bytes.
/// `Int` and `Float` point at the const pool because an inline `i64`/`f64`
/// would raise the enum's alignment to 8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    // Values.
    Unit,
    Int(ConstId),
    Float(ConstId),
    Bool(bool),
    Str(StrId),
    ConstRef(ConstId),
    FnRef(FnId),
    BuiltinFnRef(BuiltinFnId),
    BuiltinModuleRef(BuiltinItemId),

    // Locals and places.
    SlotRead {
        slot: Slot,
        mode: AccessMode,
    },
    CaptureRead(CaptureIndex),
    /// Root of a place chain. Charged zero; `Field`/`TupleField`/`Index` are
    /// walked as places when their parent is an assignment or mutable
    /// argument, and read as values otherwise.
    SlotPlace(Slot),
    SlotWrite {
        slot: Slot,
        value: IrId,
    },
    SlotInit {
        slot: Slot,
        value: IrId,
    },
    Field {
        base: IrId,
        index: u32,
    },
    TupleField {
        base: IrId,
        index: u32,
    },
    Index {
        base: IrId,
        index: IrId,
    },

    // Unary and binary operators.
    Neg(IrId),
    Not(IrId),
    Add {
        lhs: IrId,
        rhs: IrId,
    },
    Sub {
        lhs: IrId,
        rhs: IrId,
    },
    Mul {
        lhs: IrId,
        rhs: IrId,
    },
    Div {
        lhs: IrId,
        rhs: IrId,
    },
    Rem {
        lhs: IrId,
        rhs: IrId,
    },
    Eq {
        lhs: IrId,
        rhs: IrId,
    },
    Ne {
        lhs: IrId,
        rhs: IrId,
    },
    Lt {
        lhs: IrId,
        rhs: IrId,
    },
    Le {
        lhs: IrId,
        rhs: IrId,
    },
    Gt {
        lhs: IrId,
        rhs: IrId,
    },
    Ge {
        lhs: IrId,
        rhs: IrId,
    },
    And {
        lhs: IrId,
        rhs: IrId,
    },
    Or {
        lhs: IrId,
        rhs: IrId,
    },
    Coalesce {
        lhs: IrId,
        rhs: IrId,
    },

    // Calls. `MutArg` wraps a place child for a mutable argument.
    /// Arguments are `children[first_arg..]`; the callee signature supplies
    /// the count.
    CallDirect {
        target: FnId,
        first_arg: u32,
    },
    /// `children[first_child]` is the receiver; the remaining children are the
    /// arguments. The target's parameter list includes `self`.
    CallMethod {
        target: FnId,
        first_child: u32,
    },
    /// `children[first_child]` is the callee, followed by `argc` arguments.
    CallValue {
        first_child: u32,
        argc: u32,
    },
    /// Arguments start at `first_arg`; the builtin table supplies the count.
    CallBuiltinFn {
        builtin: BuiltinFnId,
        first_arg: u32,
    },
    /// `children[first_child]` is the receiver, followed by the arguments.
    CallBuiltinMethod {
        method: BuiltinMethodId,
        first_child: u32,
    },
    /// Arguments start at `first_arg`; the builtin item table supplies the
    /// count.
    CallBuiltinModule {
        item: BuiltinItemId,
        first_arg: u32,
    },
    /// Arguments start at `first_arg`; the host signature supplies the count.
    CallHost {
        host: HostId,
        first_arg: u32,
    },

    // Aggregates.
    /// Children start at the layout's `first_child`, in source order.
    /// [`crate::StructLayout`] maps each child to its declaration slot. Every
    /// field must be present because there is no base.
    Struct {
        type_id: TypeId,
        layout: StructLayoutId,
    },
    /// Children start at the layout's `first_child` with the base first,
    /// followed by the values in source order. [`crate::StructLayout`] maps
    /// each value to its declaration slot.
    StructUpdate {
        type_id: TypeId,
        layout: StructLayoutId,
    },
    /// Constructor form, `Enum.Variant(...)`. Payloads start at
    /// `first_payload`; the enum type is this node's `ty`. The post-charge is
    /// the payload count, so a zero-payload constructor still takes the
    /// interpreter's zero-unit `slot_cost(0)` charge and advances the tick
    /// counter.
    EnumVariant {
        variant: u32,
        first_payload: u32,
    },
    /// Value form, `Enum.Variant` for a zero-payload variant. The interpreter
    /// allocates the value without taking a charge, so this is a distinct node
    /// from the constructor form.
    EnumValue {
        variant: u32,
    },
    /// Payloads start at `first_payload`; the variant supplies the arity.
    PreludeVariant {
        variant: PreludeVariant,
        first_payload: u32,
    },
    Tuple(ChildRange),
    List(ChildRange),
    /// Children are key/value pairs in source order.
    Map(ChildRange),
    Range {
        start: IrId,
        end: IrId,
    },
    /// A range whose end is included.
    InclusiveRange {
        start: IrId,
        end: IrId,
    },

    // Control flow.
    /// Children are statements in order. A final `ExprStmt` supplies the
    /// block's value. Body blocks charge no entry; `ExprBlock` charges one.
    Block(ChildRange),
    ExprBlock(ChildRange),
    /// Children are `[condition, then_block, else_branch?]`.
    If(ChildRange),
    While {
        cond: IrId,
        body: IrId,
    },
    /// Children are `[iterable, body]`.
    For {
        pattern: PatternId,
        first_child: u32,
    },
    Break,
    Continue,
    Return(Option<IrId>),
    /// Arm data is in [`crate::IrFn::matches`].
    Match {
        match_set: MatchSetId,
        scrutinee: IrId,
    },
    Try(IrId),

    // Statements.
    /// Pattern data is in [`crate::IrFn::patterns`].
    Let {
        pattern: PatternId,
        value: IrId,
    },
    Assign {
        place: IrId,
        value: IrId,
    },
    AddAssign {
        place: IrId,
        value: IrId,
    },
    SubAssign {
        place: IrId,
        value: IrId,
    },
    MulAssign {
        place: IrId,
        value: IrId,
    },
    DivAssign {
        place: IrId,
        value: IrId,
    },
    RemAssign {
        place: IrId,
        value: IrId,
    },
    /// Statement wrapper for an expression. It is a safepoint except when it
    /// is the trailing expression statement of a block: the interpreter
    /// evaluates that tail without a memory check.
    ExprStmt(IrId),

    // Conversions.
    ConvertInt(IrId),
    ConvertFloat(IrId),
    ConvertStr(IrId),

    // Closures.
    /// Captures are the target function's capture list.
    MakeClosure(FnId),

    // Other.
    /// The range indexes [`crate::IrFn::str_parts`], not `children`.
    StrInterp(ChildRange),
    Assert(ChildRange),
    Panic(ChildRange),
    /// Only valid as a call argument. The child is a place node; the call
    /// protocol reads it with `Share` and writes it back on success.
    MutArg(IrId),
    /// Inserted only by a `Preserve` pass when carrying a removed node's
    /// charge.
    Charge(u32),
}

impl NodeKind {
    pub const fn binary_op(self) -> Option<BinaryOp> {
        match self {
            Self::Add { .. } => Some(BinaryOp::Add),
            Self::Sub { .. } => Some(BinaryOp::Sub),
            Self::Mul { .. } => Some(BinaryOp::Mul),
            Self::Div { .. } => Some(BinaryOp::Div),
            Self::Rem { .. } => Some(BinaryOp::Rem),
            Self::Eq { .. } => Some(BinaryOp::Eq),
            Self::Ne { .. } => Some(BinaryOp::Ne),
            Self::Lt { .. } => Some(BinaryOp::Lt),
            Self::Le { .. } => Some(BinaryOp::Le),
            Self::Gt { .. } => Some(BinaryOp::Gt),
            Self::Ge { .. } => Some(BinaryOp::Ge),
            Self::And { .. } => Some(BinaryOp::And),
            Self::Or { .. } => Some(BinaryOp::Or),
            Self::Coalesce { .. } => Some(BinaryOp::Coalesce),
            _ => None,
        }
    }

    pub const fn unary_op(self) -> Option<UnaryOp> {
        match self {
            Self::Neg(_) => Some(UnaryOp::Neg),
            Self::Not(_) => Some(UnaryOp::Not),
            _ => None,
        }
    }

    pub const fn compound_op(self) -> Option<CompoundOp> {
        match self {
            Self::AddAssign { .. } => Some(CompoundOp::Add),
            Self::SubAssign { .. } => Some(CompoundOp::Sub),
            Self::MulAssign { .. } => Some(CompoundOp::Mul),
            Self::DivAssign { .. } => Some(CompoundOp::Div),
            Self::RemAssign { .. } => Some(CompoundOp::Rem),
            _ => None,
        }
    }

    pub const fn is_short_circuit(self) -> bool {
        matches!(
            self,
            Self::And { .. } | Self::Or { .. } | Self::Coalesce { .. }
        )
    }

    /// Calls whose effects are not locally known.
    pub const fn is_call(self) -> bool {
        matches!(
            self,
            Self::CallDirect { .. }
                | Self::CallMethod { .. }
                | Self::CallValue { .. }
                | Self::CallBuiltinFn { .. }
                | Self::CallBuiltinMethod { .. }
                | Self::CallBuiltinModule { .. }
                | Self::CallHost { .. }
        )
    }
}

/// One node in a function's arena.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Node {
    pub kind: NodeKind,
    pub ty: TyId,
    pub charge: Charge,
    pub effects: Effects,
}

impl Node {
    pub const fn new(kind: NodeKind, ty: TyId, charge: Charge, effects: Effects) -> Self {
        Self {
            kind,
            ty,
            charge,
            effects,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BinaryOp, CompoundOp, NodeKind, UnaryOp};
    use crate::id::IrId;

    #[test]
    fn operator_helpers_map_the_variant_discriminant() {
        let add = NodeKind::Add {
            lhs: IrId(0),
            rhs: IrId(1),
        };
        assert_eq!(add.binary_op(), Some(BinaryOp::Add));
        assert!(!add.is_short_circuit());
        assert_eq!(
            NodeKind::And {
                lhs: IrId(0),
                rhs: IrId(1)
            }
            .binary_op(),
            Some(BinaryOp::And)
        );
        assert!(
            NodeKind::Coalesce {
                lhs: IrId(0),
                rhs: IrId(1)
            }
            .is_short_circuit()
        );

        assert_eq!(NodeKind::Not(IrId(0)).unary_op(), Some(UnaryOp::Not));
        assert_eq!(
            NodeKind::AddAssign {
                place: IrId(0),
                value: IrId(1)
            }
            .compound_op(),
            Some(CompoundOp::Add)
        );
        assert_eq!(NodeKind::Unit.binary_op(), None);
    }

    #[test]
    fn call_predicate_covers_all_call_forms() {
        assert!(
            NodeKind::CallValue {
                first_child: 0,
                argc: 0
            }
            .is_call()
        );
        assert!(
            NodeKind::CallHost {
                host: crate::id::HostId(0),
                first_arg: 0
            }
            .is_call()
        );
        assert!(!NodeKind::FnRef(crate::id::FnId(0)).is_call());
    }
}
