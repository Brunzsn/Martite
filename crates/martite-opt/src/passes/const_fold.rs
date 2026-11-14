//! Constant folding for scalar operators under `Preserve`.
//!
//! The pass only rewrites childless scalar leaves. Comparisons and strings are
//! deliberately excluded: they carry runtime-dependent charges and need a
//! separate cost argument. Every rewrite carries the removed subtree's exact
//! charge, so fuel and trampoline ticks are unchanged.

use std::collections::BTreeMap;

use martite_ir::{ConstValue, Effects, IrFn, IrId, NodeKind, Ty};

use crate::error::RewriteError;
use crate::pass::{Changed, CostPolicy, FnPass, PassCx};
use crate::rewrite::Rewriter;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Scalar {
    Int(i64),
    Float(f64),
    Bool(bool),
}

/// Fold scalar integer, float and boolean operators with literal operands.
pub struct ConstFold;

impl FnPass for ConstFold {
    fn name(&self) -> &'static str {
        "const-fold"
    }

    fn cost_policy(&self) -> CostPolicy {
        CostPolicy::Preserve
    }

    fn run(&mut self, function: &mut IrFn, cx: &mut PassCx<'_>) -> Changed {
        let Some(body) = function.body else {
            return Changed::NO;
        };
        let Some(program) = cx.program() else {
            cx.record_error(RewriteError::new(
                function.id,
                Some(function.source),
                function.span(body),
                Some(body),
                crate::error::RewriteErrorKind::MissingProgram,
            ));
            return Changed::NO;
        };
        let mut rewriter = Rewriter::new(program, cx.policy());
        let mut folded = BTreeMap::new();
        if let Err(error) = walk(function, body, cx, &mut folded, &mut rewriter) {
            cx.record_error(error);
            return Changed::NO;
        }
        match rewriter.finish(function) {
            Ok((changed, stats)) => {
                cx.record(stats);
                changed
            }
            Err(error) => {
                cx.record_error(error);
                Changed::NO
            }
        }
    }
}

fn walk(
    function: &IrFn,
    id: IrId,
    cx: &mut PassCx<'_>,
    folded: &mut BTreeMap<IrId, Scalar>,
    rewriter: &mut Rewriter<'_>,
) -> Result<Option<Scalar>, RewriteError> {
    stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || {
        if let Some(value) = scalar_of(function, id, cx, folded) {
            return Ok(Some(value));
        }
        let Some(node) = function.node(id).copied() else {
            return Ok(None);
        };
        for child in cx.children_of(function, id) {
            walk(function, child, cx, folded, rewriter)?;
        }
        let Some(value) = eval_node(function, id, node.kind, cx, folded) else {
            return Ok(None);
        };
        let kind = match value {
            Scalar::Int(value) => NodeKind::Int(cx.intern_int(value)),
            Scalar::Float(value) => NodeKind::Float(cx.intern_float(value)),
            Scalar::Bool(value) => NodeKind::Bool(value),
        };
        rewriter.replace_value(function, id, kind, node.ty)?;
        folded.insert(id, value);
        Ok(Some(value))
    })
}

fn scalar_of(
    function: &IrFn,
    id: IrId,
    cx: &PassCx<'_>,
    folded: &BTreeMap<IrId, Scalar>,
) -> Option<Scalar> {
    if let Some(value) = folded.get(&id) {
        return Some(*value);
    }
    let node = function.node(id)?;
    match node.kind {
        NodeKind::Int(constant) => match cx.constant(constant)? {
            ConstValue::Int(value) => Some(Scalar::Int(value)),
            _ => None,
        },
        NodeKind::Float(constant) => match cx.constant(constant)? {
            ConstValue::Float(value) => Some(Scalar::Float(value)),
            _ => None,
        },
        NodeKind::Bool(value) => Some(Scalar::Bool(value)),
        // A folded value is represented as `{ charge(n); literal }`. Keep the
        // literal visible to an enclosing operator so nested folding works.
        NodeKind::ExprBlock(range) => {
            let children = function.child_slice(range);
            if children.len() != 2
                || !matches!(function.node(children[0])?.kind, NodeKind::Charge(_))
            {
                return None;
            }
            let inner = match function.node(children[1])?.kind {
                NodeKind::ExprStmt(inner) => inner,
                _ => return None,
            };
            scalar_of(function, inner, cx, folded)
        }
        _ => None,
    }
}

fn eval_node(
    function: &IrFn,
    id: IrId,
    kind: NodeKind,
    cx: &PassCx<'_>,
    folded: &BTreeMap<IrId, Scalar>,
) -> Option<Scalar> {
    let node = function.node(id)?;
    if !foldable_effects(function, node.effects, node.kind, id) {
        return None;
    }
    let lhs = |child: IrId| scalar_of(function, child, cx, folded);
    match kind {
        NodeKind::Neg(inner) => match lhs(inner)? {
            Scalar::Int(value) => value.checked_neg().map(Scalar::Int),
            Scalar::Float(value) => Some(Scalar::Float(-value)),
            Scalar::Bool(_) => None,
        },
        NodeKind::Not(inner) => match lhs(inner)? {
            Scalar::Bool(value) => Some(Scalar::Bool(!value)),
            _ => None,
        },
        NodeKind::Add { lhs: left, rhs } => arithmetic(
            lhs(left)?,
            lhs(rhs)?,
            |a, b| a.checked_add(b),
            |a, b| a + b,
            matches!(cx.ty(node.ty), Some(Ty::Int)),
            matches!(cx.ty(node.ty), Some(Ty::Float)),
        ),
        NodeKind::Sub { lhs: left, rhs } => arithmetic(
            lhs(left)?,
            lhs(rhs)?,
            |a, b| a.checked_sub(b),
            |a, b| a - b,
            matches!(cx.ty(node.ty), Some(Ty::Int)),
            matches!(cx.ty(node.ty), Some(Ty::Float)),
        ),
        NodeKind::Mul { lhs: left, rhs } => arithmetic(
            lhs(left)?,
            lhs(rhs)?,
            |a, b| a.checked_mul(b),
            |a, b| a * b,
            matches!(cx.ty(node.ty), Some(Ty::Int)),
            matches!(cx.ty(node.ty), Some(Ty::Float)),
        ),
        NodeKind::Div { lhs: left, rhs } => arithmetic(
            lhs(left)?,
            lhs(rhs)?,
            |a, b| a.checked_div(b),
            |a, b| a / b,
            matches!(cx.ty(node.ty), Some(Ty::Int)),
            matches!(cx.ty(node.ty), Some(Ty::Float)),
        ),
        NodeKind::Rem { lhs: left, rhs } => arithmetic(
            lhs(left)?,
            lhs(rhs)?,
            |a, b| a.checked_rem(b),
            |a, b| a % b,
            matches!(cx.ty(node.ty), Some(Ty::Int)),
            matches!(cx.ty(node.ty), Some(Ty::Float)),
        ),
        _ => None,
    }
}

fn arithmetic(
    left: Scalar,
    right: Scalar,
    int_op: impl FnOnce(i64, i64) -> Option<i64>,
    float_op: impl FnOnce(f64, f64) -> f64,
    int_result: bool,
    float_result: bool,
) -> Option<Scalar> {
    match (left, right) {
        (Scalar::Int(left), Scalar::Int(right)) if int_result => {
            int_op(left, right).map(Scalar::Int)
        }
        (Scalar::Float(left), Scalar::Float(right)) if float_result => {
            let value = float_op(left, right);
            value.is_finite().then_some(Scalar::Float(value))
        }
        _ => None,
    }
}

fn foldable_effects(function: &IrFn, effects: Effects, kind: NodeKind, id: IrId) -> bool {
    let allowed = Effects::MAY_TRAP | Effects::PANICS | Effects::SAFEPOINT;
    if !allowed.contains(effects) {
        return false;
    }
    if effects.contains(Effects::SAFEPOINT) && !function.is_safepoint(id) {
        return false;
    }
    matches!(
        kind,
        NodeKind::Neg(_)
            | NodeKind::Not(_)
            | NodeKind::Add { .. }
            | NodeKind::Sub { .. }
            | NodeKind::Mul { .. }
            | NodeKind::Div { .. }
            | NodeKind::Rem { .. }
    )
}
