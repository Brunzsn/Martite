//! Per-node fuel schedule.
//!
//! Design section 8.1: every evaluated expression charges `pre` on entry and
//! `post` after its children; loops charge `per_iter` once per iteration. The
//! schedule is static. Dynamic costs are still computed at run time and are
//! marked with [`crate::Effects::DYN_COST`].

/// Static fuel schedule for one node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Charge {
    /// Charged on entry, before children (usually 1 for an expression).
    pub pre: u32,
    /// Charged after children (for example an aggregate's slot count).
    pub post: u32,
    /// Charged once per loop iteration (0 for non-loops).
    pub per_iter: u32,
}

impl Charge {
    pub const ZERO: Self = Self::new(0, 0, 0);

    pub const fn new(pre: u32, post: u32, per_iter: u32) -> Self {
        Self {
            pre,
            post,
            per_iter,
        }
    }

    /// The entry charge of an evaluated expression.
    pub const fn expression() -> Self {
        Self::new(1, 0, 0)
    }

    /// A loop's per-iteration charge. The entry charge is carried separately.
    pub const fn iteration() -> Self {
        Self::new(0, 0, 1)
    }

    /// A static charge taken after children (for example a list literal's
    /// element count).
    pub const fn post(post: u32) -> Self {
        Self::new(0, post, 0)
    }

    pub const fn is_zero(self) -> bool {
        self.pre == 0 && self.post == 0 && self.per_iter == 0
    }
}

#[cfg(test)]
mod tests {
    use super::Charge;

    #[test]
    fn constructors_match_the_schedule() {
        assert_eq!(Charge::default(), Charge::ZERO);
        assert_eq!(Charge::expression(), Charge::new(1, 0, 0));
        assert_eq!(Charge::iteration(), Charge::new(0, 0, 1));
        assert_eq!(Charge::post(3), Charge::new(0, 3, 0));
    }

    #[test]
    fn zero_is_explicit() {
        assert!(Charge::ZERO.is_zero());
        assert!(!Charge::expression().is_zero());
        assert!(!Charge::iteration().is_zero());
        assert!(!Charge::post(1).is_zero());
    }
}
