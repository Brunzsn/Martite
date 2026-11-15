//! Builtin signatures and effect facts, independent of checker or IR types.

/// The receiver kind of a builtin method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinReceiver {
    List,
    Map,
    Str,
    Int,
    Float,
    Range,
    Option,
    Result,
}

impl BuiltinReceiver {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Map => "map",
            Self::Str => "str",
            Self::Int => "int",
            Self::Float => "float",
            Self::Range => "range",
            Self::Option => "Option",
            Self::Result => "Result",
        }
    }
}

/// The namespace class of a builtin-module item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinItemKind {
    Const,
    Fn,
    Enum,
}

/// A builtin signature in a checker- and IR-neutral form.
///
/// `Param` refers to the item's own generic parameter (currently only the
/// single `T` used by `std.math.min`, `max` and `clamp`). `Json` is resolved to
/// the program's opaque JSON type by each consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinTy {
    Unit,
    Bool,
    Int,
    Float,
    Str,
    Never,
    Json,
    Param(u8),
    List(&'static BuiltinTy),
    Option(&'static BuiltinTy),
    Result(&'static BuiltinTy, &'static BuiltinTy),
    Fn(&'static BuiltinFnSig),
}

impl BuiltinTy {
    pub const fn fn_sig(self) -> Option<&'static BuiltinFnSig> {
        match self {
            Self::Fn(sig) => Some(sig),
            _ => None,
        }
    }

    /// The number of arguments when this type describes a callable signature.
    pub const fn arity(self) -> Option<u32> {
        match self {
            Self::Fn(sig) => Some(sig.params.len() as u32),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuiltinFnSig {
    pub params: &'static [BuiltinTy],
    pub ret: BuiltinTy,
}

/// Extra typing rules attached to a builtin call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinGenerics {
    None,
    /// All arguments share one type and that type must implement `Ord`.
    SameOrd,
}

/// Effect facts for one builtin.
///
/// These are intentionally a separate vocabulary from `martite-ir::Effects`:
/// the IR maps each flag onto its own bitset once, instead of repeating an
/// effect table per builtin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuiltinEffects(u16);

impl BuiltinEffects {
    pub const NONE: Self = Self(0);

    pub const MAY_TRAP: Self = Self(1 << 0);
    pub const READS_HEAP: Self = Self(1 << 1);
    pub const WRITES_HEAP: Self = Self(1 << 2);
    pub const SHARES: Self = Self(1 << 3);
    pub const ALLOCATES: Self = Self(1 << 4);
    pub const DYN_COST: Self = Self(1 << 5);
    pub const LOGS: Self = Self(1 << 6);
    pub const PANICS: Self = Self(1 << 7);
    pub const CALLS: Self = Self(1 << 8);

    pub const ALL: Self = Self((1 << 9) - 1);

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}
