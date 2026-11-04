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
        let mut receiver_value = if mut_self {
            let place = self.eval_place(base)?;
            let value = self.read_place(&place)?;
            Some((place, value))
        } else {
            None
        };
        let mut value = match &receiver_value {
            Some((_, value)) => value.clone(),
            None => self.eval_expr(base)?,
        };
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            if arg.mutable {
                return self.trap(self.internal_panic("unexpected mut argument"));
            }
            values.push(self.eval_expr(&arg.expr)?);
        }
        let result = self.apply_builtin_method(receiver, name, &mut value, values, expr.span)?;
        if let Some((place, _)) = receiver_value.take() {
            self.write_place(place, value)?;
        }
        self.check_value_depth(&result, expr.span)?;
        self.check_value_memory(&result)?;
        Ok(result)
    }

    fn apply_builtin_method(
        &mut self,
        receiver: BuiltinReceiver,
        name: &str,
        receiver_value: &mut Value,
        args: Vec<Value>,
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
        args: Vec<Value>,
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
                self.charge(items.len() as u64)?;
                Ok(Value::Bool(items.iter().any(|v| value_eq(v, &needle))))
            }
            "map" => {
                let closure = closure_arg(&args, 0);
                let snapshot = items.clone();
                self.charge(snapshot.len() as u64)?;
                let mut out = Vec::with_capacity(snapshot.len());
                for item in snapshot.iter() {
                    out.push(
                        self.call_closure(&closure, vec![item.clone()])
                            .map_err(EvalError::Trap)?,
                    );
                }
                Ok(Value::list(out))
            }
            "filter" => {
                let closure = closure_arg(&args, 0);
                let snapshot = items.clone();
                self.charge(snapshot.len() as u64)?;
                let mut out = Vec::new();
                for item in snapshot.iter() {
                    let keep = self
                        .call_closure(&closure, vec![item.clone()])
                        .map_err(EvalError::Trap)?;
                    if matches!(keep, Value::Bool(true)) {
                        out.push(item.clone());
                    }
                }
                Ok(Value::list(out))
            }
            "fold" => {
                let closure = closure_arg(&args, 1);
                let mut init = args.into_iter().next().unwrap_or(Value::Unit);
                let snapshot = items.clone();
                self.charge(snapshot.len() as u64)?;
                for item in snapshot.iter() {
                    init = self
                        .call_closure(&closure, vec![init, item.clone()])
                        .map_err(EvalError::Trap)?;
                }
                Ok(init)
            }
            "any" => {
                let closure = closure_arg(&args, 0);
                let snapshot = items.clone();
                for item in snapshot.iter() {
                    self.charge(1)?;
                    if matches!(
                        self.call_closure(&closure, vec![item.clone()])
                            .map_err(EvalError::Trap)?,
                        Value::Bool(true)
                    ) {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            "all" => {
                let closure = closure_arg(&args, 0);
                let snapshot = items.clone();
                for item in snapshot.iter() {
                    self.charge(1)?;
                    if !matches!(
                        self.call_closure(&closure, vec![item.clone()])
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
                self.charge(out.len() as u64)?;
                Ok(Value::list(out))
            }
            "reversed" => {
                let mut out: Vec<Value> = items.iter().cloned().collect();
                out.reverse();
                self.charge(out.len() as u64)?;
                Ok(Value::list(out))
            }
            "sorted" => {
                let mut out: Vec<Value> = items.iter().cloned().collect();
                let n = out.len() as u64;
                self.charge(n * (n + 1).ilog2().max(1) as u64)?;
                out.sort_by(value_ord);
                self.ensure_deadline()?;
                Ok(Value::list(out))
            }
            "sorted_by" => {
                let closure = closure_arg(&args, 0);
                let snapshot = items.clone();
                let n = snapshot.len() as u64;
                self.charge(n.saturating_mul((n + 1).ilog2().max(1) as u64))?;
                let mut keyed = Vec::with_capacity(snapshot.len());
                for item in snapshot.iter() {
                    let key = self
                        .call_closure(&closure, vec![item.clone()])
                        .map_err(EvalError::Trap)?;
                    keyed.push((key, item.clone()));
                }
                keyed.sort_by(|a, b| value_ord(&a.0, &b.0));
                self.ensure_deadline()?;
                Ok(Value::list(keyed.into_iter().map(|(_, v)| v).collect()))
            }
            "join" => {
                let sep = args
                    .first()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                let mut out = String::new();
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(&sep);
                    }
                    if let Some(s) = item.as_str() {
                        out.push_str(s);
                    }
                }
                self.charge((out.len() as u64).div_ceil(8))?;
                Ok(Value::str(out))
            }
            "push" => {
                let value = args.into_iter().next().unwrap_or(Value::Unit);
                self.charge(1)?;
                Rc::make_mut(items).push(value);
                Ok(Value::Unit)
            }
            "pop" => Ok(Rc::make_mut(items)
                .pop()
                .map(|v| Value::Option(Some(Box::new(v))))
                .unwrap_or(Value::Option(None))),
            "sort" => {
                let n = items.len() as u64;
                self.charge(n * (n + 1).ilog2().max(1) as u64)?;
                Rc::make_mut(items).sort_by(value_ord);
                self.ensure_deadline()?;
                Ok(Value::Unit)
            }
            "insert" => {
                let index = int_arg(&args, 0).unwrap_or(0);
                let value = args.into_iter().nth(1).unwrap_or(Value::Unit);
                let list = Rc::make_mut(items);
                if index < 0 || index as usize > list.len() {
                    return self.trap(self.panic("list insert index out of bounds", span));
                }
                list.insert(index as usize, value);
                Ok(Value::Unit)
            }
            "remove_at" => {
                let index = int_arg(&args, 0).unwrap_or(0);
                let list = Rc::make_mut(items);
                if index < 0 || index as usize >= list.len() {
                    return self.trap(self.panic("list index out of bounds", span));
                }
                Ok(list.remove(index as usize))
            }
            "extend" => {
                let other = args.into_iter().next().unwrap_or(Value::Unit);
                if let Value::List(other) = other {
                    self.charge(other.len() as u64)?;
                    Rc::make_mut(items).extend(other.iter().cloned());
                }
                Ok(Value::Unit)
            }
            "clear" => {
                self.charge(items.len() as u64)?;
                Rc::make_mut(items).clear();
                Ok(Value::Unit)
            }
            _ => self.trap(self.internal_panic(format!("unknown list method `{name}`"))),
        }
    }

    fn map_method(
        &mut self,
        name: &str,
        receiver: &mut Value,
        args: Vec<Value>,
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
                self.charge(map.len() as u64)?;
                Ok(Value::list(
                    map.keys().iter().map(key_to_value).collect(),
                ))
            }
            "values" => {
                self.charge(map.len() as u64)?;
                Ok(Value::list(map.values()))
            }
            "entries" => {
                self.charge(map.len() as u64)?;
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
                let removed = Rc::make_mut(map).remove(&key);
                Ok(removed
                    .map(|v| Value::Option(Some(Box::new(v))))
                    .unwrap_or(Value::Option(None)))
            }
            "clear" => {
                self.charge(map.len() as u64)?;
                Rc::make_mut(map).clear();
                Ok(Value::Unit)
            }
            _ => self.trap(self.internal_panic(format!("unknown map method `{name}`"))),
        }
    }

    fn str_method(
        &mut self,
        name: &str,
        receiver: &mut Value,
        args: Vec<Value>,
        span: Span,
    ) -> EvalResult<Value> {
        let Value::Str(text) = receiver else {
            return self.trap(self.internal_panic("receiver is not a str"));
        };
        match name {
            "len" => {
                self.charge((text.len() as u64).div_ceil(8))?;
                Ok(Value::Int(text.chars().count() as i64))
            }
            "chars" => {
                self.charge((text.len() as u64).div_ceil(8))?;
                Ok(Value::list(
                    text.chars()
                        .map(|c| Value::str(c.to_string()))
                        .collect(),
                ))
            }
            "split" => {
                let sep = args
                    .first()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                let parts: Vec<Value> = if sep.is_empty() {
                    text.chars().map(|c| Value::str(c.to_string())).collect()
                } else {
                    text.split(&sep).map(Value::str).collect()
                };
                self.charge((text.len() as u64).div_ceil(8))?;
                Ok(Value::list(parts))
            }
            "trim" => {
                let out = text.trim().to_string();
                self.charge((out.len() as u64).div_ceil(8))?;
                Ok(Value::str(out))
            }
            "contains" => {
                let needle = args
                    .first()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                self.charge((text.len() as u64).div_ceil(8))?;
                Ok(Value::Bool(text.contains(&needle)))
            }
            "find" => {
                let needle = args
                    .first()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                self.charge((text.len() as u64).div_ceil(8))?;
                Ok(text
                    .find(&needle)
                    .map(|byte| Value::Int(text[..byte].chars().count() as i64))
                    .map(|v| Value::Option(Some(Box::new(v))))
                    .unwrap_or(Value::Option(None)))
            }
            "starts_with" => {
                let needle = args
                    .first()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                self.charge((text.len() as u64).div_ceil(8))?;
                Ok(Value::Bool(text.starts_with(&needle)))
            }
            "ends_with" => {
                let needle = args
                    .first()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                self.charge((text.len() as u64).div_ceil(8))?;
                Ok(Value::Bool(text.ends_with(&needle)))
            }
            "replace" => {
                let from = args
                    .first()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                let to = args
                    .get(1)
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_default();
                if from.is_empty() {
                    return self.trap(self.panic("replace with an empty `from`", span));
                }
                let out = text.replace(&from, &to);
                self.charge((out.len() as u64).div_ceil(8))?;
                Ok(Value::str(out))
            }
            "to_upper" => {
                let out = text.to_uppercase();
                self.charge((out.len() as u64).div_ceil(8))?;
                Ok(Value::str(out))
            }
            "to_lower" => {
                let out = text.to_lowercase();
                self.charge((out.len() as u64).div_ceil(8))?;
                Ok(Value::str(out))
            }
            "parse_int" => {
                self.charge((text.len() as u64).div_ceil(8))?;
                Ok(match parse_int(&text) {
                    Some(v) => Value::Option(Some(Box::new(Value::Int(v)))),
                    None => Value::Option(None),
                })
            }
            "parse_float" => {
                self.charge((text.len() as u64).div_ceil(8))?;
                Ok(match parse_float(&text) {
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
        args: Vec<Value>,
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
        args: Vec<Value>,
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
        args: Vec<Value>,
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
                self.charge(len.max(0) as u64)?;
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
        args: Vec<Value>,
        span: Span,
    ) -> EvalResult<Value> {
        let Value::Option(option) = receiver else {
            return self.trap(self.internal_panic("receiver is not an Option"));
        };
        match name {
            "is_some" => Ok(Value::Bool(option.is_some())),
            "is_none" => Ok(Value::Bool(option.is_none())),
            "map" => {
                let closure = closure_arg(&args, 0);
                match option.clone() {
                    Some(value) => Ok(Value::Option(Some(Box::new(
                        self.call_closure(&closure, vec![*value])
                            .map_err(EvalError::Trap)?,
                    )))),
                    None => Ok(Value::Option(None)),
                }
            }
            "and_then" => {
                let closure = closure_arg(&args, 0);
                match option.clone() {
                    Some(value) => self
                        .call_closure(&closure, vec![*value])
                        .map_err(EvalError::Trap),
                    None => Ok(Value::Option(None)),
                }
            }
            "unwrap_or" => Ok(match option {
                Some(value) => (**value).clone(),
                None => args.into_iter().next().unwrap_or(Value::Unit),
            }),
            "ok_or" => Ok(match option.clone() {
                Some(value) => Value::Result(Ok(value)),
                None => Value::Result(Err(Box::new(
                    args.into_iter().next().unwrap_or(Value::Unit),
                ))),
            }),
            "expect" => match option {
                Some(value) => Ok((**value).clone()),
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
        args: Vec<Value>,
        span: Span,
    ) -> EvalResult<Value> {
        let Value::Result(result) = receiver else {
            return self.trap(self.internal_panic("receiver is not a Result"));
        };
        match name {
            "is_ok" => Ok(Value::Bool(result.is_ok())),
            "is_err" => Ok(Value::Bool(result.is_err())),
            "map" => {
                let closure = closure_arg(&args, 0);
                match result.clone() {
                    Ok(value) => Ok(Value::Result(Ok(Box::new(
                        self.call_closure(&closure, vec![*value])
                            .map_err(EvalError::Trap)?,
                    )))),
                    Err(error) => Ok(Value::Result(Err(error))),
                }
            }
            "map_err" => {
                let closure = closure_arg(&args, 0);
                match result.clone() {
                    Ok(value) => Ok(Value::Result(Ok(value))),
                    Err(error) => Ok(Value::Result(Err(Box::new(
                        self.call_closure(&closure, vec![*error])
                            .map_err(EvalError::Trap)?,
                    )))),
                }
            }
            "and_then" => {
                let closure = closure_arg(&args, 0);
                match result.clone() {
                    Ok(value) => self
                        .call_closure(&closure, vec![*value])
                        .map_err(EvalError::Trap),
                    Err(error) => Ok(Value::Result(Err(error))),
                }
            }
            "unwrap_or" => Ok(match result {
                Ok(value) => (**value).clone(),
                Err(_) => args.into_iter().next().unwrap_or(Value::Unit),
            }),
            "ok" => Ok(match result.clone() {
                Ok(value) => Value::Option(Some(value)),
                Err(_) => Value::Option(None),
            }),
            "expect" => match result {
                Ok(value) => Ok((**value).clone()),
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
}

fn closure_arg(args: &[Value], index: usize) -> Closure {
    match args.get(index) {
        Some(Value::Fn(closure)) => (**closure).clone(),
        _ => Closure {
            target: ClosureTarget::Lambda {
                params: Vec::new(),
                body: LambdaBody::Expr(Box::new(Expr {
                    id: martite_syntax::NodeId(0),
                    span: Span::empty(0),
                    kind: ExprKind::Unit,
                })),
            },
            captures: HashMap::new(),
        },
    }
}

fn int_arg(args: &[Value], index: usize) -> Option<i64> {
    match args.get(index) {
        Some(Value::Int(v)) => Some(*v),
        _ => None,
    }
}

fn range_len(start: i64, end: i64, inclusive: bool) -> i64 {
    if inclusive {
        if start > end {
            0
        } else {
            end.saturating_sub(start).saturating_add(1)
        }
    } else if start >= end {
        0
    } else {
        end.saturating_sub(start)
    }
}

fn parse_int(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut i = 0;
    if bytes[0] == b'-' {
        i = 1;
    }
    if i == bytes.len() {
        return None;
    }
    if !bytes[i..].iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<i64>().ok()
}

fn parse_float(text: &str) -> Option<f64> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let mut i = 0;
    if bytes[0] == b'-' {
        i = 1;
    }
    let int_start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i == int_start {
        return None;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        let frac_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == frac_start {
            return None;
        }
    }
    if i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let exp_start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == exp_start {
            return None;
        }
    }
    if i != bytes.len() {
        return None;
    }
    let value = text.parse::<f64>().ok()?;
    value.is_finite().then_some(value)
}
