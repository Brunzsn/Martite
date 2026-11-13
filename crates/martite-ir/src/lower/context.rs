use std::collections::HashMap;

use martite_base::Span;
use martite_check::{BindingId, LambdaInfo, Resolution, Ty as CheckTy};
use martite_syntax::{Block, Expr, LambdaBody, PatKind};

use crate::charge::Charge;
use crate::effects::Effects;
use crate::id::{CaptureIndex, ConstId, MatchSetId, Slot, StrId};
use crate::node::{ChildRange, Node, NodeKind, StrPart};
use crate::pattern::{MatchSet, Pattern, StructLayout};
use crate::program::IrFn;
use crate::types::TyId;
use crate::{ConstValue, IrId, PatternId, StructLayoutId};

use super::{BodySource, LowerError, Lowerer, SlotPlan};

/// Where a pattern is being lowered.
///
/// Match arms keep every or-pattern alternative. Binding positions (`let`,
/// `for` and lambda parameters) require an irrefutable pattern, so lowering
/// keeps only the first alternative as the design doc section 6.3 specifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PatternPosition {
    Match,
    Binding,
}

pub(crate) struct FnLowerer<'a, 'p> {
    pub(crate) lowerer: &'a mut Lowerer<'p>,
    pub(crate) function: IrFn,
    pub(crate) plan: SlotPlan,
    /// Captured bindings available in this lambda's frame.
    pub(crate) captures: HashMap<BindingId, CaptureIndex>,
    depth: u32,
}

impl<'a, 'p> FnLowerer<'a, 'p> {
    pub(crate) fn new(
        lowerer: &'a mut Lowerer<'p>,
        function: IrFn,
        plan: SlotPlan,
        captures: HashMap<BindingId, CaptureIndex>,
    ) -> Self {
        Self {
            lowerer,
            function,
            plan,
            captures,
            depth: 0,
        }
    }

    pub(crate) fn lower_body(mut self, body: BodySource<'_>) -> Result<IrFn, LowerError> {
        let body = match body {
            BodySource::Block(block) => self.lower_function_block(block)?,
            BodySource::Lambda(LambdaBody::Block(block)) => self.lower_function_block(&block)?,
            BodySource::Lambda(LambdaBody::Expr(expr)) => self.lower_lambda_expr_body(&expr)?,
        };
        self.function.body = Some(body);
        Ok(self.function)
    }

    /// Lower the destructured parameter patterns of a lambda.
    ///
    /// Plain bindings already own the parameter slots; only patterns need a
    /// `Pattern` payload.
    pub(crate) fn lower_lambda_params(&mut self, info: &LambdaInfo) -> Result<(), LowerError> {
        for (index, param) in info.params.iter().enumerate() {
            if matches!(param.pattern.kind, PatKind::Bind(_)) {
                continue;
            }
            let span = param.span;
            let pattern = self.lower_pattern(&param.pattern, PatternPosition::Binding)?;
            if index >= self.function.params.len() {
                return Err(self.internal(span, "lambda parameter is out of range"));
            }
            self.function.params[index].pattern = Some(pattern);
        }
        Ok(())
    }

    fn lower_lambda_expr_body(&mut self, expr: &Expr) -> Result<IrId, LowerError> {
        let value = self.lower_expr(expr)?;
        let ty = self.node_ty(value);
        let unit = self.unit();
        // Expression-bodied lambdas evaluate the expression directly in the
        // interpreter, with no memory check after it.
        let stmt = self.push(
            NodeKind::ExprStmt(value),
            unit,
            Charge::ZERO,
            expr.span,
            false,
        );
        let range = self.append_children(vec![stmt]);
        Ok(self.push(NodeKind::Block(range), ty, Charge::ZERO, expr.span, true))
    }

    pub(crate) fn unit(&self) -> TyId {
        self.lowerer.unit
    }

    pub(crate) fn type_id(&mut self, ty: &CheckTy) -> Result<TyId, LowerError> {
        self.lowerer.type_id(ty)
    }

    pub(crate) fn expr_type(&mut self, expr: &Expr) -> Result<TyId, LowerError> {
        let ty = self
            .lowerer
            .program
            .type_of(expr.id)
            .cloned()
            .ok_or_else(|| {
                self.error(
                    expr.span,
                    super::LowerErrorKind::Internal {
                        message: format!("expression #{} has no checked type", expr.id.0),
                    },
                )
            })?;
        self.type_id(&ty)
    }

    pub(crate) fn resolution(&self, expr: &Expr) -> Result<Resolution, LowerError> {
        self.lowerer
            .program
            .resolution(expr.id)
            .copied()
            .ok_or_else(|| self.internal(expr.span, "expression has no resolution"))
    }

    /// The checker's resolution for `expr`, if it recorded one.
    ///
    /// Calls use this to mirror the interpreter's dispatch: only a `Var` or
    /// `Field` callee has a resolution, and anything else is a value call.
    pub(crate) fn resolution_opt(&self, expr: &Expr) -> Option<Resolution> {
        self.lowerer.program.resolution(expr.id).copied()
    }

    pub(crate) fn node_ty(&self, id: IrId) -> TyId {
        self.function
            .node(id)
            .map(|node| node.ty)
            .unwrap_or_else(|| self.unit())
    }

    pub(crate) fn push(
        &mut self,
        kind: NodeKind,
        ty: TyId,
        charge: Charge,
        span: Span,
        safepoint: bool,
    ) -> IrId {
        let id = IrId::new(self.function.nodes.len() as u32);
        self.function
            .nodes
            .push(Node::new(kind, ty, charge, Effects::NONE));
        self.function.spans.push(span);
        self.function.synthetic.push(None);
        self.function.safepoints.push(safepoint);
        id
    }

    pub(crate) fn append_children(&mut self, children: Vec<IrId>) -> ChildRange {
        let start = self.function.children.len() as u32;
        let len = children.len() as u32;
        self.function.children.extend(children);
        ChildRange::new(start, len)
    }

    pub(crate) fn push_layout(&mut self, layout: StructLayout) -> StructLayoutId {
        let id = StructLayoutId::new(self.function.struct_layouts.len() as u32);
        self.function.struct_layouts.push(layout);
        id
    }

    pub(crate) fn push_pattern(&mut self, pattern: Pattern) -> PatternId {
        let id = PatternId::new(self.function.patterns.len() as u32);
        self.function.patterns.push(pattern);
        id
    }

    pub(crate) fn push_match_set(&mut self, set: MatchSet) -> MatchSetId {
        let id = MatchSetId::new(self.function.matches.len() as u32);
        self.function.matches.push(set);
        id
    }

    pub(crate) fn push_str_parts(&mut self, parts: Vec<StrPart>) -> ChildRange {
        let start = self.function.str_parts.len() as u32;
        let len = parts.len() as u32;
        self.function.str_parts.extend(parts);
        ChildRange::new(start, len)
    }

    pub(crate) fn intern_str(&mut self, text: &str) -> StrId {
        self.lowerer.intern_str(text)
    }

    pub(crate) fn literal_str(&mut self, text: &str) -> StrId {
        self.lowerer.literal_str(text)
    }

    pub(crate) fn push_const(&mut self, value: ConstValue) -> ConstId {
        self.lowerer.push_const(value)
    }

    pub(crate) fn fn_id(&self, def: martite_check::DefId) -> Option<crate::FnId> {
        self.lowerer.fn_ids.get(&def).copied()
    }

    pub(crate) fn const_id(&self, def: martite_check::DefId) -> Option<ConstId> {
        self.lowerer.const_ids.get(&def).copied()
    }

    pub(crate) fn enter(&mut self, span: Span) -> Result<(), LowerError> {
        self.depth += 1;
        if self.depth > crate::MAX_NODE_DEPTH {
            return Err(self.error(
                span,
                super::LowerErrorKind::DepthLimit {
                    limit: crate::MAX_NODE_DEPTH,
                },
            ));
        }
        Ok(())
    }

    pub(crate) fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    pub(crate) fn error(&self, span: Span, kind: super::LowerErrorKind) -> LowerError {
        LowerError {
            fn_id: Some(self.function.id),
            source: Some(self.function.source),
            span: Some(span),
            kind,
        }
    }

    pub(crate) fn internal(&self, span: Span, message: impl Into<String>) -> LowerError {
        self.error(
            span,
            super::LowerErrorKind::Internal {
                message: message.into(),
            },
        )
    }

    pub(crate) fn unsupported(&self, span: Span, construct: &'static str) -> LowerError {
        self.error(span, super::LowerErrorKind::Unsupported { construct })
    }

    pub(crate) fn lower_function_block(&mut self, block: &Block) -> Result<IrId, LowerError> {
        let (id, _) = self.lower_block(block, super::stmt::BlockKind::FunctionBody, None)?;
        Ok(id)
    }

    /// The checker type of a slot, as an IR type id.
    pub(crate) fn slot_ty(&self, slot: Slot) -> TyId {
        self.function
            .slot_info
            .get(slot.index())
            .map(|info| info.ty)
            .unwrap_or_else(|| self.unit())
    }
}
