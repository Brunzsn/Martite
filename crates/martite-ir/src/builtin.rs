//! IR-owned builtin vocabulary.
//!
//! Runtime values and call targets store [`BuiltinFnId`] or [`BuiltinItemId`]
//! instead of checker enums or resolver ids. The tables are canonical: the
//! resolver's `BuiltinItem` is mapped here by module and name, never by its
//! private numeric id.

use crate::id::{BuiltinFnId, BuiltinItemId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuiltinFnDef {
    pub name: &'static str,
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
