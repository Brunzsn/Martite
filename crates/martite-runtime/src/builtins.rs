/// True when evaluating `expr` can call user or host code. Calls are the only
/// way an argument can mutate the receiver binding, so call-free arguments let
/// a mutable builtin use the direct root path without changing evaluation
/// order.
fn expr_may_call(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Unit
        | ExprKind::Var(_)
        | ExprKind::Lambda { .. } => false,
        ExprKind::Str(parts) => parts.iter().any(|part| match part {
            StrExprPart::Text(_) => false,
            StrExprPart::Expr(expr) => expr_may_call(expr),
        }),
        ExprKind::Tuple(items) | ExprKind::List(items) => items.iter().any(expr_may_call),
        ExprKind::Map(entries) => entries
            .iter()
            .any(|(key, value)| expr_may_call(key) || expr_may_call(value)),
        ExprKind::StructLit { fields, base, .. } => {
            base.as_deref().is_some_and(expr_may_call)
                || fields
                    .iter()
                    .any(|field| field.value.as_ref().is_some_and(expr_may_call))
        }
        ExprKind::Unary { expr, .. } | ExprKind::Try(expr) | ExprKind::Convert { expr, .. } => {
            expr_may_call(expr)
        }
        ExprKind::Binary { lhs, rhs, .. } => expr_may_call(lhs) || expr_may_call(rhs),
        ExprKind::Range { start, end, .. } => expr_may_call(start) || expr_may_call(end),
        ExprKind::Field { base, .. } => expr_may_call(base),
        ExprKind::Index { base, index } => expr_may_call(base) || expr_may_call(index),
        ExprKind::If { .. }
        | ExprKind::Match { .. }
        | ExprKind::Block(_)
        | ExprKind::Call { .. } => true,
    }
}

impl<'p, 'h> Runtime<'p, 'h> {
    fn call_builtin_method(
        &mut self,
        expr: &Expr,
        receiver: BuiltinReceiver,
        name: &str,
        base: &Expr,
        args: &[martite_syntax::Arg],
    ) -> EvalResult<Value> {
        let mut_self = match receiver {
            BuiltinReceiver::List => matches!(
                name,
                "push" | "pop" | "sort" | "insert" | "remove_at" | "extend" | "clear"
            ),
            BuiltinReceiver::Map => matches!(name, "remove" | "clear"),
            _ => false,
        };
        if !mut_self {
            let mut value = self.eval_expr(base)?;
            let mut values = CallArgs::with_capacity(args.len());
            for arg in args {
                if arg.mutable {
                    return self.trap(self.internal_panic("unexpected mut argument"));
                }
                values.push(self.eval_expr(&arg.expr)?);
            }
            let result =
                self.apply_builtin_method(receiver, name, &mut value, values, expr.span)?;
            self.check_value_depth(&result, expr.span)?;
            self.note_value(&result);
            return Ok(result);
        }

        let place = self.eval_place(base)?;
        let direct_root = place.accessors.is_empty()
            && args
                .iter()
                .all(|arg| !arg.mutable && !expr_may_call(&arg.expr));
        let snapshot = if direct_root {
            if self.local_value(place.root).is_none() {
                return self.trap(self.internal_panic("unbound place root"));
            }
            None
        } else {
            Some(self.read_place(&place)?)
        };
        let mut values = CallArgs::with_capacity(args.len());
        for arg in args {
            if arg.mutable {
                return self.trap(self.internal_panic("unexpected mut argument"));
            }
            values.push(self.eval_expr(&arg.expr)?);
        }
        self.check_builtin_mutation_depth(name, &values, place.accessors.len())?;

        let mut root = self.take_place_root(&place)?;
        let result = if let Some(mut value) = snapshot {
            if place.accessors.is_empty() {
                drop(root);
                let result =
                    self.apply_builtin_method(receiver, name, &mut value, values, expr.span)?;
                self.current_locals_mut().insert(place.root, value);
                result
            } else {
                let replaced = self.take_path_value(&mut root, &place.accessors)?;
                drop(replaced);
                let result =
                    self.apply_builtin_method(receiver, name, &mut value, values, expr.span)?;
                self.set_path(&mut root, &place.accessors, value, MAX_VALUE_DEPTH)?;
                self.current_locals_mut().insert(place.root, root);
                result
            }
        } else {
            let mut value = root;
            let result =
                self.apply_builtin_method(receiver, name, &mut value, values, expr.span)?;
            self.current_locals_mut().insert(place.root, value);
            result
        };
        self.check_value_depth(&result, expr.span)?;
        self.note_value(&result);
        Ok(result)
    }

    fn apply_builtin_method(
        &mut self,
        receiver: BuiltinReceiver,
        name: &str,
        receiver_value: &mut Value,
        args: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        match receiver {
            BuiltinReceiver::List => self.list_method(name, receiver_value, args, span),
            BuiltinReceiver::Map => self.map_method(name, receiver_value, args, span),
            BuiltinReceiver::Str => self.str_method(name, receiver_value, args, span),
            BuiltinReceiver::Int => self.int_method(name, receiver_value, args, span),
            BuiltinReceiver::Float => self.float_method(name, receiver_value, args, span),
            BuiltinReceiver::Range => self.range_method(name, receiver_value, args, span),
            BuiltinReceiver::Option => self.option_method(name, receiver_value, args, span),
            BuiltinReceiver::Result => self.result_method(name, receiver_value, args, span),
        }
    }

    fn list_method(
        &mut self,
        name: &str,
        receiver: &mut Value,
        args: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        let Value::List(items) = receiver else {
            return self.trap(self.internal_panic("receiver is not a list"));
        };
        match name {
            "len" => Ok(Value::Int(items.len() as i64)),
            "get" => {
                let index = int_arg(&args, 0);
                Ok(match index {
                    Some(i) if i >= 0 => items
                        .get(i as usize)
                        .cloned()
                        .map(|v| Value::Option(Some(Box::new(v))))
                        .unwrap_or(Value::Option(None)),
                    _ => Value::Option(None),
                })
            }
            "first" => Ok(items
                .first()
                .cloned()
                .map(|v| Value::Option(Some(Box::new(v))))
                .unwrap_or(Value::Option(None))),
            "last" => Ok(items
                .last()
                .cloned()
                .map(|v| Value::Option(Some(Box::new(v))))
                .unwrap_or(Value::Option(None))),
            "contains" => {
                let needle = args.into_iter().next().unwrap_or(Value::Unit);
                self.charge(fuel::slot_cost(items.len() as u64))?;
                Ok(Value::Bool(items.iter().any(|v| value_eq(v, &needle))))
            }
            "map" => {
                let closure = self.closure_arg(&args, 0)?;
                let snapshot = items.clone();
                self.charge(fuel::slot_cost(snapshot.len() as u64))?;
                let mut out = Vec::with_capacity(snapshot.len());
                for item in snapshot.iter() {
                    out.push(
                        self.call_closure(closure, smallvec![item.clone()])
                            .map_err(EvalError::Trap)?,
                    );
                }
                Ok(Value::list(out))
            }
            "filter" => {
                let closure = self.closure_arg(&args, 0)?;
                let snapshot = items.clone();
                self.charge(fuel::slot_cost(snapshot.len() as u64))?;
                let mut out = Vec::new();
                for item in snapshot.iter() {
                    let keep = self
                        .call_closure(closure, smallvec![item.clone()])
                        .map_err(EvalError::Trap)?;
                    if matches!(keep, Value::Bool(true)) {
                        out.push(item.clone());
                    }
                }
                Ok(Value::list(out))
            }
            "fold" => {
                let mut args = args;
                let mut init = if args.is_empty() {
                    Value::Unit
                } else {
                    args.remove(0)
                };
                let closure = self.closure_arg(&args, 0)?;
                let snapshot = items.clone();
                self.charge(fuel::slot_cost(snapshot.len() as u64))?;
                for item in snapshot.iter() {
                    init = self
                        .call_closure(closure, smallvec![init, item.clone()])
                        .map_err(EvalError::Trap)?;
                }
                Ok(init)
            }
            "any" => {
                let closure = self.closure_arg(&args, 0)?;
                let snapshot = items.clone();
                for item in snapshot.iter() {
                    self.charge(fuel::ITERATION_COST)?;
                    if matches!(
                        self.call_closure(closure, smallvec![item.clone()])
                            .map_err(EvalError::Trap)?,
                        Value::Bool(true)
                    ) {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            "all" => {
                let closure = self.closure_arg(&args, 0)?;
                let snapshot = items.clone();
                for item in snapshot.iter() {
                    self.charge(fuel::ITERATION_COST)?;
                    if !matches!(
                        self.call_closure(closure, smallvec![item.clone()])
                            .map_err(EvalError::Trap)?,
                        Value::Bool(true)
                    ) {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }
            "enumerate" => {
                let out: Vec<Value> = items
                    .iter()
                    .enumerate()
                    .map(|(i, v)| Value::tuple(vec![Value::Int(i as i64), v.clone()]))
                    .collect();
                self.charge(fuel::slot_cost(out.len() as u64))?;
                Ok(Value::list(out))
            }
            "reversed" => {
                let mut out: Vec<Value> = items.iter().cloned().collect();
                out.reverse();
                self.charge(fuel::slot_cost(out.len() as u64))?;
                Ok(Value::list(out))
            }
            "sorted" => {
                let mut out: Vec<Value> = items.iter().cloned().collect();
                self.charge(fuel::sort_cost(out.len() as u64))?;
                out.sort_by(value_ord);
                self.ensure_deadline()?;
                Ok(Value::list(out))
            }
            "sorted_by" => {
                let closure = self.closure_arg(&args, 0)?;
                let snapshot = items.clone();
                self.charge(fuel::sort_cost(snapshot.len() as u64))?;
                let mut keyed = Vec::with_capacity(snapshot.len());
                for item in snapshot.iter() {
                    let key = self
                        .call_closure(closure, smallvec![item.clone()])
                        .map_err(EvalError::Trap)?;
                    keyed.push((key, item.clone()));
                }
                keyed.sort_by(|a, b| value_ord(&a.0, &b.0));
                self.ensure_deadline()?;
                Ok(Value::list(keyed.into_iter().map(|(_, v)| v).collect()))
            }
            "join" => {
                let sep = args.first().and_then(|v| v.as_str()).unwrap_or_default();
                let mut out = String::new();
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(sep);
                    }
                    if let Some(s) = item.as_str() {
                        out.push_str(s);
                    }
                }
                self.charge(fuel::slot_cost(items.len() as u64))?;
                self.charge(fuel::string_cost(out.len() as u64))?;
                Ok(Value::str(out))
            }
            "push" => {
                let value = args.into_iter().next().unwrap_or(Value::Unit);
                let n = items.len();
                self.charge(fuel::SLOT_COST)?;
                self.note_alloc(8);
                let list = self.cow_vec_mut(items, n)?;
                list.push(value);
                Ok(Value::Unit)
            }
            "pop" => {
                let n = items.len();
                self.charge(fuel::SLOT_COST)?;
                let list = self.cow_vec_mut(items, n)?;
                Ok(list
                    .pop()
                    .map(|v| Value::Option(Some(Box::new(v))))
                    .unwrap_or(Value::Option(None)))
            }
            "sort" => {
                let n = items.len();
                self.charge(fuel::sort_cost(n as u64))?;
                let list = self.cow_vec_mut(items, n)?;
                list.sort_by(value_ord);
                self.ensure_deadline()?;
                Ok(Value::Unit)
            }
            "insert" => {
                let index = int_arg(&args, 0).unwrap_or(0);
                let value = args.into_iter().nth(1).unwrap_or(Value::Unit);
                let n = items.len();
                if index < 0 || index as usize > n {
                    return self.trap(self.panic("list insert index out of bounds", span));
                }
                let index = index as usize;
                self.charge(fuel::list_insert_cost(n as u64, index as u64))?;
                self.note_alloc(8);
                let list = self.cow_vec_mut(items, n)?;
                list.insert(index, value);
                Ok(Value::Unit)
            }
            "remove_at" => {
                let index = int_arg(&args, 0).unwrap_or(0);
                let n = items.len();
                if index < 0 || index as usize >= n {
                    return self.trap(self.panic("list index out of bounds", span));
                }
                let index = index as usize;
                self.charge(fuel::list_remove_cost(n as u64, index as u64))?;
                let list = self.cow_vec_mut(items, n)?;
                Ok(list.remove(index))
            }
            "extend" => {
                let other = args.into_iter().next().unwrap_or(Value::Unit);
                if let Value::List(other) = other {
                    let n = items.len();
                    self.charge(fuel::slot_cost(other.len() as u64))?;
                    self.note_alloc(8 * other.len() as u64);
                    let list = self.cow_vec_mut(items, n)?;
                    list.extend_values(other.iter().cloned());
                }
                Ok(Value::Unit)
            }
            "clear" => {
                self.charge(fuel::slot_cost(items.len() as u64))?;
                if Rc::strong_count(items) == 1 {
                    Rc::make_mut(items).clear();
                } else {
                    self.note_alloc(16);
                    *items = Rc::new(SeqValue::new(Vec::new()));
                }
                Ok(Value::Unit)
            }
            _ => self.trap(self.internal_panic(format!("unknown list method `{name}`"))),
        }
    }

    fn map_method(
        &mut self,
        name: &str,
        receiver: &mut Value,
        args: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        let Value::Map(map) = receiver else {
            return self.trap(self.internal_panic("receiver is not a map"));
        };
        match name {
            "len" => Ok(Value::Int(map.len() as i64)),
            "get" => {
                let key = args.first().cloned().unwrap_or(Value::Unit);
                let key = key_from_value(&key)
                    .map_err(|msg| EvalError::Trap(self.panic(msg, span)))?;
                Ok(map
                    .get(&key)
                    .cloned()
                    .map(|v| Value::Option(Some(Box::new(v))))
                    .unwrap_or(Value::Option(None)))
            }
            "has" => {
                let key = args.first().cloned().unwrap_or(Value::Unit);
                let key = key_from_value(&key)
                    .map_err(|msg| EvalError::Trap(self.panic(msg, span)))?;
                Ok(Value::Bool(map.get(&key).is_some()))
            }
            "keys" => {
                self.charge(fuel::slot_cost(map.len() as u64))?;
                Ok(Value::list(
                    map.keys().iter().map(key_to_value).collect(),
                ))
            }
            "values" => {
                self.charge(fuel::slot_cost(map.len() as u64))?;
                Ok(Value::list(map.values()))
            }
            "entries" => {
                self.charge(fuel::map_cost(map.len() as u64))?;
                Ok(Value::list(
                    map.entries()
                        .iter()
                        .map(|(k, v)| Value::tuple(vec![key_to_value(k), v.clone()]))
                        .collect(),
                ))
            }
            "remove" => {
                let key = args.first().cloned().unwrap_or(Value::Unit);
                let key = key_from_value(&key)
                    .map_err(|msg| EvalError::Trap(self.panic(msg, span)))?;
                if map.get(&key).is_none() {
                    return Ok(Value::Option(None));
                }
                let n = map.len();
                let slots = n.saturating_mul(2);
                let map = self.cow_map_mut(map, slots)?;
                let (index, removed) = map
                    .remove_with_index(&key)
                    .expect("key was checked before mutation");
                self.charge(fuel::map_remove_cost(n as u64, index as u64))?;
                Ok(Value::Option(Some(Box::new(removed))))
            }
            "clear" => {
                let n = map.len();
                self.charge(fuel::map_cost(n as u64))?;
                if Rc::strong_count(map) == 1 {
                    Rc::make_mut(map).clear();
                } else {
                    self.note_alloc(16);
                    *map = Rc::new(MapValue::new(OrderedMap::new()));
                }
                Ok(Value::Unit)
            }
            _ => self.trap(self.internal_panic(format!("unknown map method `{name}`"))),
        }
    }

    fn str_method(
        &mut self,
        name: &str,
        receiver: &mut Value,
        args: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        let Value::Str(text) = receiver else {
            return self.trap(self.internal_panic("receiver is not a str"));
        };
        match name {
            "len" => {
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(Value::Int(text.chars().count() as i64))
            }
            "chars" => {
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(Value::list(
                    text.chars()
                        .map(|c| Value::str(c.to_string()))
                        .collect(),
                ))
            }
            "split" => {
                let sep = args.first().and_then(|v| v.as_str()).unwrap_or_default();
                let parts: Vec<Value> = if sep.is_empty() {
                    text.chars().map(|c| Value::str(c.to_string())).collect()
                } else {
                    text.split(sep).map(|part| Value::Str(Rc::from(part))).collect()
                };
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(Value::list(parts))
            }
            "trim" => {
                let out = text.trim();
                self.charge(fuel::string_cost(out.len() as u64))?;
                Ok(Value::Str(Rc::from(out)))
            }
            "contains" => {
                let needle = args.first().and_then(|v| v.as_str()).unwrap_or_default();
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(Value::Bool(text.contains(needle)))
            }
            "find" => {
                let needle = args.first().and_then(|v| v.as_str()).unwrap_or_default();
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(text
                    .find(needle)
                    .map(|byte| Value::Int(text[..byte].chars().count() as i64))
                    .map(|v| Value::Option(Some(Box::new(v))))
                    .unwrap_or(Value::Option(None)))
            }
            "starts_with" => {
                let needle = args.first().and_then(|v| v.as_str()).unwrap_or_default();
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(Value::Bool(text.starts_with(needle)))
            }
            "ends_with" => {
                let needle = args.first().and_then(|v| v.as_str()).unwrap_or_default();
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(Value::Bool(text.ends_with(needle)))
            }
            "replace" => {
                let from = args.first().and_then(|v| v.as_str()).unwrap_or_default();
                let to = args.get(1).and_then(|v| v.as_str()).unwrap_or_default();
                if from.is_empty() {
                    return self.trap(self.panic("replace with an empty `from`", span));
                }
                let out = text.replace(from, to);
                self.charge(fuel::string_cost(out.len() as u64))?;
                Ok(Value::str(out))
            }
            "to_upper" => {
                let out = text.to_uppercase();
                self.charge(fuel::string_cost(out.len() as u64))?;
                Ok(Value::str(out))
            }
            "to_lower" => {
                let out = text.to_lowercase();
                self.charge(fuel::string_cost(out.len() as u64))?;
                Ok(Value::str(out))
            }
            "parse_int" => {
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(match parse_int(text) {
                    Some(v) => Value::Option(Some(Box::new(Value::Int(v)))),
                    None => Value::Option(None),
                })
            }
            "parse_float" => {
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(match parse_float(text) {
                    Some(v) => Value::Option(Some(Box::new(Value::Float(v)))),
                    None => Value::Option(None),
                })
            }
            _ => self.trap(self.internal_panic(format!("unknown str method `{name}`"))),
        }
    }

    fn int_method(
        &mut self,
        name: &str,
        receiver: &mut Value,
        args: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        let Value::Int(value) = receiver else {
            return self.trap(self.internal_panic("receiver is not an int"));
        };
        match name {
            "abs" => value
                .checked_abs()
                .map(Value::Int)
                .ok_or_else(|| EvalError::Trap(self.panic("integer overflow", span))),
            "pow" => {
                let exponent = int_arg(&args, 0).unwrap_or(0);
                if exponent < 0 {
                    return self.trap(self.panic("negative exponent", span));
                }
                if exponent == 0 {
                    return Ok(Value::Int(1));
                }
                match *value {
                    0 => Ok(Value::Int(0)),
                    1 => Ok(Value::Int(1)),
                    -1 if exponent % 2 == 0 => Ok(Value::Int(1)),
                    -1 => Ok(Value::Int(-1)),
                    _ => {
                        let exponent = u32::try_from(exponent).map_err(|_| {
                            EvalError::Trap(self.panic("integer overflow", span))
                        })?;
                        value
                            .checked_pow(exponent)
                            .map(Value::Int)
                            .ok_or_else(|| EvalError::Trap(self.panic("integer overflow", span)))
                    }
                }
            }
            _ => self.trap(self.internal_panic(format!("unknown int method `{name}`"))),
        }
    }

    fn float_method(
        &mut self,
        name: &str,
        receiver: &mut Value,
        args: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        let Value::Float(value) = receiver else {
            return self.trap(self.internal_panic("receiver is not a float"));
        };
        match name {
            "abs" => Ok(Value::Float(libm::fabs(*value))),
            "floor" => Ok(Value::Float(libm::floor(*value))),
            "ceil" => Ok(Value::Float(libm::ceil(*value))),
            "trunc" => Ok(Value::Float(libm::trunc(*value))),
            "sqrt" => Ok(Value::Float(libm::sqrt(*value))),
            "pow" => {
                let exponent = args
                    .first()
                    .and_then(|v| match v {
                        Value::Float(f) => Some(*f),
                        _ => None,
                    })
                    .unwrap_or(0.0);
                Ok(Value::Float(libm::pow(*value, exponent)))
            }
            "round" => {
                let digits = int_arg(&args, 0).unwrap_or(0);
                if digits < 0 {
                    return self.trap(self.panic("round digits must be non-negative", span));
                }
                // More digits than an f64 can represent cannot change the
                // value, and `10f64.powi` would overflow to infinity.
                if digits > 308 {
                    return Ok(Value::Float(*value));
                }
                let factor = 10f64.powi(digits as i32);
                Ok(Value::Float(libm::round(*value * factor) / factor))
            }
            "is_nan" => Ok(Value::Bool(value.is_nan())),
            "is_finite" => Ok(Value::Bool(value.is_finite())),
            _ => self.trap(self.internal_panic(format!("unknown float method `{name}`"))),
        }
    }

    fn range_method(
        &mut self,
        name: &str,
        receiver: &mut Value,
        args: CallArgs,
        _span: Span,
    ) -> EvalResult<Value> {
        let Value::Range {
            start,
            end,
            inclusive,
        } = receiver
        else {
            return self.trap(self.internal_panic("receiver is not a range"));
        };
        let len = range_len(*start, *end, *inclusive);
        match name {
            "len" => Ok(Value::Int(len)),
            "contains" => {
                let value = int_arg(&args, 0).unwrap_or(0);
                let inside = value >= *start
                    && if *inclusive { value <= *end } else { value < *end };
                Ok(Value::Bool(inside))
            }
            "to_list" => {
                self.charge(fuel::slot_cost(len.max(0) as u64))?;
                let mut out = Vec::new();
                let last = if *inclusive { Some(*end) } else { end.checked_sub(1) };
                if let Some(last) = last {
                    let mut i = *start;
                    while i <= last {
                        out.push(Value::Int(i));
                        if i == i64::MAX {
                            break;
                        }
                        i += 1;
                    }
                }
                Ok(Value::list(out))
            }
            _ => self.trap(self.internal_panic(format!("unknown range method `{name}`"))),
        }
    }

    fn option_method(
        &mut self,
        name: &str,
        receiver: &mut Value,
        args: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        // The receiver is an owned temporary for every non-mut builtin method,
        // so take it instead of deep-cloning the payload.
        let Value::Option(option) = std::mem::replace(receiver, Value::Unit) else {
            return self.trap(self.internal_panic("receiver is not an Option"));
        };
        match name {
            "is_some" => Ok(Value::Bool(option.is_some())),
            "is_none" => Ok(Value::Bool(option.is_none())),
            "map" => {
                let closure = self.closure_arg(&args, 0)?;
                match option {
                    Some(value) => Ok(Value::Option(Some(Box::new(
                        self.call_closure(closure, smallvec![*value])
                            .map_err(EvalError::Trap)?,
                    )))),
                    None => Ok(Value::Option(None)),
                }
            }
            "and_then" => {
                let closure = self.closure_arg(&args, 0)?;
                match option {
                    Some(value) => self
                        .call_closure(closure, smallvec![*value])
                        .map_err(EvalError::Trap),
                    None => Ok(Value::Option(None)),
                }
            }
            "unwrap_or" => Ok(match option {
                Some(value) => *value,
                None => args.into_iter().next().unwrap_or(Value::Unit),
            }),
            "ok_or" => Ok(match option {
                Some(value) => Value::Result(Ok(value)),
                None => Value::Result(Err(Box::new(
                    args.into_iter().next().unwrap_or(Value::Unit),
                ))),
            }),
            "expect" => match option {
                Some(value) => Ok(*value),
                None => {
                    let message = args
                        .first()
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| "called `expect` on None".into());
                    self.trap(self.panic(message, span))
                }
            },
            _ => self.trap(self.internal_panic(format!("unknown Option method `{name}`"))),
        }
    }

    fn result_method(
        &mut self,
        name: &str,
        receiver: &mut Value,
        args: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        // The receiver is an owned temporary for every non-mut builtin method,
        // so take it instead of deep-cloning the payload.
        let Value::Result(result) = std::mem::replace(receiver, Value::Unit) else {
            return self.trap(self.internal_panic("receiver is not a Result"));
        };
        match name {
            "is_ok" => Ok(Value::Bool(result.is_ok())),
            "is_err" => Ok(Value::Bool(result.is_err())),
            "map" => {
                let closure = self.closure_arg(&args, 0)?;
                match result {
                    Ok(value) => Ok(Value::Result(Ok(Box::new(
                        self.call_closure(closure, smallvec![*value])
                            .map_err(EvalError::Trap)?,
                    )))),
                    Err(error) => Ok(Value::Result(Err(error))),
                }
            }
            "map_err" => {
                let closure = self.closure_arg(&args, 0)?;
                match result {
                    Ok(value) => Ok(Value::Result(Ok(value))),
                    Err(error) => Ok(Value::Result(Err(Box::new(
                        self.call_closure(closure, smallvec![*error])
                            .map_err(EvalError::Trap)?,
                    )))),
                }
            }
            "and_then" => {
                let closure = self.closure_arg(&args, 0)?;
                match result {
                    Ok(value) => self
                        .call_closure(closure, smallvec![*value])
                        .map_err(EvalError::Trap),
                    Err(error) => Ok(Value::Result(Err(error))),
                }
            }
            "unwrap_or" => Ok(match result {
                Ok(value) => *value,
                Err(_) => args.into_iter().next().unwrap_or(Value::Unit),
            }),
            "ok" => Ok(match result {
                Ok(value) => Value::Option(Some(value)),
                Err(_) => Value::Option(None),
            }),
            "expect" => match result {
                Ok(value) => Ok(*value),
                Err(_) => {
                    let message = args
                        .first()
                        .and_then(|v| v.as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| "called `expect` on Err".into());
                    self.trap(self.panic(message, span))
                }
            },
            _ => self.trap(self.internal_panic(format!("unknown Result method `{name}`"))),
        }
    }

    fn closure_arg<'a>(&self, args: &'a [Value], index: usize) -> EvalResult<&'a FnValue> {
        match args.get(index) {
            Some(Value::Fn(function)) => Ok(function),
            _ => self.trap(self.internal_panic("expected a function argument")),
        }
    }
}


