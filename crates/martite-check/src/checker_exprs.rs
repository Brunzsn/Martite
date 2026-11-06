
#[derive(Debug, Clone)]
enum FieldKind {
    Value,
    StructField(usize),
    TupleField(u32),
    Method(DefId),
    BuiltinMethod {
        receiver: BuiltinReceiver,
        name: &'static str,
        mut_self: bool,
    },
    Variant(TypeId, usize),
    ModuleItem(DefId),
    BuiltinItem(BuiltinItem),
}

struct FieldResult {
    ty: Ty,
    kind: FieldKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PatMode {
    Let,
    Match,
    Param,
    For,
}

impl<'a> Checker<'a> {
    fn err_at(&mut self, module: ModuleId, span: Span, code: Code, message: impl Into<String>) {
        let source = self.resolved.module(module).source;
        let mut d = Diagnostic::error(code, message);
        d.source = Some(source);
        d.span = Some(span);
        self.diagnostics.push(d);
    }

    fn report_type_error(&mut self, module: ModuleId, span: Span, message: String) {
        let code = if message.contains("expects")
            || message.contains("cannot take arguments")
            || message.contains("type argument")
        {
            Code::E209
        } else {
            Code::E101
        };
        self.err_at(module, span, code, message);
    }

    // ------------------------------------------------------------------
    // Expressions
    // ------------------------------------------------------------------

    fn check_expr(&mut self, expr: &Expr, expected: Option<&Ty>, infer: &mut Infer) -> Ty {
        let ty = match &expr.kind {
            ExprKind::Var(name) => {
                if name == "None" {
                    let inner = if let Some(Ty::Option(inner)) = expected.map(|e| infer.resolve(e)) {
                        *inner
                    } else {
                        let module = self.current_module();
                        self.err_at(
                            module,
                            expr.span,
                            Code::E202,
                            "cannot infer the element type of `None`; add an annotation",
                        );
                        Ty::Error
                    };
                    self.resolutions
                        .insert(expr.id, Resolution::PreludeVariant(PreludeVariant::None));
                    Ty::option(inner)
                } else {
                    self.check_var(expr, name, infer)
                }
            }
            _ => self.infer_expr(expr, expected, infer),
        };
        let resolved = infer.resolve(&ty);
        self.expr_types.insert(expr.id, resolved.clone());
        if let Some(expected) = expected {
            let expected = infer.resolve(expected);
            if !infer.unify(&ty, &expected) && !ty.is_error() && !expected.is_error() {
                let module = self.current_module();
                self.err_at(
                    module,
                    expr.span,
                    Code::E201,
                    format!(
                        "type mismatch: expected {}, found {}",
                        expected.display(&self.types),
                        resolved.display(&self.types)
                    ),
                );
                return Ty::Error;
            }
        }
        infer.resolve(&ty)
    }

    fn infer_expr(&mut self, expr: &Expr, expected: Option<&Ty>, infer: &mut Infer) -> Ty {
        match &expr.kind {
            ExprKind::Int(_) => Ty::Int,
            ExprKind::Float(_) => Ty::Float,
            ExprKind::Bool(_) => Ty::Bool,
            ExprKind::Unit => Ty::Unit,
            ExprKind::Str(parts) => {
                for part in parts {
                    if let StrExprPart::Expr(e) = part {
                        let ty = self.check_expr(e, None, infer);
                        if !self.has_bound(&ty, Bound::Show) {
                            let module = self.current_module();
                            self.err_at(
                                module,
                                e.span,
                                Code::E205,
                                format!(
                                    "`{}` does not implement Show",
                                    ty.display(&self.types)
                                ),
                            );
                        }
                    }
                }
                Ty::Str
            }
            ExprKind::Var(_) => unreachable!("handled by check_expr"),
            ExprKind::Tuple(items) => {
                let expected_items = match expected.map(|e| infer.resolve(e)) {
                    Some(Ty::Tuple(items)) => Some(items),
                    _ => None,
                };
                let mut tys = Vec::new();
                for (i, item) in items.iter().enumerate() {
                    let exp = expected_items
                        .as_ref()
                        .and_then(|items| items.get(i))
                        .cloned();
                    tys.push(self.check_expr(item, exp.as_ref(), infer));
                }
                Ty::Tuple(tys)
            }
            ExprKind::List(items) => self.check_list(items, expected, infer),
            ExprKind::Map(entries) => self.check_map(entries, expected, infer),
            ExprKind::StructLit { name, fields, base } => {
                self.check_struct_lit(expr, name, fields, base.as_deref(), expected, infer)
            }
            ExprKind::Unary { op, expr: inner } => {
                let ty = self.check_expr(inner, None, infer);
                let ty = infer.resolve(&ty);
                match op {
                    UnaryOp::Neg => match ty {
                        Ty::Int => Ty::Int,
                        Ty::Float => Ty::Float,
                        Ty::Error => Ty::Error,
                        other => {
                            let module = self.current_module();
                            self.err_at(
                                module,
                                expr.span,
                                Code::E203,
                                format!("unary `-` is not defined for {}", other.display(&self.types)),
                            );
                            Ty::Error
                        }
                    },
                    UnaryOp::Not => {
                        if !infer.unify(&ty, &Ty::Bool) {
                            let module = self.current_module();
                            self.err_at(
                                module,
                                expr.span,
                                Code::E203,
                                "`not` requires a bool".to_string(),
                            );
                        }
                        Ty::Bool
                    }
                }
            }
            ExprKind::Binary { op, lhs, rhs } => self.check_binary(*op, lhs, rhs, expr.span, infer),
            ExprKind::Range { start, end, .. } => {
                let start_ty = self.check_expr(start, Some(&Ty::Int), infer);
                let end_ty = self.check_expr(end, Some(&Ty::Int), infer);
                let _ = (start_ty, end_ty);
                Ty::Range
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => self.check_if(expr, cond, then_branch, else_branch.as_deref(), expected, infer),
            ExprKind::Match { scrutinee, arms } => {
                self.check_match(expr, scrutinee, arms, expected, infer)
            }
            ExprKind::Lambda { params, ret, body } => {
                self.check_lambda(expr.id, params, ret.as_ref(), body, expected, infer)
            }
            ExprKind::Block(block) => self.check_block(block, expected, infer),
            ExprKind::Field { .. } => {
                let result = self.resolve_field(expr, expected, infer);
                result.ty
            }
            ExprKind::Call { .. } => self.check_call(expr, expected, infer),
            ExprKind::Index { base, index } => self.check_index(expr, base, index, expected, infer),
            ExprKind::Try(inner) => self.check_try(expr, inner, infer),
            ExprKind::Convert { to, expr: inner } => self.check_convert(expr, *to, inner, infer),
        }
    }

    fn check_list(&mut self, items: &[Expr], expected: Option<&Ty>, infer: &mut Infer) -> Ty {
        let expected_elem = match expected.map(|e| infer.resolve(e)) {
            Some(Ty::List(t)) => Some(*t),
            _ => None,
        };
        if items.is_empty() {
            if let Some(t) = expected_elem {
                return Ty::list(t);
            }
            let module = self.current_module();
            self.err_at(
                module,
                self.current_span_placeholder(items),
                Code::E202,
                "cannot infer the element type of `[]`; add an annotation",
            );
            return Ty::list(Ty::Error);
        }
        let mut elem_ty = expected_elem;
        for (i, item) in items.iter().enumerate() {
            let exp = elem_ty.clone();
            let ty = self.check_expr(item, exp.as_ref(), infer);
            if i == 0 && elem_ty.is_none() {
                elem_ty = Some(ty);
            }
        }
        Ty::list(elem_ty.unwrap_or(Ty::Error))
    }

    fn current_span_placeholder(&self, _items: &[Expr]) -> Span {
        Span::empty(0)
    }

    fn check_map(
        &mut self,
        entries: &[(Expr, Expr)],
        expected: Option<&Ty>,
        infer: &mut Infer,
    ) -> Ty {
        let (mut key_ty, mut val_ty) = match expected.map(|e| infer.resolve(e)) {
            Some(Ty::Map(k, v)) => (Some(*k), Some(*v)),
            _ => (None, None),
        };
        if entries.is_empty() {
            if let (Some(k), Some(v)) = (key_ty, val_ty) {
                return Ty::Map(Box::new(k), Box::new(v));
            }
            let module = self.current_module();
            self.err_at(
                module,
                Span::empty(0),
                Code::E202,
                "cannot infer the type of `{}`; add an annotation",
            );
            return Ty::Map(Box::new(Ty::Error), Box::new(Ty::Error));
        }
        for (i, (k, v)) in entries.iter().enumerate() {
            let kt = self.check_expr(k, key_ty.as_ref(), infer);
            let vt = self.check_expr(v, val_ty.as_ref(), infer);
            if i == 0 {
                if key_ty.is_none() {
                    key_ty = Some(kt.clone());
                }
                if val_ty.is_none() {
                    val_ty = Some(vt.clone());
                }
            }
        }
        let kt = key_ty.unwrap_or(Ty::Error);
        let vt = val_ty.unwrap_or(Ty::Error);
        if !self.map_key_ok(&kt) {
            let module = self.current_module();
            self.err_at(
                module,
                entries.first().map(|(k, _)| k.span).unwrap_or_else(|| Span::empty(0)),
                Code::E205,
                format!(
                    "{} cannot be used as a map key",
                    kt.display(&self.types)
                ),
            );
        }
        Ty::Map(Box::new(kt), Box::new(vt))
    }

    fn check_binary(
        &mut self,
        op: BinaryOp,
        lhs: &Expr,
        rhs: &Expr,
        span: Span,
        infer: &mut Infer,
    ) -> Ty {
        match op {
            BinaryOp::And | BinaryOp::Or => {
                self.check_expr(lhs, Some(&Ty::Bool), infer);
                self.check_expr(rhs, Some(&Ty::Bool), infer);
                return Ty::Bool;
            }
            BinaryOp::Coalesce => {
                let lhs_raw = self.check_expr(lhs, None, infer);
                let lhs_ty = infer.resolve(&lhs_raw);
                let inner = match lhs_ty {
                    Ty::Option(t) => Some(*t),
                    Ty::Error => Some(Ty::Error),
                    other => {
                        let module = self.current_module();
                        self.err_at(
                            module,
                            lhs.span,
                            Code::E203,
                            format!(
                                "`??` requires an optional value, found {}",
                                other.display(&self.types)
                            ),
                        );
                        Some(Ty::Error)
                    }
                };
                let rhs_ty = self.check_expr(rhs, inner.as_ref(), infer);
                return inner.unwrap_or(rhs_ty);
            }
            BinaryOp::Eq | BinaryOp::Ne => {
                let lhs_ty = self.check_expr(lhs, None, infer);
                self.check_expr(rhs, Some(&lhs_ty), infer);
                if !self.has_bound(&lhs_ty, Bound::Eq) {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        span,
                        Code::E205,
                        format!("{} does not implement Eq", lhs_ty.display(&self.types)),
                    );
                }
                return Ty::Bool;
            }
            BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
                let lhs_ty = self.check_expr(lhs, None, infer);
                self.check_expr(rhs, Some(&lhs_ty), infer);
                if !self.has_bound(&lhs_ty, Bound::Ord) {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        span,
                        Code::E205,
                        format!("{} does not implement Ord", lhs_ty.display(&self.types)),
                    );
                }
                return Ty::Bool;
            }
            _ => {}
        }

        let lhs_raw = self.check_expr(lhs, None, infer);
        let lhs_ty = infer.resolve(&lhs_raw);
        if lhs_ty.is_error() {
            self.check_expr(rhs, None, infer);
            return Ty::Error;
        }
        match op {
            BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => {
                let want = match &lhs_ty {
                    Ty::Int => Ty::Int,
                    Ty::Float if op != BinaryOp::Rem => Ty::Float,
                    Ty::Str if op == BinaryOp::Add => Ty::Str,
                    _ => {
                        let module = self.current_module();
                        self.err_at(
                            module,
                            span,
                            Code::E203,
                            format!(
                                "operator {} is not defined for {}",
                                op.as_str(),
                                lhs_ty.display(&self.types)
                            ),
                        );
                        self.check_expr(rhs, None, infer);
                        return Ty::Error;
                    }
                };
                let rhs_ty = self.check_expr(rhs, None, infer);
                if !infer.matches(&rhs_ty, &want) {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        span,
                        Code::E203,
                        format!(
                            "operator {} is not defined for {} and {}",
                            op.as_str(),
                            lhs_ty.display(&self.types),
                            infer.resolve(&rhs_ty).display(&self.types)
                        ),
                    );
                    return Ty::Error;
                }
                want
            }
            _ => Ty::Error,
        }
    }

    fn check_if(
        &mut self,
        expr: &Expr,
        cond: &Expr,
        then_branch: &Block,
        else_branch: Option<&Expr>,
        expected: Option<&Ty>,
        infer: &mut Infer,
    ) -> Ty {
        self.check_expr(cond, Some(&Ty::Bool), infer);
        let then_ty = self.check_block(then_branch, expected, infer);
        let Some(else_branch) = else_branch else {
            if !infer.unify(&then_ty, &Ty::Unit) && !then_ty.is_never() {
                let module = self.current_module();
                self.err_at(
                    module,
                    then_branch.span,
                    Code::E201,
                    format!(
                        "an `if` without `else` must have type (), found {}",
                        then_ty.display(&self.types)
                    ),
                );
            }
            return Ty::Unit;
        };
        let else_ty = match expected {
            Some(e) => self.check_expr(else_branch, Some(e), infer),
            None => self.check_expr(else_branch, None, infer),
        };
        if let Some(e) = expected {
            let _ = e;
            return infer.resolve(e);
        }
        self.join_types(then_ty, else_ty, expr.span)
    }

    fn join_types(&mut self, a: Ty, b: Ty, span: Span) -> Ty {
        let a = a;
        let b = b;
        if a.is_never() {
            return b;
        }
        if b.is_never() {
            return a;
        }
        if a.is_error() {
            return b;
        }
        if b.is_error() {
            return a;
        }
        if a == b {
            return a;
        }
        let module = self.current_module();
        self.err_at(
            module,
            span,
            Code::E201,
            format!(
                "branches have incompatible types {} and {}",
                a.display(&self.types),
                b.display(&self.types)
            ),
        );
        Ty::Error
    }

    fn check_match(
        &mut self,
        expr: &Expr,
        scrutinee: &Expr,
        arms: &[MatchArm],
        expected: Option<&Ty>,
        infer: &mut Infer,
    ) -> Ty {
        let scrutinee_ty = self.check_expr(scrutinee, None, infer);
        let mut result: Option<Ty> = None;
        for arm in arms {
            self.scopes.push(HashMap::new());
            let bindings =
                self.check_pattern(&arm.pattern, &scrutinee_ty, PatMode::Match, false, infer);
            for (name, (id, _)) in &bindings {
                self.bind_existing(name, *id, arm.pattern.span);
            }
            if let Some(guard) = &arm.guard {
                self.check_expr(guard, Some(&Ty::Bool), infer);
            }
            let body_ty = match (&arm.body, expected) {
                (MatchBody::Block(b), exp) => self.check_block(b, exp, infer),
                (MatchBody::Expr(e), exp) => self.check_expr(e, exp, infer),
            };
            self.scopes.pop();
            if expected.is_some() {
                result = expected.cloned();
            } else if let Some(current) = result.take() {
                result = Some(self.join_types(current, body_ty, arm.span));
            } else {
                result = Some(body_ty);
            }
        }
        if !is_exhaustive(arms, &scrutinee_ty, &self.types) {
            let module = self.current_module();
            self.err_at(
                module,
                expr.span,
                Code::E301,
                format!(
                    "match is not exhaustive for {}",
                    scrutinee_ty.display(&self.types)
                ),
            );
        }
        result.unwrap_or(Ty::Unit)
    }

    fn check_lambda(
        &mut self,
        node: NodeId,
        params: &[LambdaParam],
        ret: Option<&TyExpr>,
        body: &LambdaBody,
        expected: Option<&Ty>,
        infer: &mut Infer,
    ) -> Ty {
        let expected_fn = match expected.map(|e| infer.resolve(e)) {
            Some(Ty::Fn(params, ret)) => Some((params, *ret)),
            _ => None,
        };
        let ret_explicit = match ret {
            Some(t) => match self.resolve_current_ty(t) {
                Ok(t) => Some(t),
                Err(msg) => {
                    let module = self.current_module();
                    self.report_type_error(module, t.span, msg);
                    None
                }
            },
            None => None,
        };
        let ret_ty = ret_explicit
            .clone()
            .or_else(|| expected_fn.as_ref().map(|(_, r)| r.clone()));
        if let Some((expected_params, _)) = &expected_fn {
            if expected_params.len() != params.len() {
                let module = self.current_module();
                self.err_at(
                    module,
                    Span::empty(0),
                    Code::E204,
                    format!(
                        "lambda takes {} parameter(s), expected {}",
                        params.len(),
                        expected_params.len()
                    ),
                );
            }
        }

        let module = self.current_module();
        let saved_ctx = self.ctx.clone();
        let boundary = self.scopes.len();
        self.lambda_stack.push(LambdaCaptureCtx {
            node,
            boundary,
            captures: HashSet::new(),
        });
        if let Some(ctx) = &mut self.ctx {
            ctx.return_ty = ret_ty.clone().unwrap_or(Ty::Error);
            ctx.loop_depth = 0;
            ctx.closure_boundary = Some(boundary);
        }
        self.scopes.push(HashMap::new());
        let mut param_tys = Vec::new();
        for (i, param) in params.iter().enumerate() {
            let ty = match &param.ty {
                Some(t) => match self.resolve_current_ty(t) {
                    Ok(t) => t,
                    Err(msg) => {
                        self.report_type_error(module, t.span, msg);
                        Ty::Error
                    }
                },
                None => expected_fn
                    .as_ref()
                    .and_then(|(ps, _)| ps.get(i).cloned())
                    .unwrap_or_else(|| {
                        self.err_at(
                            module,
                            param.span,
                            Code::E202,
                            "cannot infer this lambda parameter's type; add an annotation",
                        );
                        Ty::Error
                    }),
            };
            let bindings = self.check_pattern(&param.pattern, &ty, PatMode::Param, false, infer);
            for (name, (id, _)) in &bindings {
                self.bind_existing(name, *id, param.span);
            }
            param_tys.push(ty);
        }
        let inferred_ret = match body {
            LambdaBody::Block(b) => self.check_block(b, ret_ty.as_ref(), infer),
            LambdaBody::Expr(e) => self.check_expr(e, ret_ty.as_ref(), infer),
        };
        self.scopes.pop();
        let lambda = self.lambda_stack.pop().expect("lambda capture context");
        debug_assert_eq!(lambda.node, node);
        let mut captures: Vec<BindingId> = lambda.captures.into_iter().collect();
        captures.sort_unstable();
        self.lambdas.insert(
            node,
            LambdaInfo {
                params: params.to_vec(),
                body: body.clone(),
                captures,
            },
        );
        self.ctx = saved_ctx;
        let final_ret = ret_ty.unwrap_or(inferred_ret);
        Ty::Fn(param_tys, Box::new(final_ret))
    }

    fn check_block(&mut self, block: &Block, expected: Option<&Ty>, infer: &mut Infer) -> Ty {
        self.scopes.push(HashMap::new());
        let mut value_ty = None;
        for (i, stmt) in block.stmts.iter().enumerate() {
            let is_last = i + 1 == block.stmts.len();
            if is_last {
                if let StmtKind::Expr { expr, assign: None } = &stmt.kind {
                    let expects_unit = matches!(
                        expected.map(|e| infer.resolve(e)),
                        Some(Ty::Unit)
                    );
                    let ty = if expects_unit {
                        self.check_expr(expr, None, infer)
                    } else {
                        self.check_expr(expr, expected, infer)
                    };
                    if expects_unit
                        && !ty.is_never()
                        && !ty.is_error()
                        && !infer.matches(&ty, &Ty::Unit)
                    {
                        let module = self.current_module();
                        self.err_at(
                            module,
                            expr.span,
                            Code::E206,
                            format!(
                                "non-unit value of type {} is discarded",
                                ty.display(&self.types)
                            ),
                        );
                    }
                    value_ty = Some(if expects_unit { Ty::Unit } else { ty });
                    continue;
                }
            }
            self.check_stmt(stmt, infer);
        }
        self.scopes.pop();
        let ty = value_ty.unwrap_or(Ty::Unit);
        if let Some(expected) = expected {
            let expected = infer.resolve(expected);
            if !ty.is_never() && !infer.unify(&ty, &expected) && !ty.is_error() && !expected.is_error()
            {
                let module = self.current_module();
                self.err_at(
                    module,
                    block.span,
                    Code::E201,
                    format!(
                        "block has type {}, expected {}",
                        ty.display(&self.types),
                        expected.display(&self.types)
                    ),
                );
                return Ty::Error;
            }
        }
        infer.resolve(&ty)
    }

    fn check_stmt(&mut self, stmt: &Stmt, infer: &mut Infer) {
        match &stmt.kind {
            StmtKind::Let {
                mutable,
                pattern,
                ty,
                value,
            } => {
                let annotation = match ty {
                    Some(t) => match self.resolve_current_ty(t) {
                        Ok(t) => Some(t),
                        Err(msg) => {
                            let module = self.current_module();
                            self.report_type_error(module, t.span, msg);
                            Some(Ty::Error)
                        }
                    },
                    None => None,
                };
                let value_ty = self.check_expr(value, annotation.as_ref(), infer);
                let bind_ty = annotation.unwrap_or(value_ty);
                let bindings =
                    self.check_pattern(pattern, &bind_ty, PatMode::Let, *mutable, infer);
                for (name, (id, _)) in &bindings {
                    self.bind_existing(name, *id, pattern.span);
                }
            }
            StmtKind::While { cond, body } => {
                self.check_expr(cond, Some(&Ty::Bool), infer);
                if let Some(ctx) = &mut self.ctx {
                    ctx.loop_depth += 1;
                }
                self.check_block(body, Some(&Ty::Unit), infer);
                if let Some(ctx) = &mut self.ctx {
                    ctx.loop_depth -= 1;
                }
            }
            StmtKind::For {
                pattern,
                iter,
                body,
            } => {
                let iter_raw = self.check_expr(iter, None, infer);
                let iter_ty = infer.resolve(&iter_raw);
                let elem = match &iter_ty {
                    Ty::List(t) => Some((**t).clone()),
                    Ty::Map(k, v) => Some(Ty::Tuple(vec![(**k).clone(), (**v).clone()])),
                    Ty::Str => Some(Ty::Str),
                    Ty::Range => Some(Ty::Int),
                    Ty::Error => Some(Ty::Error),
                    other => {
                        let module = self.current_module();
                        self.err_at(
                            module,
                            iter.span,
                            Code::E203,
                            format!(
                                "{} is not iterable",
                                other.display(&self.types)
                            ),
                        );
                        Some(Ty::Error)
                    }
                };
                self.scopes.push(HashMap::new());
                if let Some(elem) = elem {
                    let bindings = self.check_pattern(pattern, &elem, PatMode::For, false, infer);
                    for (name, (id, _)) in &bindings {
                        self.bind_existing(name, *id, pattern.span);
                    }
                }
                if let Some(ctx) = &mut self.ctx {
                    ctx.loop_depth += 1;
                }
                self.check_block(body, Some(&Ty::Unit), infer);
                if let Some(ctx) = &mut self.ctx {
                    ctx.loop_depth -= 1;
                }
                self.scopes.pop();
            }
            StmtKind::Return(value) => {
                let ret = self.current_ret_ty();
                match value {
                    Some(v) => {
                        self.check_expr(v, Some(&ret), infer);
                    }
                    None => {
                        if !ret.is_error() && !infer.unify(&ret, &Ty::Unit) {
                            let module = self.current_module();
                            self.err_at(
                                module,
                                stmt.span,
                                Code::E201,
                                format!(
                                    "this function returns {}, not ()",
                                    ret.display(&self.types)
                                ),
                            );
                        }
                    }
                }
            }
            StmtKind::Break | StmtKind::Continue => {
                if self.ctx.as_ref().map(|c| c.loop_depth).unwrap_or(0) == 0 {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        stmt.span,
                        Code::E405,
                        "`break`/`continue` outside a loop",
                    );
                }
            }
            StmtKind::Expr { expr, assign } => {
                if let Some((op, rhs)) = assign {
                    let lhs_ty = self.check_expr(expr, None, infer);
                    let place = self.check_place(expr);
                    match place {
                        Some(p) => {
                            if !p.mutable {
                                let module = self.current_module();
                                self.err_at(
                                    module,
                                    expr.span,
                                    Code::E401,
                                    "cannot assign to an immutable binding",
                                );
                            }
                            if self.is_captured(p.root) {
                                let module = self.current_module();
                                self.err_at(
                                    module,
                                    expr.span,
                                    Code::E404,
                                    "cannot assign to a captured variable inside a closure",
                                );
                            }
                        }
                        None => {
                            let module = self.current_module();
                            self.err_at(
                                module,
                                expr.span,
                                Code::E401,
                                "the left-hand side of an assignment must be a place",
                            );
                        }
                    }
                    match op {
                        AssignOp::Assign => {
                            self.check_expr(rhs, Some(&lhs_ty), infer);
                        }
                        AssignOp::Add => {
                            if !matches!(
                                infer.resolve(&lhs_ty),
                                Ty::Int | Ty::Float | Ty::Str | Ty::Error
                            ) {
                                let module = self.current_module();
                                self.err_at(
                                    module,
                                    stmt.span,
                                    Code::E203,
                                    "`+=` requires int, float, or str",
                                );
                            }
                            self.check_expr(rhs, Some(&lhs_ty), infer);
                        }
                        _ => {
                            if !matches!(infer.resolve(&lhs_ty), Ty::Int | Ty::Float | Ty::Error) {
                                let module = self.current_module();
                                self.err_at(
                                    module,
                                    stmt.span,
                                    Code::E203,
                                    "compound assignment requires int or float",
                                );
                            }
                            self.check_expr(rhs, Some(&lhs_ty), infer);
                        }
                    }
                } else {
                    let ty = self.check_expr(expr, None, infer);
                    if !ty.is_never() && !ty.is_error() && !infer.matches(&ty, &Ty::Unit) {
                        let module = self.current_module();
                        self.err_at(
                            module,
                            expr.span,
                            Code::E206,
                            format!(
                                "non-unit value of type {} is discarded; write `let _ = ...`",
                                ty.display(&self.types)
                            ),
                        );
                    }
                }
            }
        }
    }

    fn current_module(&self) -> ModuleId {
        self.ctx
            .as_ref()
            .map(|c| c.module)
            .unwrap_or(self.resolved.entry)
    }

    fn current_ret_ty(&self) -> Ty {
        self.ctx
            .as_ref()
            .map(|c| c.return_ty.clone())
            .unwrap_or(Ty::Unit)
    }

    fn resolve_current_ty(&self, ty: &TyExpr) -> Result<Ty, String> {
        let ctx = self
            .ctx
            .as_ref()
            .ok_or_else(|| "type used outside a function".to_string())?;
        self.resolve_ty_in(ctx.module, ty, &ctx.type_params)
    }

    fn check_var(&mut self, expr: &Expr, name: &str, infer: &mut Infer) -> Ty {
        if name == "self" {
            if let Some(ctx) = &self.ctx {
                if let (Some(id), Some(ty)) = (ctx.self_binding, ctx.self_ty.clone()) {
                    self.note_local_use(id);
                    self.resolutions.insert(expr.id, Resolution::Local(id));
                    return ty;
                }
            }
            let module = self.current_module();
            self.err_at(
                module,
                expr.span,
                Code::E101,
                "`self` is only valid inside a method or handler",
            );
            return Ty::Error;
        }
        if let Some(id) = self.lookup_local(name) {
            self.note_local_use(id);
            let ty = self
                .bindings
                .get(&id)
                .map(|b| b.ty.clone())
                .unwrap_or(Ty::Error);
            self.resolutions.insert(expr.id, Resolution::Local(id));
            return ty;
        }

        let module = self.current_module();
        let resolved = self.resolved;
        let symbols = &resolved.module(module).symbols;
        if let Some(def) = symbols.values.get(name).copied() {
            let info = resolved.def(def);
            if let Some(sig) = self.functions.get(&def).cloned() {
                let ty = Ty::Fn(
                    sig.params.iter().map(|p| p.ty.clone()).collect(),
                    Box::new(sig.ret.clone()),
                );
                self.resolutions.insert(
                    expr.id,
                    match info.kind {
                        DefKind::Handler => Resolution::Handler(def),
                        _ => Resolution::Fn(def),
                    },
                );
                return ty;
            }
            self.resolutions.insert(expr.id, Resolution::Fn(def));
            return Ty::Error;
        }
        if let Some(def) = symbols.consts.get(name).copied() {
            let ty = self
                .const_types
                .get(&def)
                .cloned()
                .unwrap_or(Ty::Error);
            self.resolutions.insert(expr.id, Resolution::Const(def));
            return ty;
        }
        if let Some(item) = symbols.builtin_items.get(name).copied() {
            self.resolutions.insert(expr.id, Resolution::BuiltinItem(item));
            return self.builtin_item_ty(&item);
        }
        if symbols.modules.contains_key(name) {
            self.resolutions
                .insert(expr.id, Resolution::Module(symbols.modules[name]));
            self.err_at(
                module,
                expr.span,
                Code::E101,
                format!("module `{name}` is not a value"),
            );
            return Ty::Error;
        }
        if symbols.types.contains_key(name) {
            self.err_at(
                module,
                expr.span,
                Code::E101,
                format!("type `{name}` is not a value"),
            );
            return Ty::Error;
        }
        match name {
            "print" => {
                self.resolutions
                    .insert(expr.id, Resolution::BuiltinFn(BuiltinFn::Print));
                return Ty::Fn(vec![Ty::Param(0)], Box::new(Ty::Unit));
            }
            "panic" => {
                self.resolutions
                    .insert(expr.id, Resolution::BuiltinFn(BuiltinFn::Panic));
                return Ty::Fn(vec![Ty::Str], Box::new(Ty::Never));
            }
            "assert" => {
                self.resolutions
                    .insert(expr.id, Resolution::BuiltinFn(BuiltinFn::Assert));
                return Ty::Fn(vec![Ty::Bool, Ty::Str], Box::new(Ty::Unit));
            }
            "Some" | "Ok" | "Err" => {
                self.resolutions.insert(
                    expr.id,
                    Resolution::PreludeVariant(match name {
                        "Some" => PreludeVariant::Some,
                        "Ok" => PreludeVariant::Ok,
                        _ => PreludeVariant::Err,
                    }),
                );
                return Ty::Error;
            }
            _ => {}
        }
        let _ = infer;
        self.err_at(module, expr.span, Code::E101, format!("undefined name `{name}`"));
        Ty::Error
    }

    fn builtin_item_ty(&self, item: &BuiltinItem) -> Ty {
        match (item.module, item.name) {
            ("std.math", "PI") | ("std.math", "EULER") => Ty::Float,
            ("std.math", "min" | "max" | "clamp") => {
                Ty::Fn(vec![Ty::Param(0), Ty::Param(0)], Box::new(Ty::Param(0)))
            }
            ("std.math", "sin" | "cos" | "tan" | "exp" | "ln" | "log10") => {
                Ty::Fn(vec![Ty::Float], Box::new(Ty::Float))
            }
            ("std.math", "atan2") => Ty::Fn(vec![Ty::Float, Ty::Float], Box::new(Ty::Float)),
            ("std.json", "parse") => Ty::Fn(
                vec![Ty::Str],
                Box::new(Ty::Result(
                    Box::new(Ty::Named(self.json_type.unwrap_or(TypeId(0)), Vec::new())),
                    Box::new(Ty::Str),
                )),
            ),
            ("std.json", "stringify") => Ty::Fn(
                vec![Ty::Named(self.json_type.unwrap_or(TypeId(0)), Vec::new())],
                Box::new(Ty::Str),
            ),
            ("std.json", "Json") => Ty::Named(self.json_type.unwrap_or(TypeId(0)), Vec::new()),
            _ => Ty::Error,
        }
    }

    // ------------------------------------------------------------------
    // Fields and methods
    // ------------------------------------------------------------------

    fn resolve_field(&mut self, expr: &Expr, expected: Option<&Ty>, infer: &mut Infer) -> FieldResult {
        let ExprKind::Field { base, name } = &expr.kind else {
            return FieldResult {
                ty: Ty::Error,
                kind: FieldKind::Value,
            };
        };

        if let ExprKind::Var(base_name) = &base.kind {
            if self.lookup_local(base_name).is_none() && base_name != "self" {
                let module = self.current_module();
                let resolved = self.resolved;
                let symbols = &resolved.module(module).symbols;
                if let Some(target) = symbols.modules.get(base_name).copied() {
                    let result = self.resolve_module_field(expr, target, name, expected, infer);
                    return self.record_field(expr, result);
                }
                if let Some(def) = symbols.types.get(base_name).copied() {
                    if let Some(type_id) = self.def_to_type.get(&def).copied() {
                        let result = self.resolve_type_field(expr, type_id, name, expected, infer);
                        return self.record_field(expr, result);
                    }
                }
                if let Some(item) = symbols.builtin_items.get(base_name).copied() {
                    if item.name == "Json" {
                        let id = self.json_type.unwrap_or(TypeId(0));
                        let result = self.resolve_type_field(expr, id, name, expected, infer);
                        return self.record_field(expr, result);
                    }
                }
            }
        }

        let base_raw = self.check_expr(base, None, infer);
        let base_ty = infer.resolve(&base_raw);
        let result = self.resolve_value_field(base, &base_ty, name, infer);
        self.record_field(expr, result)
    }

    fn record_field(&mut self, expr: &Expr, result: FieldResult) -> FieldResult {
        match &result.kind {
            FieldKind::Value => {}
            FieldKind::StructField(index) => {
                self.resolutions
                    .insert(expr.id, Resolution::StructField(*index));
            }
            FieldKind::TupleField(index) => {
                self.resolutions
                    .insert(expr.id, Resolution::TupleField(*index));
            }
            FieldKind::Method(def) => {
                self.resolutions.insert(expr.id, Resolution::Method(*def));
            }
            FieldKind::BuiltinMethod {
                receiver, name, ..
            } => {
                self.resolutions.insert(
                    expr.id,
                    Resolution::BuiltinMethod {
                        receiver: *receiver,
                        name: *name,
                    },
                );
            }
            FieldKind::Variant(id, index) => {
                self.resolutions
                    .insert(expr.id, Resolution::Variant(*id, *index));
            }
            FieldKind::ModuleItem(def) => {
                self.resolutions
                    .insert(expr.id, Resolution::ModuleItem(*def));
            }
            FieldKind::BuiltinItem(item) => {
                self.resolutions
                    .insert(expr.id, Resolution::BuiltinItem(*item));
            }
        }
        result
    }

    fn resolve_module_field(
        &mut self,
        expr: &Expr,
        target: ModuleTarget,
        name: &FieldName,
        _expected: Option<&Ty>,
        infer: &mut Infer,
    ) -> FieldResult {
        let FieldName::Ident(name) = name else {
            let module = self.current_module();
            self.err_at(module, expr.span, Code::E204, "modules have no tuple fields");
            return FieldResult {
                ty: Ty::Error,
                kind: FieldKind::Value,
            };
        };
        match target {
            ModuleTarget::Source(mid) | ModuleTarget::Interface(mid) => {
                let resolved = self.resolved;
                let target_module = resolved.module(mid);
                if let Some(def) = target_module.symbols.values.get(name).copied() {
                    let info = resolved.def(def);
                    if mid != self.current_module() && !info.is_pub {
                        let module = self.current_module();
                        self.err_at(module, expr.span, Code::E106, format!("`{name}` is private"));
                        return FieldResult {
                            ty: Ty::Error,
                            kind: FieldKind::Value,
                        };
                    }
                    if let Some(sig) = self.functions.get(&def).cloned() {
                        let ty = Ty::Fn(
                            sig.params.iter().map(|p| p.ty.clone()).collect(),
                            Box::new(sig.ret.clone()),
                        );
                        return FieldResult {
                            ty,
                            kind: FieldKind::ModuleItem(def),
                        };
                    }
                }
                if let Some(def) = target_module.symbols.consts.get(name).copied() {
                    let info = resolved.def(def);
                    if mid != self.current_module() && !info.is_pub {
                        let module = self.current_module();
                        self.err_at(module, expr.span, Code::E106, format!("`{name}` is private"));
                        return FieldResult {
                            ty: Ty::Error,
                            kind: FieldKind::Value,
                        };
                    }
                    let ty = self
                        .const_types
                        .get(&def)
                        .cloned()
                        .unwrap_or(Ty::Error);
                    return FieldResult {
                        ty,
                        kind: FieldKind::ModuleItem(def),
                    };
                }
                let module = self.current_module();
                self.err_at(
                    module,
                    expr.span,
                    Code::E106,
                    format!("module has no accessible item `{name}`; import types by name"),
                );
                FieldResult {
                    ty: Ty::Error,
                    kind: FieldKind::Value,
                }
            }
            ModuleTarget::Builtin(module_name) => {
                let Some(item) = builtin_item(module_name, name) else {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        expr.span,
                        Code::E106,
                        format!("`{module_name}` has no item `{name}`"),
                    );
                    return FieldResult {
                        ty: Ty::Error,
                        kind: FieldKind::Value,
                    };
                };
                let ty = self.builtin_item_ty(&item);
                let _ = infer;
                FieldResult {
                    ty,
                    kind: FieldKind::BuiltinItem(item),
                }
            }
        }
    }

    fn resolve_type_field(
        &mut self,
        expr: &Expr,
        type_id: TypeId,
        name: &FieldName,
        expected: Option<&Ty>,
        infer: &mut Infer,
    ) -> FieldResult {
        let FieldName::Ident(name) = name else {
            let module = self.current_module();
            self.err_at(module, expr.span, Code::E204, "types have no tuple fields");
            return FieldResult {
                ty: Ty::Error,
                kind: FieldKind::Value,
            };
        };
        let def = self.types.get(type_id).cloned();
        if let Some(def) = &def {
            if let TypeDefKind::Enum { .. } = def.kind {
                if let Some(index) = self.types.variant_index(type_id, name) {
                    let payload = self
                        .types
                        .variant(type_id, index)
                        .map(|v| v.payload.clone())
                        .unwrap_or_default();
                    let mut args = Vec::new();
                    if !def.params.is_empty() {
                        if let Some(Ty::Named(id, expected_args)) =
                            expected.map(|e| infer.resolve(e))
                        {
                            if id == type_id && expected_args.len() == def.params.len() {
                                args = expected_args;
                            }
                        }
                        if args.is_empty() {
                            args = def.params.iter().map(|_| infer.fresh()).collect();
                        }
                    }
                    let named = Ty::Named(type_id, args.clone());
                    let payload: Vec<Ty> = payload
                        .iter()
                        .map(|t| instantiate_params(t, &args))
                        .collect();
                    self.resolutions
                        .insert(expr.id, Resolution::Variant(type_id, index));
                    if payload.is_empty() {
                        return FieldResult {
                            ty: named,
                            kind: FieldKind::Variant(type_id, index),
                        };
                    }
                    return FieldResult {
                        ty: Ty::Fn(payload, Box::new(named)),
                        kind: FieldKind::Variant(type_id, index),
                    };
                }
            }
        }
        if let Some(def_id) = self.methods.get(&(type_id, name.to_string())).copied() {
            if let Some(sig) = self.functions.get(&def_id).cloned() {
                if sig.has_self() {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        expr.span,
                        Code::E208,
                        format!("`{name}` is a method; call it on a value"),
                    );
                    return FieldResult {
                        ty: Ty::Error,
                        kind: FieldKind::Value,
                    };
                }
                let ty = Ty::Fn(
                    sig.call_params().iter().map(|p| p.ty.clone()).collect(),
                    Box::new(sig.ret.clone()),
                );
                return FieldResult {
                    ty,
                    kind: FieldKind::ModuleItem(def_id),
                };
            }
        }
        let module = self.current_module();
        self.err_at(
            module,
            expr.span,
            Code::E208,
            format!(
                "`{}` has no variant or associated function `{name}`",
                def.map(|d| d.name).unwrap_or_default()
            ),
        );
        FieldResult {
            ty: Ty::Error,
            kind: FieldKind::Value,
        }
    }

    fn resolve_value_field(
        &mut self,
        base: &Expr,
        base_ty: &Ty,
        name: &FieldName,
        infer: &mut Infer,
    ) -> FieldResult {
        match base_ty {
            Ty::Error => FieldResult {
                ty: Ty::Error,
                kind: FieldKind::Value,
            },
            Ty::Named(type_id, args) => {
                let field_name = match name {
                    FieldName::Ident(n) => n,
                    FieldName::Index(_) => "",
                };
                if let Some(index) = self.types.field_index(*type_id, field_name) {
                    let field_ty = self
                        .types
                        .field(*type_id, field_name)
                        .map(|f| instantiate_params(&f.ty, args))
                        .unwrap_or(Ty::Error);
                    return FieldResult {
                        ty: field_ty,
                        kind: FieldKind::StructField(index),
                    };
                }
                let FieldName::Ident(name) = name else {
                    let module = self.current_module();
                    self.err_at(module, base.span, Code::E204, "struct has no tuple fields");
                    return FieldResult {
                        ty: Ty::Error,
                        kind: FieldKind::Value,
                    };
                };
                if let Some(def_id) = self.methods.get(&(*type_id, name.clone())).copied() {
                    if let Some(sig) = self.functions.get(&def_id).cloned() {
                        let ty = Ty::Fn(
                            sig.call_params().iter().map(|p| p.ty.clone()).collect(),
                            Box::new(sig.ret.clone()),
                        );
                        return FieldResult {
                            ty,
                            kind: FieldKind::Method(def_id),
                        };
                    }
                }
                let module = self.current_module();
                self.err_at(
                    module,
                    base.span,
                    Code::E208,
                    format!("unknown field or method `{name}`"),
                );
                FieldResult {
                    ty: Ty::Error,
                    kind: FieldKind::Value,
                }
            }
            Ty::Tuple(items) => match name {
                FieldName::Index(index) => {
                    if let Some(ty) = items.get(*index as usize) {
                        FieldResult {
                            ty: ty.clone(),
                            kind: FieldKind::TupleField(*index),
                        }
                    } else {
                        let module = self.current_module();
                        self.err_at(
                            module,
                            base.span,
                            Code::E204,
                            format!("tuple has no field `{index}`"),
                        );
                        FieldResult {
                            ty: Ty::Error,
                            kind: FieldKind::Value,
                        }
                    }
                }
                FieldName::Ident(name) => {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        base.span,
                        Code::E204,
                        format!("tuples have no field `{name}`"),
                    );
                    FieldResult {
                        ty: Ty::Error,
                        kind: FieldKind::Value,
                    }
                }
            },
            other => {
                if let Some(receiver) = builtin_receiver(other) {
                    let FieldName::Ident(name) = name else {
                        let module = self.current_module();
                        self.err_at(module, base.span, Code::E204, "tuple index is invalid here");
                        return FieldResult {
                            ty: Ty::Error,
                            kind: FieldKind::Value,
                        };
                    };
                    if let Some(info) = builtin_method_info(receiver, name) {
                        let _ = infer;
                        return FieldResult {
                            // Methods are not first-class values; the call
                            // checker uses the field kind directly.
                            ty: Ty::Error,
                            kind: FieldKind::BuiltinMethod {
                                receiver,
                                name: info.name,
                                mut_self: info.mut_self,
                            },
                        };
                    }
                    let module = self.current_module();
                    self.err_at(
                        module,
                        base.span,
                        Code::E208,
                        format!("unknown method `{name}` for {}", other.display(&self.types)),
                    );
                    return FieldResult {
                        ty: Ty::Error,
                        kind: FieldKind::Value,
                    };
                }
                let module = self.current_module();
                self.err_at(
                    module,
                    base.span,
                    Code::E208,
                    format!("{} has no fields", other.display(&self.types)),
                );
                FieldResult {
                    ty: Ty::Error,
                    kind: FieldKind::Value,
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Calls
    // ------------------------------------------------------------------

    fn check_call(&mut self, expr: &Expr, expected: Option<&Ty>, infer: &mut Infer) -> Ty {
        let ExprKind::Call { callee, args } = &expr.kind else {
            return Ty::Error;
        };
        if let ExprKind::Var(name) = &callee.kind {
            match name.as_str() {
                "Some" | "None" | "Ok" | "Err" => {
                    return self.check_prelude_variant_call(expr, callee, name, args, expected, infer);
                }
                _ => {}
            }
        }
        if let ExprKind::Field { base, name: _ } = &callee.kind {
            let field = self.resolve_field(callee, None, infer);
            match field.kind {
                FieldKind::Method(def) => {
                    let recv_raw = self.check_expr(base, None, infer);
                    let receiver_ty = infer.resolve(&recv_raw);
                    return self.check_fn_call(
                        expr,
                        def,
                        args,
                        expected,
                        Some((base, receiver_ty)),
                        infer,
                    );
                }
                FieldKind::BuiltinMethod {
                    receiver,
                    name,
                    mut_self,
                } => {
                    let recv_raw = self.check_expr(base, None, infer);
                    let receiver_ty = infer.resolve(&recv_raw);
                    if mut_self {
                        self.require_mutable_place(base, expr.span);
                    }
                    return self.check_builtin_method(
                        expr,
                        receiver,
                        name,
                        &receiver_ty,
                        args,
                        expected,
                        infer,
                    );
                }
                FieldKind::Variant(type_id, index) => {
                    return self.check_variant_call(
                        expr, callee, type_id, index, args, expected, infer,
                    );
                }
                FieldKind::ModuleItem(def) => {
                    return self.check_fn_call(expr, def, args, expected, None, infer);
                }
                FieldKind::BuiltinItem(item) => {
                    return self.check_builtin_module_call(expr, &item, args, expected, infer);
                }
                FieldKind::Value | FieldKind::StructField(_) | FieldKind::TupleField(_) => {}
            }
        }

        let callee_raw = self.check_expr(callee, None, infer);
        let callee_ty = infer.resolve(&callee_raw);
        if let Some(Resolution::Fn(def) | Resolution::Handler(def)) =
            self.resolutions.get(&callee.id).copied()
        {
            if matches!(
                self.resolutions.get(&callee.id),
                Some(Resolution::Handler(_))
            ) && !self.ctx.as_ref().is_some_and(|c| c.is_test)
            {
                let module = self.current_module();
                self.err_at(
                    module,
                    callee.span,
                    Code::E505,
                    "handlers may only be called inside `test` blocks",
                );
            }
            return self.check_fn_call(expr, def, args, expected, None, infer);
        }
        if let Some(Resolution::BuiltinFn(function)) = self.resolutions.get(&callee.id).copied() {
            return self.check_builtin_call(expr, function, args, infer);
        }
        let Ty::Fn(params, ret) = callee_ty else {
            if !callee_ty.is_error() {
                let module = self.current_module();
                self.err_at(
                    module,
                    callee.span,
                    Code::E201,
                    format!("{} is not callable", callee_ty.display(&self.types)),
                );
            }
            return Ty::Error;
        };
        if args.len() != params.len() {
            let module = self.current_module();
            self.err_at(
                module,
                expr.span,
                Code::E204,
                format!(
                    "expected {} argument(s), found {}",
                    params.len(),
                    args.len()
                ),
            );
        }
        for (arg, param) in args.iter().zip(params.iter()) {
            if arg.mutable {
                let module = self.current_module();
                self.err_at(
                    module,
                    arg.span,
                    Code::E402,
                    "a function value has no `mut` parameters",
                );
            }
            self.check_expr(&arg.expr, Some(param), infer);
        }
        if let Some(expected) = expected {
            if !infer.unify(&ret, expected) {
                let module = self.current_module();
                self.err_at(
                    module,
                    expr.span,
                    Code::E201,
                    format!(
                        "call returns {}, expected {}",
                        ret.display(&self.types),
                        expected.display(&self.types)
                    ),
                );
                return Ty::Error;
            }
        }
        infer.resolve(&ret)
    }

    fn check_prelude_variant_call(
        &mut self,
        expr: &Expr,
        callee: &Expr,
        name: &str,
        args: &[Arg],
        expected: Option<&Ty>,
        infer: &mut Infer,
    ) -> Ty {
        let expected = expected.map(|e| infer.resolve(e));
        let variant = match name {
            "Some" => PreludeVariant::Some,
            "None" => PreludeVariant::None,
            "Ok" => PreludeVariant::Ok,
            _ => PreludeVariant::Err,
        };
        self.resolutions
            .insert(callee.id, Resolution::PreludeVariant(variant));
        match variant {
            PreludeVariant::None => {
                if !args.is_empty() {
                    let module = self.current_module();
                    self.err_at(module, expr.span, Code::E204, "`None` takes no arguments");
                }
                match expected {
                    Some(Ty::Option(inner)) => Ty::option(*inner),
                    _ => {
                        let module = self.current_module();
                        self.err_at(
                            module,
                            expr.span,
                            Code::E202,
                            "cannot infer the element type of `None`; add an annotation",
                        );
                        Ty::option(Ty::Error)
                    }
                }
            }
            PreludeVariant::Some => {
                if args.len() != 1 {
                    let module = self.current_module();
                    self.err_at(module, expr.span, Code::E204, "`Some` takes one argument");
                }
                let inner_expected = match &expected {
                    Some(Ty::Option(inner)) => Some((**inner).clone()),
                    _ => None,
                };
                let inner = match args.first() {
                    Some(arg) => {
                        if arg.mutable {
                            let module = self.current_module();
                            self.err_at(module, arg.span, Code::E402, "unexpected `mut`");
                        }
                        self.check_expr(&arg.expr, inner_expected.as_ref(), infer)
                    }
                    None => Ty::Error,
                };
                Ty::option(inner)
            }
            PreludeVariant::Ok | PreludeVariant::Err => {
                if args.len() != 1 {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        expr.span,
                        Code::E204,
                        format!("`{name}` takes one argument"),
                    );
                }
                let (ok_expected, err_expected) = match &expected {
                    Some(Ty::Result(ok, err)) => (Some((**ok).clone()), Some((**err).clone())),
                    _ => (None, None),
                };
                let payload_expected = if matches!(variant, PreludeVariant::Ok) {
                    ok_expected
                } else {
                    err_expected
                };
                let payload = match args.first() {
                    Some(arg) => {
                        if arg.mutable {
                            let module = self.current_module();
                            self.err_at(module, arg.span, Code::E402, "unexpected `mut`");
                        }
                        self.check_expr(&arg.expr, payload_expected.as_ref(), infer)
                    }
                    None => Ty::Error,
                };
                match variant {
                    PreludeVariant::Ok => {
                        let err = match &expected {
                            Some(Ty::Result(_, err)) => (**err).clone(),
                            _ => {
                                let module = self.current_module();
                                self.err_at(
                                    module,
                                    expr.span,
                                    Code::E202,
                                    "cannot infer the error type of `Ok`; add an annotation",
                                );
                                Ty::Error
                            }
                        };
                        Ty::Result(Box::new(payload), Box::new(err))
                    }
                    _ => {
                        let ok = match &expected {
                            Some(Ty::Result(ok, _)) => (**ok).clone(),
                            _ => {
                                let module = self.current_module();
                                self.err_at(
                                    module,
                                    expr.span,
                                    Code::E202,
                                    "cannot infer the success type of `Err`; add an annotation",
                                );
                                Ty::Error
                            }
                        };
                        Ty::Result(Box::new(ok), Box::new(payload))
                    }
                }
            }
        }
    }

    fn check_variant_call(
        &mut self,
        expr: &Expr,
        callee: &Expr,
        type_id: TypeId,
        index: usize,
        args: &[Arg],
        expected: Option<&Ty>,
        infer: &mut Infer,
    ) -> Ty {
        let def = self.types.get(type_id).cloned();
        let params = def.as_ref().map(|d| d.params.len()).unwrap_or(0);
        let mut type_args: Vec<Ty> = Vec::new();
        if params > 0 {
            if let Some(Ty::Named(id, expected_args)) = expected.map(|e| infer.resolve(e)) {
                if id == type_id && expected_args.len() == params {
                    type_args = expected_args;
                }
            }
            if type_args.is_empty() {
                for _ in 0..params {
                    type_args.push(infer.fresh());
                }
            }
        }
        let payload = self
            .types
            .variant(type_id, index)
            .map(|v| {
                v.payload
                    .iter()
                    .map(|t| instantiate_params(t, &type_args))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if args.len() != payload.len() {
            let module = self.current_module();
            self.err_at(
                module,
                expr.span,
                Code::E204,
                format!(
                    "variant takes {} argument(s), found {}",
                    payload.len(),
                    args.len()
                ),
            );
        }
        for (arg, param) in args.iter().zip(payload.iter()) {
            if arg.mutable {
                let module = self.current_module();
                self.err_at(module, arg.span, Code::E402, "variant arguments cannot be `mut`");
            }
            self.check_expr(&arg.expr, Some(param), infer);
        }
        let named = Ty::Named(type_id, type_args.clone());
        if let Some(expected) = expected {
            if !infer.unify(&named, expected) {
                let module = self.current_module();
                self.err_at(
                    module,
                    expr.span,
                    Code::E201,
                    format!(
                        "variant constructs {}, expected {}",
                        named.display(&self.types),
                        expected.display(&self.types)
                    ),
                );
                return Ty::Error;
            }
        }
        for arg in &type_args {
            if infer.resolve(arg).has_var() {
                let module = self.current_module();
                self.err_at(
                    module,
                    expr.span,
                    Code::E202,
                    "cannot infer the variant's type arguments; add an annotation",
                );
                break;
            }
        }
        self.resolutions
            .insert(callee.id, Resolution::Variant(type_id, index));
        infer.resolve(&named)
    }

    fn check_fn_call(
        &mut self,
        expr: &Expr,
        def: DefId,
        args: &[Arg],
        expected: Option<&Ty>,
        receiver: Option<(&Expr, Ty)>,
        infer: &mut Infer,
    ) -> Ty {
        let Some(sig) = self.functions.get(&def).cloned() else {
            return Ty::Error;
        };
        if sig.kind == FnKind::Handler && !self.ctx.as_ref().is_some_and(|c| c.is_test) {
            let module = self.current_module();
            self.err_at(
                module,
                expr.span,
                Code::E505,
                "handlers may only be called inside `test` blocks",
            );
        }
        if let Some((_, recv_ty)) = &receiver {
            if !sig.has_self() {
                let module = self.current_module();
                self.err_at(
                    module,
                    expr.span,
                    Code::E208,
                    "this function has no `self` parameter",
                );
                return Ty::Error;
            }
            if sig.self_mutable() {
                if let Some((recv_expr, _)) = receiver {
                    self.require_mutable_place(recv_expr, expr.span);
                }
            }
            let _ = recv_ty;
        }

        let mut vars = Vec::new();
        for _ in &sig.type_params {
            vars.push(infer.fresh());
        }
        let param_tys: Vec<Ty> = sig
            .params
            .iter()
            .map(|p| instantiate_params(&p.ty, &vars))
            .collect();
        let ret_ty = instantiate_params(&sig.ret, &vars);
        if let Some(exp) = expected {
            let _ = infer.unify(&ret_ty, exp);
        }
        if let Some((_, recv_ty)) = &receiver {
            if let Some(self_ty) = param_tys.first() {
                let _ = infer.unify(self_ty, recv_ty);
            }
        }

        let (call_params, call_args): (&[Ty], &[Arg]) = if let Some(_) = receiver {
            (&param_tys[1..], args)
        } else {
            (&param_tys[..], args)
        };
        if call_args.len() != call_params.len() {
            let module = self.current_module();
            self.err_at(
                module,
                expr.span,
                Code::E204,
                format!(
                    "expected {} argument(s), found {}",
                    call_params.len(),
                    call_args.len()
                ),
            );
        }

        let mut roots: Vec<(BindingId, bool)> = Vec::new();
        if let Some((recv_expr, _)) = receiver {
            if let Some(place) = self.check_place(recv_expr) {
                roots.push((place.root, sig.self_mutable()));
            }
        }

        let mut lambda_indices = Vec::new();
        for (i, arg) in call_args.iter().enumerate() {
            if matches!(arg.expr.kind, ExprKind::Lambda { .. }) {
                lambda_indices.push(i);
                continue;
            }
            let Some(param_ty) = call_params.get(i) else {
                continue;
            };
            let param_sig = sig
                .params
                .iter()
                .skip(if receiver.is_some() { 1 } else { 0 })
                .nth(i);
            let expects_mut = param_sig.map(|p| p.mutable).unwrap_or(false);
            if expects_mut != arg.mutable {
                let module = self.current_module();
                if expects_mut {
                    self.err_at(
                        module,
                        arg.span,
                        Code::E402,
                        "missing `mut` on a mutable argument",
                    );
                } else {
                    self.err_at(
                        module,
                        arg.span,
                        Code::E402,
                        "`mut` is not needed for this argument",
                    );
                }
            }
            if let Some(place) = self.check_place(&arg.expr) {
                roots.push((place.root, arg.mutable));
            }
            if arg.mutable {
                self.check_mut_arg(&arg.expr, arg.span);
            }
            self.check_expr(&arg.expr, Some(param_ty), infer);
        }
        for i in lambda_indices {
            let Some(param_ty) = call_params.get(i) else {
                continue;
            };
            let param_sig = sig
                .params
                .iter()
                .skip(if receiver.is_some() { 1 } else { 0 })
                .nth(i);
            let expects_mut = param_sig.map(|p| p.mutable).unwrap_or(false);
            if expects_mut != call_args[i].mutable {
                let module = self.current_module();
                self.err_at(
                    module,
                    call_args[i].span,
                    Code::E402,
                    if expects_mut {
                        "missing `mut` on a mutable argument"
                    } else {
                        "`mut` is not needed for this argument"
                    },
                );
            }
            if let Some(place) = self.check_place(&call_args[i].expr) {
                roots.push((place.root, call_args[i].mutable));
            }
            if call_args[i].mutable {
                self.check_mut_arg(&call_args[i].expr, call_args[i].span);
            }
            self.check_expr(&call_args[i].expr, Some(param_ty), infer);
        }
        if has_conflicting_alias(&roots) {
            let module = self.current_module();
            self.err_at(
                module,
                expr.span,
                Code::E403,
                "the same variable is passed more than once where one use is `mut`",
            );
        }

        for (index, var) in vars.iter().enumerate() {
            let resolved = infer.resolve(var);
            if resolved.has_var() {
                let module = self.current_module();
                self.err_at(
                    module,
                    expr.span,
                    Code::E202,
                    format!(
                        "cannot infer type parameter `{}`; add an annotation",
                        sig.type_params
                            .get(index)
                            .map(|p| p.name.as_str())
                            .unwrap_or("?")
                    ),
                );
                continue;
            }
            for bound in &sig.type_params[index].bounds {
                if !self.has_bound(&resolved, *bound) {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        expr.span,
                        Code::E205,
                        format!(
                            "`{}` does not satisfy `{}`",
                            resolved.display(&self.types),
                            bound.as_str()
                        ),
                    );
                }
            }
        }
        infer.resolve(&ret_ty)
    }

    fn check_mut_arg(&mut self, expr: &Expr, span: Span) {
        match self.check_place(expr) {
            Some(place) => {
                if !place.mutable {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        span,
                        Code::E402,
                        "a `mut` argument must be a mutable place",
                    );
                }
                if self.is_captured(place.root) {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        span,
                        Code::E404,
                        "cannot mutate a captured variable inside a closure",
                    );
                }
            }
            None => {
                let module = self.current_module();
                self.err_at(
                    module,
                    span,
                    Code::E402,
                    "a `mut` argument must be a place",
                );
            }
        }
    }

    fn require_mutable_place(&mut self, expr: &Expr, span: Span) {
        match self.check_place(expr) {
            Some(place) => {
                if !place.mutable {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        span,
                        Code::E401,
                        "a mutating call requires a mutable place",
                    );
                }
                if self.is_captured(place.root) {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        span,
                        Code::E404,
                        "cannot mutate a captured variable inside a closure",
                    );
                }
            }
            None => {
                let module = self.current_module();
                self.err_at(
                    module,
                    span,
                    Code::E401,
                    "a mutating call requires a place",
                );
            }
        }
    }

    fn check_builtin_call(
        &mut self,
        expr: &Expr,
        function: BuiltinFn,
        args: &[Arg],
        infer: &mut Infer,
    ) -> Ty {
        match function {
            BuiltinFn::Print => {
                if args.len() != 1 {
                    let module = self.current_module();
                    self.err_at(module, expr.span, Code::E204, "`print` takes one argument");
                }
                if let Some(arg) = args.first() {
                    let ty = self.check_expr(&arg.expr, None, infer);
                    if !self.has_bound(&ty, Bound::Show) {
                        let module = self.current_module();
                        self.err_at(
                            module,
                            arg.span,
                            Code::E205,
                            format!("{} does not implement Show", ty.display(&self.types)),
                        );
                    }
                }
                Ty::Unit
            }
            BuiltinFn::Panic => {
                if args.len() != 1 {
                    let module = self.current_module();
                    self.err_at(module, expr.span, Code::E204, "`panic` takes one argument");
                }
                if let Some(arg) = args.first() {
                    self.check_expr(&arg.expr, Some(&Ty::Str), infer);
                }
                Ty::Never
            }
            BuiltinFn::Assert => {
                if args.len() != 2 {
                    let module = self.current_module();
                    self.err_at(module, expr.span, Code::E204, "`assert` takes two arguments");
                }
                if let Some(arg) = args.first() {
                    self.check_expr(&arg.expr, Some(&Ty::Bool), infer);
                }
                if let Some(arg) = args.get(1) {
                    self.check_expr(&arg.expr, Some(&Ty::Str), infer);
                }
                Ty::Unit
            }
        }
    }

    fn check_builtin_module_call(
        &mut self,
        expr: &Expr,
        item: &BuiltinItem,
        args: &[Arg],
        expected: Option<&Ty>,
        infer: &mut Infer,
    ) -> Ty {
        match (item.module, item.name) {
            ("std.math", "min" | "max") => {
                self.check_generic_ord_call(expr, args, expected, infer, 2)
            }
            ("std.math", "clamp") => {
                self.check_generic_ord_call(expr, args, expected, infer, 3)
            }
            ("std.math", "atan2") => {
                self.check_fixed_args(expr, args, &[Ty::Float, Ty::Float], infer)
            }
            (
                "std.math",
                "sin" | "cos" | "tan" | "exp" | "ln" | "log10",
            ) => {
                self.check_fixed_args(expr, args, &[Ty::Float], infer);
                Ty::Float
            }
            ("std.json", "parse") => {
                self.check_fixed_args(expr, args, &[Ty::Str], infer);
                let json = Ty::Named(self.json_type.unwrap_or(TypeId(0)), Vec::new());
                Ty::Result(Box::new(json), Box::new(Ty::Str))
            }
            ("std.json", "stringify") => {
                let json = Ty::Named(self.json_type.unwrap_or(TypeId(0)), Vec::new());
                self.check_fixed_args(expr, args, &[json], infer);
                Ty::Str
            }
            _ => Ty::Error,
        }
    }

    fn check_fixed_args(
        &mut self,
        expr: &Expr,
        args: &[Arg],
        params: &[Ty],
        infer: &mut Infer,
    ) -> Ty {
        if args.len() != params.len() {
            let module = self.current_module();
            self.err_at(
                module,
                expr.span,
                Code::E204,
                format!("expected {} argument(s), found {}", params.len(), args.len()),
            );
        }
        for (arg, param) in args.iter().zip(params) {
            self.check_expr(&arg.expr, Some(param), infer);
        }
        Ty::Unit
    }

    fn check_generic_ord_call(
        &mut self,
        expr: &Expr,
        args: &[Arg],
        expected: Option<&Ty>,
        infer: &mut Infer,
        arity: usize,
    ) -> Ty {
        if args.len() != arity {
            let module = self.current_module();
            self.err_at(
                module,
                expr.span,
                Code::E204,
                format!("expected {arity} argument(s), found {}", args.len()),
            );
        }
        let mut ty = None;
        for arg in args {
            let arg_ty = self.check_expr(&arg.expr, ty.as_ref(), infer);
            if ty.is_none() {
                ty = Some(arg_ty);
            }
        }
        let ty = ty.unwrap_or(Ty::Error);
        if !self.has_bound(&ty, Bound::Ord) {
            let module = self.current_module();
            self.err_at(
                module,
                expr.span,
                Code::E205,
                format!("{} does not implement Ord", ty.display(&self.types)),
            );
        }
        if let Some(exp) = expected {
            let _ = infer.unify(&ty, exp);
        }
        ty
    }

    // ------------------------------------------------------------------
    // Indexing, try, conversions
    // ------------------------------------------------------------------

    fn check_index(
        &mut self,
        expr: &Expr,
        base: &Expr,
        index: &Expr,
        _expected: Option<&Ty>,
        infer: &mut Infer,
    ) -> Ty {
        let base_raw = self.check_expr(base, None, infer);
        let base_ty = infer.resolve(&base_raw);
        match base_ty {
            Ty::List(inner) => {
                let idx_ty = self.check_expr(index, None, infer);
                let idx_ty = infer.resolve(&idx_ty);
                match idx_ty {
                    Ty::Int => *inner,
                    Ty::Range => Ty::list((*inner).clone()),
                    Ty::Error => Ty::Error,
                    other => {
                        let module = self.current_module();
                        self.err_at(
                            module,
                            index.span,
                            Code::E201,
                            format!(
                                "list index must be int or range, found {}",
                                other.display(&self.types)
                            ),
                        );
                        Ty::Error
                    }
                }
            }
            Ty::Map(key, value) => {
                self.check_expr(index, Some(&key), infer);
                *value
            }
            Ty::Str => {
                let idx_ty = self.check_expr(index, Some(&Ty::Range), infer);
                if !infer.matches(&idx_ty, &Ty::Range) && !idx_ty.is_error() {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        index.span,
                        Code::E201,
                        "str index must be a range",
                    );
                }
                Ty::Str
            }
            Ty::Error => Ty::Error,
            other => {
                let module = self.current_module();
                self.err_at(
                    module,
                    expr.span,
                    Code::E203,
                    format!("cannot index {}", other.display(&self.types)),
                );
                self.check_expr(index, None, infer);
                Ty::Error
            }
        }
    }

    fn check_try(&mut self, expr: &Expr, inner: &Expr, infer: &mut Infer) -> Ty {
        let inner_raw = self.check_expr(inner, None, infer);
        let inner_ty = infer.resolve(&inner_raw);
        let ret = self.current_ret_ty();
        match inner_ty {
            Ty::Option(t) => {
                if !matches!(infer.resolve(&ret), Ty::Option(_) | Ty::Error) {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        expr.span,
                        Code::E207,
                        "`?` on an Option requires the enclosing function to return Option",
                    );
                }
                *t
            }
            Ty::Result(t, e) => match infer.resolve(&ret) {
                Ty::Result(_, ret_e) => {
                    if !infer.unify(&e, &ret_e) {
                        let module = self.current_module();
                        self.err_at(
                            module,
                            expr.span,
                            Code::E207,
                            "`?` requires the same error type as the enclosing function",
                        );
                    }
                    *t
                }
                Ty::Error => *t,
                _ => {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        expr.span,
                        Code::E207,
                        "`?` on a Result requires the enclosing function to return Result",
                    );
                    *t
                }
            },
            Ty::Error => Ty::Error,
            other => {
                let module = self.current_module();
                self.err_at(
                    module,
                    expr.span,
                    Code::E203,
                    format!("`?` is not defined for {}", other.display(&self.types)),
                );
                Ty::Error
            }
        }
    }

    fn check_convert(
        &mut self,
        expr: &Expr,
        to: PrimTy,
        inner: &Expr,
        infer: &mut Infer,
    ) -> Ty {
        match to {
            PrimTy::Int => {
                self.check_expr(inner, Some(&Ty::Float), infer);
                Ty::Int
            }
            PrimTy::Float => {
                self.check_expr(inner, Some(&Ty::Int), infer);
                Ty::Float
            }
            PrimTy::Str => {
                let ty = self.check_expr(inner, None, infer);
                if !self.has_bound(&ty, Bound::Show) {
                    let module = self.current_module();
                    self.err_at(
                        module,
                        expr.span,
                        Code::E205,
                        format!("{} does not implement Show", ty.display(&self.types)),
                    );
                }
                Ty::Str
            }
            _ => Ty::Error,
        }
    }

    // ------------------------------------------------------------------
    // Struct literals
    // ------------------------------------------------------------------

    fn check_struct_lit(
        &mut self,
        expr: &Expr,
        name: &str,
        fields: &[FieldInit],
        base: Option<&Expr>,
        expected: Option<&Ty>,
        infer: &mut Infer,
    ) -> Ty {
        let module = self.current_module();
        let resolved = self.resolved;
        let symbols = &resolved.module(module).symbols;
        let Some(def) = symbols.types.get(name).copied() else {
            self.err_at(module, expr.span, Code::E101, format!("unknown type `{name}`"));
            return Ty::Error;
        };
        let Some(type_id) = self.def_to_type.get(&def).copied() else {
            return Ty::Error;
        };
        let type_def = match self.types.get(type_id).cloned() {
            Some(d) => d,
            None => return Ty::Error,
        };
        let TypeDefKind::Struct { fields: decl_fields } = &type_def.kind else {
            self.err_at(
                module,
                expr.span,
                Code::E304,
                format!("`{name}` is not a struct"),
            );
            return Ty::Error;
        };
        let mut type_args: Vec<Ty> = Vec::new();
        if !type_def.params.is_empty() {
            if let Some(Ty::Named(id, args)) = expected.map(|e| infer.resolve(e)) {
                if id == type_id && args.len() == type_def.params.len() {
                    type_args = args;
                }
            }
            if type_args.is_empty() {
                for _ in 0..type_def.params.len() {
                    type_args.push(infer.fresh());
                }
            }
        }
        let named = Ty::Named(type_id, type_args.clone());
        self.resolutions.insert(expr.id, Resolution::TypeName(type_id));
        let slots = fields
            .iter()
            .enumerate()
            .filter_map(|(source, field)| {
                decl_fields
                    .iter()
                    .position(|decl| decl.name == field.name)
                    .map(|decl| (source, decl))
            })
            .collect();
        self.struct_lit_fields.insert(expr.id, slots);

        if let Some(base) = base {
            self.check_expr(base, Some(&named), infer);
        } else {
            for decl in decl_fields {
                let count = fields.iter().filter(|f| f.name == decl.name).count();
                if count == 0 {
                    self.err_at(
                        module,
                        expr.span,
                        Code::E204,
                        format!("missing field `{}`", decl.name),
                    );
                } else if count > 1 {
                    self.err_at(
                        module,
                        expr.span,
                        Code::E204,
                        format!("duplicate field `{}`", decl.name),
                    );
                }
            }
        }
        let mut seen = HashSet::new();
        for field in fields {
            if !seen.insert(field.name.clone()) {
                continue;
            }
            let Some(decl) = decl_fields.iter().find(|f| f.name == field.name) else {
                self.err_at(
                    module,
                    field.span,
                    Code::E204,
                    format!("unknown field `{}`", field.name),
                );
                continue;
            };
            let field_ty = instantiate_params(&decl.ty, &type_args);
            let Some(value) = &field.value else {
                continue;
            };
            self.check_expr(value, Some(&field_ty), infer);
        }
        for arg in &type_args {
            if infer.resolve(arg).has_var() {
                self.err_at(
                    module,
                    expr.span,
                    Code::E202,
                    "cannot infer the struct's type arguments; add an annotation",
                );
                break;
            }
        }
        named
    }

    // ------------------------------------------------------------------
    // Places and captures
    // ------------------------------------------------------------------

    fn check_place(&mut self, expr: &Expr) -> Option<Place> {
        match &expr.kind {
            ExprKind::Var(name) if name == "self" => {
                let ctx = self.ctx.as_ref()?;
                let id = ctx.self_binding?;
                Some(Place {
                    root: id,
                    mutable: ctx.self_mutable,
                })
            }
            ExprKind::Var(name) => {
                let id = self.lookup_local(name)?;
                let info = self.bindings.get(&id)?;
                Some(Place {
                    root: id,
                    mutable: info.mutable,
                })
            }
            ExprKind::Field { base, .. } => self.check_place(base),
            ExprKind::Index { base, .. } => self.check_place(base),
            _ => None,
        }
    }

    fn is_captured(&self, root: BindingId) -> bool {
        let Some(ctx) = &self.ctx else { return false };
        let Some(boundary) = ctx.closure_boundary else {
            return false;
        };
        self.bindings
            .get(&root)
            .is_some_and(|b| b.scope_depth < boundary)
    }
}

#[derive(Clone, Copy)]
struct Place {
    root: BindingId,
    mutable: bool,
}

fn has_conflicting_alias(roots: &[(BindingId, bool)]) -> bool {
    for (i, (a, a_mut)) in roots.iter().enumerate() {
        for (b, b_mut) in roots.iter().skip(i + 1) {
            if a == b && (*a_mut || *b_mut) {
                return true;
            }
        }
    }
    false
}

#[derive(Clone, Copy)]
struct BuiltinMethodInfo {
    name: &'static str,
    mut_self: bool,
}

fn builtin_method_info(receiver: BuiltinReceiver, name: &str) -> Option<BuiltinMethodInfo> {
    let (known, mut_self): (&'static [&'static str], bool) = match receiver {
        BuiltinReceiver::List => (
            &[
                "len", "get", "first", "last", "contains", "map", "filter", "fold", "any",
                "all", "enumerate", "reversed", "sorted", "sorted_by", "join", "push", "pop",
                "sort", "insert", "remove_at", "extend", "clear",
            ],
            matches!(
                name,
                "push" | "pop" | "sort" | "insert" | "remove_at" | "extend" | "clear"
            ),
        ),
        BuiltinReceiver::Map => (
            &[
                "len", "get", "has", "keys", "values", "entries", "remove", "clear",
            ],
            matches!(name, "remove" | "clear"),
        ),
        BuiltinReceiver::Str => (
            &[
                "len",
                "chars",
                "split",
                "trim",
                "contains",
                "find",
                "starts_with",
                "ends_with",
                "replace",
                "to_upper",
                "to_lower",
                "parse_int",
                "parse_float",
            ],
            false,
        ),
        BuiltinReceiver::Int => (&["abs", "pow"], false),
        BuiltinReceiver::Float => (
            &[
                "abs", "floor", "ceil", "trunc", "sqrt", "pow", "round", "is_nan",
                "is_finite",
            ],
            false,
        ),
        BuiltinReceiver::Range => (&["len", "contains", "to_list"], false),
        BuiltinReceiver::Option => (
            &[
                "is_some", "is_none", "map", "and_then", "unwrap_or", "ok_or", "expect",
            ],
            false,
        ),
        BuiltinReceiver::Result => (
            &[
                "is_ok", "is_err", "map", "map_err", "and_then", "unwrap_or", "ok", "expect",
            ],
            false,
        ),
    };
    let name = known.iter().find(|candidate| **candidate == name)?;
    Some(BuiltinMethodInfo { name, mut_self })
}

fn builtin_receiver(ty: &Ty) -> Option<BuiltinReceiver> {
    match ty {
        Ty::List(_) => Some(BuiltinReceiver::List),
        Ty::Map(_, _) => Some(BuiltinReceiver::Map),
        Ty::Str => Some(BuiltinReceiver::Str),
        Ty::Int => Some(BuiltinReceiver::Int),
        Ty::Float => Some(BuiltinReceiver::Float),
        Ty::Range => Some(BuiltinReceiver::Range),
        Ty::Option(_) => Some(BuiltinReceiver::Option),
        Ty::Result(_, _) => Some(BuiltinReceiver::Result),
        _ => None,
    }
}
