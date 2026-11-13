use martite_base::Span;
use martite_check::{BuiltinFn, BuiltinItem, DefId, DefKind, Resolution};
use martite_syntax::{
    Arg, BinaryOp as SyntaxBinaryOp, Expr, ExprKind, FieldInit, PatKind, Pattern as SyntaxPattern,
    PrimTy, StrExprPart, UnaryOp as SyntaxUnaryOp,
};

use crate::charge::Charge;
use crate::id::{BuiltinFnId, BuiltinItemId, IrId};
use crate::node::{AccessMode, NodeKind, StrPart};
use crate::pattern::Pattern;
use crate::program::{ConstValue, FnKind, PreludeVariant};
use crate::types::{TyId, TypeDefKind, TypeId};
use crate::{PatternId, StructLayout};

use super::LowerError;
use super::context::FnLowerer;
use super::stmt::BlockKind;

impl FnLowerer<'_, '_> {
    pub(crate) fn lower_expr(&mut self, expr: &Expr) -> Result<IrId, LowerError> {
        stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || {
            self.enter(expr.span)?;
            let ty = self.expr_type(expr)?;
            let id = self.lower_expr_inner(expr, ty)?;
            self.leave();
            Ok(id)
        })
    }

    fn lower_expr_inner(&mut self, expr: &Expr, ty: TyId) -> Result<IrId, LowerError> {
        match &expr.kind {
            ExprKind::Int(value) => {
                let constant = self.push_const(ConstValue::Int(*value));
                Ok(self.push(
                    NodeKind::Int(constant),
                    ty,
                    Charge::expression(),
                    expr.span,
                    false,
                ))
            }
            ExprKind::Float(value) => {
                let constant = self.push_const(ConstValue::Float(*value));
                Ok(self.push(
                    NodeKind::Float(constant),
                    ty,
                    Charge::expression(),
                    expr.span,
                    false,
                ))
            }
            ExprKind::Bool(value) => Ok(self.push(
                NodeKind::Bool(*value),
                ty,
                Charge::expression(),
                expr.span,
                false,
            )),
            ExprKind::Unit => {
                Ok(self.push(NodeKind::Unit, ty, Charge::expression(), expr.span, false))
            }
            ExprKind::Str(parts) => self.lower_string(expr, parts, ty),
            ExprKind::Var(_) => self.lower_var(expr, ty),
            ExprKind::Tuple(items) => self.lower_sequence(expr, items, ty, true),
            ExprKind::List(items) => self.lower_sequence(expr, items, ty, false),
            ExprKind::Map(entries) => self.lower_map(expr, entries, ty),
            ExprKind::StructLit { fields, base, .. } => {
                self.lower_struct_lit(expr, fields, base.as_deref(), ty)
            }
            ExprKind::Unary { op, expr: inner } => {
                let inner = self.lower_expr(inner)?;
                let kind = match op {
                    SyntaxUnaryOp::Neg => NodeKind::Neg(inner),
                    SyntaxUnaryOp::Not => NodeKind::Not(inner),
                };
                Ok(self.push(kind, ty, Charge::expression(), expr.span, false))
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let lhs = self.lower_expr(lhs)?;
                let rhs = self.lower_expr(rhs)?;
                let kind = binary_node(*op, lhs, rhs);
                Ok(self.push(kind, ty, Charge::expression(), expr.span, false))
            }
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                let start = self.lower_expr(start)?;
                let end = self.lower_expr(end)?;
                let kind = if *inclusive {
                    NodeKind::InclusiveRange { start, end }
                } else {
                    NodeKind::Range { start, end }
                };
                Ok(self.push(kind, ty, Charge::expression(), expr.span, false))
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cond = self.lower_expr(cond)?;
                let (then_block, _) = self.lower_block(then_branch, BlockKind::Body, None)?;
                let mut children = vec![cond, then_block];
                if let Some(else_branch) = else_branch {
                    children.push(self.lower_expr(else_branch)?);
                }
                let range = self.append_children(children);
                Ok(self.push(
                    NodeKind::If(range),
                    ty,
                    Charge::expression(),
                    expr.span,
                    false,
                ))
            }
            ExprKind::Match { .. } => Err(self.unsupported(expr.span, "match")),
            ExprKind::Lambda { .. } => Err(self.unsupported(expr.span, "lambda")),
            ExprKind::Block(block) => {
                let (id, _) = self.lower_block(block, BlockKind::Expr, Some(ty))?;
                Ok(id)
            }
            ExprKind::Call { callee, args } => self.lower_call(expr, callee, args, ty),
            ExprKind::Field { base, .. } => self.lower_field(expr, base, ty),
            ExprKind::Index { base, index } => {
                let base = self.lower_expr(base)?;
                let index = self.lower_expr(index)?;
                Ok(self.push(
                    NodeKind::Index { base, index },
                    ty,
                    Charge::expression(),
                    expr.span,
                    false,
                ))
            }
            ExprKind::Try(inner) => {
                let inner = self.lower_expr(inner)?;
                Ok(self.push(
                    NodeKind::Try(inner),
                    ty,
                    Charge::expression(),
                    expr.span,
                    false,
                ))
            }
            ExprKind::Convert { to, expr: inner } => {
                let inner = self.lower_expr(inner)?;
                let kind = match to {
                    PrimTy::Int => NodeKind::ConvertInt(inner),
                    PrimTy::Float => NodeKind::ConvertFloat(inner),
                    PrimTy::Str => NodeKind::ConvertStr(inner),
                    _ => return Err(self.internal(expr.span, "unsupported primitive conversion")),
                };
                Ok(self.push(kind, ty, Charge::expression(), expr.span, false))
            }
        }
    }

    fn lower_string(
        &mut self,
        expr: &Expr,
        parts: &[StrExprPart],
        ty: TyId,
    ) -> Result<IrId, LowerError> {
        if parts
            .iter()
            .all(|part| matches!(part, StrExprPart::Text(_)))
        {
            let mut text = String::new();
            for part in parts {
                if let StrExprPart::Text(part) = part {
                    text.push_str(part);
                }
            }
            let string = self.intern_str(&text);
            return Ok(self.push(
                NodeKind::Str(string),
                ty,
                Charge::expression(),
                expr.span,
                false,
            ));
        }
        let mut lowered = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                StrExprPart::Text(text) => {
                    if !text.is_empty() {
                        let string = self.intern_str(text);
                        lowered.push(StrPart::Text(string));
                    }
                }
                StrExprPart::Expr(expr) => {
                    let id = self.lower_expr(expr)?;
                    lowered.push(StrPart::Expr(id));
                }
            }
        }
        let range = self.push_str_parts(lowered);
        Ok(self.push(
            NodeKind::StrInterp(range),
            ty,
            Charge::expression(),
            expr.span,
            false,
        ))
    }

    fn lower_sequence(
        &mut self,
        expr: &Expr,
        items: &[Expr],
        ty: TyId,
        tuple: bool,
    ) -> Result<IrId, LowerError> {
        let mut children = Vec::with_capacity(items.len());
        for item in items {
            children.push(self.lower_expr(item)?);
        }
        let post = children.len() as u32;
        let range = self.append_children(children);
        let kind = if tuple {
            NodeKind::Tuple(range)
        } else {
            NodeKind::List(range)
        };
        Ok(self.push(kind, ty, Charge::new(1, post, 0), expr.span, false))
    }

    fn lower_map(
        &mut self,
        expr: &Expr,
        entries: &[(Expr, Expr)],
        ty: TyId,
    ) -> Result<IrId, LowerError> {
        let mut children = Vec::with_capacity(entries.len() * 2);
        for (key, value) in entries {
            children.push(self.lower_expr(key)?);
            children.push(self.lower_expr(value)?);
        }
        let post = children.len() as u32;
        let range = self.append_children(children);
        Ok(self.push(
            NodeKind::Map(range),
            ty,
            Charge::new(1, post, 0),
            expr.span,
            false,
        ))
    }

    fn lower_var(&mut self, expr: &Expr, ty: TyId) -> Result<IrId, LowerError> {
        let resolution = self.resolution(expr)?;
        let node = match resolution {
            Resolution::Local(binding) => {
                let slot = self.slot_for(binding, expr.span)?;
                NodeKind::SlotRead {
                    slot,
                    mode: AccessMode::Share,
                }
            }
            Resolution::Const(def) => NodeKind::ConstRef(self.const_ref(def, expr.span)?),
            Resolution::Fn(def) | Resolution::Handler(def) | Resolution::Test(def) => {
                NodeKind::FnRef(self.fn_ref(def, expr.span)?)
            }
            Resolution::ModuleItem(def) => self.def_ref_node(def, expr.span)?,
            Resolution::BuiltinFn(function) => NodeKind::BuiltinFnRef(builtin_fn_id(function)),
            Resolution::BuiltinItem(item) => self.builtin_item_node(&item, expr.span)?,
            Resolution::Variant(type_id, index) => {
                let type_id = TypeId::new(type_id.0);
                if self.variant_payload_count(type_id, index)? != 0 {
                    return Err(self.unsupported(expr.span, "variant value"));
                }
                NodeKind::EnumVariant {
                    variant: u32::try_from(index)
                        .map_err(|_| self.internal(expr.span, "variant index exceeds u32"))?,
                    first_payload: 0,
                }
            }
            Resolution::PreludeVariant(variant) => {
                if !matches!(variant, martite_check::PreludeVariant::None) {
                    return Err(self.internal(expr.span, "prelude constructor used as a value"));
                }
                NodeKind::PreludeVariant {
                    variant: PreludeVariant::None,
                    first_payload: 0,
                }
            }
            Resolution::Method(_) | Resolution::BuiltinMethod { .. } => {
                return Err(self.unsupported(expr.span, "method value"));
            }
            _ => return Err(self.internal(expr.span, "variable has no runtime value")),
        };
        Ok(self.push(node, ty, Charge::expression(), expr.span, false))
    }

    fn lower_field(&mut self, expr: &Expr, base: &Expr, ty: TyId) -> Result<IrId, LowerError> {
        let resolution = self.resolution(expr)?;
        let node = match resolution {
            Resolution::StructField(index) => {
                let base = self.lower_expr(base)?;
                let index = u32::try_from(index)
                    .map_err(|_| self.internal(expr.span, "struct field index exceeds u32"))?;
                NodeKind::Field { base, index }
            }
            Resolution::TupleField(index) => {
                let base = self.lower_expr(base)?;
                NodeKind::TupleField { base, index }
            }
            Resolution::Variant(type_id, index) => {
                let type_id = TypeId::new(type_id.0);
                if self.variant_payload_count(type_id, index)? != 0 {
                    return Err(self.unsupported(expr.span, "variant value"));
                }
                NodeKind::EnumVariant {
                    variant: u32::try_from(index)
                        .map_err(|_| self.internal(expr.span, "variant index exceeds u32"))?,
                    first_payload: 0,
                }
            }
            Resolution::ModuleItem(def) => self.def_ref_node(def, expr.span)?,
            Resolution::BuiltinItem(item) => self.builtin_item_node(&item, expr.span)?,
            Resolution::Method(_) | Resolution::BuiltinMethod { .. } => {
                return Err(self.unsupported(expr.span, "method value"));
            }
            _ => return Err(self.internal(expr.span, "field has no runtime value")),
        };
        Ok(self.push(node, ty, Charge::expression(), expr.span, false))
    }

    fn lower_call(
        &mut self,
        expr: &Expr,
        callee: &Expr,
        args: &[Arg],
        ty: TyId,
    ) -> Result<IrId, LowerError> {
        let resolution = self.resolution(callee)?;
        match resolution {
            Resolution::BuiltinFn(BuiltinFn::Assert) => {
                let children = self.lower_plain_args(args)?;
                if children.len() != 2 {
                    return Err(self.internal(expr.span, "assert arity mismatch"));
                }
                let range = self.append_children(children);
                Ok(self.push(
                    NodeKind::Assert(range),
                    ty,
                    Charge::expression(),
                    expr.span,
                    false,
                ))
            }
            Resolution::BuiltinFn(BuiltinFn::Panic) => {
                let children = self.lower_plain_args(args)?;
                if children.len() != 1 {
                    return Err(self.internal(expr.span, "panic arity mismatch"));
                }
                let range = self.append_children(children);
                Ok(self.push(
                    NodeKind::Panic(range),
                    ty,
                    Charge::expression(),
                    expr.span,
                    false,
                ))
            }
            Resolution::BuiltinFn(_) => Err(self.unsupported(expr.span, "call")),
            Resolution::PreludeVariant(variant) => {
                let children = self.lower_plain_args(args)?;
                let expected = if matches!(variant, martite_check::PreludeVariant::None) {
                    0
                } else {
                    1
                };
                if children.len() != expected {
                    return Err(self.internal(expr.span, "prelude constructor arity mismatch"));
                }
                let variant = convert_prelude(variant);
                let has_payload = !children.is_empty();
                let first_payload = if has_payload {
                    self.append_children(children).start
                } else {
                    0
                };
                let post = if has_payload { 1 } else { 0 };
                Ok(self.push(
                    NodeKind::PreludeVariant {
                        variant,
                        first_payload,
                    },
                    ty,
                    Charge::new(1, post, 0),
                    expr.span,
                    false,
                ))
            }
            Resolution::Variant(type_id, index) => {
                let type_id = TypeId::new(type_id.0);
                let payload = self.variant_payload_count(type_id, index)?;
                let children = self.lower_plain_args(args)?;
                if children.len() != payload {
                    return Err(self.internal(expr.span, "variant constructor arity mismatch"));
                }
                let first_payload = self.append_children(children).start;
                Ok(self.push(
                    NodeKind::EnumVariant {
                        variant: u32::try_from(index)
                            .map_err(|_| self.internal(expr.span, "variant index exceeds u32"))?,
                        first_payload,
                    },
                    ty,
                    Charge::new(1, payload as u32, 0),
                    expr.span,
                    false,
                ))
            }
            _ => Err(self.unsupported(expr.span, "call")),
        }
    }

    fn lower_plain_args(&mut self, args: &[Arg]) -> Result<Vec<IrId>, LowerError> {
        let mut children = Vec::with_capacity(args.len());
        for arg in args {
            if arg.mutable {
                return Err(self.unsupported(arg.span, "mut argument"));
            }
            children.push(self.lower_expr(&arg.expr)?);
        }
        Ok(children)
    }

    fn lower_struct_lit(
        &mut self,
        expr: &Expr,
        fields: &[FieldInit],
        base: Option<&Expr>,
        ty: TyId,
    ) -> Result<IrId, LowerError> {
        let resolution = self.resolution(expr)?;
        let Resolution::TypeName(check_type_id) = resolution else {
            return Err(self.internal(expr.span, "struct literal has no type resolution"));
        };
        let type_id = TypeId::new(check_type_id.0);
        let decl_count = self.struct_field_count(type_id, expr.span)?;
        let slots = self
            .lowerer
            .program
            .struct_lit_fields
            .get(&expr.id)
            .cloned()
            .ok_or_else(|| self.internal(expr.span, "struct literal has no field slots"))?;

        let base = base.map(|base| self.lower_expr(base)).transpose()?;
        let mut values = Vec::with_capacity(slots.len());
        for (source, declaration) in slots {
            let field = fields.get(source).ok_or_else(|| {
                self.internal(expr.span, "struct literal field index is out of range")
            })?;
            let value = field
                .value
                .as_ref()
                .ok_or_else(|| self.internal(expr.span, "struct literal field has no value"))?;
            let id = self.lower_expr(value)?;
            values.push((declaration, id));
        }

        let post = u32::try_from(decl_count)
            .map_err(|_| self.internal(expr.span, "struct field count exceeds u32"))?;
        match base {
            None => {
                if values.len() != decl_count {
                    return Err(self.internal(
                        expr.span,
                        "struct literal does not cover every declaration field",
                    ));
                }
                let mut seen = vec![false; decl_count];
                let mut children = Vec::with_capacity(values.len());
                let mut layout_fields = Vec::with_capacity(values.len());
                for (declaration, id) in values {
                    if declaration >= decl_count || seen[declaration] {
                        return Err(self.internal(expr.span, "invalid struct literal field slot"));
                    }
                    seen[declaration] = true;
                    children.push(id);
                    layout_fields.push(
                        u32::try_from(declaration).map_err(|_| {
                            self.internal(expr.span, "struct field index exceeds u32")
                        })?,
                    );
                }
                if seen.iter().any(|present| !present) {
                    return Err(self.internal(
                        expr.span,
                        "struct literal does not cover every declaration field",
                    ));
                }
                let first_child = self.append_children(children).start;
                let layout = self.push_layout(StructLayout {
                    first_child,
                    fields: layout_fields,
                });
                Ok(self.push(
                    NodeKind::Struct { type_id, layout },
                    ty,
                    Charge::new(1, post, 0),
                    expr.span,
                    false,
                ))
            }
            Some(base) => {
                let mut children = Vec::with_capacity(values.len() + 1);
                children.push(base);
                let mut layout_fields = Vec::with_capacity(values.len());
                for (declaration, id) in values {
                    if declaration >= decl_count {
                        return Err(self.internal(expr.span, "invalid struct update field slot"));
                    }
                    children.push(id);
                    layout_fields.push(
                        u32::try_from(declaration).map_err(|_| {
                            self.internal(expr.span, "struct field index exceeds u32")
                        })?,
                    );
                }
                let first_child = self.append_children(children).start;
                let layout = self.push_layout(StructLayout {
                    first_child,
                    fields: layout_fields,
                });
                Ok(self.push(
                    NodeKind::StructUpdate { type_id, layout },
                    ty,
                    Charge::new(1, post, 0),
                    expr.span,
                    false,
                ))
            }
        }
    }

    pub(crate) fn lower_place(&mut self, expr: &Expr) -> Result<IrId, LowerError> {
        stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || self.lower_place_inner(expr))
    }

    fn lower_place_inner(&mut self, expr: &Expr) -> Result<IrId, LowerError> {
        self.enter(expr.span)?;
        let ty = self.expr_type(expr)?;
        let node = match &expr.kind {
            ExprKind::Var(_) => match self.resolution(expr)? {
                Resolution::Local(binding) => {
                    let slot = self.slot_for(binding, expr.span)?;
                    NodeKind::SlotPlace(slot)
                }
                _ => return Err(self.internal(expr.span, "variable is not assignable")),
            },
            ExprKind::Field { base, .. } => match self.resolution(expr)? {
                Resolution::StructField(index) => {
                    let base = self.lower_place(base)?;
                    let index = u32::try_from(index)
                        .map_err(|_| self.internal(expr.span, "struct field index exceeds u32"))?;
                    NodeKind::Field { base, index }
                }
                Resolution::TupleField(index) => {
                    let base = self.lower_place(base)?;
                    NodeKind::TupleField { base, index }
                }
                _ => return Err(self.internal(expr.span, "field is not assignable")),
            },
            ExprKind::Index { base, index } => {
                let base = self.lower_place(base)?;
                let index = self.lower_expr(index)?;
                NodeKind::Index { base, index }
            }
            _ => return Err(self.internal(expr.span, "expression is not a place")),
        };
        let id = self.push(node, ty, Charge::ZERO, expr.span, false);
        self.leave();
        Ok(id)
    }

    pub(crate) fn lower_pattern(
        &mut self,
        pattern: &SyntaxPattern,
    ) -> Result<PatternId, LowerError> {
        stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || {
            self.lower_pattern_inner(pattern)
        })
    }

    fn lower_pattern_inner(&mut self, pattern: &SyntaxPattern) -> Result<PatternId, LowerError> {
        self.enter(pattern.span)?;
        let lowered = match &pattern.kind {
            PatKind::Wild => Pattern::Wild,
            PatKind::Bind(_) => {
                let slot = self
                    .plan
                    .pattern_slots
                    .get(&pattern.id)
                    .copied()
                    .ok_or_else(|| self.internal(pattern.span, "pattern has no slot"))?;
                Pattern::Bind {
                    slot,
                    ty: self.slot_ty(slot),
                }
            }
            _ => return Err(self.unsupported(pattern.span, "pattern")),
        };
        let id = self.push_pattern(lowered);
        self.leave();
        Ok(id)
    }

    fn const_ref(&mut self, def: DefId, span: Span) -> Result<crate::ConstId, LowerError> {
        self.const_id(def)
            .ok_or_else(|| self.internal(span, "constant has no pool entry"))
    }

    fn fn_ref(&mut self, def: DefId, span: Span) -> Result<crate::FnId, LowerError> {
        let id = self
            .fn_id(def)
            .ok_or_else(|| self.internal(span, "function has no IR id"))?;
        let kind = self.lowerer.fns[id.index()].kind;
        if matches!(kind, FnKind::Host(_) | FnKind::Method(_) | FnKind::Lambda) {
            return Err(self.unsupported(span, "function value kind"));
        }
        Ok(id)
    }

    fn def_ref_node(&mut self, def: DefId, span: Span) -> Result<NodeKind, LowerError> {
        if let Some(id) = self.const_id(def) {
            return Ok(NodeKind::ConstRef(id));
        }
        Ok(NodeKind::FnRef(self.fn_ref(def, span)?))
    }

    fn builtin_item_node(
        &mut self,
        item: &BuiltinItem,
        span: Span,
    ) -> Result<NodeKind, LowerError> {
        let Some(id) = BuiltinItemId::lookup(item.module, item.name) else {
            return Err(self.internal(span, "unknown builtin module item"));
        };
        match item.kind {
            DefKind::Const | DefKind::Fn => Ok(NodeKind::BuiltinModuleRef(id)),
            _ => Err(self.unsupported(span, "builtin item value")),
        }
    }

    fn variant_payload_count(&self, type_id: TypeId, index: usize) -> Result<usize, LowerError> {
        let Some(def) = self.lowerer.types.get(type_id) else {
            return Err(LowerError::internal(format!(
                "variant type #{} is missing from the IR table",
                type_id.0
            )));
        };
        let TypeDefKind::Enum { variants } = &def.kind else {
            return Err(LowerError::internal(format!(
                "variant type #{} is not an enum",
                type_id.0
            )));
        };
        variants
            .get(index)
            .map(|variant| variant.payload.len())
            .ok_or_else(|| LowerError::internal("variant index is out of range"))
    }

    fn struct_field_count(&self, type_id: TypeId, span: Span) -> Result<usize, LowerError> {
        let Some(def) = self.lowerer.types.get(type_id) else {
            return Err(self.internal(span, "struct type is missing from the IR table"));
        };
        let TypeDefKind::Struct { fields } = &def.kind else {
            return Err(self.internal(span, "struct literal type is not a struct"));
        };
        Ok(fields.len())
    }
}

fn binary_node(op: SyntaxBinaryOp, lhs: IrId, rhs: IrId) -> NodeKind {
    match op {
        SyntaxBinaryOp::Add => NodeKind::Add { lhs, rhs },
        SyntaxBinaryOp::Sub => NodeKind::Sub { lhs, rhs },
        SyntaxBinaryOp::Mul => NodeKind::Mul { lhs, rhs },
        SyntaxBinaryOp::Div => NodeKind::Div { lhs, rhs },
        SyntaxBinaryOp::Rem => NodeKind::Rem { lhs, rhs },
        SyntaxBinaryOp::Eq => NodeKind::Eq { lhs, rhs },
        SyntaxBinaryOp::Ne => NodeKind::Ne { lhs, rhs },
        SyntaxBinaryOp::Lt => NodeKind::Lt { lhs, rhs },
        SyntaxBinaryOp::Le => NodeKind::Le { lhs, rhs },
        SyntaxBinaryOp::Gt => NodeKind::Gt { lhs, rhs },
        SyntaxBinaryOp::Ge => NodeKind::Ge { lhs, rhs },
        SyntaxBinaryOp::And => NodeKind::And { lhs, rhs },
        SyntaxBinaryOp::Or => NodeKind::Or { lhs, rhs },
        SyntaxBinaryOp::Coalesce => NodeKind::Coalesce { lhs, rhs },
    }
}

fn builtin_fn_id(function: BuiltinFn) -> BuiltinFnId {
    match function {
        BuiltinFn::Print => BuiltinFnId::PRINT,
        BuiltinFn::Panic => BuiltinFnId::PANIC,
        BuiltinFn::Assert => BuiltinFnId::ASSERT,
    }
}

fn convert_prelude(variant: martite_check::PreludeVariant) -> PreludeVariant {
    match variant {
        martite_check::PreludeVariant::None => PreludeVariant::None,
        martite_check::PreludeVariant::Some => PreludeVariant::Some,
        martite_check::PreludeVariant::Ok => PreludeVariant::Ok,
        martite_check::PreludeVariant::Err => PreludeVariant::Err,
    }
}
