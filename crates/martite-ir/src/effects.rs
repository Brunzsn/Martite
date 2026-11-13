//! Subtree effect summary stored on every node.
//!
//! Effects are facts about a node's whole subtree, not an optimization. A
//! parent's stored flags must be a superset of its own effects and all child
//! effects; the verifier recomputes them in #101.

use std::fmt;
use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign};

/// A set of effect flags.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Effects(u32);

impl Effects {
    pub const NONE: Self = Self(0);

    /// Can raise a trap other than fuel/memory/depth/deadline.
    pub const MAY_TRAP: Self = Self(1 << 0);
    /// Touches frame slots.
    pub const READS_LOCAL: Self = Self(1 << 1);
    pub const WRITES_LOCAL: Self = Self(1 << 2);
    /// Reads or mutates aggregate data.
    pub const READS_HEAP: Self = Self(1 << 3);
    pub const WRITES_HEAP: Self = Self(1 << 4);
    /// Takes an extra reference to a heap value (heap-typed `SlotRead` with
    /// `AccessMode::Share`). Sharing is semantic; see design section 7, S1.
    pub const SHARES: Self = Self(1 << 5);
    /// Allocates and is accounted by the memory census.
    pub const ALLOCATES: Self = Self(1 << 6);
    /// Charges fuel computed from runtime values.
    pub const DYN_COST: Self = Self(1 << 7);
    /// Memory or deadline may be checked here.
    pub const SAFEPOINT: Self = Self(1 << 8);
    /// Calls a user function or closure whose effects are not locally known.
    pub const CALLS: Self = Self(1 << 9);
    /// Host call (also implies a possible `HostFault`).
    pub const HOST: Self = Self(1 << 10);
    /// Prints or appends to the runtime log.
    pub const LOGS: Self = Self(1 << 11);
    /// Can panic or assert.
    pub const PANICS: Self = Self(1 << 12);
    /// Contains `break`, `continue`, `return` or `Try` that leaves the
    /// subtree.
    pub const NONLOCAL_EXIT: Self = Self(1 << 13);

    pub const ALL: Self = Self((1 << 14) - 1);

    pub const fn bits(self) -> u32 {
        self.0
    }

    pub const fn from_bits(bits: u32) -> Option<Self> {
        if bits & !Self::ALL.0 == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    pub const fn from_bits_truncate(bits: u32) -> Self {
        Self(bits & Self::ALL.0)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn names(self) -> impl Iterator<Item = &'static str> {
        FLAGS
            .iter()
            .filter_map(move |(name, flag)| self.contains(*flag).then_some(*name))
    }
}

/// Every defined flag, in bit order. Used by the debug formatting and later by
/// the IR printer.
pub const FLAGS: &[(&str, Effects)] = &[
    ("MAY_TRAP", Effects::MAY_TRAP),
    ("READS_LOCAL", Effects::READS_LOCAL),
    ("WRITES_LOCAL", Effects::WRITES_LOCAL),
    ("READS_HEAP", Effects::READS_HEAP),
    ("WRITES_HEAP", Effects::WRITES_HEAP),
    ("SHARES", Effects::SHARES),
    ("ALLOCATES", Effects::ALLOCATES),
    ("DYN_COST", Effects::DYN_COST),
    ("SAFEPOINT", Effects::SAFEPOINT),
    ("CALLS", Effects::CALLS),
    ("HOST", Effects::HOST),
    ("LOGS", Effects::LOGS),
    ("PANICS", Effects::PANICS),
    ("NONLOCAL_EXIT", Effects::NONLOCAL_EXIT),
];

impl fmt::Debug for Effects {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Effects(")?;
        let mut first = true;
        for name in self.names() {
            if !first {
                f.write_str("|")?;
            }
            f.write_str(name)?;
            first = false;
        }
        if first {
            f.write_str("NONE")?;
        }
        f.write_str(")")
    }
}

impl BitOr for Effects {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        self.union(rhs)
    }
}

impl BitOrAssign for Effects {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for Effects {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl BitAndAssign for Effects {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

#[cfg(test)]
mod tests {
    use super::{Effects, FLAGS};

    #[test]
    fn flags_are_distinct_and_cover_all() {
        let mut all = Effects::NONE;
        for (index, (_, flag)) in FLAGS.iter().enumerate() {
            assert_eq!(flag.bits().count_ones(), 1);
            assert!(!all.intersects(*flag), "duplicate flag at index {index}");
            all |= *flag;
        }
        assert_eq!(all, Effects::ALL);
        assert_eq!(FLAGS.len(), 14);
    }

    #[test]
    fn union_and_contains() {
        let effects = Effects::MAY_TRAP | Effects::CALLS | Effects::SAFEPOINT;
        assert!(effects.contains(Effects::MAY_TRAP));
        assert!(effects.contains(Effects::CALLS | Effects::SAFEPOINT));
        assert!(effects.contains(Effects::NONE));
        assert!(!effects.contains(Effects::HOST));
        assert!(effects.contains(Effects::MAY_TRAP));
        assert!(!effects.intersects(Effects::HOST));
    }

    #[test]
    fn bits_round_trip() {
        assert_eq!(Effects::from_bits(Effects::ALL.bits()), Some(Effects::ALL));
        assert_eq!(Effects::from_bits(0), Some(Effects::NONE));
        assert_eq!(Effects::from_bits(1 << 31), None);
        assert_eq!(Effects::from_bits_truncate(1 << 31), Effects::NONE);
    }

    #[test]
    fn debug_lists_names() {
        assert_eq!(format!("{:?}", Effects::NONE), "Effects(NONE)");
        assert_eq!(
            format!("{:?}", Effects::MAY_TRAP | Effects::CALLS),
            "Effects(MAY_TRAP|CALLS)"
        );
    }
}
