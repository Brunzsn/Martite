//! Dense arena indices used by the IR.
//!
//! Every id is a `u32` newtype. [`IrId`] is local to one function: it indexes
//! [`crate::IrFn::nodes`] and the parallel [`crate::IrFn::spans`],
//! [`crate::IrFn::synthetic`] vectors. Cross-function references always name a
//! [`FnId`], never a bare [`IrId`].

macro_rules! define_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u32);

        impl $name {
            pub const fn new(raw: u32) -> Self {
                Self(raw)
            }

            pub const fn index(self) -> usize {
                self.0 as usize
            }
        }
    };
}

define_id!(
    /// Identifies one function, method, handler, test, hook, host stub or
    /// lambda in [`crate::IrProgram::fns`].
    FnId
);
define_id!(
    /// Identifies a node inside one function's node arena.
    IrId
);
define_id!(
    /// Identifies a value in the constant pool.
    ConstId
);
define_id!(
    /// Identifies an interned static string.
    StrId
);
define_id!(
    /// Identifies a host function in [`crate::IrProgram::hosts`].
    HostId
);
define_id!(
    /// Identifies a frame slot in one function. Slots are never reused across
    /// bindings (design section 7, rule M2).
    Slot
);
define_id!(
    /// Identifies one capture in a lambda's capture list.
    CaptureIndex
);
define_id!(
    /// Identifies a resolved pattern in [`crate::IrFn::patterns`].
    PatternId
);
define_id!(
    /// Identifies an arm table in [`crate::IrFn::matches`].
    MatchSetId
);
define_id!(
    /// Identifies a field layout in [`crate::IrFn::struct_layouts`].
    StructLayoutId
);
define_id!(
    /// Identifies a builtin function value or call target.
    BuiltinFnId
);
define_id!(
    /// Identifies a builtin-module item value or call target.
    BuiltinItemId
);
define_id!(
    /// Identifies a builtin method in [`crate::builtin::BUILTIN_METHODS`].
    BuiltinMethodId
);
