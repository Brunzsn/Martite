//! Martite IR optimizer.
//!
//! The pass manager, rewriter and first pass land together so their shape is
//! driven by a real rewrite. `const-fold` is the default pass under the
//! `Preserve` cost policy: it recomputes literal scalar operators, carries the
//! removed subtree's exact charge and never touches safepoints or sharing.

mod error;
mod pass;
mod passes;
mod rewrite;

pub use error::{OptimizeError, RewriteError, RewriteErrorKind};
pub use pass::{
    Changed, CostPolicy, FnPass, OptimizeOptions, OptimizeReport, PassCx, PassManager, PassReport,
    ProgramPass, RewriteStats,
};
pub use passes::const_fold::ConstFold;
pub use rewrite::Rewriter;

use martite_ir::IrProgram;

/// Run the default optimizer (`const-fold`) over `program`.
pub fn optimize(
    program: &mut IrProgram,
    options: OptimizeOptions,
) -> Result<OptimizeReport, OptimizeError> {
    let mut manager = PassManager::new(options);
    manager.add_fn_pass(ConstFold);
    manager.run(program)
}
