//! IR-owned builtin vocabulary.
//!
//! Runtime values and call targets store [`BuiltinFnId`] or [`BuiltinItemId`]
//! instead of checker enums or resolver ids. The tables are canonical: the
//! resolver's `BuiltinItem` is mapped here by module and name, never by its
//! private numeric id.

use crate::id::{BuiltinFnId, BuiltinItemId, BuiltinMethodId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuiltinFnDef {
    pub name: &'static str,
}

/// The receiver kind of a builtin method. The checker has the same set; the IR
/// owns this copy so lowering and the verifier never read checker enums.
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

/// One builtin method. `BuiltinMethodId` indexes [`BUILTIN_METHODS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuiltinMethodDef {
    pub receiver: BuiltinReceiver,
    pub name: &'static str,
    /// The receiver is a mutable place (`push`, `sort`, `remove`, ...).
    pub mut_self: bool,
}

pub const BUILTIN_FNS: [BuiltinFnDef; 3] = [
    BuiltinFnDef { name: "print" },
    BuiltinFnDef { name: "panic" },
    BuiltinFnDef { name: "assert" },
];

impl BuiltinFnId {
    pub const PRINT: Self = Self(0);
    pub const PANIC: Self = Self(1);
    pub const ASSERT: Self = Self(2);

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
        match self {
            Self::PRINT | Self::PANIC => Some(1),
            Self::ASSERT => Some(2),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BuiltinItemKind {
    Const,
    Fn,
    Enum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuiltinItemDef {
    pub module: &'static str,
    pub name: &'static str,
    pub kind: BuiltinItemKind,
}

pub const BUILTIN_ITEMS: [BuiltinItemDef; 15] = [
    BuiltinItemDef {
        module: "std.math",
        name: "PI",
        kind: BuiltinItemKind::Const,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "EULER",
        kind: BuiltinItemKind::Const,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "min",
        kind: BuiltinItemKind::Fn,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "max",
        kind: BuiltinItemKind::Fn,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "clamp",
        kind: BuiltinItemKind::Fn,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "sin",
        kind: BuiltinItemKind::Fn,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "cos",
        kind: BuiltinItemKind::Fn,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "tan",
        kind: BuiltinItemKind::Fn,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "atan2",
        kind: BuiltinItemKind::Fn,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "exp",
        kind: BuiltinItemKind::Fn,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "ln",
        kind: BuiltinItemKind::Fn,
    },
    BuiltinItemDef {
        module: "std.math",
        name: "log10",
        kind: BuiltinItemKind::Fn,
    },
    BuiltinItemDef {
        module: "std.json",
        name: "Json",
        kind: BuiltinItemKind::Enum,
    },
    BuiltinItemDef {
        module: "std.json",
        name: "parse",
        kind: BuiltinItemKind::Fn,
    },
    BuiltinItemDef {
        module: "std.json",
        name: "stringify",
        kind: BuiltinItemKind::Fn,
    },
];

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
        match (item.module, item.name) {
            ("std.math", "min" | "max") => Some(2),
            ("std.math", "clamp") => Some(3),
            ("std.math", "atan2") => Some(2),
            ("std.math", "sin" | "cos" | "tan" | "exp" | "ln" | "log10") => Some(1),
            ("std.json", "parse" | "stringify") => Some(1),
            _ => None,
        }
    }
}

/// Canonical builtin-method table. The order is the `BuiltinMethodId` order.
pub const BUILTIN_METHODS: &[BuiltinMethodDef] = &[
    // List.
    method(BuiltinReceiver::List, "len", false),
    method(BuiltinReceiver::List, "get", false),
    method(BuiltinReceiver::List, "first", false),
    method(BuiltinReceiver::List, "last", false),
    method(BuiltinReceiver::List, "contains", false),
    method(BuiltinReceiver::List, "map", false),
    method(BuiltinReceiver::List, "filter", false),
    method(BuiltinReceiver::List, "fold", false),
    method(BuiltinReceiver::List, "any", false),
    method(BuiltinReceiver::List, "all", false),
    method(BuiltinReceiver::List, "enumerate", false),
    method(BuiltinReceiver::List, "reversed", false),
    method(BuiltinReceiver::List, "sorted", false),
    method(BuiltinReceiver::List, "sorted_by", false),
    method(BuiltinReceiver::List, "join", false),
    method(BuiltinReceiver::List, "push", true),
    method(BuiltinReceiver::List, "pop", true),
    method(BuiltinReceiver::List, "sort", true),
    method(BuiltinReceiver::List, "insert", true),
    method(BuiltinReceiver::List, "remove_at", true),
    method(BuiltinReceiver::List, "extend", true),
    method(BuiltinReceiver::List, "clear", true),
    // Map.
    method(BuiltinReceiver::Map, "len", false),
    method(BuiltinReceiver::Map, "get", false),
    method(BuiltinReceiver::Map, "has", false),
    method(BuiltinReceiver::Map, "keys", false),
    method(BuiltinReceiver::Map, "values", false),
    method(BuiltinReceiver::Map, "entries", false),
    method(BuiltinReceiver::Map, "remove", true),
    method(BuiltinReceiver::Map, "clear", true),
    // Str.
    method(BuiltinReceiver::Str, "len", false),
    method(BuiltinReceiver::Str, "chars", false),
    method(BuiltinReceiver::Str, "split", false),
    method(BuiltinReceiver::Str, "trim", false),
    method(BuiltinReceiver::Str, "contains", false),
    method(BuiltinReceiver::Str, "find", false),
    method(BuiltinReceiver::Str, "starts_with", false),
    method(BuiltinReceiver::Str, "ends_with", false),
    method(BuiltinReceiver::Str, "replace", false),
    method(BuiltinReceiver::Str, "to_upper", false),
    method(BuiltinReceiver::Str, "to_lower", false),
    method(BuiltinReceiver::Str, "parse_int", false),
    method(BuiltinReceiver::Str, "parse_float", false),
    // Int.
    method(BuiltinReceiver::Int, "abs", false),
    method(BuiltinReceiver::Int, "pow", false),
    // Float.
    method(BuiltinReceiver::Float, "abs", false),
    method(BuiltinReceiver::Float, "floor", false),
    method(BuiltinReceiver::Float, "ceil", false),
    method(BuiltinReceiver::Float, "trunc", false),
    method(BuiltinReceiver::Float, "sqrt", false),
    method(BuiltinReceiver::Float, "pow", false),
    method(BuiltinReceiver::Float, "round", false),
    method(BuiltinReceiver::Float, "is_nan", false),
    method(BuiltinReceiver::Float, "is_finite", false),
    // Range.
    method(BuiltinReceiver::Range, "len", false),
    method(BuiltinReceiver::Range, "contains", false),
    method(BuiltinReceiver::Range, "to_list", false),
    // Option.
    method(BuiltinReceiver::Option, "is_some", false),
    method(BuiltinReceiver::Option, "is_none", false),
    method(BuiltinReceiver::Option, "map", false),
    method(BuiltinReceiver::Option, "and_then", false),
    method(BuiltinReceiver::Option, "unwrap_or", false),
    method(BuiltinReceiver::Option, "ok_or", false),
    method(BuiltinReceiver::Option, "expect", false),
    // Result.
    method(BuiltinReceiver::Result, "is_ok", false),
    method(BuiltinReceiver::Result, "is_err", false),
    method(BuiltinReceiver::Result, "map", false),
    method(BuiltinReceiver::Result, "map_err", false),
    method(BuiltinReceiver::Result, "and_then", false),
    method(BuiltinReceiver::Result, "unwrap_or", false),
    method(BuiltinReceiver::Result, "ok", false),
    method(BuiltinReceiver::Result, "expect", false),
];

const fn method(receiver: BuiltinReceiver, name: &'static str, mut_self: bool) -> BuiltinMethodDef {
    BuiltinMethodDef {
        receiver,
        name,
        mut_self,
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
        let def = self.def()?;
        builtin_method_arity(def.receiver, def.name)
    }
}

/// Argument count (excluding the receiver) for one builtin method.
///
/// The evaluator needs this because call nodes do not store argument counts;
/// the verifier uses the same table so execution and verification agree.
pub fn builtin_method_arity(receiver: BuiltinReceiver, name: &str) -> Option<u32> {
    use BuiltinReceiver::*;
    Some(match (receiver, name) {
        (
            List,
            "len" | "first" | "last" | "pop" | "enumerate" | "reversed" | "sorted" | "sort"
            | "clear",
        ) => 0,
        (
            List,
            "get" | "contains" | "map" | "filter" | "any" | "all" | "sorted_by" | "join" | "push"
            | "remove_at" | "extend",
        ) => 1,
        (List, "fold" | "insert") => 2,
        (Map, "len" | "keys" | "values" | "entries" | "clear") => 0,
        (Map, "get" | "has" | "remove") => 1,
        (Str, "len" | "chars" | "trim" | "to_upper" | "to_lower" | "parse_int" | "parse_float") => {
            0
        }
        (Str, "split" | "contains" | "find" | "starts_with" | "ends_with") => 1,
        (Str, "replace") => 2,
        (Int, "abs") => 0,
        (Int, "pow") => 1,
        (Float, "abs" | "floor" | "ceil" | "trunc" | "sqrt" | "is_nan" | "is_finite") => 0,
        (Float, "pow" | "round") => 1,
        (Range, "len" | "to_list") => 0,
        (Range, "contains") => 1,
        (Option, "is_some" | "is_none") => 0,
        (Option, "map" | "and_then" | "unwrap_or" | "ok_or" | "expect") => 1,
        (Result, "is_ok" | "is_err" | "ok") => 0,
        (Result, "map" | "map_err" | "and_then" | "unwrap_or" | "expect") => 1,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_tables_are_indexed_by_their_ids() {
        for (index, item) in BUILTIN_ITEMS.iter().enumerate() {
            let id = BuiltinItemId::new(index as u32);
            assert_eq!(id.item(), Some(item));
            assert_eq!(BuiltinItemId::lookup(item.module, item.name), Some(id));
        }
        for (index, function) in BUILTIN_FNS.iter().enumerate() {
            let id = BuiltinFnId::new(index as u32);
            assert_eq!(id.def(), Some(function));
            assert_eq!(BuiltinFnId::lookup(function.name), Some(id));
        }
    }

    #[test]
    fn builtin_methods_are_indexed_and_unique() {
        for (index, method) in BUILTIN_METHODS.iter().enumerate() {
            let id = BuiltinMethodId::new(index as u32);
            assert_eq!(id.def(), Some(method));
            assert_eq!(
                BuiltinMethodId::lookup(method.receiver, method.name),
                Some(id)
            );
        }
        for (index, a) in BUILTIN_METHODS.iter().enumerate() {
            for b in BUILTIN_METHODS.iter().skip(index + 1) {
                assert!(
                    a.receiver != b.receiver || a.name != b.name,
                    "duplicate builtin method {} {}",
                    a.receiver.as_str(),
                    a.name
                );
            }
        }
        assert_eq!(BUILTIN_METHODS.len(), 72);
        assert!(
            BuiltinMethodId::lookup(BuiltinReceiver::List, "push")
                .expect("push")
                .def()
                .expect("push def")
                .mut_self
        );
        assert!(
            !BuiltinMethodId::lookup(BuiltinReceiver::List, "len")
                .expect("len")
                .def()
                .expect("len def")
                .mut_self
        );
    }

    #[test]
    fn builtin_arities_cover_the_tables() {
        for (index, function) in BUILTIN_FNS.iter().enumerate() {
            assert!(
                BuiltinFnId::new(index as u32).arity().is_some(),
                "missing arity for builtin function {}",
                function.name
            );
        }
        for (index, item) in BUILTIN_ITEMS.iter().enumerate() {
            let arity = BuiltinItemId::new(index as u32).arity();
            assert_eq!(
                arity.is_some(),
                item.kind == BuiltinItemKind::Fn,
                "arity for {}.{}",
                item.module,
                item.name
            );
        }
        for (index, method) in BUILTIN_METHODS.iter().enumerate() {
            assert_eq!(
                BuiltinMethodId::new(index as u32).arity(),
                builtin_method_arity(method.receiver, method.name),
                "arity for {} {}",
                method.receiver.as_str(),
                method.name
            );
            assert!(
                BuiltinMethodId::new(index as u32).arity().is_some(),
                "missing arity for builtin method {} {}",
                method.receiver.as_str(),
                method.name
            );
        }
        assert_eq!(BuiltinFnId::PRINT.arity(), Some(1));
        assert_eq!(BuiltinFnId::ASSERT.arity(), Some(2));
        assert_eq!(BuiltinItemId::MATH_MIN.arity(), Some(2));
        assert_eq!(BuiltinItemId::MATH_CLAMP.arity(), Some(3));
        assert_eq!(BuiltinItemId::MATH_PI.arity(), None);
        assert_eq!(
            BuiltinMethodId::lookup(BuiltinReceiver::List, "fold")
                .expect("fold")
                .arity(),
            Some(2)
        );
    }

    #[test]
    fn named_builtin_ids_match_the_canonical_table() {
        assert_eq!(BuiltinItemId::MATH_PI.item().unwrap().name, "PI");
        assert_eq!(BuiltinItemId::JSON.item().unwrap().name, "Json");
        assert_eq!(
            BuiltinItemId::JSON_STRINGIFY.item().unwrap().module,
            "std.json"
        );
        assert_eq!(BuiltinFnId::PRINT.def().unwrap().name, "print");
        assert!(BuiltinItemId::lookup("std.math", "missing").is_none());
        assert!(BuiltinFnId::lookup("missing").is_none());
    }
}
