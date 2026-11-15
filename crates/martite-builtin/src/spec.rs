//! The canonical builtin tables.
//!
//! Adding a builtin means adding one row here and its execution arm in the
//! runtime. Checker and verifier signature handling is driven by these rows.

use crate::id::{BuiltinFnId, BuiltinItemId, BuiltinMethodId};
use crate::ty::{
    BuiltinEffects, BuiltinFnSig, BuiltinGenerics, BuiltinItemKind, BuiltinReceiver, BuiltinTy,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuiltinFnDef {
    pub name: &'static str,
    pub sig: BuiltinFnSig,
    pub effects: BuiltinEffects,
    pub safepoint: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuiltinItemDef {
    pub module: &'static str,
    pub name: &'static str,
    pub kind: BuiltinItemKind,
    /// The item's value type. Function items use [`BuiltinTy::Fn`].
    pub ty: BuiltinTy,
    pub generics: BuiltinGenerics,
    pub effects: BuiltinEffects,
    pub safepoint: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuiltinMethodDef {
    pub receiver: BuiltinReceiver,
    pub name: &'static str,
    /// Number of arguments after the receiver.
    pub arity: u32,
    /// The receiver is a mutable place (`push`, `sort`, `remove`, ...).
    pub mut_self: bool,
    /// The method calls a user function or closure argument.
    pub calls_closure: bool,
    pub effects: BuiltinEffects,
    /// The evaluator checks memory or the deadline after this method.
    pub safepoint: bool,
}

const PRINT: BuiltinEffects = BuiltinEffects::LOGS
    .union(BuiltinEffects::DYN_COST)
    .union(BuiltinEffects::ALLOCATES);
const PANIC: BuiltinEffects = BuiltinEffects::PANICS.union(BuiltinEffects::MAY_TRAP);
const MATH: BuiltinEffects = BuiltinEffects::DYN_COST;
const JSON: BuiltinEffects = BuiltinEffects::DYN_COST
    .union(BuiltinEffects::ALLOCATES)
    .union(BuiltinEffects::READS_HEAP);
const METHOD: BuiltinEffects = BuiltinEffects::DYN_COST
    .union(BuiltinEffects::READS_HEAP)
    .union(BuiltinEffects::ALLOCATES)
    .union(BuiltinEffects::SHARES)
    .union(BuiltinEffects::MAY_TRAP)
    .union(BuiltinEffects::PANICS);

const P0: BuiltinTy = BuiltinTy::Param(0);
const FLOAT: BuiltinTy = BuiltinTy::Float;
const STR: BuiltinTy = BuiltinTy::Str;
const UNIT: BuiltinTy = BuiltinTy::Unit;
const NEVER: BuiltinTy = BuiltinTy::Never;
const BOOL: BuiltinTy = BuiltinTy::Bool;

const PRINT_SIG: BuiltinFnSig = BuiltinFnSig {
    params: &[P0],
    ret: UNIT,
};
const PANIC_SIG: BuiltinFnSig = BuiltinFnSig {
    params: &[STR],
    ret: NEVER,
};
const ASSERT_SIG: BuiltinFnSig = BuiltinFnSig {
    params: &[BOOL, STR],
    ret: UNIT,
};

const MATH_MIN_SIG: BuiltinFnSig = BuiltinFnSig {
    params: &[P0, P0],
    ret: P0,
};
const MATH_CLAMP_SIG: BuiltinFnSig = BuiltinFnSig {
    params: &[P0, P0, P0],
    ret: P0,
};
const MATH_UNARY_SIG: BuiltinFnSig = BuiltinFnSig {
    params: &[FLOAT],
    ret: FLOAT,
};
const MATH_ATAN2_SIG: BuiltinFnSig = BuiltinFnSig {
    params: &[FLOAT, FLOAT],
    ret: FLOAT,
};
const JSON_PARSE_TY: BuiltinTy = BuiltinTy::Result(&BuiltinTy::Json, &STR);
const JSON_PARSE_SIG: BuiltinFnSig = BuiltinFnSig {
    params: &[STR],
    ret: JSON_PARSE_TY,
};
const JSON_STRINGIFY_SIG: BuiltinFnSig = BuiltinFnSig {
    params: &[BuiltinTy::Json],
    ret: STR,
};

/// The prelude builtin functions. The order is the [`BuiltinFnId`] order.
pub const BUILTIN_FNS: &[BuiltinFnDef] = &[
    BuiltinFnDef {
        name: "print",
        sig: PRINT_SIG,
        effects: PRINT,
        safepoint: true,
    },
    BuiltinFnDef {
        name: "panic",
        sig: PANIC_SIG,
        effects: PANIC,
        safepoint: false,
    },
    BuiltinFnDef {
        name: "assert",
        sig: ASSERT_SIG,
        effects: PANIC,
        safepoint: false,
    },
];

/// The canonical builtin-module items. The order is the [`BuiltinItemId`] order.
pub const BUILTIN_ITEMS: &[BuiltinItemDef] = &[
    BuiltinItemDef {
        module: "std.math",
        name: "PI",
        kind: BuiltinItemKind::Const,
        ty: FLOAT,
        generics: BuiltinGenerics::None,
        effects: MATH,
        safepoint: false,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "EULER",
        kind: BuiltinItemKind::Const,
        ty: FLOAT,
        generics: BuiltinGenerics::None,
        effects: MATH,
        safepoint: false,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "min",
        kind: BuiltinItemKind::Fn,
        ty: BuiltinTy::Fn(&MATH_MIN_SIG),
        generics: BuiltinGenerics::SameOrd,
        effects: MATH,
        safepoint: false,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "max",
        kind: BuiltinItemKind::Fn,
        ty: BuiltinTy::Fn(&MATH_MIN_SIG),
        generics: BuiltinGenerics::SameOrd,
        effects: MATH,
        safepoint: false,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "clamp",
        kind: BuiltinItemKind::Fn,
        ty: BuiltinTy::Fn(&MATH_CLAMP_SIG),
        generics: BuiltinGenerics::SameOrd,
        effects: MATH,
        safepoint: false,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "sin",
        kind: BuiltinItemKind::Fn,
        ty: BuiltinTy::Fn(&MATH_UNARY_SIG),
        generics: BuiltinGenerics::None,
        effects: MATH,
        safepoint: false,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "cos",
        kind: BuiltinItemKind::Fn,
        ty: BuiltinTy::Fn(&MATH_UNARY_SIG),
        generics: BuiltinGenerics::None,
        effects: MATH,
        safepoint: false,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "tan",
        kind: BuiltinItemKind::Fn,
        ty: BuiltinTy::Fn(&MATH_UNARY_SIG),
        generics: BuiltinGenerics::None,
        effects: MATH,
        safepoint: false,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "atan2",
        kind: BuiltinItemKind::Fn,
        ty: BuiltinTy::Fn(&MATH_ATAN2_SIG),
        generics: BuiltinGenerics::None,
        effects: MATH,
        safepoint: false,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "exp",
        kind: BuiltinItemKind::Fn,
        ty: BuiltinTy::Fn(&MATH_UNARY_SIG),
        generics: BuiltinGenerics::None,
        effects: MATH,
        safepoint: false,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "ln",
        kind: BuiltinItemKind::Fn,
        ty: BuiltinTy::Fn(&MATH_UNARY_SIG),
        generics: BuiltinGenerics::None,
        effects: MATH,
        safepoint: false,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "log10",
        kind: BuiltinItemKind::Fn,
        ty: BuiltinTy::Fn(&MATH_UNARY_SIG),
        generics: BuiltinGenerics::None,
        effects: MATH,
        safepoint: false,
    },
    BuiltinItemDef {
        module: "std.json",
        name: "Json",
        kind: BuiltinItemKind::Enum,
        ty: BuiltinTy::Json,
        generics: BuiltinGenerics::None,
        effects: BuiltinEffects::NONE,
        safepoint: false,
    },
    BuiltinItemDef {
        module: "std.json",
        name: "parse",
        kind: BuiltinItemKind::Fn,
        ty: BuiltinTy::Fn(&JSON_PARSE_SIG),
        generics: BuiltinGenerics::None,
        effects: JSON,
        safepoint: true,
    },
    BuiltinItemDef {
        module: "std.json",
        name: "stringify",
        kind: BuiltinItemKind::Fn,
        ty: BuiltinTy::Fn(&JSON_STRINGIFY_SIG),
        generics: BuiltinGenerics::None,
        effects: JSON,
        safepoint: false,
    },
];

/// The canonical builtin methods. The order is the [`BuiltinMethodId`] order.
pub const BUILTIN_METHODS: &[BuiltinMethodDef] = &[
    // List.
    method(BuiltinReceiver::List, "len", 0, false, false, false),
    method(BuiltinReceiver::List, "get", 1, false, false, false),
    method(BuiltinReceiver::List, "first", 0, false, false, false),
    method(BuiltinReceiver::List, "last", 0, false, false, false),
    method(BuiltinReceiver::List, "contains", 1, false, false, false),
    method(BuiltinReceiver::List, "map", 1, false, true, false),
    method(BuiltinReceiver::List, "filter", 1, false, true, false),
    method(BuiltinReceiver::List, "fold", 2, false, true, false),
    method(BuiltinReceiver::List, "any", 1, false, true, false),
    method(BuiltinReceiver::List, "all", 1, false, true, false),
    method(BuiltinReceiver::List, "enumerate", 0, false, false, false),
    method(BuiltinReceiver::List, "reversed", 0, false, false, false),
    method(BuiltinReceiver::List, "sorted", 0, false, false, true),
    method(BuiltinReceiver::List, "sorted_by", 1, false, true, true),
    method(BuiltinReceiver::List, "join", 1, false, false, false),
    method(BuiltinReceiver::List, "push", 1, true, false, false),
    method(BuiltinReceiver::List, "pop", 0, true, false, false),
    method(BuiltinReceiver::List, "sort", 0, true, false, true),
    method(BuiltinReceiver::List, "insert", 2, true, false, false),
    method(BuiltinReceiver::List, "remove_at", 1, true, false, false),
    method(BuiltinReceiver::List, "extend", 1, true, false, false),
    method(BuiltinReceiver::List, "clear", 0, true, false, false),
    // Map.
    method(BuiltinReceiver::Map, "len", 0, false, false, false),
    method(BuiltinReceiver::Map, "get", 1, false, false, false),
    method(BuiltinReceiver::Map, "has", 1, false, false, false),
    method(BuiltinReceiver::Map, "keys", 0, false, false, false),
    method(BuiltinReceiver::Map, "values", 0, false, false, false),
    method(BuiltinReceiver::Map, "entries", 0, false, false, false),
    method(BuiltinReceiver::Map, "remove", 1, true, false, false),
    method(BuiltinReceiver::Map, "clear", 0, true, false, false),
    // Str.
    method(BuiltinReceiver::Str, "len", 0, false, false, false),
    method(BuiltinReceiver::Str, "chars", 0, false, false, false),
    method(BuiltinReceiver::Str, "split", 1, false, false, false),
    method(BuiltinReceiver::Str, "trim", 0, false, false, false),
    method(BuiltinReceiver::Str, "contains", 1, false, false, false),
    method(BuiltinReceiver::Str, "find", 1, false, false, false),
    method(BuiltinReceiver::Str, "starts_with", 1, false, false, false),
    method(BuiltinReceiver::Str, "ends_with", 1, false, false, false),
    method(BuiltinReceiver::Str, "replace", 2, false, false, false),
    method(BuiltinReceiver::Str, "to_upper", 0, false, false, false),
    method(BuiltinReceiver::Str, "to_lower", 0, false, false, false),
    method(BuiltinReceiver::Str, "parse_int", 0, false, false, false),
    method(BuiltinReceiver::Str, "parse_float", 0, false, false, false),
    // Int.
    method(BuiltinReceiver::Int, "abs", 0, false, false, false),
    method(BuiltinReceiver::Int, "pow", 1, false, false, false),
    // Float.
    method(BuiltinReceiver::Float, "abs", 0, false, false, false),
    method(BuiltinReceiver::Float, "floor", 0, false, false, false),
    method(BuiltinReceiver::Float, "ceil", 0, false, false, false),
    method(BuiltinReceiver::Float, "trunc", 0, false, false, false),
    method(BuiltinReceiver::Float, "sqrt", 0, false, false, false),
    method(BuiltinReceiver::Float, "pow", 1, false, false, false),
    method(BuiltinReceiver::Float, "round", 1, false, false, false),
    method(BuiltinReceiver::Float, "is_nan", 0, false, false, false),
    method(BuiltinReceiver::Float, "is_finite", 0, false, false, false),
    // Range.
    method(BuiltinReceiver::Range, "len", 0, false, false, false),
    method(BuiltinReceiver::Range, "contains", 1, false, false, false),
    method(BuiltinReceiver::Range, "to_list", 0, false, false, false),
    // Option.
    method(BuiltinReceiver::Option, "is_some", 0, false, false, false),
    method(BuiltinReceiver::Option, "is_none", 0, false, false, false),
    method(BuiltinReceiver::Option, "map", 1, false, true, false),
    method(BuiltinReceiver::Option, "and_then", 1, false, true, false),
    method(BuiltinReceiver::Option, "unwrap_or", 1, false, false, false),
    method(BuiltinReceiver::Option, "ok_or", 1, false, false, false),
    method(BuiltinReceiver::Option, "expect", 1, false, false, false),
    // Result.
    method(BuiltinReceiver::Result, "is_ok", 0, false, false, false),
    method(BuiltinReceiver::Result, "is_err", 0, false, false, false),
    method(BuiltinReceiver::Result, "map", 1, false, true, false),
    method(BuiltinReceiver::Result, "map_err", 1, false, true, false),
    method(BuiltinReceiver::Result, "and_then", 1, false, true, false),
    method(BuiltinReceiver::Result, "unwrap_or", 1, false, false, false),
    method(BuiltinReceiver::Result, "ok", 0, false, false, false),
    method(BuiltinReceiver::Result, "expect", 1, false, false, false),
];

const fn method(
    receiver: BuiltinReceiver,
    name: &'static str,
    arity: u32,
    mut_self: bool,
    calls_closure: bool,
    safepoint: bool,
) -> BuiltinMethodDef {
    let mut effects = METHOD;
    if mut_self {
        effects = effects.union(BuiltinEffects::WRITES_HEAP);
    }
    if calls_closure {
        effects = effects.union(BuiltinEffects::CALLS);
    }
    BuiltinMethodDef {
        receiver,
        name,
        arity,
        mut_self,
        calls_closure,
        effects,
        safepoint,
    }
}

impl BuiltinFnId {
    pub fn def(self) -> Option<&'static BuiltinFnDef> {
        BUILTIN_FNS.get(self.index())
    }

    pub fn lookup(name: &str) -> Option<Self> {
        BUILTIN_FNS
            .iter()
            .position(|def| def.name == name)
            .map(|index| Self(index as u32))
    }

    /// Number of arguments the evaluator reads for a direct call node.
    pub fn arity(self) -> Option<u32> {
        self.def().map(|def| def.sig.params.len() as u32)
    }
}

impl BuiltinItemId {
    pub fn item(self) -> Option<&'static BuiltinItemDef> {
        BUILTIN_ITEMS.get(self.index())
    }

    pub fn lookup(module: &str, name: &str) -> Option<Self> {
        BUILTIN_ITEMS
            .iter()
            .position(|item| item.module == module && item.name == name)
            .map(|index| Self(index as u32))
    }

    /// Number of arguments the evaluator reads for a direct call node.
    ///
    /// Only `Fn` items are callable; constants and enums return `None`.
    pub fn arity(self) -> Option<u32> {
        let item = self.item()?;
        if item.kind != BuiltinItemKind::Fn {
            return None;
        }
        item.ty.arity()
    }
}

impl BuiltinMethodId {
    pub fn def(self) -> Option<&'static BuiltinMethodDef> {
        BUILTIN_METHODS.get(self.index())
    }

    pub fn lookup(receiver: BuiltinReceiver, name: &str) -> Option<Self> {
        BUILTIN_METHODS
            .iter()
            .position(|method| method.receiver == receiver && method.name == name)
            .map(|index| Self(index as u32))
    }

    /// Number of arguments after the receiver that the evaluator reads.
    pub fn arity(self) -> Option<u32> {
        self.def().map(|def| def.arity)
    }
}

/// Look up a builtin module name from any item it contains.
pub fn builtin_module(path: &str) -> Option<&'static str> {
    BUILTIN_ITEMS
        .iter()
        .find(|item| item.module == path)
        .map(|item| item.module)
}
