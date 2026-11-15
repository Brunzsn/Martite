//! IR-facing view of the canonical builtin vocabulary.
//!
//! The tables live in `martite-builtin` so the resolver, checker, verifier and
//! runtime cannot drift. This module only re-exports that vocabulary and maps
//! its effect facts onto the IR's subtree effect bitset.

pub use martite_builtin::{
    BUILTIN_FNS, BUILTIN_ITEMS, BUILTIN_METHODS, BuiltinEffects, BuiltinFnDef, BuiltinFnSig,
    BuiltinGenerics, BuiltinItemDef, BuiltinItemKind, BuiltinMethodDef, BuiltinReceiver, BuiltinTy,
    builtin_module,
};

use crate::effects::Effects;

impl From<BuiltinEffects> for Effects {
    fn from(effects: BuiltinEffects) -> Self {
        let mut out = Self::NONE;
        if effects.contains(BuiltinEffects::MAY_TRAP) {
            out |= Self::MAY_TRAP;
        }
        if effects.contains(BuiltinEffects::READS_HEAP) {
            out |= Self::READS_HEAP;
        }
        if effects.contains(BuiltinEffects::WRITES_HEAP) {
            out |= Self::WRITES_HEAP;
        }
        if effects.contains(BuiltinEffects::SHARES) {
            out |= Self::SHARES;
        }
        if effects.contains(BuiltinEffects::ALLOCATES) {
            out |= Self::ALLOCATES;
        }
        if effects.contains(BuiltinEffects::DYN_COST) {
            out |= Self::DYN_COST;
        }
        if effects.contains(BuiltinEffects::LOGS) {
            out |= Self::LOGS;
        }
        if effects.contains(BuiltinEffects::PANICS) {
            out |= Self::PANICS;
        }
        if effects.contains(BuiltinEffects::CALLS) {
            out |= Self::CALLS;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_effect_maps_to_an_ir_effect() {
        let expected = Effects::MAY_TRAP
            | Effects::READS_HEAP
            | Effects::WRITES_HEAP
            | Effects::SHARES
            | Effects::ALLOCATES
            | Effects::DYN_COST
            | Effects::LOGS
            | Effects::PANICS
            | Effects::CALLS;
        assert_eq!(Effects::from(BuiltinEffects::ALL), expected);
        assert_eq!(Effects::from(BuiltinEffects::NONE), Effects::NONE);
    }
}
