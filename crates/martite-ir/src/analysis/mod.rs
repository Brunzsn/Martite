//! On-demand slot analyses for one IR function.
//!
//! Nothing here is stored on [`crate::Node`] or [`crate::IrFn`]. A pass or the
//! verifier asks for an analysis, gets a value holding the per-node facts, and
//! drops it when done. The pass manager in `martite-opt` will cache these.
//!
//! * [`DefiniteAssignment`] is the forward must-analysis behind design section
//!   12: every slot read must be dominated by a write. Joins intersect, loop
//!   headers are seeded from the pre-state, unreachable code is vacuously
//!   assigned, and [`crate::AccessMode::Take`] unassigns its slot.
//! * [`Liveness`] is the backward may-analysis passes need for rule M2. It is
//!   use-liveness, not the memory census's "live frame slots": the census still
//!   counts every slot in a live frame.
//! * [`check_slot_initialization`] enforces one initialization site per slot,
//!   the static half of "slots are never reused across bindings".

mod definite;
mod liveness;
mod slots;

pub use definite::{DefiniteAssignment, UnboundRead};
pub use liveness::Liveness;
pub use slots::{SlotBindingError, SlotSet, check_slot_initialization};

pub(crate) use slots::{pattern_binding_set, pattern_binds_irrefutably};
