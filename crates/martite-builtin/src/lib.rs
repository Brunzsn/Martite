//! Canonical builtin vocabulary for Martite.
//!
//! The resolver, checker, IR and runtime all consume these tables. The crate is
//! deliberately dependency-free: it sits below `martite-resolve` and
//! `martite-ir` so neither has to duplicate builtin names, arities, signatures,
//! effects or safepoint facts.

mod id;
mod spec;
mod ty;

pub use id::{BuiltinFnId, BuiltinItemId, BuiltinMethodId};
pub use spec::{
    BUILTIN_FNS, BUILTIN_ITEMS, BUILTIN_METHODS, BuiltinFnDef, BuiltinItemDef, BuiltinMethodDef,
    builtin_module,
};
pub use ty::{
    BuiltinEffects, BuiltinFnSig, BuiltinGenerics, BuiltinItemKind, BuiltinReceiver, BuiltinTy,
};

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn tables_are_indexed_by_their_ids() {
        for (index, function) in BUILTIN_FNS.iter().enumerate() {
            let id = BuiltinFnId::new(index as u32);
            assert_eq!(id.def(), Some(function));
            assert_eq!(BuiltinFnId::lookup(function.name), Some(id));
        }
        for (index, item) in BUILTIN_ITEMS.iter().enumerate() {
            let id = BuiltinItemId::new(index as u32);
            assert_eq!(id.item(), Some(item));
            assert_eq!(BuiltinItemId::lookup(item.module, item.name), Some(id));
        }
        for (index, method) in BUILTIN_METHODS.iter().enumerate() {
            let id = BuiltinMethodId::new(index as u32);
            assert_eq!(id.def(), Some(method));
            assert_eq!(
                BuiltinMethodId::lookup(method.receiver, method.name),
                Some(id)
            );
        }
    }

    #[test]
    fn builtin_tables_are_unique() {
        let mut names = HashSet::new();
        for function in BUILTIN_FNS {
            assert!(
                names.insert(function.name),
                "duplicate function {}",
                function.name
            );
        }

        let mut items = HashSet::new();
        for item in BUILTIN_ITEMS {
            assert!(
                items.insert((item.module, item.name)),
                "duplicate item {}.{}",
                item.module,
                item.name
            );
        }

        let mut methods = HashSet::new();
        for method in BUILTIN_METHODS {
            assert!(
                methods.insert((method.receiver, method.name)),
                "duplicate method {} {}",
                method.receiver.as_str(),
                method.name
            );
        }
    }

    #[test]
    fn builtin_tables_are_complete() {
        for function in BUILTIN_FNS {
            assert!(
                BuiltinFnId::lookup(function.name)
                    .and_then(BuiltinFnId::arity)
                    .is_some(),
                "missing signature for function {}",
                function.name
            );
        }

        for item in BUILTIN_ITEMS {
            let arity =
                BuiltinItemId::lookup(item.module, item.name).and_then(BuiltinItemId::arity);
            assert_eq!(
                arity.is_some(),
                item.kind == BuiltinItemKind::Fn,
                "arity for {}.{}",
                item.module,
                item.name
            );
        }

        let receivers: HashSet<_> = BUILTIN_METHODS
            .iter()
            .map(|method| method.receiver)
            .collect();
        for receiver in [
            BuiltinReceiver::List,
            BuiltinReceiver::Map,
            BuiltinReceiver::Str,
            BuiltinReceiver::Int,
            BuiltinReceiver::Float,
            BuiltinReceiver::Range,
            BuiltinReceiver::Option,
            BuiltinReceiver::Result,
        ] {
            assert!(
                receivers.contains(&receiver),
                "no methods for receiver {}",
                receiver.as_str()
            );
        }

        for method in BUILTIN_METHODS {
            let id = BuiltinMethodId::lookup(method.receiver, method.name)
                .expect("every method must be findable");
            assert_eq!(id.arity(), Some(method.arity));
        }
    }

    #[test]
    fn named_builtin_ids_match_the_canonical_table() {
        assert_eq!(BuiltinItemId::MATH_PI.item().unwrap().name, "PI");
        assert_eq!(BuiltinItemId::MATH_EULER.item().unwrap().name, "EULER");
        assert_eq!(BuiltinItemId::MATH_MIN.item().unwrap().name, "min");
        assert_eq!(BuiltinItemId::MATH_MAX.item().unwrap().name, "max");
        assert_eq!(BuiltinItemId::MATH_CLAMP.item().unwrap().name, "clamp");
        assert_eq!(BuiltinItemId::MATH_SIN.item().unwrap().name, "sin");
        assert_eq!(BuiltinItemId::MATH_COS.item().unwrap().name, "cos");
        assert_eq!(BuiltinItemId::MATH_TAN.item().unwrap().name, "tan");
        assert_eq!(BuiltinItemId::MATH_ATAN2.item().unwrap().name, "atan2");
        assert_eq!(BuiltinItemId::MATH_EXP.item().unwrap().name, "exp");
        assert_eq!(BuiltinItemId::MATH_LN.item().unwrap().name, "ln");
        assert_eq!(BuiltinItemId::MATH_LOG10.item().unwrap().name, "log10");
        assert_eq!(BuiltinItemId::JSON.item().unwrap().name, "Json");
        assert_eq!(BuiltinItemId::JSON_PARSE.item().unwrap().name, "parse");
        assert_eq!(
            BuiltinItemId::JSON_STRINGIFY.item().unwrap().name,
            "stringify"
        );
        assert_eq!(
            BuiltinItemId::JSON_STRINGIFY.item().unwrap().module,
            "std.json"
        );

        assert_eq!(BuiltinFnId::PRINT.def().unwrap().name, "print");
        assert_eq!(BuiltinFnId::PANIC.def().unwrap().name, "panic");
        assert_eq!(BuiltinFnId::ASSERT.def().unwrap().name, "assert");
    }

    #[test]
    fn builtin_arities_cover_the_tables() {
        assert_eq!(BuiltinFnId::PRINT.arity(), Some(1));
        assert_eq!(BuiltinFnId::ASSERT.arity(), Some(2));
        assert_eq!(BuiltinItemId::MATH_MIN.arity(), Some(2));
        assert_eq!(BuiltinItemId::MATH_CLAMP.arity(), Some(3));
        assert_eq!(BuiltinItemId::MATH_ATAN2.arity(), Some(2));
        assert_eq!(BuiltinItemId::MATH_SIN.arity(), Some(1));
        assert_eq!(BuiltinItemId::MATH_PI.arity(), None);
        assert_eq!(BuiltinItemId::JSON.arity(), None);
        assert_eq!(
            BuiltinMethodId::lookup(BuiltinReceiver::List, "fold")
                .expect("fold")
                .arity(),
            Some(2)
        );
        assert_eq!(
            BuiltinMethodId::lookup(BuiltinReceiver::List, "push")
                .expect("push")
                .def()
                .expect("push def")
                .mut_self,
            true
        );
    }

    #[test]
    fn item_signatures_match_their_kind() {
        for item in BUILTIN_ITEMS {
            let is_fn = item.kind == BuiltinItemKind::Fn;
            assert_eq!(
                item.ty.fn_sig().is_some(),
                is_fn,
                "signature kind for {}.{}",
                item.module,
                item.name
            );
        }

        let min = BuiltinItemId::MATH_MIN.item().unwrap();
        assert_eq!(min.generics, BuiltinGenerics::SameOrd);
        let atan2 = BuiltinItemId::MATH_ATAN2.item().unwrap();
        assert_eq!(
            atan2.ty,
            BuiltinTy::Fn(&BuiltinFnSig {
                params: &[BuiltinTy::Float, BuiltinTy::Float],
                ret: BuiltinTy::Float,
            })
        );
        let parse = BuiltinItemId::JSON_PARSE.item().unwrap();
        assert_eq!(
            parse.ty,
            BuiltinTy::Fn(&BuiltinFnSig {
                params: &[BuiltinTy::Str],
                ret: BuiltinTy::Result(&BuiltinTy::Json, &BuiltinTy::Str),
            })
        );
    }

    #[test]
    fn effects_and_safepoints_are_consistent() {
        assert!(BuiltinFnId::PRINT.def().unwrap().safepoint);
        assert!(!BuiltinFnId::PANIC.def().unwrap().safepoint);
        assert!(BuiltinItemId::JSON_PARSE.item().unwrap().safepoint);
        assert!(!BuiltinItemId::JSON_STRINGIFY.item().unwrap().safepoint);

        for method in BUILTIN_METHODS {
            assert_eq!(
                method.safepoint,
                matches!(
                    (method.receiver, method.name),
                    (BuiltinReceiver::List, "sorted" | "sorted_by" | "sort")
                ),
                "safepoint for {} {}",
                method.receiver.as_str(),
                method.name
            );
            assert_eq!(
                method.mut_self,
                matches!(
                    (method.receiver, method.name),
                    (
                        BuiltinReceiver::List,
                        "push" | "pop" | "sort" | "insert" | "remove_at" | "extend" | "clear"
                    ) | (BuiltinReceiver::Map, "remove" | "clear")
                ),
                "mutability for {} {}",
                method.receiver.as_str(),
                method.name
            );
        }

        assert!(
            BuiltinItemId::JSON_PARSE
                .item()
                .unwrap()
                .effects
                .contains(BuiltinEffects::ALLOCATES)
        );
        assert!(
            BuiltinFnId::PRINT
                .def()
                .unwrap()
                .effects
                .contains(BuiltinEffects::LOGS)
        );
    }

    #[test]
    fn missing_names_are_not_found() {
        assert!(BuiltinItemId::lookup("std.math", "missing").is_none());
        assert!(BuiltinFnId::lookup("missing").is_none());
        assert!(BuiltinMethodId::lookup(BuiltinReceiver::List, "missing").is_none());
        assert_eq!(builtin_module("std.math"), Some("std.math"));
        assert_eq!(builtin_module("std.json"), Some("std.json"));
        assert_eq!(builtin_module("std.missing"), None);
    }
}
