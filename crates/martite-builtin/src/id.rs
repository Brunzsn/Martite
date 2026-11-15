//! Dense ids for the canonical builtin vocabulary.

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
    /// Identifies a prelude builtin function.
    BuiltinFnId
);
define_id!(
    /// Identifies a builtin-module item.
    BuiltinItemId
);
define_id!(
    /// Identifies a builtin method.
    BuiltinMethodId
);

impl BuiltinFnId {
    pub const PRINT: Self = Self(0);
    pub const PANIC: Self = Self(1);
    pub const ASSERT: Self = Self(2);
}

impl BuiltinItemId {
    pub const MATH_PI: Self = Self(0);
    pub const MATH_EULER: Self = Self(1);
    pub const MATH_MIN: Self = Self(2);
    pub const MATH_MAX: Self = Self(3);
    pub const MATH_CLAMP: Self = Self(4);
    pub const MATH_SIN: Self = Self(5);
    pub const MATH_COS: Self = Self(6);
    pub const MATH_TAN: Self = Self(7);
    pub const MATH_ATAN2: Self = Self(8);
    pub const MATH_EXP: Self = Self(9);
    pub const MATH_LN: Self = Self(10);
    pub const MATH_LOG10: Self = Self(11);
    pub const JSON: Self = Self(12);
    pub const JSON_PARSE: Self = Self(13);
    pub const JSON_STRINGIFY: Self = Self(14);
}
