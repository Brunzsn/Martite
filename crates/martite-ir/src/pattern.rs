//! Resolved patterns, match arm tables and struct-update layouts.
//!
//! Patterns charge nothing; only guards and bodies evaluate. Lowering and the
//! two-phase binding protocol land in #105. The data model already records the
//! binding slots so the verifier can check writes without walking checker side
//! tables.

use crate::id::{IrId, PatternId, Slot, StrId};
use crate::program::PreludeVariant;
use crate::types::{TyId, TypeId};

/// A resolved pattern.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Pattern {
    /// Matches any value and binds nothing.
    Wild,
    /// Binds the matched value to one slot.
    Bind {
        slot: Slot,
        ty: TyId,
    },
    Int(i64),
    Bool(bool),
    Str(StrId),
    Tuple(Vec<Pattern>),
    Variant {
        type_id: TypeId,
        variant: u32,
        args: Vec<Pattern>,
    },
    Prelude {
        variant: PreludeVariant,
        args: Vec<Pattern>,
    },
    /// Struct fields in declaration order with their resolved offsets.
    Struct {
        type_id: TypeId,
        fields: Vec<(u32, Pattern)>,
    },
    /// Alternatives bind the matched alternative.
    Or(Vec<Pattern>),
}

/// One arm of a `match`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MatchArm {
    pub pattern: PatternId,
    pub guard: Option<IrId>,
    pub body: IrId,
}

/// Ordered arm table for one `Match` node.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct MatchSet {
    pub arms: Vec<MatchArm>,
}

/// Field offsets for a struct literal with a base expression.
///
/// The `StructUpdate` node's children are `[base, values...]`; `fields[i]` is
/// the declaration slot of `values[i]`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct StructLayout {
    pub fields: Vec<u32>,
}
