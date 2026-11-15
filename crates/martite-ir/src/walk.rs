//! Shared IR tree traversal.
//!
//! [`children_of`] is the single source of truth for the semantic children of a
//! node. The verifier, effects, the slot analyses and the optimizer all use it;
//! none of them owns child enumeration. Children stored inline in
//! [`NodeKind`] and children stored in per-node payload arenas are both
//! returned, in evaluation order.

use crate::id::IrId;
use crate::node::{Node, NodeKind, StrPart};
use crate::program::{IrFn, IrProgram, PreludeVariant};
use crate::types::{Ty, TypeDefKind};

/// Returns the semantic children of `node` in evaluation order.
///
/// Child ids stored inline in [`NodeKind`] and child ids stored in per-node
/// payload arenas are both returned. Call nodes return their arguments (and
/// receiver, when present); `Match` returns its scrutinee plus every arm guard
/// and body; `StrInterp` returns only its expression parts.
pub fn children_of(program: &IrProgram, function: &IrFn, node: Node) -> Vec<IrId> {
    let mut children = Vec::new();
    match node.kind {
        NodeKind::Field { base, .. }
        | NodeKind::TupleField { base, .. }
        | NodeKind::Neg(base)
        | NodeKind::Not(base)
        | NodeKind::ConvertInt(base)
        | NodeKind::ConvertFloat(base)
        | NodeKind::ConvertStr(base)
        | NodeKind::SlotWrite { value: base, .. }
        | NodeKind::SlotInit { value: base, .. }
        | NodeKind::Return(Some(base))
        | NodeKind::Try(base)
        | NodeKind::ExprStmt(base)
        | NodeKind::MutArg(base) => children.push(base),
        NodeKind::Index { base, index } => {
            children.push(base);
            children.push(index);
        }
        NodeKind::Add { lhs, rhs }
        | NodeKind::Sub { lhs, rhs }
        | NodeKind::Mul { lhs, rhs }
        | NodeKind::Div { lhs, rhs }
        | NodeKind::Rem { lhs, rhs }
        | NodeKind::Eq { lhs, rhs }
        | NodeKind::Ne { lhs, rhs }
        | NodeKind::Lt { lhs, rhs }
        | NodeKind::Le { lhs, rhs }
        | NodeKind::Gt { lhs, rhs }
        | NodeKind::Ge { lhs, rhs }
        | NodeKind::And { lhs, rhs }
        | NodeKind::Or { lhs, rhs }
        | NodeKind::Coalesce { lhs, rhs }
        | NodeKind::Assign {
            place: lhs,
            value: rhs,
        }
        | NodeKind::AddAssign {
            place: lhs,
            value: rhs,
        }
        | NodeKind::SubAssign {
            place: lhs,
            value: rhs,
        }
        | NodeKind::MulAssign {
            place: lhs,
            value: rhs,
        }
        | NodeKind::DivAssign {
            place: lhs,
            value: rhs,
        }
        | NodeKind::RemAssign {
            place: lhs,
            value: rhs,
        } => {
            children.push(lhs);
            children.push(rhs);
        }
        NodeKind::Let { value, .. } => children.push(value),
        NodeKind::While { cond, body } => {
            children.push(cond);
            children.push(body);
        }
        NodeKind::CallDirect { target, first_arg } => {
            if let Some(target) = program.function(target) {
                for index in 0..target.params.len() {
                    if let Some(child) = function.children.get(first_arg as usize + index) {
                        children.push(*child);
                    }
                }
            }
        }
        NodeKind::CallMethod {
            target,
            first_child,
        } => {
            if let Some(target) = program.function(target) {
                for index in 0..target.params.len() {
                    if let Some(child) = function.children.get(first_child as usize + index) {
                        children.push(*child);
                    }
                }
            }
        }
        NodeKind::CallValue { first_child, argc } => {
            for index in 0..=argc {
                if let Some(child) = function.children.get(first_child as usize + index as usize) {
                    children.push(*child);
                }
            }
        }
        NodeKind::CallBuiltinFn { builtin, first_arg } => {
            if let Some(arity) = builtin.arity() {
                for index in 0..arity {
                    if let Some(child) = function.children.get(first_arg as usize + index as usize)
                    {
                        children.push(*child);
                    }
                }
            }
        }
        NodeKind::CallBuiltinMethod {
            method,
            first_child,
        } => {
            if let Some(arity) = method.arity() {
                for index in 0..=arity {
                    if let Some(child) =
                        function.children.get(first_child as usize + index as usize)
                    {
                        children.push(*child);
                    }
                }
            }
        }
        NodeKind::CallBuiltinModule { item, first_arg } => {
            if let Some(arity) = item.arity() {
                for index in 0..arity {
                    if let Some(child) = function.children.get(first_arg as usize + index as usize)
                    {
                        children.push(*child);
                    }
                }
            }
        }
        NodeKind::CallHost { host, first_arg } => {
            if let Some(host) = program.host(host) {
                for index in 0..host.params.len() {
                    if let Some(child) = function.children.get(first_arg as usize + index) {
                        children.push(*child);
                    }
                }
            }
        }
        NodeKind::Struct { layout, .. } => {
            let layout = function.struct_layout(layout);
            let first_child = layout.map(|layout| layout.first_child).unwrap_or(0);
            let fields = layout.map(|layout| layout.fields.len()).unwrap_or(0);
            for index in 0..fields {
                if let Some(child) = function.children.get(first_child as usize + index) {
                    children.push(*child);
                }
            }
        }
        NodeKind::StructUpdate { type_id, layout } => {
            let layout = function.struct_layout(layout);
            let first_child = layout.map(|layout| layout.first_child).unwrap_or(0);
            let fields = layout.map(|layout| layout.fields.len()).unwrap_or(0);
            let _ = type_id;
            for index in 0..=fields {
                if let Some(child) = function.children.get(first_child as usize + index) {
                    children.push(*child);
                }
            }
        }
        NodeKind::EnumVariant {
            variant,
            first_payload,
        } => {
            let count = program
                .types
                .get_ty(node.ty)
                .and_then(|ty| match ty {
                    Ty::Named(type_id, _) => program.types.get(*type_id),
                    _ => None,
                })
                .and_then(|def| match &def.kind {
                    TypeDefKind::Enum { variants } => variants
                        .get(variant as usize)
                        .map(|variant| variant.payload.len()),
                    _ => None,
                })
                .unwrap_or(0);
            for index in 0..count {
                if let Some(child) = function.children.get(first_payload as usize + index) {
                    children.push(*child);
                }
            }
        }
        NodeKind::EnumValue { .. } => {}
        NodeKind::PreludeVariant {
            variant,
            first_payload,
        } => {
            let count = match variant {
                PreludeVariant::None => 0,
                PreludeVariant::Some | PreludeVariant::Ok | PreludeVariant::Err => 1,
            };
            for index in 0..count {
                if let Some(child) = function.children.get(first_payload as usize + index) {
                    children.push(*child);
                }
            }
        }
        NodeKind::Tuple(range)
        | NodeKind::List(range)
        | NodeKind::Map(range)
        | NodeKind::Block(range)
        | NodeKind::ExprBlock(range)
        | NodeKind::If(range)
        | NodeKind::Assert(range)
        | NodeKind::Panic(range) => {
            children.extend(
                function
                    .children
                    .get(range.as_range())
                    .unwrap_or(&[])
                    .iter()
                    .copied(),
            );
        }
        NodeKind::For { first_child, .. } => {
            for index in 0..2 {
                if let Some(child) = function.children.get(first_child as usize + index) {
                    children.push(*child);
                }
            }
        }
        NodeKind::Match {
            match_set,
            scrutinee,
        } => {
            children.push(scrutinee);
            if let Some(set) = function.match_set(match_set) {
                for arm in &set.arms {
                    if let Some(guard) = arm.guard {
                        children.push(guard);
                    }
                    children.push(arm.body);
                }
            }
        }
        NodeKind::StrInterp(range) => {
            if let Some(parts) = function.str_parts.get(range.as_range()) {
                for part in parts {
                    if let StrPart::Expr(expr) = part {
                        children.push(*expr);
                    }
                }
            }
        }
        NodeKind::MakeClosure(_)
        | NodeKind::FnRef(_)
        | NodeKind::BuiltinFnRef(_)
        | NodeKind::BuiltinModuleRef(_)
        | NodeKind::Unit
        | NodeKind::Int(_)
        | NodeKind::Float(_)
        | NodeKind::Bool(_)
        | NodeKind::Str(_)
        | NodeKind::ConstRef(_)
        | NodeKind::SlotRead { .. }
        | NodeKind::CaptureRead(_)
        | NodeKind::SlotPlace(_)
        | NodeKind::Break
        | NodeKind::Continue
        | NodeKind::Return(None)
        | NodeKind::Charge(_) => {}
        NodeKind::Range { start, end } | NodeKind::InclusiveRange { start, end } => {
            children.push(start);
            children.push(end);
        }
    }
    children
}
