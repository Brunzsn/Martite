//! Binding collection and frame-slot assignment.
//!
//! Rule M2: one slot per declared binding, no reuse. Parameters already own
//! the first slots; body bindings, including destructured lambda parameter
//! patterns and patterns in unreachable code, are collected from the AST and
//! sorted by `BindingId` for a stable frame layout. The `pattern_bindings`
//! checker side table is read once here and the resulting `NodeId -> Slot` map
//! is reused by the pattern lowerer.

use std::collections::{HashMap, HashSet};

use martite_check::{BindingId, CheckedProgram};
use martite_syntax::{Block, Expr, ExprKind, MatchBody, NodeId, PatKind, Pattern, Stmt, StmtKind};

/// The root of a function body for slot planning.
#[derive(Clone, Copy)]
pub(crate) enum BodyRoot<'a> {
    Block(&'a Block),
    Expr(&'a Expr),
}

use crate::id::Slot;
use crate::program::SlotInfo;
use crate::types::TypeTable;

use super::LowerError;

#[derive(Debug, Clone, Default)]
pub(crate) struct SlotPlan {
    pub(crate) pattern_slots: HashMap<NodeId, Slot>,
    pub(crate) binding_slots: HashMap<BindingId, Slot>,
}

impl SlotPlan {
    pub(crate) fn add_parameter(&mut self, binding: BindingId, slot: Slot) {
        self.binding_slots.insert(binding, slot);
    }
}

pub(crate) fn plan_slots(
    program: &CheckedProgram,
    types: &mut TypeTable,
    root: BodyRoot<'_>,
    first_slot: u32,
    parameter_patterns: &[&Pattern],
) -> Result<(SlotPlan, Vec<SlotInfo>), LowerError> {
    let mut walker = Walker {
        program,
        bindings: Vec::new(),
        seen: HashSet::new(),
        pattern_bindings: HashMap::new(),
    };
    for pattern in parameter_patterns {
        walker.walk_pattern(pattern)?;
    }
    match root {
        BodyRoot::Block(block) => walker.walk_block(block)?,
        BodyRoot::Expr(expr) => walker.walk_expr(expr)?,
    }

    let mut bindings = walker.bindings;
    bindings.sort_unstable_by_key(|(_, binding)| binding.0);

    let mut binding_slots = HashMap::new();
    let mut slot_info = Vec::with_capacity(bindings.len());
    for (index, (_, binding)) in bindings.iter().enumerate() {
        let info = program.bindings.get(binding).ok_or_else(|| {
            LowerError::internal(format!("binding #{} has no checker info", binding.0))
        })?;
        if info.ty.is_error() || info.ty.has_var() {
            return Err(LowerError::internal(format!(
                "binding #{} has an unresolved type",
                binding.0
            )));
        }
        let ty = types.ty_from_checker(&info.ty);
        let slot = Slot::new(first_slot + index as u32);
        binding_slots.insert(*binding, slot);
        slot_info.push(SlotInfo {
            ty,
            mutable: info.mutable,
            name: Some(info.name.clone().into_boxed_str()),
        });
    }

    let mut pattern_slots = HashMap::with_capacity(walker.pattern_bindings.len());
    for (node, binding) in walker.pattern_bindings {
        if let Some(slot) = binding_slots.get(&binding) {
            pattern_slots.insert(node, *slot);
        }
    }

    Ok((
        SlotPlan {
            pattern_slots,
            binding_slots,
        },
        slot_info,
    ))
}

struct Walker<'a> {
    program: &'a CheckedProgram,
    bindings: Vec<(NodeId, BindingId)>,
    seen: HashSet<BindingId>,
    pattern_bindings: HashMap<NodeId, BindingId>,
}

impl Walker<'_> {
    fn walk_block(&mut self, block: &Block) -> Result<(), LowerError> {
        stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || {
            for stmt in &block.stmts {
                self.walk_stmt(stmt)?;
            }
            Ok(())
        })
    }

    fn walk_stmt(&mut self, stmt: &Stmt) -> Result<(), LowerError> {
        match &stmt.kind {
            StmtKind::Let { pattern, value, .. } => {
                self.walk_pattern(pattern)?;
                self.walk_expr(value)?;
            }
            StmtKind::While { cond, body } => {
                self.walk_expr(cond)?;
                self.walk_block(body)?;
            }
            StmtKind::For {
                pattern,
                iter,
                body,
            } => {
                self.walk_pattern(pattern)?;
                self.walk_expr(iter)?;
                self.walk_block(body)?;
            }
            StmtKind::Return(value) => {
                if let Some(value) = value {
                    self.walk_expr(value)?;
                }
            }
            StmtKind::Expr { expr, assign } => {
                self.walk_expr(expr)?;
                if let Some((_, rhs)) = assign {
                    self.walk_expr(rhs)?;
                }
            }
            StmtKind::Break | StmtKind::Continue => {}
        }
        Ok(())
    }

    fn walk_expr(&mut self, expr: &Expr) -> Result<(), LowerError> {
        stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || self.walk_expr_inner(expr))
    }

    fn walk_expr_inner(&mut self, expr: &Expr) -> Result<(), LowerError> {
        match &expr.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Unit
            | ExprKind::Var(_) => {}
            ExprKind::Str(parts) => {
                for part in parts {
                    if let martite_syntax::StrExprPart::Expr(expr) = part {
                        self.walk_expr(expr)?;
                    }
                }
            }
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                for item in items {
                    self.walk_expr(item)?;
                }
            }
            ExprKind::Map(entries) => {
                for (key, value) in entries {
                    self.walk_expr(key)?;
                    self.walk_expr(value)?;
                }
            }
            ExprKind::StructLit { fields, base, .. } => {
                if let Some(base) = base {
                    self.walk_expr(base)?;
                }
                for field in fields {
                    if let Some(value) = &field.value {
                        self.walk_expr(value)?;
                    }
                }
            }
            ExprKind::Unary { expr: inner, .. } => self.walk_expr(inner)?,
            ExprKind::Binary { lhs, rhs, .. } => {
                self.walk_expr(lhs)?;
                self.walk_expr(rhs)?;
            }
            ExprKind::Range { start, end, .. } => {
                self.walk_expr(start)?;
                self.walk_expr(end)?;
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.walk_expr(cond)?;
                self.walk_block(then_branch)?;
                if let Some(else_branch) = else_branch {
                    self.walk_expr(else_branch)?;
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee)?;
                for arm in arms {
                    self.walk_pattern(&arm.pattern)?;
                    if let Some(guard) = &arm.guard {
                        self.walk_expr(guard)?;
                    }
                    match &arm.body {
                        MatchBody::Block(block) => self.walk_block(block)?,
                        MatchBody::Expr(expr) => self.walk_expr(expr)?,
                    }
                }
            }
            ExprKind::Lambda { .. } => {}
            ExprKind::Block(block) => self.walk_block(block)?,
            ExprKind::Field { base, .. } => self.walk_expr(base)?,
            ExprKind::Call { callee, args } => {
                self.walk_expr(callee)?;
                for arg in args {
                    self.walk_expr(&arg.expr)?;
                }
            }
            ExprKind::Index { base, index } => {
                self.walk_expr(base)?;
                self.walk_expr(index)?;
            }
            ExprKind::Try(inner) => self.walk_expr(inner)?,
            ExprKind::Convert { expr: inner, .. } => self.walk_expr(inner)?,
        }
        Ok(())
    }

    fn walk_pattern(&mut self, pattern: &Pattern) -> Result<(), LowerError> {
        stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || {
            self.walk_pattern_inner(pattern)
        })
    }

    fn walk_pattern_inner(&mut self, pattern: &Pattern) -> Result<(), LowerError> {
        match &pattern.kind {
            PatKind::Wild | PatKind::Int(_) | PatKind::Bool(_) | PatKind::Str(_) => {}
            PatKind::Bind(_) => {
                let binding = self
                    .program
                    .pattern_bindings
                    .get(&pattern.id)
                    .copied()
                    .ok_or_else(|| {
                        LowerError::internal(format!("pattern #{} has no binding id", pattern.id.0))
                    })?;
                self.pattern_bindings.insert(pattern.id, binding);
                if self.seen.insert(binding) {
                    self.bindings.push((pattern.id, binding));
                }
            }
            PatKind::Tuple(items) | PatKind::Or(items) => {
                for item in items {
                    self.walk_pattern(item)?;
                }
            }
            PatKind::Variant { args, .. } => {
                for item in args {
                    self.walk_pattern(item)?;
                }
            }
            PatKind::Struct { fields, .. } => {
                for field in fields {
                    self.walk_pattern(&field.pattern)?;
                }
            }
        }
        Ok(())
    }
}
