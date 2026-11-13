//! Fill stored effect summaries after the node tree is built.
//!
//! The verifier owns the leaf-effect rules. This pass reuses
//! [`EffectsCheck::leaf_effects`] so lowering and verification cannot drift,
//! then unions each node's children and safepoint flag bottom-up. Call effects
//! are read from a callee-summary snapshot, so the pass reaches a fixpoint once
//! calls are lowered.

use crate::effects::Effects;
use crate::id::IrId;
use crate::program::{IrFn, IrProgram};
use crate::verify::{EffectsCheck, children_of};

use super::LowerError;

pub(crate) fn fill(program: &mut IrProgram) -> Result<(), LowerError> {
    let mut summaries = vec![Effects::NONE; program.fns.len()];
    loop {
        let snapshot = summaries.clone();
        let outputs = {
            let check = EffectsCheck::with_summaries(program, snapshot);
            let mut outputs = Vec::with_capacity(program.fns.len());
            let mut next = summaries.clone();
            for function in &program.fns {
                let mut effects = vec![Effects::NONE; function.nodes.len()];
                if let Some(body) = function.body {
                    let summary = recompute(program, function, body, &check, &mut effects);
                    next[function.id.index()] = summary;
                }
                outputs.push(effects);
            }
            (outputs, next)
        };
        let (outputs, next) = outputs;
        for (function, node_effects) in program.fns.iter_mut().zip(outputs) {
            for (node, effects) in function.nodes.iter_mut().zip(node_effects) {
                node.effects = effects;
            }
        }
        if next == summaries {
            break;
        }
        summaries = next;
    }
    Ok(())
}

fn recompute(
    program: &IrProgram,
    function: &IrFn,
    id: IrId,
    check: &EffectsCheck<'_>,
    out: &mut [Effects],
) -> Effects {
    stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || {
        let Some(node) = function.node(id).copied() else {
            return Effects::NONE;
        };
        let mut effects = check.leaf_effects(function, node);
        for child in children_of(program, function, node) {
            effects |= recompute(program, function, child, check, out);
        }
        if function.is_safepoint(id) {
            effects |= Effects::SAFEPOINT;
        }
        if let Some(slot) = out.get_mut(id.index()) {
            *slot = effects;
        }
        effects
    })
}
