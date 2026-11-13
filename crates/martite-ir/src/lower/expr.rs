use martite_base::Span;
use martite_check::{
    BuiltinFn, BuiltinItem, BuiltinReceiver as CheckBuiltinReceiver, DefId, DefKind, PatResolution,
    Resolution,
};
use martite_syntax::{
    Arg, BinaryOp as SyntaxBinaryOp, Expr, ExprKind, FieldInit, FieldPat,
    MatchArm as SyntaxMatchArm, MatchBody, PatKind, Pattern as SyntaxPattern, PrimTy, StrExprPart,
    UnaryOp as SyntaxUnaryOp,
};

use crate::builtin::BuiltinReceiver as IrBuiltinReceiver;
use crate::charge::Charge;
use crate::id::{BuiltinFnId, BuiltinItemId, BuiltinMethodId, IrId, Slot};
use crate::node::{AccessMode, NodeKind, StrPart};
use crate::pattern::{MatchArm, MatchSet, Pattern};
use crate::program::{
    CaptureInfo, CaptureSource, ConstValue, FnKind, IrFn, ParamInfo, PreludeVariant, SlotInfo,
};
use crate::types::{TyId, TypeDefKind, TypeId};
use crate::{PatternId, StructLayout};

use super::LowerError;
use super::context::{FnLowerer, PatternPosition};
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
            ExprKind::Match { scrutinee, arms } => self.lower_match(expr, scrutinee, arms, ty),
            ExprKind::Lambda { .. } => self.lower_lambda(expr, ty),
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
        let post = u32::try_from(children.len())
            .map_err(|_| self.internal(expr.span, "aggregate element count exceeds u32"))?;
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
        let range = self.append_children(children);
        // The interpreter charges `map_cost(map.len())` after inserting the
        // entries, so duplicate keys reduce the post-charge. The static
        // schedule carries no post; the evaluator computes the dynamic cost.
        Ok(self.push(
            NodeKind::Map(range),
            ty,
            Charge::expression(),
            expr.span,
            false,
        ))
    }

    fn lower_var(&mut self, expr: &Expr, ty: TyId) -> Result<IrId, LowerError> {
        let resolution = self.resolution(expr)?;
        let node = match resolution {
            Resolution::Local(binding) => {
                if let Some(slot) = self.plan.binding_slots.get(&binding).copied() {
                    NodeKind::SlotRead {
                        slot,
                        mode: AccessMode::Share,
                    }
                } else if let Some(index) = self.captures.get(&binding).copied() {
                    NodeKind::CaptureRead(index)
                } else {
                    return Err(self.internal(expr.span, "binding has no frame slot or capture"));
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

    fn lower_lambda(&mut self, expr: &Expr, ty: TyId) -> Result<IrId, LowerError> {
        let info = self
            .lowerer
            .program
            .lambda(expr.id)
            .cloned()
            .ok_or_else(|| self.internal(expr.span, "lambda expression has no metadata"))?;
        if let Some(id) = self.lowerer.lambda_ids.get(&expr.id).copied() {
            return Ok(self.push(
                NodeKind::MakeClosure(id),
                ty,
                Charge::expression(),
                expr.span,
                false,
            ));
        }
        let check_ty = self
            .lowerer
            .program
            .type_of(expr.id)
            .cloned()
            .ok_or_else(|| self.internal(expr.span, "lambda expression has no checked type"))?;
        let martite_check::Ty::Fn(param_tys, ret_ty) = check_ty else {
            return Err(self.internal(expr.span, "lambda expression is not a function type"));
        };
        if param_tys.len() != info.params.len() {
            return Err(self.internal(
                expr.span,
                "lambda parameter count does not match its function type",
            ));
        }

        let mut sources = Vec::with_capacity(info.captures.len());
        for binding in &info.captures {
            let source = if let Some(slot) = self.plan.binding_slots.get(binding).copied() {
                CaptureSource::Slot(slot)
            } else if let Some(index) = self.captures.get(binding).copied() {
                CaptureSource::Capture(index)
            } else {
                return Err(self.internal(expr.span, "captured binding is not in scope"));
            };
            sources.push((*binding, source));
        }

        let mut params = Vec::with_capacity(info.params.len());
        let mut slot_info = Vec::with_capacity(info.params.len());
        for (index, (param, ty)) in info.params.iter().zip(&param_tys).enumerate() {
            let ty = self.type_id(ty)?;
            let plain = matches!(param.pattern.kind, PatKind::Bind(_));
            let name: Box<str> = match &param.pattern.kind {
                PatKind::Bind(name) if name != "_" => name.clone().into_boxed_str(),
                _ => "_".into(),
            };
            params.push(ParamInfo {
                slot: Slot::new(index as u32),
                name: name.clone(),
                ty,
                mutable: false,
                is_self: false,
                pattern: None,
            });
            slot_info.push(SlotInfo {
                ty,
                mutable: false,
                name: if plain { Some(name) } else { None },
            });
        }

        let mut captures = Vec::with_capacity(sources.len());
        for (binding, source) in sources {
            let info = self.lowerer.program.bindings.get(&binding).ok_or_else(|| {
                self.internal(expr.span, "captured binding has no checker metadata")
            })?;
            let ty = info.ty.clone();
            let name = info.name.clone().into_boxed_str();
            let ty = self.type_id(&ty)?;
            captures.push(CaptureInfo {
                source,
                ty,
                name: Some(name),
            });
        }

        let ret = self.type_id(&ret_ty)?;
        let id = crate::FnId::new(self.lowerer.fns.len() as u32);
        self.lowerer.fns.push(IrFn {
            id,
            name: "<lambda>".into(),
            source: self.function.source,
            kind: FnKind::Lambda,
            params,
            ret,
            locals: info.params.len() as u32,
            slot_info,
            captures,
            patterns: Vec::new(),
            matches: Vec::new(),
            struct_layouts: Vec::new(),
            str_parts: Vec::new(),
            nodes: Vec::new(),
            spans: Vec::new(),
            synthetic: Vec::new(),
            safepoints: Vec::new(),
            children: Vec::new(),
            body: None,
        });
        self.lowerer.lambda_ids.insert(expr.id, id);
        self.lowerer.lambda_nodes.insert(id, expr.id);
        Ok(self.push(
            NodeKind::MakeClosure(id),
            ty,
            Charge::expression(),
            expr.span,
            false,
        ))
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

    /// Lower a call by mirroring the interpreter's `eval_call` dispatch.
    ///
    /// A syntactic `Var` or `Field` callee that resolves to a function, method
    /// or builtin is not evaluated as a value; every other callee is. Mutable
    /// arguments lower to [`NodeKind::MutArg`] around their place chain, in
    /// source order (receiver first). Writeback is part of call semantics, so
    /// no separate writeback nodes are emitted.
    fn lower_call(
        &mut self,
        expr: &Expr,
        callee: &Expr,
        args: &[Arg],
        ty: TyId,
    ) -> Result<IrId, LowerError> {
        if let ExprKind::Field { base, .. } = &callee.kind {
            match self.resolution_opt(callee) {
                Some(Resolution::Method(def)) => {
                    return self.lower_method_call(base, def, args, ty, expr.span);
                }
                Some(Resolution::BuiltinMethod { receiver, name }) => {
                    return self
                        .lower_builtin_method_call(base, receiver, name, args, ty, expr.span);
                }
                Some(Resolution::Variant(type_id, index)) => {
                    return self.lower_variant_call(type_id, index, args, ty, expr.span);
                }
                Some(Resolution::ModuleItem(def)) => {
                    return self.lower_named_call(def, args, ty, expr.span);
                }
                Some(Resolution::BuiltinItem(item)) => {
                    return self.lower_builtin_module_call(&item, args, ty, expr.span);
                }
                _ => {}
            }
        }
        if let ExprKind::Var(_) = &callee.kind {
            match self.resolution_opt(callee) {
                Some(Resolution::Fn(def))
                | Some(Resolution::Handler(def))
                | Some(Resolution::Test(def))
                | Some(Resolution::ModuleItem(def)) => {
                    return self.lower_named_call(def, args, ty, expr.span);
                }
                Some(Resolution::BuiltinFn(function)) => {
                    return self.lower_builtin_fn_call(function, args, ty, expr.span);
                }
                Some(Resolution::PreludeVariant(variant)) => {
                    return self.lower_prelude_call(variant, args, ty, expr.span);
                }
                Some(Resolution::Variant(type_id, index)) => {
                    return self.lower_variant_call(type_id, index, args, ty, expr.span);
                }
                Some(Resolution::BuiltinItem(item)) => {
                    return self.lower_builtin_module_call(&item, args, ty, expr.span);
                }
                _ => {}
            }
        }
        self.lower_value_call(callee, args, ty, expr.span)
    }

    /// Lower a callee that is not evaluated: functions, associated functions,
    /// handlers and tests.
    ///
    /// Handlers with `self` receive their state as the first argument, so they
    /// lower to [`NodeKind::CallMethod`] and write the state back on success.
    fn lower_named_call(
        &mut self,
        def: DefId,
        args: &[Arg],
        ty: TyId,
        span: Span,
    ) -> Result<IrId, LowerError> {
        let Some(target) = self.fn_id(def) else {
            return Err(self.internal(span, "called function has no IR id"));
        };
        let (kind, self_first) = {
            let function = &self.lowerer.fns[target.index()];
            (
                function.kind,
                function.params.first().is_some_and(|param| param.is_self),
            )
        };
        if let FnKind::Host(host) = kind {
            if args.iter().any(|arg| arg.mutable) {
                return Err(self.internal(span, "host functions have no `mut` parameters"));
            }
            let children = self.lower_plain_args(args)?;
            let first_arg = self.append_children(children).start;
            return Ok(self.push(
                NodeKind::CallHost { host, first_arg },
                ty,
                Charge::expression(),
                span,
                true,
            ));
        }
        if self_first && args.is_empty() {
            return Err(self.internal(span, "self call without a receiver argument"));
        }
        let children = self.lower_call_args(args)?;
        let range = self.append_children(children);
        let kind = if self_first {
            NodeKind::CallMethod {
                target,
                first_child: range.start,
            }
        } else {
            NodeKind::CallDirect {
                target,
                first_arg: range.start,
            }
        };
        Ok(self.push(kind, ty, Charge::expression(), span, false))
    }

    fn lower_method_call(
        &mut self,
        receiver: &Expr,
        def: DefId,
        args: &[Arg],
        ty: TyId,
        span: Span,
    ) -> Result<IrId, LowerError> {
        let Some(target) = self.fn_id(def) else {
            return Err(self.internal(span, "called method has no IR id"));
        };
        let (is_self, self_mutable) = self.lowerer.fns[target.index()]
            .params
            .first()
            .map(|param| (param.is_self, param.mutable))
            .unwrap_or((false, false));
        if !is_self {
            return Err(self.internal(span, "method call to a function without self"));
        }
        let receiver = if self_mutable {
            self.lower_mut_arg(receiver, receiver.span)?
        } else {
            self.lower_expr(receiver)?
        };
        let mut children = Vec::with_capacity(args.len() + 1);
        children.push(receiver);
        children.extend(self.lower_call_args(args)?);
        let first_child = self.append_children(children).start;
        Ok(self.push(
            NodeKind::CallMethod {
                target,
                first_child,
            },
            ty,
            Charge::expression(),
            span,
            false,
        ))
    }

    fn lower_builtin_method_call(
        &mut self,
        receiver: &Expr,
        receiver_kind: CheckBuiltinReceiver,
        name: &str,
        args: &[Arg],
        ty: TyId,
        span: Span,
    ) -> Result<IrId, LowerError> {
        let receiver_kind = convert_builtin_receiver(receiver_kind);
        let Some(method) = BuiltinMethodId::lookup(receiver_kind, name) else {
            return Err(self.internal(span, "unknown builtin method"));
        };
        let Some(def) = method.def() else {
            return Err(self.internal(span, "builtin method has no definition"));
        };
        let receiver = if def.mut_self {
            self.lower_mut_arg(receiver, receiver.span)?
        } else {
            self.lower_expr(receiver)?
        };
        let mut children = Vec::with_capacity(args.len() + 1);
        children.push(receiver);
        children.extend(self.lower_plain_args(args)?);
        let first_child = self.append_children(children).start;
        // The sort family checks the deadline after sorting.
        let safepoint = method
            .def()
            .is_some_and(|def| matches!(def.name, "sorted" | "sorted_by" | "sort"));
        Ok(self.push(
            NodeKind::CallBuiltinMethod {
                method,
                first_child,
            },
            ty,
            Charge::expression(),
            span,
            safepoint,
        ))
    }

    fn lower_builtin_fn_call(
        &mut self,
        function: BuiltinFn,
        args: &[Arg],
        ty: TyId,
        span: Span,
    ) -> Result<IrId, LowerError> {
        match function {
            BuiltinFn::Assert => {
                let children = self.lower_plain_args(args)?;
                if children.len() != 2 {
                    return Err(self.internal(span, "assert arity mismatch"));
                }
                let range = self.append_children(children);
                Ok(self.push(
                    NodeKind::Assert(range),
                    ty,
                    Charge::expression(),
                    span,
                    false,
                ))
            }
            BuiltinFn::Panic => {
                let children = self.lower_plain_args(args)?;
                if children.len() != 1 {
                    return Err(self.internal(span, "panic arity mismatch"));
                }
                let range = self.append_children(children);
                Ok(self.push(
                    NodeKind::Panic(range),
                    ty,
                    Charge::expression(),
                    span,
                    false,
                ))
            }
            BuiltinFn::Print => {
                let children = self.lower_plain_args(args)?;
                if children.len() != 1 {
                    return Err(self.internal(span, "print arity mismatch"));
                }
                let first_arg = self.append_children(children).start;
                Ok(self.push(
                    NodeKind::CallBuiltinFn {
                        builtin: BuiltinFnId::PRINT,
                        first_arg,
                    },
                    ty,
                    Charge::expression(),
                    span,
                    true,
                ))
            }
        }
    }

    fn lower_builtin_module_call(
        &mut self,
        item: &BuiltinItem,
        args: &[Arg],
        ty: TyId,
        span: Span,
    ) -> Result<IrId, LowerError> {
        if item.kind != DefKind::Fn {
            return Err(self.unsupported(span, "builtin item value"));
        }
        let Some(id) = BuiltinItemId::lookup(item.module, item.name) else {
            return Err(self.internal(span, "unknown builtin module item"));
        };
        let children = self.lower_plain_args(args)?;
        let first_arg = self.append_children(children).start;
        // `std.json.parse` checks the input size against the memory cap.
        let safepoint = id == BuiltinItemId::JSON_PARSE;
        Ok(self.push(
            NodeKind::CallBuiltinModule {
                item: id,
                first_arg,
            },
            ty,
            Charge::expression(),
            span,
            safepoint,
        ))
    }

    fn lower_prelude_call(
        &mut self,
        variant: martite_check::PreludeVariant,
        args: &[Arg],
        ty: TyId,
        span: Span,
    ) -> Result<IrId, LowerError> {
        let children = self.lower_plain_args(args)?;
        let expected = if matches!(variant, martite_check::PreludeVariant::None) {
            0
        } else {
            1
        };
        if children.len() != expected {
            return Err(self.internal(span, "prelude constructor arity mismatch"));
        }
        let variant = convert_prelude(variant);
        let has_payload = !children.is_empty();
        let first_payload = if has_payload {
            self.append_children(children).start
        } else {
            0
        };
        // The interpreter boxes Option/Result payloads but charges no slot
        // cost for them.
        Ok(self.push(
            NodeKind::PreludeVariant {
                variant,
                first_payload,
            },
            ty,
            Charge::expression(),
            span,
            false,
        ))
    }

    fn lower_variant_call(
        &mut self,
        type_id: martite_check::TypeId,
        index: usize,
        args: &[Arg],
        ty: TyId,
        span: Span,
    ) -> Result<IrId, LowerError> {
        let type_id = TypeId::new(type_id.0);
        let payload = self.variant_payload_count(type_id, index)?;
        let children = self.lower_plain_args(args)?;
        if children.len() != payload {
            return Err(self.internal(span, "variant constructor arity mismatch"));
        }
        let first_payload = self.append_children(children).start;
        let post = u32::try_from(payload)
            .map_err(|_| self.internal(span, "variant payload count exceeds u32"))?;
        Ok(self.push(
            NodeKind::EnumVariant {
                variant: u32::try_from(index)
                    .map_err(|_| self.internal(span, "variant index exceeds u32"))?,
                first_payload,
            },
            ty,
            Charge::new(1, post, 0),
            span,
            false,
        ))
    }

    fn lower_value_call(
        &mut self,
        callee: &Expr,
        args: &[Arg],
        ty: TyId,
        span: Span,
    ) -> Result<IrId, LowerError> {
        let callee = self.lower_expr(callee)?;
        let mut children = Vec::with_capacity(args.len() + 1);
        children.push(callee);
        children.extend(self.lower_plain_args(args)?);
        let first_child = self.append_children(children).start;
        Ok(self.push(
            NodeKind::CallValue {
                first_child,
                argc: args.len() as u32,
            },
            ty,
            Charge::expression(),
            span,
            false,
        ))
    }

    fn lower_call_args(&mut self, args: &[Arg]) -> Result<Vec<IrId>, LowerError> {
        let mut children = Vec::with_capacity(args.len());
        for arg in args {
            if arg.mutable {
                children.push(self.lower_mut_arg(&arg.expr, arg.span)?);
            } else {
                children.push(self.lower_expr(&arg.expr)?);
            }
        }
        Ok(children)
    }

    fn lower_mut_arg(&mut self, expr: &Expr, span: Span) -> Result<IrId, LowerError> {
        let place = self.lower_place(expr)?;
        let ty = self.node_ty(place);
        Ok(self.push(NodeKind::MutArg(place), ty, Charge::ZERO, span, false))
    }

    fn lower_plain_args(&mut self, args: &[Arg]) -> Result<Vec<IrId>, LowerError> {
        let mut children = Vec::with_capacity(args.len());
        for arg in args {
            if arg.mutable {
                return Err(self.internal(
                    arg.span,
                    "`mut` argument on a call form without mutable parameters",
                ));
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

    fn lower_match(
        &mut self,
        expr: &Expr,
        scrutinee: &Expr,
        arms: &[SyntaxMatchArm],
        ty: TyId,
    ) -> Result<IrId, LowerError> {
        let scrutinee = self.lower_expr(scrutinee)?;
        let mut lowered = Vec::with_capacity(arms.len());
        for arm in arms {
            let pattern = self.lower_pattern(&arm.pattern, PatternPosition::Match)?;
            let guard = arm
                .guard
                .as_ref()
                .map(|guard| self.lower_expr(guard))
                .transpose()?;
            let body = match &arm.body {
                MatchBody::Block(block) => self.lower_block(block, BlockKind::Body, None)?.0,
                MatchBody::Expr(body) => self.lower_expr(body)?,
            };
            lowered.push(MatchArm {
                pattern,
                guard,
                body,
            });
        }
        let match_set = self.push_match_set(MatchSet { arms: lowered });
        Ok(self.push(
            NodeKind::Match {
                match_set,
                scrutinee,
            },
            ty,
            Charge::expression(),
            expr.span,
            false,
        ))
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
                    if let Some(slot) = self.plan.binding_slots.get(&binding).copied() {
                        NodeKind::SlotPlace(slot)
                    } else if self.captures.contains_key(&binding) {
                        return Err(self
                            .internal(expr.span, "captured variable is not an assignable place"));
                    } else {
                        return Err(self.internal(expr.span, "binding has no frame slot"));
                    }
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
        position: PatternPosition,
    ) -> Result<PatternId, LowerError> {
        stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || {
            let lowered = self.lower_pattern_value(pattern, position)?;
            Ok(self.push_pattern(lowered))
        })
    }

    fn lower_pattern_value(
        &mut self,
        pattern: &SyntaxPattern,
        position: PatternPosition,
    ) -> Result<Pattern, LowerError> {
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
            PatKind::Int(value) => Pattern::Int(*value),
            PatKind::Bool(value) => Pattern::Bool(*value),
            PatKind::Str(text) => Pattern::Str(self.intern_str(text)),
            PatKind::Tuple(items) => Pattern::Tuple(
                items
                    .iter()
                    .map(|item| self.lower_pattern_value(item, position))
                    .collect::<Result<_, _>>()?,
            ),
            PatKind::Variant { args, .. } => self.lower_variant_pattern(pattern, args, position)?,
            PatKind::Struct { fields, .. } => {
                self.lower_struct_pattern(pattern, fields, position)?
            }
            PatKind::Or(items) => match position {
                PatternPosition::Binding => {
                    let first = items.first().ok_or_else(|| {
                        self.internal(pattern.span, "or-pattern has no alternatives")
                    })?;
                    self.lower_pattern_value(first, position)?
                }
                PatternPosition::Match => Pattern::Or(
                    items
                        .iter()
                        .map(|item| self.lower_pattern_value(item, position))
                        .collect::<Result<_, _>>()?,
                ),
            },
        };
        self.leave();
        Ok(lowered)
    }

    fn lower_variant_pattern(
        &mut self,
        pattern: &SyntaxPattern,
        args: &[SyntaxPattern],
        position: PatternPosition,
    ) -> Result<Pattern, LowerError> {
        let resolution = self
            .lowerer
            .program
            .pattern_resolutions
            .get(&pattern.id)
            .copied()
            .ok_or_else(|| self.internal(pattern.span, "variant pattern has no resolution"))?;
        let args = args
            .iter()
            .map(|arg| self.lower_pattern_value(arg, position))
            .collect::<Result<Vec<_>, _>>()?;
        match resolution {
            PatResolution::Prelude(variant) => Ok(Pattern::Prelude {
                variant: convert_prelude(variant),
                args,
            }),
            PatResolution::Variant(type_id, index) => Ok(Pattern::Variant {
                type_id: TypeId::new(type_id.0),
                variant: u32::try_from(index)
                    .map_err(|_| self.internal(pattern.span, "variant index exceeds u32"))?,
                args,
            }),
            PatResolution::Struct(_) => {
                Err(self.internal(pattern.span, "variant pattern resolved as a struct pattern"))
            }
        }
    }

    fn lower_struct_pattern(
        &mut self,
        pattern: &SyntaxPattern,
        fields: &[FieldPat],
        position: PatternPosition,
    ) -> Result<Pattern, LowerError> {
        let resolution = self
            .lowerer
            .program
            .pattern_resolutions
            .get(&pattern.id)
            .copied()
            .ok_or_else(|| self.internal(pattern.span, "struct pattern has no resolution"))?;
        let PatResolution::Struct(check_type_id) = resolution else {
            return Err(self.internal(pattern.span, "struct pattern has a variant resolution"));
        };
        let slots = self
            .lowerer
            .program
            .struct_pattern_fields
            .get(&pattern.id)
            .cloned()
            .ok_or_else(|| {
                self.internal(pattern.span, "struct pattern has no declaration slots")
            })?;
        if slots.len() != fields.len() {
            return Err(self.internal(
                pattern.span,
                "struct pattern field mapping does not cover its fields",
            ));
        }
        let mut lowered = Vec::with_capacity(fields.len());
        for (source, declaration) in slots {
            let field = fields.get(source).ok_or_else(|| {
                self.internal(pattern.span, "struct pattern field index is out of range")
            })?;
            let field = self.lower_pattern_value(&field.pattern, position)?;
            lowered.push((
                u32::try_from(declaration)
                    .map_err(|_| self.internal(pattern.span, "struct field index exceeds u32"))?,
                field,
            ));
        }
        lowered.sort_by_key(|(declaration, _)| *declaration);
        Ok(Pattern::Struct {
            type_id: TypeId::new(check_type_id.0),
            fields: lowered,
        })
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

fn convert_builtin_receiver(receiver: CheckBuiltinReceiver) -> IrBuiltinReceiver {
    match receiver {
        CheckBuiltinReceiver::List => IrBuiltinReceiver::List,
        CheckBuiltinReceiver::Map => IrBuiltinReceiver::Map,
        CheckBuiltinReceiver::Str => IrBuiltinReceiver::Str,
        CheckBuiltinReceiver::Int => IrBuiltinReceiver::Int,
        CheckBuiltinReceiver::Float => IrBuiltinReceiver::Float,
        CheckBuiltinReceiver::Range => IrBuiltinReceiver::Range,
        CheckBuiltinReceiver::Option => IrBuiltinReceiver::Option,
        CheckBuiltinReceiver::Result => IrBuiltinReceiver::Result,
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
