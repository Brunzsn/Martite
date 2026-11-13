//! IR-owned type identifiers.
//!
//! Two indices are kept separate:
//!
//! - [`TyId`] identifies an erased structural type used by a node, slot or
//!   host signature.
//! - [`TypeId`] identifies a nominal type definition used by runtime values,
//!   method dispatch, constants and plugin state.
//!
//! The table contents are #99's scope. This issue only reserves the ids and
//! the [`TypeTable`] field on [`crate::IrProgram`] so its shape is stable.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TyId(pub u32);

impl TyId {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeId(pub u32);

impl TypeId {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// IR-owned type table.
///
/// #99 fills this with interned structural types (`TyId`) and nominal
/// definitions (`TypeId`). Keeping the empty table here means `IrProgram` does
/// not need to change shape when the contents land.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct TypeTable;
