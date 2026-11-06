
impl Checker {
    fn check_method_arg(
        &mut self,
        args: &[Arg],
        index: usize,
        expected: &Ty,
        infer: &mut Infer,
    ) -> Option<Ty> {
        let arg = args.get(index)?;
        if arg.mutable {
            let module = self.current_module();
            self.err_at(module, arg.span, Code::E402, "unexpected `mut` argument");
        }
        Some(self.check_expr(&arg.expr, Some(expected), infer))
    }

    fn check_builtin_method(
        &mut self,
        expr: &Expr,
        receiver: BuiltinReceiver,
        name: &str,
        receiver_ty: &Ty,
        args: &[Arg],
        expected: Option<&Ty>,
        infer: &mut Infer,
    ) -> Ty {
        let module = self.current_module();
        let arity = |checker: &mut Self, n: usize| {
            if args.len() != n {
                checker.err_at(
                    module,
                    expr.span,
                    Code::E204,
                    format!("method `{name}` takes {n} argument(s), found {}", args.len()),
                );
            }
        };
        let mut ret = Ty::Error;
        match receiver {
            BuiltinReceiver::List => {
                let Ty::List(inner) = receiver_ty else {
                    return Ty::Error;
                };
                let t = (**inner).clone();
                match name {
                    "len" => {
                        arity(self, 0);
                        ret = Ty::Int;
                    }
                    "get" | "insert" => {
                        arity(self, if name == "insert" { 2 } else { 1 });
                        if name == "insert" {
                            self.check_method_arg(args, 0, &Ty::Int, infer);
                            self.check_method_arg(args, 1, &t, infer);
                            ret = Ty::Unit;
                        } else {
                            self.check_method_arg(args, 0, &Ty::Int, infer);
                            ret = Ty::option(t);
                        }
                    }
                    "first" | "last" | "pop" => {
                        arity(self, 0);
                        ret = Ty::option(t);
                    }
                    "contains" => {
                        arity(self, 1);
                        self.check_method_arg(args, 0, &t, infer);
                        if !self.has_bound(&t, Bound::Eq) {
                            self.err_at(
                                module,
                                expr.span,
                                Code::E205,
                                format!("{} does not implement Eq", t.display(&self.types)),
                            );
                        }
                        ret = Ty::Bool;
                    }
                    "map" => {
                        arity(self, 1);
                        let u = infer.fresh();
                        self.check_method_arg(args, 0, &Ty::Fn(vec![t], Box::new(u.clone())), infer);
                        if infer.resolve(&u).has_var() {
                            self.err_at(
                                module,
                                expr.span,
                                Code::E202,
                                "cannot infer the result type of `map`; add an annotation",
                            );
                            ret = Ty::list(Ty::Error);
                        } else {
                            ret = Ty::list(infer.resolve(&u));
                        }
                    }
                    "filter" | "any" | "all" => {
                        arity(self, 1);
                        self.check_method_arg(
                            args,
                            0,
                            &Ty::Fn(vec![t.clone()], Box::new(Ty::Bool)),
                            infer,
                        );
                        ret = if name == "filter" { Ty::list(t) } else { Ty::Bool };
                    }
                    "fold" => {
                        arity(self, 2);
                        let u = infer.fresh();
                        self.check_method_arg(args, 0, &u, infer);
                        self.check_method_arg(
                            args,
                            1,
                            &Ty::Fn(vec![u.clone(), t.clone()], Box::new(u.clone())),
                            infer,
                        );
                        if infer.resolve(&u).has_var() {
                            self.err_at(
                                module,
                                expr.span,
                                Code::E202,
                                "cannot infer the accumulator type of `fold`; add an annotation",
                            );
                            ret = Ty::Error;
                        } else {
                            ret = infer.resolve(&u);
                        }
                    }
                    "enumerate" => {
                        arity(self, 0);
                        ret = Ty::list(Ty::Tuple(vec![Ty::Int, t]));
                    }
                    "reversed" => {
                        arity(self, 0);
                        ret = Ty::list(t);
                    }
                    "sorted" | "sort" => {
                        arity(self, 0);
                        if !self.has_bound(&t, Bound::Ord) {
                            self.err_at(
                                module,
                                expr.span,
                                Code::E205,
                                format!("{} does not implement Ord", t.display(&self.types)),
                            );
                        }
                        ret = if name == "sorted" { Ty::list(t) } else { Ty::Unit };
                    }
                    "sorted_by" => {
                        arity(self, 1);
                        let k = infer.fresh();
                        self.check_method_arg(
                            args,
                            0,
                            &Ty::Fn(vec![t.clone()], Box::new(k.clone())),
                            infer,
                        );
                        let key = infer.resolve(&k);
                        if key.has_var() {
                            self.err_at(
                                module,
                                expr.span,
                                Code::E202,
                                "cannot infer the sort key type; add an annotation",
                            );
                        } else if !self.has_bound(&key, Bound::Ord) {
                            self.err_at(
                                module,
                                expr.span,
                                Code::E205,
                                format!("{} does not implement Ord", key.display(&self.types)),
                            );
                        }
                        ret = Ty::list(t);
                    }
                    "join" => {
                        arity(self, 1);
                        if t != Ty::Str {
                            self.err_at(
                                module,
                                expr.span,
                                Code::E208,
                                "`join` is only available on list[str]",
                            );
                        }
                        self.check_method_arg(args, 0, &Ty::Str, infer);
                        ret = Ty::Str;
                    }
                    "push" => {
                        arity(self, 1);
                        self.check_method_arg(args, 0, &t, infer);
                        ret = Ty::Unit;
                    }
                    "remove_at" => {
                        arity(self, 1);
                        self.check_method_arg(args, 0, &Ty::Int, infer);
                        ret = t;
                    }
                    "extend" => {
                        arity(self, 1);
                        self.check_method_arg(args, 0, &Ty::list(t), infer);
                        ret = Ty::Unit;
                    }
                    "clear" => {
                        arity(self, 0);
                        ret = Ty::Unit;
                    }
                    _ => {
                        self.err_at(
                            module,
                            expr.span,
                            Code::E208,
                            format!("unknown method `{name}` for list"),
                        );
                    }
                }
            }
            BuiltinReceiver::Map => {
                let Ty::Map(key_ty, value_ty) = receiver_ty else {
                    return Ty::Error;
                };
                let k = (**key_ty).clone();
                let v = (**value_ty).clone();
                match name {
                    "len" => {
                        arity(self, 0);
                        ret = Ty::Int;
                    }
                    "get" | "has" | "remove" => {
                        arity(self, 1);
                        self.check_method_arg(args, 0, &k, infer);
                        ret = if name == "has" {
                            Ty::Bool
                        } else if name == "get" {
                            Ty::option(v)
                        } else {
                            Ty::option(v)
                        };
                    }
                    "keys" => {
                        arity(self, 0);
                        ret = Ty::list(k);
                    }
                    "values" => {
                        arity(self, 0);
                        ret = Ty::list(v);
                    }
                    "entries" => {
                        arity(self, 0);
                        ret = Ty::list(Ty::Tuple(vec![k, v]));
                    }
                    "clear" => {
                        arity(self, 0);
                        ret = Ty::Unit;
                    }
                    _ => {
                        self.err_at(
                            module,
                            expr.span,
                            Code::E208,
                            format!("unknown method `{name}` for map"),
                        );
                    }
                }
            }
            BuiltinReceiver::Str => match name {
                "len" => {
                    arity(self, 0);
                    ret = Ty::Int;
                }
                "chars" => {
                    arity(self, 0);
                    ret = Ty::list(Ty::Str);
                }
                "split" => {
                    arity(self, 1);
                    self.check_method_arg(args, 0, &Ty::Str, infer);
                    ret = Ty::list(Ty::Str);
                }
                "trim" | "to_upper" | "to_lower" => {
                    arity(self, 0);
                    ret = Ty::Str;
                }
                "contains" => {
                    arity(self, 1);
                    self.check_method_arg(args, 0, &Ty::Str, infer);
                    ret = Ty::Bool;
                }
                "find" => {
                    arity(self, 1);
                    self.check_method_arg(args, 0, &Ty::Str, infer);
                    ret = Ty::option(Ty::Int);
                }
                "starts_with" | "ends_with" => {
                    arity(self, 1);
                    self.check_method_arg(args, 0, &Ty::Str, infer);
                    ret = Ty::Bool;
                }
                "replace" => {
                    arity(self, 2);
                    self.check_method_arg(args, 0, &Ty::Str, infer);
                    self.check_method_arg(args, 1, &Ty::Str, infer);
                    ret = Ty::Str;
                }
                "parse_int" => {
                    arity(self, 0);
                    ret = Ty::option(Ty::Int);
                }
                "parse_float" => {
                    arity(self, 0);
                    ret = Ty::option(Ty::Float);
                }
                _ => {
                    self.err_at(
                        module,
                        expr.span,
                        Code::E208,
                        format!("unknown method `{name}` for str"),
                    );
                }
            },
            BuiltinReceiver::Int => match name {
                "abs" => {
                    arity(self, 0);
                    ret = Ty::Int;
                }
                "pow" => {
                    arity(self, 1);
                    self.check_method_arg(args, 0, &Ty::Int, infer);
                    ret = Ty::Int;
                }
                _ => {
                    self.err_at(
                        module,
                        expr.span,
                        Code::E208,
                        format!("unknown method `{name}` for int"),
                    );
                }
            },
            BuiltinReceiver::Float => match name {
                "abs" | "floor" | "ceil" | "trunc" | "sqrt" => {
                    arity(self, 0);
                    ret = Ty::Float;
                }
                "pow" => {
                    arity(self, 1);
                    self.check_method_arg(args, 0, &Ty::Float, infer);
                    ret = Ty::Float;
                }
                "round" => {
                    arity(self, 1);
                    self.check_method_arg(args, 0, &Ty::Int, infer);
                    ret = Ty::Float;
                }
                "is_nan" | "is_finite" => {
                    arity(self, 0);
                    ret = Ty::Bool;
                }
                _ => {
                    self.err_at(
                        module,
                        expr.span,
                        Code::E208,
                        format!("unknown method `{name}` for float"),
                    );
                }
            },
            BuiltinReceiver::Range => match name {
                "len" => {
                    arity(self, 0);
                    ret = Ty::Int;
                }
                "contains" => {
                    arity(self, 1);
                    self.check_method_arg(args, 0, &Ty::Int, infer);
                    ret = Ty::Bool;
                }
                "to_list" => {
                    arity(self, 0);
                    ret = Ty::list(Ty::Int);
                }
                _ => {
                    self.err_at(
                        module,
                        expr.span,
                        Code::E208,
                        format!("unknown method `{name}` for range"),
                    );
                }
            },
            BuiltinReceiver::Option => {
                let Ty::Option(inner) = receiver_ty else {
                    return Ty::Error;
                };
                let t = (**inner).clone();
                match name {
                    "is_some" | "is_none" => {
                        arity(self, 0);
                        ret = Ty::Bool;
                    }
                    "map" => {
                        arity(self, 1);
                        let u = infer.fresh();
                        self.check_method_arg(args, 0, &Ty::Fn(vec![t], Box::new(u.clone())), infer);
                        ret = Ty::option(infer.resolve(&u));
                    }
                    "and_then" => {
                        arity(self, 1);
                        let u = infer.fresh();
                        self.check_method_arg(
                            args,
                            0,
                            &Ty::Fn(vec![t], Box::new(Ty::option(u.clone()))),
                            infer,
                        );
                        ret = Ty::option(infer.resolve(&u));
                    }
                    "unwrap_or" => {
                        arity(self, 1);
                        self.check_method_arg(args, 0, &t, infer);
                        ret = t;
                    }
                    "ok_or" => {
                        arity(self, 1);
                        let e = infer.fresh();
                        self.check_method_arg(args, 0, &e, infer);
                        ret = Ty::Result(Box::new(t), Box::new(infer.resolve(&e)));
                    }
                    "expect" => {
                        arity(self, 1);
                        self.check_method_arg(args, 0, &Ty::Str, infer);
                        ret = t;
                    }
                    _ => {
                        self.err_at(
                            module,
                            expr.span,
                            Code::E208,
                            format!("unknown method `{name}` for Option"),
                        );
                    }
                }
            }
            BuiltinReceiver::Result => {
                let Ty::Result(ok_ty, err_ty) = receiver_ty else {
                    return Ty::Error;
                };
                let t = (**ok_ty).clone();
                let e = (**err_ty).clone();
                match name {
                    "is_ok" | "is_err" => {
                        arity(self, 0);
                        ret = Ty::Bool;
                    }
                    "map" => {
                        arity(self, 1);
                        let u = infer.fresh();
                        self.check_method_arg(args, 0, &Ty::Fn(vec![t], Box::new(u.clone())), infer);
                        ret = Ty::Result(Box::new(infer.resolve(&u)), Box::new(e));
                    }
                    "map_err" => {
                        arity(self, 1);
                        let f = infer.fresh();
                        self.check_method_arg(args, 0, &Ty::Fn(vec![e], Box::new(f.clone())), infer);
                        ret = Ty::Result(Box::new(t), Box::new(infer.resolve(&f)));
                    }
                    "and_then" => {
                        arity(self, 1);
                        let u = infer.fresh();
                        self.check_method_arg(
                            args,
                            0,
                            &Ty::Fn(
                                vec![t],
                                Box::new(Ty::Result(Box::new(u.clone()), Box::new(e.clone()))),
                            ),
                            infer,
                        );
                        ret = Ty::Result(Box::new(infer.resolve(&u)), Box::new(e));
                    }
                    "unwrap_or" => {
                        arity(self, 1);
                        self.check_method_arg(args, 0, &t, infer);
                        ret = t;
                    }
                    "ok" => {
                        arity(self, 0);
                        ret = Ty::option(t);
                    }
                    "expect" => {
                        arity(self, 1);
                        self.check_method_arg(args, 0, &Ty::Str, infer);
                        ret = t;
                    }
                    _ => {
                        self.err_at(
                            module,
                            expr.span,
                            Code::E208,
                            format!("unknown method `{name}` for Result"),
                        );
                    }
                }
            }
        }
        if let Some(exp) = expected {
            let _ = infer.unify(&ret, exp);
        }
        infer.resolve(&ret)
    }

    // ------------------------------------------------------------------
    // Patterns
    // ------------------------------------------------------------------

    fn check_pattern(
        &mut self,
        pat: &Pattern,
        ty: &Ty,
        mode: PatMode,
        mutable: bool,
        infer: &mut Infer,
    ) -> HashMap<String, (BindingId, Ty)> {
        let mut bindings = HashMap::new();
        self.check_pattern_with(pat, ty, mode, mutable, &mut bindings, infer);
        if mode != PatMode::Match && !self.is_irrefutable(pat, ty) {
            let module = self.current_module();
            self.err_at(
                module,
                pat.span,
                Code::E302,
                "this pattern is refutable; use `match` instead",
            );
        }
        bindings
    }

    fn check_pattern_with(
        &mut self,
        pat: &Pattern,
        ty: &Ty,
        mode: PatMode,
        mutable: bool,
        bindings: &mut HashMap<String, (BindingId, Ty)>,
        infer: &mut Infer,
    ) {
        let module = self.current_module();
        match &pat.kind {
            PatKind::Wild => {}
            PatKind::Bind(name) => {
                if let Some((id, existing_ty)) = bindings.get(name) {
                    if !infer.matches(existing_ty, ty) {
                        self.err_at(
                            module,
                            pat.span,
                            Code::E303,
                            format!("`{name}` is bound with conflicting types"),
                        );
                    }
                    self.pattern_bindings.insert(pat.id, *id);
                    return;
                }
                if self.is_visible_name(name) {
                    self.err_at(
                        module,
                        pat.span,
                        Code::E103,
                        format!("`{name}` shadows an existing binding"),
                    );
                }
                let depth = self.scopes.len().saturating_sub(1);
                let id = self.alloc_binding(name, ty.clone(), mutable, pat.id, false, depth);
                bindings.insert(name.clone(), (id, ty.clone()));
                self.pattern_bindings.insert(pat.id, id);
            }
            PatKind::Int(_) => {
                if !matches!(infer.resolve(ty), Ty::Int | Ty::Error) {
                    self.err_at(
                        module,
                        pat.span,
                        Code::E304,
                        format!("int pattern cannot match {}", ty.display(&self.types)),
                    );
                }
            }
            PatKind::Bool(_) => {
                if !matches!(infer.resolve(ty), Ty::Bool | Ty::Error) {
                    self.err_at(
                        module,
                        pat.span,
                        Code::E304,
                        format!("bool pattern cannot match {}", ty.display(&self.types)),
                    );
                }
            }
            PatKind::Str(_) => {
                if !matches!(infer.resolve(ty), Ty::Str | Ty::Error) {
                    self.err_at(
                        module,
                        pat.span,
                        Code::E304,
                        format!("str pattern cannot match {}", ty.display(&self.types)),
                    );
                }
            }
            PatKind::Tuple(items) => {
                let tys = match infer.resolve(ty) {
                    Ty::Tuple(tys) if tys.len() == items.len() => tys,
                    Ty::Error => vec![Ty::Error; items.len()],
                    other => {
                        self.err_at(
                            module,
                            pat.span,
                            Code::E304,
                            format!(
                                "tuple pattern cannot match {}",
                                other.display(&self.types)
                            ),
                        );
                        vec![Ty::Error; items.len()]
                    }
                };
                for (item, item_ty) in items.iter().zip(tys.iter()) {
                    self.check_pattern_with(
                        item,
                        item_ty,
                        mode,
                        mutable,
                        bindings,
                        infer,
                    );
                }
            }
            PatKind::Variant {
                qualifier,
                name,
                args,
            } => {
                let actual = infer.resolve(ty);
                let (payload, resolution, variant_name) = match &actual {
                    Ty::Option(inner) => {
                        if name == "Some" {
                            (
                                vec![(**inner).clone()],
                                PatResolution::Prelude(PreludeVariant::Some),
                                "Some",
                            )
                        } else if name == "None" {
                            (Vec::new(), PatResolution::Prelude(PreludeVariant::None), "None")
                        } else {
                            self.err_at(
                                module,
                                pat.span,
                                Code::E304,
                                format!("unknown Option variant `{name}`"),
                            );
                            (Vec::new(), PatResolution::Prelude(PreludeVariant::None), name.as_str())
                        }
                    }
                    Ty::Result(ok, err) => {
                        if name == "Ok" {
                            (
                                vec![(**ok).clone()],
                                PatResolution::Prelude(PreludeVariant::Ok),
                                "Ok",
                            )
                        } else if name == "Err" {
                            (
                                vec![(**err).clone()],
                                PatResolution::Prelude(PreludeVariant::Err),
                                "Err",
                            )
                        } else {
                            self.err_at(
                                module,
                                pat.span,
                                Code::E304,
                                format!("unknown Result variant `{name}`"),
                            );
                            (Vec::new(), PatResolution::Prelude(PreludeVariant::Err), name.as_str())
                        }
                    }
                    Ty::Named(id, type_args) => {
                        if let Some(qualifier) = qualifier {
                            let symbols = &self.resolved.module(module).symbols;
                            let expected_id = symbols
                                .types
                                .get(qualifier)
                                .and_then(|d| self.def_to_type.get(d).copied());
                            if expected_id != Some(*id) {
                                self.err_at(
                                    module,
                                    pat.span,
                                    Code::E304,
                                    format!("pattern qualifier `{qualifier}` does not match"),
                                );
                            }
                        }
                        if let Some(index) = self.types.variant_index(*id, name) {
                            let payload = self
                                .types
                                .variant(*id, index)
                                .map(|v| {
                                    v.payload
                                        .iter()
                                        .map(|t| instantiate_params(t, type_args))
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default();
                            (
                                payload,
                                PatResolution::Variant(*id, index),
                                name.as_str(),
                            )
                        } else {
                            self.err_at(
                                module,
                                pat.span,
                                Code::E304,
                                format!("unknown variant `{name}`"),
                            );
                            (Vec::new(), PatResolution::Variant(*id, 0), name.as_str())
                        }
                    }
                    Ty::Error => (vec![Ty::Error; args.len()], PatResolution::Prelude(PreludeVariant::None), name.as_str()),
                    other => {
                        self.err_at(
                            module,
                            pat.span,
                            Code::E304,
                            format!(
                                "variant pattern cannot match {}",
                                other.display(&self.types)
                            ),
                        );
                        (vec![Ty::Error; args.len()], PatResolution::Prelude(PreludeVariant::None), name.as_str())
                    }
                };
                let _ = variant_name;
                if args.len() != payload.len() {
                    self.err_at(
                        module,
                        pat.span,
                        Code::E304,
                        format!(
                            "variant `{name}` has {} payload value(s), pattern has {}",
                            payload.len(),
                            args.len()
                        ),
                    );
                }
                self.pattern_resolutions.insert(pat.id, resolution);
                for (arg, arg_ty) in args.iter().zip(payload.iter()) {
                    self.check_pattern_with(arg, arg_ty, mode, mutable, bindings, infer);
                }
            }
            PatKind::Struct { name, fields, rest } => {
                let actual = infer.resolve(ty);
                let (type_id, def_fields) = match &actual {
                    Ty::Named(id, type_args) => {
                        let def = self.types.get(*id).cloned();
                        match def {
                            Some(d) => match &d.kind {
                                TypeDefKind::Struct { fields } => (
                                    *id,
                                    fields
                                        .iter()
                                        .map(|f| {
                                            (f.name.clone(), instantiate_params(&f.ty, type_args))
                                        })
                                        .collect::<Vec<_>>(),
                                ),
                                _ => {
                                    self.err_at(
                                        module,
                                        pat.span,
                                        Code::E304,
                                        "struct pattern used on a non-struct type",
                                    );
                                    (*id, Vec::new())
                                }
                            },
                            None => (*id, Vec::new()),
                        }
                    }
                    Ty::Error => (TypeId(0), Vec::new()),
                    other => {
                        self.err_at(
                            module,
                            pat.span,
                            Code::E304,
                            format!(
                                "struct pattern cannot match {}",
                                other.display(&self.types)
                            ),
                        );
                        (TypeId(0), Vec::new())
                    }
                };
                if type_id != TypeId(0) {
                    let symbols = &self.resolved.module(module).symbols;
                    if let Some(def) = symbols.types.get(name) {
                        if self.def_to_type.get(def).copied() != Some(type_id) {
                            self.err_at(
                                module,
                                pat.span,
                                Code::E304,
                                format!("pattern type `{name}` does not match the scrutinee"),
                            );
                        }
                    }
                    self.pattern_resolutions
                        .insert(pat.id, PatResolution::Struct(type_id));
                }
                let mut named = HashSet::new();
                let mut slots = Vec::with_capacity(fields.len());
                for (source, field) in fields.iter().enumerate() {
                    if !named.insert(field.name.clone()) {
                        self.err_at(
                            module,
                            field.span,
                            Code::E204,
                            format!("duplicate field pattern `{}`", field.name),
                        );
                        continue;
                    }
                    if let Some(decl) = def_fields.iter().position(|(n, _)| n == &field.name) {
                        slots.push((source, decl));
                        self.check_pattern_with(
                            &field.pattern,
                            &def_fields[decl].1,
                            mode,
                            mutable,
                            bindings,
                            infer,
                        );
                    } else {
                        self.err_at(
                            module,
                            field.span,
                            Code::E204,
                            format!("unknown field `{}`", field.name),
                        );
                    }
                }
                if type_id != TypeId(0) {
                    self.struct_pattern_fields.insert(pat.id, slots);
                }
                if !rest {
                    for (field_name, _) in &def_fields {
                        if !named.contains(field_name) {
                            self.err_at(
                                module,
                                pat.span,
                                Code::E204,
                                format!("missing field pattern `{field_name}`; add `..`"),
                            );
                        }
                    }
                }
            }
            PatKind::Or(alternatives) => {
                let Some(first) = alternatives.first() else { return };
                let first_bindings = self.collect_pattern_bindings(first, ty);
                for (name, bind_ty) in &first_bindings {
                    if !bindings.contains_key(name) {
                        let depth = self.scopes.len().saturating_sub(1);
                        let id =
                            self.alloc_binding(name, bind_ty.clone(), mutable, pat.id, false, depth);
                        bindings.insert(name.clone(), (id, bind_ty.clone()));
                    }
                }
                let base_names: HashSet<String> = bindings.keys().cloned().collect();
                for alternative in alternatives {
                    let mut alternative_bindings = bindings.clone();
                    self.check_pattern_with(
                        alternative,
                        ty,
                        mode,
                        mutable,
                        &mut alternative_bindings,
                        infer,
                    );
                    let names: HashSet<String> = alternative_bindings.keys().cloned().collect();
                    if names != base_names {
                        self.err_at(
                            module,
                            alternative.span,
                            Code::E303,
                            "or-pattern alternatives must bind the same names",
                        );
                    }
                    for (name, (_, ty)) in &alternative_bindings {
                        if let Some((_, expected)) = bindings.get(name) {
                            if !infer.matches(ty, expected) {
                                self.err_at(
                                    module,
                                    alternative.span,
                                    Code::E303,
                                    format!("`{name}` is bound with different types"),
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    fn collect_pattern_bindings(&self, pat: &Pattern, ty: &Ty) -> Vec<(String, Ty)> {
        let mut out = Vec::new();
        self.collect_pattern_bindings_into(pat, ty, &mut out);
        out
    }

    fn collect_pattern_bindings_into(
        &self,
        pat: &Pattern,
        ty: &Ty,
        out: &mut Vec<(String, Ty)>,
    ) {
        match &pat.kind {
            PatKind::Bind(name) => out.push((name.clone(), ty.clone())),
            PatKind::Or(alternatives) => {
                if let Some(first) = alternatives.first() {
                    self.collect_pattern_bindings_into(first, ty, out);
                }
            }
            PatKind::Tuple(items) => {
                let tys = match ty {
                    Ty::Tuple(tys) => tys.clone(),
                    _ => vec![Ty::Error; items.len()],
                };
                for (item, item_ty) in items.iter().zip(tys.iter()) {
                    self.collect_pattern_bindings_into(item, item_ty, out);
                }
            }
            PatKind::Variant { name, args, .. } => {
                let payload: Vec<Ty> = match ty {
                    Ty::Option(inner) => {
                        if name == "Some" {
                            vec![(**inner).clone()]
                        } else {
                            Vec::new()
                        }
                    }
                    Ty::Result(ok, err) => {
                        if name == "Ok" {
                            vec![(**ok).clone()]
                        } else {
                            vec![(**err).clone()]
                        }
                    }
                    Ty::Named(id, args) => self
                        .types
                        .variant_index(*id, name)
                        .and_then(|index| self.types.variant(*id, index))
                        .map(|v| {
                            v.payload
                                .iter()
                                .map(|t| instantiate_params(t, args))
                                .collect()
                        })
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };
                for (arg, arg_ty) in args.iter().zip(payload.iter()) {
                    self.collect_pattern_bindings_into(arg, arg_ty, out);
                }
            }
            PatKind::Struct { fields, .. } => {
                let def_fields: Vec<(String, Ty)> = match ty {
                    Ty::Named(id, args) => match self.types.get(*id).map(|d| &d.kind) {
                        Some(TypeDefKind::Struct { fields }) => fields
                            .iter()
                            .map(|f| (f.name.clone(), instantiate_params(&f.ty, args)))
                            .collect(),
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                };
                for field in fields {
                    if let Some((_, field_ty)) =
                        def_fields.iter().find(|(n, _)| n == &field.name)
                    {
                        self.collect_pattern_bindings_into(&field.pattern, field_ty, out);
                    }
                }
            }
            PatKind::Wild | PatKind::Int(_) | PatKind::Bool(_) | PatKind::Str(_) => {}
        }
    }

    fn is_irrefutable(&self, pat: &Pattern, ty: &Ty) -> bool {
        match &pat.kind {
            PatKind::Wild | PatKind::Bind(_) => true,
            PatKind::Tuple(items) => match ty {
                Ty::Tuple(tys) => items
                    .iter()
                    .zip(tys)
                    .all(|(p, t)| self.is_irrefutable(p, t)),
                _ => false,
            },
            PatKind::Struct { fields, rest, .. } => {
                if let Ty::Named(id, args) = ty {
                    if let Some(TypeDefKind::Struct { fields: def_fields }) =
                        self.types.get(*id).map(|d| &d.kind)
                    {
                        if !rest && fields.len() != def_fields.len() {
                            return false;
                        }
                        return fields.iter().all(|field| {
                            def_fields
                                .iter()
                                .find(|f| f.name == field.name)
                                .map(|f| {
                                    self.is_irrefutable(
                                        &field.pattern,
                                        &instantiate_params(&f.ty, args),
                                    )
                                })
                                .unwrap_or(false)
                        });
                    }
                }
                false
            }
            PatKind::Variant {
                name,
                args: payload,
                ..
            } => {
                let variant_count = match ty {
                    Ty::Option(_) | Ty::Result(_, _) => 2,
                    Ty::Named(id, _) => match self.types.get(*id).map(|d| &d.kind) {
                        Some(TypeDefKind::Enum { variants }) => variants.len(),
                        _ => return false,
                    },
                    _ => return false,
                };
                if variant_count != 1 {
                    return false;
                }
                let payload_tys: Vec<Ty> = match ty {
                    Ty::Named(id, args) => self
                        .types
                        .variant_index(*id, name)
                        .and_then(|index| self.types.variant(*id, index))
                        .map(|v| {
                            v.payload
                                .iter()
                                .map(|t| instantiate_params(t, args))
                                .collect()
                        })
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };
                payload
                    .iter()
                    .zip(payload_ty(&payload_tys))
                    .all(|(p, t)| self.is_irrefutable(p, &t))
            }
            PatKind::Or(alternatives) => alternatives
                .iter()
                .all(|alt| self.is_irrefutable(alt, ty)),
            PatKind::Int(_) | PatKind::Bool(_) | PatKind::Str(_) => false,
        }
    }

    // ------------------------------------------------------------------
    // Bounds
    // ------------------------------------------------------------------

    fn has_bound(&self, ty: &Ty, bound: Bound) -> bool {
        let params = self
            .ctx
            .as_ref()
            .map(|c| c.type_params.clone())
            .unwrap_or_default();
        has_bound_ty(ty, bound, &params, &self.types, &mut HashSet::new())
    }

    fn map_key_ok(&self, ty: &Ty) -> bool {
        map_key_ok_ty(ty, &self.types, &mut HashSet::new())
    }

    fn is_data_type(&self, ty: &Ty) -> bool {
        is_data_type(ty, &self.types)
    }
}

fn payload_ty(payload: &[Ty]) -> Vec<Ty> {
    payload.to_vec()
}

fn has_bound_ty(
    ty: &Ty,
    bound: Bound,
    params: &[TypeParam],
    types: &TypeTable,
    visited: &mut HashSet<TypeId>,
) -> bool {
    if let Ty::Param(i) = ty {
        return params
            .get(*i as usize)
            .is_some_and(|p| p.bounds.contains(&bound));
    }
    match bound {
        Bound::Eq | Bound::Show => is_data_type_visit(ty, types, visited),
        Bound::Ord => is_ord_visit(ty, types, visited),
    }
}

fn is_data_type_visit(ty: &Ty, types: &TypeTable, visited: &mut HashSet<TypeId>) -> bool {
    match ty {
        Ty::Fn(_, _) => false,
        Ty::Tuple(items) => items.iter().all(|t| is_data_type_visit(t, types, visited)),
        Ty::List(t) | Ty::Option(t) => is_data_type_visit(t, types, visited),
        Ty::Map(k, v) | Ty::Result(k, v) => {
            is_data_type_visit(k, types, visited) && is_data_type_visit(v, types, visited)
        }
        Ty::Named(id, args) => {
            if !visited.insert(*id) {
                return true;
            }
            let result = match types.get(*id).map(|d| &d.kind) {
                Some(TypeDefKind::Opaque) | None => true,
                Some(TypeDefKind::Struct { fields }) => fields
                    .iter()
                    .all(|f| is_data_type_visit(&instantiate_params(&f.ty, args), types, visited)),
                Some(TypeDefKind::Enum { variants }) => variants.iter().all(|v| {
                    v.payload
                        .iter()
                        .all(|t| is_data_type_visit(&instantiate_params(t, args), types, visited))
                }),
            };
            visited.remove(id);
            result
        }
        Ty::Param(_) | Ty::Var(_) | Ty::Error => true,
        _ => true,
    }
}

fn is_ord_visit(ty: &Ty, types: &TypeTable, visited: &mut HashSet<TypeId>) -> bool {
    match ty {
        Ty::Bool | Ty::Int | Ty::Float | Ty::Str => true,
        Ty::Tuple(items) => items.iter().all(|t| is_ord_visit(t, types, visited)),
        Ty::List(t) | Ty::Option(t) => is_ord_visit(t, types, visited),
        Ty::Result(a, b) => is_ord_visit(a, types, visited) && is_ord_visit(b, types, visited),
        Ty::Named(id, args) => {
            if !visited.insert(*id) {
                return true;
            }
            let result = match types.get(*id).map(|d| &d.kind) {
                Some(TypeDefKind::Opaque) | None => true,
                Some(TypeDefKind::Struct { fields }) => fields
                    .iter()
                    .all(|f| is_ord_visit(&instantiate_params(&f.ty, args), types, visited)),
                Some(TypeDefKind::Enum { variants }) => variants.iter().all(|v| {
                    v.payload
                        .iter()
                        .all(|t| is_ord_visit(&instantiate_params(t, args), types, visited))
                }),
            };
            visited.remove(id);
            result
        }
        Ty::Param(_) | Ty::Var(_) | Ty::Error => true,
        Ty::Unit | Ty::Range | Ty::Never | Ty::Map(_, _) | Ty::Fn(_, _) => false,
    }
}

fn map_key_ok_ty(ty: &Ty, types: &TypeTable, visited: &mut HashSet<TypeId>) -> bool {
    match ty {
        Ty::Int | Ty::Str | Ty::Bool => true,
        Ty::Tuple(items) => items.iter().all(|t| map_key_ok_ty(t, types, visited)),
        Ty::Option(t) => map_key_ok_ty(t, types, visited),
        Ty::Result(a, b) => {
            map_key_ok_ty(a, types, visited) && map_key_ok_ty(b, types, visited)
        }
        Ty::Named(id, args) => {
            if !visited.insert(*id) {
                return true;
            }
            let result = match types.get(*id).map(|d| &d.kind) {
                Some(TypeDefKind::Opaque) | None => true,
                Some(TypeDefKind::Struct { fields }) => fields
                    .iter()
                    .all(|f| map_key_ok_ty(&instantiate_params(&f.ty, args), types, visited)),
                Some(TypeDefKind::Enum { variants }) => variants.iter().all(|v| {
                    v.payload
                        .iter()
                        .all(|t| map_key_ok_ty(&instantiate_params(t, args), types, visited))
                }),
            };
            visited.remove(id);
            result
        }
        Ty::Param(_) | Ty::Var(_) | Ty::Error => true,
        _ => false,
    }
}
