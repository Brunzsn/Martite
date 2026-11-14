//! Fill stored effect summaries after the node tree is built.
//!
//! The verifier owns the leaf-effect rules. Lowering and the optimizer share
//! [`crate::effects::recompute`] so they cannot drift.

use crate::program::IrProgram;

use super::LowerError;

pub(crate) fn fill(program: &mut IrProgram) -> Result<(), LowerError> {
    crate::effects::recompute(program);
    Ok(())
}
