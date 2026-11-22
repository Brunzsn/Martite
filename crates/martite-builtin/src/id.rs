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

impl BuiltinMethodId {
    // The canonical method order. Each constant is the dispatch key the IR
    // carries, so the runtime never matches method names.
    pub const LIST_LEN: Self = Self(0);
    pub const LIST_GET: Self = Self(1);
    pub const LIST_FIRST: Self = Self(2);
    pub const LIST_LAST: Self = Self(3);
    pub const LIST_CONTAINS: Self = Self(4);
    pub const LIST_MAP: Self = Self(5);
    pub const LIST_FILTER: Self = Self(6);
    pub const LIST_FOLD: Self = Self(7);
    pub const LIST_ANY: Self = Self(8);
    pub const LIST_ALL: Self = Self(9);
    pub const LIST_ENUMERATE: Self = Self(10);
    pub const LIST_REVERSED: Self = Self(11);
    pub const LIST_SORTED: Self = Self(12);
    pub const LIST_SORTED_BY: Self = Self(13);
    pub const LIST_JOIN: Self = Self(14);
    pub const LIST_PUSH: Self = Self(15);
    pub const LIST_POP: Self = Self(16);
    pub const LIST_SORT: Self = Self(17);
    pub const LIST_INSERT: Self = Self(18);
    pub const LIST_REMOVE_AT: Self = Self(19);
    pub const LIST_EXTEND: Self = Self(20);
    pub const LIST_CLEAR: Self = Self(21);

    pub const MAP_LEN: Self = Self(22);
    pub const MAP_GET: Self = Self(23);
    pub const MAP_HAS: Self = Self(24);
    pub const MAP_KEYS: Self = Self(25);
    pub const MAP_VALUES: Self = Self(26);
    pub const MAP_ENTRIES: Self = Self(27);
    pub const MAP_REMOVE: Self = Self(28);
    pub const MAP_CLEAR: Self = Self(29);

    pub const STR_LEN: Self = Self(30);
    pub const STR_CHARS: Self = Self(31);
    pub const STR_SPLIT: Self = Self(32);
    pub const STR_TRIM: Self = Self(33);
    pub const STR_CONTAINS: Self = Self(34);
    pub const STR_FIND: Self = Self(35);
    pub const STR_STARTS_WITH: Self = Self(36);
    pub const STR_ENDS_WITH: Self = Self(37);
    pub const STR_REPLACE: Self = Self(38);
    pub const STR_TO_UPPER: Self = Self(39);
    pub const STR_TO_LOWER: Self = Self(40);
    pub const STR_PARSE_INT: Self = Self(41);
    pub const STR_PARSE_FLOAT: Self = Self(42);

    pub const INT_ABS: Self = Self(43);
    pub const INT_POW: Self = Self(44);

    pub const FLOAT_ABS: Self = Self(45);
    pub const FLOAT_FLOOR: Self = Self(46);
    pub const FLOAT_CEIL: Self = Self(47);
    pub const FLOAT_TRUNC: Self = Self(48);
    pub const FLOAT_SQRT: Self = Self(49);
    pub const FLOAT_POW: Self = Self(50);
    pub const FLOAT_ROUND: Self = Self(51);
    pub const FLOAT_IS_NAN: Self = Self(52);
    pub const FLOAT_IS_FINITE: Self = Self(53);

    pub const RANGE_LEN: Self = Self(54);
    pub const RANGE_CONTAINS: Self = Self(55);
    pub const RANGE_TO_LIST: Self = Self(56);

    pub const OPTION_IS_SOME: Self = Self(57);
    pub const OPTION_IS_NONE: Self = Self(58);
    pub const OPTION_MAP: Self = Self(59);
    pub const OPTION_AND_THEN: Self = Self(60);
    pub const OPTION_UNWRAP_OR: Self = Self(61);
    pub const OPTION_OK_OR: Self = Self(62);
    pub const OPTION_EXPECT: Self = Self(63);

    pub const RESULT_IS_OK: Self = Self(64);
    pub const RESULT_IS_ERR: Self = Self(65);
    pub const RESULT_MAP: Self = Self(66);
    pub const RESULT_MAP_ERR: Self = Self(67);
    pub const RESULT_AND_THEN: Self = Self(68);
    pub const RESULT_UNWRAP_OR: Self = Self(69);
    pub const RESULT_OK: Self = Self(70);
    pub const RESULT_EXPECT: Self = Self(71);
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
