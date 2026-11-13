use martite_syntax::{AssignOp, Block, Stmt, StmtKind};

use crate::charge::Charge;
use crate::id::IrId;
use crate::node::NodeKind;
use crate::types::TyId;

use super::LowerError;
use super::context::{FnLowerer, PatternPosition};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlockKind {
    FunctionBody,
    Body,
    Expr,
}

impl FnLowerer<'_, '_> {
    pub(crate) fn lower_block(
        &mut self,
        block: &Block,
        kind: BlockKind,
        expr_ty: Option<TyId>,
    ) -> Result<(IrId, TyId), LowerError> {
        stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || {
            self.lower_block_inner(block, kind, expr_ty)
        })
    }

    fn lower_block_inner(
        &mut self,
        block: &Block,
        kind: BlockKind,
        expr_ty: Option<TyId>,
    ) -> Result<(IrId, TyId), LowerError> {
        self.enter(block.span)?;
        let mut children = Vec::new();
        let mut tail_ty = None;
        for (index, stmt) in block.stmts.iter().enumerate() {
            let is_last = index + 1 == block.stmts.len();
            if is_last
                && let StmtKind::Expr {
                    expr, assign: None, ..
                } = &stmt.kind
            {
                let value = self.lower_expr(expr)?;
                let ty = self.node_ty(value);
                let unit = self.unit();
                children.push(self.push(
                    NodeKind::ExprStmt(value),
                    unit,
                    Charge::ZERO,
                    stmt.span,
                    true,
                ));
                tail_ty = Some(ty);
                continue;
            }
            children.push(self.lower_stmt(stmt)?);
        }
        let unit = self.unit();
        let ty = expr_ty.unwrap_or_else(|| tail_ty.unwrap_or(unit));
        let range = self.append_children(children);
        let (node_kind, charge, safepoint) = match kind {
            BlockKind::FunctionBody => (NodeKind::Block(range), Charge::ZERO, true),
            BlockKind::Body => (NodeKind::Block(range), Charge::ZERO, false),
            BlockKind::Expr => (NodeKind::ExprBlock(range), Charge::expression(), false),
        };
        let id = self.push(node_kind, ty, charge, block.span, safepoint);
        self.leave();
        Ok((id, ty))
    }

    pub(crate) fn lower_stmt(&mut self, stmt: &Stmt) -> Result<IrId, LowerError> {
        stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || {
            self.enter(stmt.span)?;
            let id = self.lower_stmt_inner(stmt)?;
            self.leave();
            Ok(id)
        })
    }

    fn lower_stmt_inner(&mut self, stmt: &Stmt) -> Result<IrId, LowerError> {
        let unit = self.unit();
        match &stmt.kind {
            StmtKind::Let { pattern, value, .. } => {
                let value = self.lower_expr(value)?;
                let pattern = self.lower_pattern(pattern, PatternPosition::Binding)?;
                Ok(self.push(
                    NodeKind::Let { pattern, value },
                    unit,
                    Charge::ZERO,
                    stmt.span,
                    true,
                ))
            }
            StmtKind::While { cond, body } => {
                let cond = self.lower_expr(cond)?;
                let (body, _) = self.lower_block(body, BlockKind::Body, None)?;
                Ok(self.push(
                    NodeKind::While { cond, body },
                    unit,
                    Charge::new(0, 0, 1),
                    stmt.span,
                    false,
                ))
            }
            StmtKind::For {
                pattern,
                iter,
                body,
            } => {
                let iterable = self.lower_expr(iter)?;
                let pattern = self.lower_pattern(pattern, PatternPosition::Binding)?;
                let (body, _) = self.lower_block(body, BlockKind::Body, None)?;
                let first_child = self.append_children(vec![iterable, body]).start;
                Ok(self.push(
                    NodeKind::For {
                        pattern,
                        first_child,
                    },
                    unit,
                    Charge::new(0, 0, 1),
                    stmt.span,
                    false,
                ))
            }
            StmtKind::Return(value) => {
                let value = value
                    .as_ref()
                    .map(|expr| self.lower_expr(expr))
                    .transpose()?;
                Ok(self.push(NodeKind::Return(value), unit, Charge::ZERO, stmt.span, true))
            }
            StmtKind::Break => Ok(self.push(NodeKind::Break, unit, Charge::ZERO, stmt.span, false)),
            StmtKind::Continue => {
                Ok(self.push(NodeKind::Continue, unit, Charge::ZERO, stmt.span, false))
            }
            StmtKind::Expr { expr, assign } => match assign {
                None => {
                    let value = self.lower_expr(expr)?;
                    Ok(self.push(
                        NodeKind::ExprStmt(value),
                        unit,
                        Charge::ZERO,
                        stmt.span,
                        true,
                    ))
                }
                Some((op, rhs)) => {
                    let place = self.lower_place(expr)?;
                    let value = self.lower_expr(rhs)?;
                    let kind = match op {
                        AssignOp::Assign => NodeKind::Assign { place, value },
                        AssignOp::Add => NodeKind::AddAssign { place, value },
                        AssignOp::Sub => NodeKind::SubAssign { place, value },
                        AssignOp::Mul => NodeKind::MulAssign { place, value },
                        AssignOp::Div => NodeKind::DivAssign { place, value },
                        AssignOp::Rem => NodeKind::RemAssign { place, value },
                    };
                    Ok(self.push(kind, unit, Charge::ZERO, stmt.span, true))
                }
            },
        }
    }
}
