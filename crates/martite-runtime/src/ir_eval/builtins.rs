//! Builtin execution for the IR evaluator (#109).
//!
//! The method bodies implement the edition's standard library. Calls to user
//! functions and closures go through `call_closure_value`.

use super::*;
use crate::ops::{int_arg, parse_float, parse_int, range_len};
use crate::value::{OrderedMap, key_to_value};
use martite_ir::{BuiltinMethodId, BuiltinReceiver};
use smallvec::smallvec;

impl<'p, 'h> IrRuntime<'p, 'h> {
    pub(super) fn call_builtin_fn_values(
        &mut self,
        function: BuiltinFnId,
        values: &[Value],
        span: Span,
    ) -> EvalResult<Value> {
        match function {
            BuiltinFnId::PRINT => {
                let value = values.first().cloned().unwrap_or(Value::Unit);
                let epoch = self.scan_epoch;
                self.charge(fuel::show_cost(value.meta().slots))?;
                self.pin_value(&value, epoch)?;
                // Reserve the string header and let the render buffer grow only
                // into the remaining memory budget.
                let limit = self.memory_headroom(Some(&value))?.saturating_sub(16);
                let limit = limit.min(usize::MAX as u64) as usize;
                let mut text = ShowOutput::new(limit);
                write_show(&mut text, &value, &self.program.types, true);
                if text.is_overflowed() {
                    return self.trap(Trap::OutOfMemory);
                }
                let text = text.into_string();
                self.charge(fuel::string_cost(text.len() as u64))?;
                self.note_log(text);
                self.maybe_check_memory()?;
                Ok(Value::Unit)
            }
            BuiltinFnId::PANIC => {
                let message = values
                    .first()
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_else(|| "panic".into());
                self.trap(self.panic(message, span))
            }
            BuiltinFnId::ASSERT => {
                let condition = values.first().cloned().unwrap_or(Value::Bool(false));
                let message = values
                    .get(1)
                    .and_then(|value| value.as_str().map(str::to_string))
                    .unwrap_or_else(|| "assertion failed".into());
                if matches!(condition, Value::Bool(true)) {
                    Ok(Value::Unit)
                } else {
                    self.trap(self.panic(message, span))
                }
            }
            _ => self.trap(self.internal_panic("unknown builtin function")),
        }
    }

    pub(super) fn call_builtin_item_values(
        &mut self,
        item: BuiltinItemId,
        values: &[Value],
        span: Span,
    ) -> EvalResult<Value> {
        match item {
            BuiltinItemId::MATH_MIN | BuiltinItemId::MATH_MAX => {
                let a = values.first().cloned().unwrap_or(Value::Unit);
                let b = values.get(1).cloned().unwrap_or(Value::Unit);
                let ord = value_ord(&a, &b);
                let take_a = if item == BuiltinItemId::MATH_MIN {
                    ord.is_le()
                } else {
                    ord.is_ge()
                };
                Ok(if take_a { a } else { b })
            }
            BuiltinItemId::MATH_CLAMP => {
                let x = values.first().cloned().unwrap_or(Value::Unit);
                let lo = values.get(1).cloned().unwrap_or(Value::Unit);
                let hi = values.get(2).cloned().unwrap_or(Value::Unit);
                if value_ord(&x, &lo).is_lt() {
                    Ok(lo)
                } else if value_ord(&x, &hi).is_gt() {
                    Ok(hi)
                } else {
                    Ok(x)
                }
            }
            BuiltinItemId::MATH_SIN
            | BuiltinItemId::MATH_COS
            | BuiltinItemId::MATH_TAN
            | BuiltinItemId::MATH_EXP
            | BuiltinItemId::MATH_LN
            | BuiltinItemId::MATH_LOG10 => {
                let Value::Float(x) = values.first().cloned().unwrap_or(Value::Float(0.0)) else {
                    return self.trap(self.internal_panic("expected float"));
                };
                Ok(Value::Float(match item {
                    BuiltinItemId::MATH_SIN => libm::sin(x),
                    BuiltinItemId::MATH_COS => libm::cos(x),
                    BuiltinItemId::MATH_TAN => libm::tan(x),
                    BuiltinItemId::MATH_EXP => libm::exp(x),
                    BuiltinItemId::MATH_LN => libm::log(x),
                    _ => libm::log10(x),
                }))
            }
            BuiltinItemId::MATH_ATAN2 => {
                let Value::Float(y) = values.first().cloned().unwrap_or(Value::Float(0.0)) else {
                    return self.trap(self.internal_panic("expected float"));
                };
                let Value::Float(x) = values.get(1).cloned().unwrap_or(Value::Float(0.0)) else {
                    return self.trap(self.internal_panic("expected float"));
                };
                Ok(Value::Float(libm::atan2(y, x)))
            }
            BuiltinItemId::JSON_PARSE => {
                let Some(text) = values.first().and_then(|value| value.as_str()) else {
                    return self.trap(self.internal_panic("expected str"));
                };
                self.charge(fuel::string_cost(text.len() as u64))?;
                let json_type = self.program.entries.json_type;
                let budget = self.memory_headroom(values.first())?;
                match crate::json::parse_bounded(text, json_type, budget) {
                    Ok(value) => {
                        self.check_value_depth(&value, span)?;
                        self.ensure_output_capacity(value.meta().bytes.saturating_add(16))?;
                        self.note_value(&value);
                        self.note_alloc(16);
                        self.maybe_check_memory()?;
                        Ok(Value::Result(Ok(Box::new(value))))
                    }
                    Err(crate::json::ParseError::Invalid(message)) => {
                        self.note_alloc(16 + 16 + message.len() as u64);
                        Ok(Value::Result(Err(Box::new(Value::str(message)))))
                    }
                    Err(crate::json::ParseError::TooLarge) => self.trap(Trap::OutOfMemory),
                }
            }
            BuiltinItemId::JSON_STRINGIFY => {
                let Some(value) = values.first() else {
                    return self.trap(self.internal_panic("expected Json"));
                };
                let limit = self.memory_headroom(values.first())?;
                let text = match crate::json::stringify_limited(value, &self.program.types, limit) {
                    Ok(text) => text,
                    Err(()) => return self.trap(Trap::OutOfMemory),
                };
                self.charge(fuel::string_cost(text.len() as u64))?;
                self.note_alloc(16 + text.len() as u64);
                Ok(Value::str(text))
            }
            _ => self.trap(self.internal_panic("unknown builtin module item")),
        }
    }

    pub(super) fn call_builtin_method(
        &mut self,
        method: BuiltinMethodId,
        first_child: u32,
        span: Span,
    ) -> EvalResult<Value> {
        let Some(def) = method.def() else {
            return self.trap(self.internal_panic("unknown builtin method"));
        };
        let arity = def.arity;
        let mut_self = def.mut_self;
        let function = self.current_function();
        let Some(receiver_id) = function.children.get(first_child as usize).copied() else {
            return self.trap(self.internal_panic("builtin method has no receiver"));
        };

        if !mut_self {
            let mut receiver = self.eval(receiver_id)?;
            let args = self.builtin_method_args(first_child, arity)?;
            let result = self.apply_builtin_method(method, &mut receiver, args, span)?;
            self.check_value_depth(&result, span)?;
            self.note_value(&result);
            return Ok(result);
        }

        let Some(node) = function.node(receiver_id).copied() else {
            return self.trap(self.internal_panic("builtin method has no receiver"));
        };
        let NodeKind::MutArg(place) = node.kind else {
            return self.trap(self.internal_panic("mutable builtin receiver must be a place"));
        };
        let place = self.eval_place(place)?;
        let mut receiver = self.read_place(&place)?;
        let args = self.builtin_method_args(first_child, arity)?;
        self.check_builtin_mutation_depth(method, &args, place.accessors.len())?;

        let mut root = self.take_place_root(&place)?;
        let (result, root) = if place.accessors.is_empty() {
            drop(root);
            let result = self.apply_builtin_method(method, &mut receiver, args, span)?;
            (result, receiver)
        } else {
            let replaced = self.take_path_value(&mut root, &place.accessors)?;
            drop(replaced);
            let result = self.apply_builtin_method(method, &mut receiver, args, span)?;
            self.set_path(&mut root, &place.accessors, receiver, MAX_VALUE_DEPTH)?;
            (result, root)
        };
        self.set_slot(place.root, root);
        self.check_value_depth(&result, span)?;
        self.note_value(&result);
        Ok(result)
    }

    fn builtin_method_args(&mut self, first_child: u32, arity: u32) -> EvalResult<CallArgs> {
        let function = self.current_function();
        let mut args = CallArgs::with_capacity(arity as usize);
        for index in 0..arity as usize {
            let Some(child) = function
                .children
                .get(first_child as usize + 1 + index)
                .copied()
            else {
                return self.trap(self.internal_panic("builtin method argument is out of range"));
            };
            let Some(node) = function.node(child).copied() else {
                return self.trap(self.internal_panic("builtin method argument is out of range"));
            };
            if matches!(node.kind, NodeKind::MutArg(_)) {
                return self
                    .trap(self.internal_panic("builtin method arguments cannot be mutable"));
            }
            args.push(self.eval(child)?);
        }
        Ok(args)
    }

    fn apply_builtin_method(
        &mut self,
        method: BuiltinMethodId,
        receiver_value: &mut Value,
        args: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        let Some(def) = method.def() else {
            return self.trap(self.internal_panic("unknown builtin method"));
        };
        match def.receiver {
            BuiltinReceiver::List => self.list_method(method, receiver_value, args, span),
            BuiltinReceiver::Map => self.map_method(method, receiver_value, args, span),
            BuiltinReceiver::Str => self.str_method(method, receiver_value, args, span),
            BuiltinReceiver::Int => self.int_method(method, receiver_value, args, span),
            BuiltinReceiver::Float => self.float_method(method, receiver_value, args, span),
            BuiltinReceiver::Range => self.range_method(method, receiver_value, args, span),
            BuiltinReceiver::Option => self.option_method(method, receiver_value, args, span),
            BuiltinReceiver::Result => self.result_method(method, receiver_value, args, span),
        }
    }

    fn list_method(
        &mut self,
        method: BuiltinMethodId,
        receiver: &mut Value,
        args: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        let Value::List(items) = receiver else {
            return self.trap(self.internal_panic("receiver is not a list"));
        };
        match method {
            BuiltinMethodId::LIST_LEN => Ok(Value::Int(items.len() as i64)),
            BuiltinMethodId::LIST_GET => {
                let index = int_arg(&args, 0);
                Ok(match index {
                    Some(i) if i >= 0 => items
                        .get(i as usize)
                        .cloned()
                        .map(|value| Value::Option(Some(Box::new(value))))
                        .unwrap_or(Value::Option(None)),
                    _ => Value::Option(None),
                })
            }
            BuiltinMethodId::LIST_FIRST => Ok(items
                .first()
                .cloned()
                .map(|value| Value::Option(Some(Box::new(value))))
                .unwrap_or(Value::Option(None))),
            BuiltinMethodId::LIST_LAST => Ok(items
                .last()
                .cloned()
                .map(|value| Value::Option(Some(Box::new(value))))
                .unwrap_or(Value::Option(None))),
            BuiltinMethodId::LIST_CONTAINS => {
                let needle = args.into_iter().next().unwrap_or(Value::Unit);
                let needle_meta = needle.meta();
                self.charge(fuel::slot_cost(items.len() as u64))?;
                for value in items.iter() {
                    let meta = value.meta();
                    self.charge(fuel::comparison_cost(
                        meta.slots.max(needle_meta.slots),
                        meta.string_slots.max(needle_meta.string_slots),
                    ))?;
                    if value_eq(value, &needle) {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            BuiltinMethodId::LIST_MAP => {
                let closure = self.closure_arg(&args, 0)?;
                let snapshot = items.clone();
                self.charge(fuel::slot_cost(snapshot.len() as u64))?;
                let mut out = Vec::with_capacity(snapshot.len());
                match closure {
                    FnValue::Closure(closure) => {
                        let program = self.program;
                        let Some(function_info) = program.function(closure.target) else {
                            return self
                                .trap(self.internal_panic("closure target is out of range"));
                        };
                        for item in snapshot.iter() {
                            let (value, _) = self
                                .call_resolved(
                                    function_info,
                                    smallvec![item.clone()],
                                    closure.captures.clone(),
                                )
                                .map_err(EvalError::from)?;
                            out.push(value);
                        }
                    }
                    FnValue::Def(function) => {
                        for item in snapshot.iter() {
                            out.push(
                                self.call_function(*function, smallvec![item.clone()])
                                    .map_err(EvalError::from)?,
                            );
                        }
                    }
                }
                Ok(Value::list(out))
            }
            BuiltinMethodId::LIST_FILTER => {
                let closure = self.closure_arg(&args, 0)?;
                let snapshot = items.clone();
                self.charge(fuel::slot_cost(snapshot.len() as u64))?;
                let mut out = Vec::new();
                match closure {
                    FnValue::Closure(closure) => {
                        let program = self.program;
                        let Some(function_info) = program.function(closure.target) else {
                            return self
                                .trap(self.internal_panic("closure target is out of range"));
                        };
                        for item in snapshot.iter() {
                            let (keep, _) = self
                                .call_resolved(
                                    function_info,
                                    smallvec![item.clone()],
                                    closure.captures.clone(),
                                )
                                .map_err(EvalError::from)?;
                            if matches!(keep, Value::Bool(true)) {
                                out.push(item.clone());
                            }
                        }
                    }
                    FnValue::Def(function) => {
                        for item in snapshot.iter() {
                            let keep = self
                                .call_function(*function, smallvec![item.clone()])
                                .map_err(EvalError::from)?;
                            if matches!(keep, Value::Bool(true)) {
                                out.push(item.clone());
                            }
                        }
                    }
                }
                Ok(Value::list(out))
            }
            BuiltinMethodId::LIST_FOLD => {
                let mut args = args;
                let mut init = if args.is_empty() {
                    Value::Unit
                } else {
                    args.remove(0)
                };
                let closure = self.closure_arg(&args, 0)?;
                let snapshot = items.clone();
                self.charge(fuel::slot_cost(snapshot.len() as u64))?;
                match closure {
                    FnValue::Closure(closure) => {
                        let program = self.program;
                        let Some(function_info) = program.function(closure.target) else {
                            return self
                                .trap(self.internal_panic("closure target is out of range"));
                        };
                        for item in snapshot.iter() {
                            let (next, _) = self
                                .call_resolved(
                                    function_info,
                                    smallvec![init, item.clone()],
                                    closure.captures.clone(),
                                )
                                .map_err(EvalError::from)?;
                            init = next;
                        }
                    }
                    FnValue::Def(function) => {
                        for item in snapshot.iter() {
                            init = self
                                .call_function(*function, smallvec![init, item.clone()])
                                .map_err(EvalError::from)?;
                        }
                    }
                }
                Ok(init)
            }
            BuiltinMethodId::LIST_ANY => {
                let closure = self.closure_arg(&args, 0)?;
                let snapshot = items.clone();
                for item in snapshot.iter() {
                    self.charge(fuel::ITERATION_COST)?;
                    if matches!(
                        self.call_closure_value(closure, smallvec![item.clone()])
                            .map_err(EvalError::from)?,
                        Value::Bool(true)
                    ) {
                        return Ok(Value::Bool(true));
                    }
                }
                Ok(Value::Bool(false))
            }
            BuiltinMethodId::LIST_ALL => {
                let closure = self.closure_arg(&args, 0)?;
                let snapshot = items.clone();
                for item in snapshot.iter() {
                    self.charge(fuel::ITERATION_COST)?;
                    if !matches!(
                        self.call_closure_value(closure, smallvec![item.clone()])
                            .map_err(EvalError::from)?,
                        Value::Bool(true)
                    ) {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }
            BuiltinMethodId::LIST_ENUMERATE => {
                let out: Vec<Value> = items
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        Value::tuple(vec![Value::Int(index as i64), value.clone()])
                    })
                    .collect();
                self.charge(fuel::slot_cost(out.len() as u64))?;
                Ok(Value::list(out))
            }
            BuiltinMethodId::LIST_REVERSED => {
                let mut out: Vec<Value> = items.iter().cloned().collect();
                out.reverse();
                self.charge(fuel::slot_cost(out.len() as u64))?;
                Ok(Value::list(out))
            }
            BuiltinMethodId::LIST_SORTED => {
                let mut out: Vec<Value> = items.iter().cloned().collect();
                self.charge(fuel::sort_cost(out.len() as u64))?;
                out.sort_by(value_ord);
                self.ensure_deadline()?;
                Ok(Value::list(out))
            }
            BuiltinMethodId::LIST_SORTED_BY => {
                let closure = self.closure_arg(&args, 0)?;
                let snapshot = items.clone();
                self.charge(fuel::sort_cost(snapshot.len() as u64))?;
                let mut keyed = Vec::with_capacity(snapshot.len());
                for item in snapshot.iter() {
                    let key = self
                        .call_closure_value(closure, smallvec![item.clone()])
                        .map_err(EvalError::from)?;
                    keyed.push((key, item.clone()));
                }
                keyed.sort_by(|a, b| value_ord(&a.0, &b.0));
                self.ensure_deadline()?;
                Ok(Value::list(
                    keyed.into_iter().map(|(_, value)| value).collect(),
                ))
            }
            BuiltinMethodId::LIST_JOIN => {
                let separator = args
                    .first()
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let projected = items.iter().fold(0u64, |total, item| {
                    total.saturating_add(item.as_str().map_or(0, |text| text.len() as u64))
                });
                let projected = projected.saturating_add(
                    (separator.len() as u64).saturating_mul(items.len().saturating_sub(1) as u64),
                );
                self.charge(fuel::slot_cost(items.len() as u64))?;
                self.charge(fuel::string_cost(projected))?;
                self.ensure_output_capacity(16u64.saturating_add(projected))?;
                let Ok(capacity) = usize::try_from(projected) else {
                    return self.trap(Trap::OutOfMemory);
                };
                let mut out = String::new();
                if out.try_reserve_exact(capacity).is_err() {
                    return self.trap(Trap::OutOfMemory);
                }
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(separator);
                    }
                    if let Some(text) = item.as_str() {
                        out.push_str(text);
                    }
                }
                Ok(Value::str(out))
            }
            BuiltinMethodId::LIST_PUSH => {
                let value = args.into_iter().next().unwrap_or(Value::Unit);
                let len = items.len();
                self.charge(fuel::SLOT_COST)?;
                self.note_alloc(8);
                let list = self.cow_vec_mut(items, len)?;
                list.push(value);
                Ok(Value::Unit)
            }
            BuiltinMethodId::LIST_POP => {
                let len = items.len();
                self.charge(fuel::SLOT_COST)?;
                let list = self.cow_vec_mut(items, len)?;
                Ok(list
                    .pop()
                    .map(|value| Value::Option(Some(Box::new(value))))
                    .unwrap_or(Value::Option(None)))
            }
            BuiltinMethodId::LIST_SORT => {
                let len = items.len();
                self.charge(fuel::sort_cost(len as u64))?;
                let list = self.cow_vec_mut(items, len)?;
                list.sort_by(value_ord);
                self.ensure_deadline()?;
                Ok(Value::Unit)
            }
            BuiltinMethodId::LIST_INSERT => {
                let index = int_arg(&args, 0).unwrap_or(0);
                let value = args.into_iter().nth(1).unwrap_or(Value::Unit);
                let len = items.len();
                if index < 0 || index as usize > len {
                    return self.trap(self.panic("list insert index out of bounds", span));
                }
                let index = index as usize;
                self.charge(fuel::list_insert_cost(len as u64, index as u64))?;
                self.note_alloc(8);
                let list = self.cow_vec_mut(items, len)?;
                list.insert(index, value);
                Ok(Value::Unit)
            }
            BuiltinMethodId::LIST_REMOVE_AT => {
                let index = int_arg(&args, 0).unwrap_or(0);
                let len = items.len();
                if index < 0 || index as usize >= len {
                    return self.trap(self.panic("list index out of bounds", span));
                }
                let index = index as usize;
                self.charge(fuel::list_remove_cost(len as u64, index as u64))?;
                let list = self.cow_vec_mut(items, len)?;
                Ok(list.remove(index))
            }
            BuiltinMethodId::LIST_EXTEND => {
                let other = args.into_iter().next().unwrap_or(Value::Unit);
                if let Value::List(other) = other {
                    let len = items.len();
                    self.charge(fuel::slot_cost(other.len() as u64))?;
                    self.note_alloc(8 * other.len() as u64);
                    let list = self.cow_vec_mut(items, len)?;
                    list.extend_values(other.iter().cloned());
                }
                Ok(Value::Unit)
            }
            BuiltinMethodId::LIST_CLEAR => {
                self.charge(fuel::slot_cost(items.len() as u64))?;
                if Rc::strong_count(items) == 1 {
                    Rc::make_mut(items).clear();
                } else {
                    self.note_alloc(16);
                    *items = Rc::new(SeqValue::new(Vec::new()));
                }
                Ok(Value::Unit)
            }
            _ => self.trap(self.internal_panic(format!(
                "unknown list method `{}`",
                method.def().map_or("<invalid>", |def| def.name)
            ))),
        }
    }

    fn map_method(
        &mut self,
        method: BuiltinMethodId,
        receiver: &mut Value,
        args: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        let Value::Map(map) = receiver else {
            return self.trap(self.internal_panic("receiver is not a map"));
        };
        match method {
            BuiltinMethodId::MAP_LEN => Ok(Value::Int(map.len() as i64)),
            BuiltinMethodId::MAP_GET => {
                let key = args.first().cloned().unwrap_or(Value::Unit);
                self.charge(map_key_cost(&key))?;
                let key = self.map_key_ref_at(&key, span)?;
                Ok(map
                    .get(&key)
                    .cloned()
                    .map(|value| Value::Option(Some(Box::new(value))))
                    .unwrap_or(Value::Option(None)))
            }
            BuiltinMethodId::MAP_HAS => {
                let key = args.first().cloned().unwrap_or(Value::Unit);
                self.charge(map_key_cost(&key))?;
                let key = self.map_key_ref_at(&key, span)?;
                Ok(Value::Bool(map.get(&key).is_some()))
            }
            BuiltinMethodId::MAP_KEYS => {
                self.charge(fuel::slot_cost(map.len() as u64))?;
                Ok(Value::list(map.keys().iter().map(key_to_value).collect()))
            }
            BuiltinMethodId::MAP_VALUES => {
                self.charge(fuel::slot_cost(map.len() as u64))?;
                Ok(Value::list(map.values()))
            }
            BuiltinMethodId::MAP_ENTRIES => {
                self.charge(fuel::map_cost(map.len() as u64))?;
                Ok(Value::list(
                    map.entries()
                        .iter()
                        .map(|(key, value)| Value::tuple(vec![key_to_value(key), value.clone()]))
                        .collect(),
                ))
            }
            BuiltinMethodId::MAP_REMOVE => {
                let key = args.first().cloned().unwrap_or(Value::Unit);
                self.charge(map_key_cost(&key))?;
                let key = self.map_key_ref_at(&key, span)?;
                if map.get(&key).is_none() {
                    return Ok(Value::Option(None));
                }
                let len = map.len();
                let slots = len.saturating_mul(2);
                let map = self.cow_map_mut(map, slots)?;
                let (index, removed) = map
                    .remove_with_index(&key)
                    .expect("key was checked before mutation");
                self.charge(fuel::map_remove_cost(len as u64, index as u64))?;
                Ok(Value::Option(Some(Box::new(removed))))
            }
            BuiltinMethodId::MAP_CLEAR => {
                let len = map.len();
                self.charge(fuel::map_cost(len as u64))?;
                if Rc::strong_count(map) == 1 {
                    Rc::make_mut(map).clear();
                } else {
                    self.note_alloc(16);
                    *map = Rc::new(MapValue::new(OrderedMap::new()));
                }
                Ok(Value::Unit)
            }
            _ => self.trap(self.internal_panic(format!(
                "unknown map method `{}`",
                method.def().map_or("<invalid>", |def| def.name)
            ))),
        }
    }

    fn str_method(
        &mut self,
        method: BuiltinMethodId,
        receiver: &mut Value,
        args: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        let Value::Str(text) = receiver else {
            return self.trap(self.internal_panic("receiver is not a str"));
        };
        match method {
            BuiltinMethodId::STR_LEN => {
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(Value::Int(text.chars().count() as i64))
            }
            BuiltinMethodId::STR_CHARS => {
                self.charge(fuel::string_cost(text.len() as u64))?;
                let count = text.chars().count();
                let projected = 16u64
                    .saturating_add(8u64.saturating_mul(count as u64))
                    .saturating_add(16u64.saturating_mul(count as u64))
                    .saturating_add(text.len() as u64);
                self.ensure_output_capacity(projected)?;
                let mut out = Vec::new();
                if out.try_reserve_exact(count).is_err() {
                    return self.trap(Trap::OutOfMemory);
                }
                out.extend(
                    text.chars()
                        .map(|character| Value::str(character.to_string())),
                );
                Ok(Value::list(out))
            }
            BuiltinMethodId::STR_SPLIT => {
                let separator = args
                    .first()
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                self.charge(fuel::string_cost(text.len() as u64))?;
                let count = if separator.is_empty() {
                    text.chars().count()
                } else {
                    text.split(separator).count()
                };
                let projected = 16u64
                    .saturating_add(8u64.saturating_mul(count as u64))
                    .saturating_add(16u64.saturating_mul(count as u64))
                    .saturating_add(text.len() as u64);
                self.ensure_output_capacity(projected)?;
                let mut parts = Vec::new();
                if parts.try_reserve_exact(count).is_err() {
                    return self.trap(Trap::OutOfMemory);
                }
                if separator.is_empty() {
                    parts.extend(
                        text.chars()
                            .map(|character| Value::str(character.to_string())),
                    );
                } else {
                    parts.extend(text.split(separator).map(|part| Value::Str(Rc::from(part))));
                }
                Ok(Value::list(parts))
            }
            BuiltinMethodId::STR_TRIM => {
                self.charge(fuel::string_cost(text.len() as u64))?;
                let out = text.trim();
                self.ensure_output_capacity(16u64.saturating_add(out.len() as u64))?;
                Ok(Value::Str(Rc::from(out)))
            }
            BuiltinMethodId::STR_CONTAINS => {
                let needle = args
                    .first()
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(Value::Bool(text.contains(needle)))
            }
            BuiltinMethodId::STR_FIND => {
                let needle = args
                    .first()
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(text
                    .find(needle)
                    .map(|byte| Value::Int(text[..byte].chars().count() as i64))
                    .map(|value| Value::Option(Some(Box::new(value))))
                    .unwrap_or(Value::Option(None)))
            }
            BuiltinMethodId::STR_STARTS_WITH => {
                let needle = args
                    .first()
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(Value::Bool(text.starts_with(needle)))
            }
            BuiltinMethodId::STR_ENDS_WITH => {
                let needle = args
                    .first()
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(Value::Bool(text.ends_with(needle)))
            }
            BuiltinMethodId::STR_REPLACE => {
                let from = args
                    .first()
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let to = args
                    .get(1)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                if from.is_empty() {
                    return self.trap(self.panic("replace with an empty `from`", span));
                }
                // Charge the input scan before doing it, then the extra output
                // bytes when the replacement expands. `max(input, output)`
                // keeps small-input fuel unchanged.
                self.charge(fuel::string_cost(text.len() as u64))?;
                let mut out_len = text.len() as u64;
                let mut scan: &str = text;
                while let Some(index) = scan.find(from) {
                    let matched = from.len() as i64 - to.len() as i64;
                    if matched > 0 {
                        out_len = out_len.saturating_sub(matched as u64);
                    } else {
                        out_len = out_len.saturating_add((-matched) as u64);
                    }
                    scan = &scan[index + from.len()..];
                }
                let extra =
                    fuel::string_cost(out_len).saturating_sub(fuel::string_cost(text.len() as u64));
                self.charge(extra)?;
                self.ensure_output_capacity(16u64.saturating_add(out_len))?;
                let Ok(capacity) = usize::try_from(out_len) else {
                    return self.trap(Trap::OutOfMemory);
                };
                let mut out = String::new();
                if out.try_reserve_exact(capacity).is_err() {
                    return self.trap(Trap::OutOfMemory);
                }
                let mut rest: &str = text;
                while let Some(index) = rest.find(from) {
                    out.push_str(&rest[..index]);
                    out.push_str(to);
                    rest = &rest[index + from.len()..];
                }
                out.push_str(rest);
                Ok(Value::str(out))
            }
            BuiltinMethodId::STR_TO_UPPER => {
                let out = text.to_uppercase();
                self.charge(fuel::string_cost(out.len() as u64))?;
                Ok(Value::str(out))
            }
            BuiltinMethodId::STR_TO_LOWER => {
                let out = text.to_lowercase();
                self.charge(fuel::string_cost(out.len() as u64))?;
                Ok(Value::str(out))
            }
            BuiltinMethodId::STR_PARSE_INT => {
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(match parse_int(text) {
                    Some(value) => Value::Option(Some(Box::new(Value::Int(value)))),
                    None => Value::Option(None),
                })
            }
            BuiltinMethodId::STR_PARSE_FLOAT => {
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(match parse_float(text) {
                    Some(value) => Value::Option(Some(Box::new(Value::Float(value)))),
                    None => Value::Option(None),
                })
            }
            _ => self.trap(self.internal_panic(format!(
                "unknown str method `{}`",
                method.def().map_or("<invalid>", |def| def.name)
            ))),
        }
    }

    fn int_method(
        &mut self,
        method: BuiltinMethodId,
        receiver: &mut Value,
        args: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        let Value::Int(value) = receiver else {
            return self.trap(self.internal_panic("receiver is not an int"));
        };
        match method {
            BuiltinMethodId::INT_ABS => value
                .checked_abs()
                .map(Value::Int)
                .ok_or_else(|| EvalError::from(self.panic("integer overflow", span))),
            BuiltinMethodId::INT_POW => {
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
                        let exponent = u32::try_from(exponent)
                            .map_err(|_| EvalError::from(self.panic("integer overflow", span)))?;
                        value
                            .checked_pow(exponent)
                            .map(Value::Int)
                            .ok_or_else(|| EvalError::from(self.panic("integer overflow", span)))
                    }
                }
            }
            _ => self.trap(self.internal_panic(format!(
                "unknown int method `{}`",
                method.def().map_or("<invalid>", |def| def.name)
            ))),
        }
    }

    fn float_method(
        &mut self,
        method: BuiltinMethodId,
        receiver: &mut Value,
        args: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        let Value::Float(value) = receiver else {
            return self.trap(self.internal_panic("receiver is not a float"));
        };
        match method {
            BuiltinMethodId::FLOAT_ABS => Ok(Value::Float(libm::fabs(*value))),
            BuiltinMethodId::FLOAT_FLOOR => Ok(Value::Float(libm::floor(*value))),
            BuiltinMethodId::FLOAT_CEIL => Ok(Value::Float(libm::ceil(*value))),
            BuiltinMethodId::FLOAT_TRUNC => Ok(Value::Float(libm::trunc(*value))),
            BuiltinMethodId::FLOAT_SQRT => Ok(Value::Float(libm::sqrt(*value))),
            BuiltinMethodId::FLOAT_POW => {
                let exponent = args
                    .first()
                    .and_then(|value| match value {
                        Value::Float(value) => Some(*value),
                        _ => None,
                    })
                    .unwrap_or(0.0);
                Ok(Value::Float(libm::pow(*value, exponent)))
            }
            BuiltinMethodId::FLOAT_ROUND => {
                let digits = int_arg(&args, 0).unwrap_or(0);
                if digits < 0 {
                    return self.trap(self.panic("round digits must be non-negative", span));
                }
                if digits > 308 {
                    return Ok(Value::Float(*value));
                }
                let factor = 10f64.powi(digits as i32);
                Ok(Value::Float(libm::round(*value * factor) / factor))
            }
            BuiltinMethodId::FLOAT_IS_NAN => Ok(Value::Bool(value.is_nan())),
            BuiltinMethodId::FLOAT_IS_FINITE => Ok(Value::Bool(value.is_finite())),
            _ => self.trap(self.internal_panic(format!(
                "unknown float method `{}`",
                method.def().map_or("<invalid>", |def| def.name)
            ))),
        }
    }

    fn range_method(
        &mut self,
        method: BuiltinMethodId,
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
        match method {
            BuiltinMethodId::RANGE_LEN => Ok(Value::Int(len)),
            BuiltinMethodId::RANGE_CONTAINS => {
                let value = int_arg(&args, 0).unwrap_or(0);
                let inside = value >= *start
                    && if *inclusive {
                        value <= *end
                    } else {
                        value < *end
                    };
                Ok(Value::Bool(inside))
            }
            BuiltinMethodId::RANGE_TO_LIST => {
                let count = len.max(0) as u64;
                self.charge(fuel::slot_cost(count))?;
                let projected = 16u64.saturating_add(8u64.saturating_mul(count));
                self.ensure_output_capacity(projected)?;
                let Ok(capacity) = usize::try_from(count) else {
                    return self.trap(Trap::OutOfMemory);
                };
                let mut out = Vec::new();
                if out.try_reserve_exact(capacity).is_err() {
                    return self.trap(Trap::OutOfMemory);
                }
                let last = if *inclusive {
                    Some(*end)
                } else {
                    end.checked_sub(1)
                };
                if let Some(last) = last {
                    let mut value = *start;
                    while value <= last {
                        out.push(Value::Int(value));
                        if value == i64::MAX {
                            break;
                        }
                        value += 1;
                    }
                }
                Ok(Value::list(out))
            }
            _ => self.trap(self.internal_panic(format!(
                "unknown range method `{}`",
                method.def().map_or("<invalid>", |def| def.name)
            ))),
        }
    }

    fn option_method(
        &mut self,
        method: BuiltinMethodId,
        receiver: &mut Value,
        args: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        let Value::Option(option) = std::mem::replace(receiver, Value::Unit) else {
            return self.trap(self.internal_panic("receiver is not an Option"));
        };
        match method {
            BuiltinMethodId::OPTION_IS_SOME => Ok(Value::Bool(option.is_some())),
            BuiltinMethodId::OPTION_IS_NONE => Ok(Value::Bool(option.is_none())),
            BuiltinMethodId::OPTION_MAP => {
                let closure = self.closure_arg(&args, 0)?;
                match option {
                    Some(value) => Ok(Value::Option(Some(Box::new(
                        self.call_closure_value(closure, smallvec![*value])
                            .map_err(EvalError::from)?,
                    )))),
                    None => Ok(Value::Option(None)),
                }
            }
            BuiltinMethodId::OPTION_AND_THEN => {
                let closure = self.closure_arg(&args, 0)?;
                match option {
                    Some(value) => self
                        .call_closure_value(closure, smallvec![*value])
                        .map_err(EvalError::from),
                    None => Ok(Value::Option(None)),
                }
            }
            BuiltinMethodId::OPTION_UNWRAP_OR => Ok(match option {
                Some(value) => *value,
                None => args.into_iter().next().unwrap_or(Value::Unit),
            }),
            BuiltinMethodId::OPTION_OK_OR => Ok(match option {
                Some(value) => Value::Result(Ok(value)),
                None => Value::Result(Err(Box::new(
                    args.into_iter().next().unwrap_or(Value::Unit),
                ))),
            }),
            BuiltinMethodId::OPTION_EXPECT => match option {
                Some(value) => Ok(*value),
                None => {
                    let message = args
                        .first()
                        .and_then(|value| value.as_str().map(str::to_string))
                        .unwrap_or_else(|| "called `expect` on None".into());
                    self.trap(self.panic(message, span))
                }
            },
            _ => self.trap(self.internal_panic(format!(
                "unknown Option method `{}`",
                method.def().map_or("<invalid>", |def| def.name)
            ))),
        }
    }

    fn result_method(
        &mut self,
        method: BuiltinMethodId,
        receiver: &mut Value,
        args: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        let Value::Result(result) = std::mem::replace(receiver, Value::Unit) else {
            return self.trap(self.internal_panic("receiver is not a Result"));
        };
        match method {
            BuiltinMethodId::RESULT_IS_OK => Ok(Value::Bool(result.is_ok())),
            BuiltinMethodId::RESULT_IS_ERR => Ok(Value::Bool(result.is_err())),
            BuiltinMethodId::RESULT_MAP => {
                let closure = self.closure_arg(&args, 0)?;
                match result {
                    Ok(value) => Ok(Value::Result(Ok(Box::new(
                        self.call_closure_value(closure, smallvec![*value])
                            .map_err(EvalError::from)?,
                    )))),
                    Err(error) => Ok(Value::Result(Err(error))),
                }
            }
            BuiltinMethodId::RESULT_MAP_ERR => {
                let closure = self.closure_arg(&args, 0)?;
                match result {
                    Ok(value) => Ok(Value::Result(Ok(value))),
                    Err(error) => Ok(Value::Result(Err(Box::new(
                        self.call_closure_value(closure, smallvec![*error])
                            .map_err(EvalError::from)?,
                    )))),
                }
            }
            BuiltinMethodId::RESULT_AND_THEN => {
                let closure = self.closure_arg(&args, 0)?;
                match result {
                    Ok(value) => self
                        .call_closure_value(closure, smallvec![*value])
                        .map_err(EvalError::from),
                    Err(error) => Ok(Value::Result(Err(error))),
                }
            }
            BuiltinMethodId::RESULT_UNWRAP_OR => Ok(match result {
                Ok(value) => *value,
                Err(_) => args.into_iter().next().unwrap_or(Value::Unit),
            }),
            BuiltinMethodId::RESULT_OK => Ok(match result {
                Ok(value) => Value::Option(Some(value)),
                Err(_) => Value::Option(None),
            }),
            BuiltinMethodId::RESULT_EXPECT => match result {
                Ok(value) => Ok(*value),
                Err(_) => {
                    let message = args
                        .first()
                        .and_then(|value| value.as_str().map(str::to_string))
                        .unwrap_or_else(|| "called `expect` on Err".into());
                    self.trap(self.panic(message, span))
                }
            },
            _ => self.trap(self.internal_panic(format!(
                "unknown Result method `{}`",
                method.def().map_or("<invalid>", |def| def.name)
            ))),
        }
    }

    fn closure_arg<'a>(&self, args: &'a [Value], index: usize) -> EvalResult<&'a FnValue> {
        match args.get(index) {
            Some(Value::Fn(function)) => Ok(function),
            _ => self.trap(self.internal_panic("expected a function argument")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::KeyValue;
    use smallvec::smallvec;

    fn limits(fuel: u64, memory: u64) -> ExecLimits {
        ExecLimits {
            fuel,
            memory,
            depth: 200,
            deadline: None,
        }
    }

    fn trap_of(result: EvalResult<Value>) -> Trap {
        match result {
            Err(EvalError::Trap(trap)) => *trap,
            Ok(value) => panic!("expected a trap, got {value:?}"),
            Err(other) => panic!("expected a trap, got {other:?}"),
        }
    }

    fn span() -> martite_base::Span {
        martite_base::Span::empty(0)
    }

    fn nested_json(levels: usize) -> String {
        format!("{}0{}", "[".repeat(levels), "]".repeat(levels))
    }

    #[test]
    fn print_log_is_counted_and_capped() {
        let program = IrProgram::default();
        let mut runtime = IrRuntime::new(&program, limits(1_000_000, 1024));
        let text = Value::str("abcdefgh");
        let mut trapped = false;
        for _ in 0..100 {
            match runtime.call_builtin_fn_values(
                BuiltinFnId::PRINT,
                std::slice::from_ref(&text),
                span(),
            ) {
                Ok(_) => {}
                Err(EvalError::Trap(trap)) if matches!(*trap, Trap::OutOfMemory) => {
                    trapped = true;
                    break;
                }
                Err(other) => panic!("unexpected error: {other:?}"),
            }
        }
        assert!(trapped, "the log must be capped by the memory limit");
        assert!(runtime.log.len() < 100, "the log must stop growing");
        assert!(
            runtime.log_bytes <= 1024 + 24,
            "the log census overshot by more than one push: {}",
            runtime.log_bytes
        );
    }

    #[test]
    fn range_to_list_checks_projected_memory_before_allocating() {
        let program = IrProgram::default();
        let mut runtime = IrRuntime::new(&program, limits(1_000_000, 4096));
        let mut range = Value::Range {
            start: 0,
            end: 300_000,
            inclusive: false,
        };
        let result = runtime.apply_builtin_method(
            BuiltinMethodId::RANGE_TO_LIST,
            &mut range,
            CallArgs::new(),
            span(),
        );
        assert!(matches!(trap_of(result), Trap::OutOfMemory));
    }

    #[test]
    fn range_to_list_keeps_its_fuel_and_result_for_small_ranges() {
        let program = IrProgram::default();
        let mut runtime = IrRuntime::new(&program, limits(1000, 8 << 20));
        let mut range = Value::Range {
            start: 0,
            end: 4,
            inclusive: false,
        };
        let value = runtime
            .apply_builtin_method(
                BuiltinMethodId::RANGE_TO_LIST,
                &mut range,
                CallArgs::new(),
                span(),
            )
            .expect("small range");
        let Value::List(items) = value else {
            panic!("expected a list");
        };
        assert_eq!(items.len(), 4);
        assert_eq!(runtime.fuel_used(), 4);
    }

    #[test]
    fn json_parse_returns_recoverable_errors_for_deep_nesting() {
        let program = IrProgram::default();
        let mut runtime = IrRuntime::new(&program, limits(10_000_000, 8 << 20));

        let parse = |runtime: &mut IrRuntime<'_, '_>, levels: usize| {
            let text = Value::str(nested_json(levels));
            runtime
                .call_builtin_item_values(BuiltinItemId::JSON_PARSE, &[text], span())
                .expect("JSON parse returns a Result")
        };

        let Value::Result(Err(message)) = parse(&mut runtime, 70) else {
            panic!("70-level JSON must be a recoverable error");
        };
        assert_eq!(
            message.as_str(),
            Some("JSON nesting is too deep"),
            "conversion must reject before the value-depth trap"
        );

        let Value::Result(Err(_)) = parse(&mut runtime, 130) else {
            panic!("130-level JSON must stay a recoverable error");
        };

        let Value::Result(Ok(value)) = parse(&mut runtime, 63) else {
            panic!("63-level JSON must convert below the value-depth ceiling");
        };
        assert!(!value_depth_exceeds(&value, MAX_VALUE_DEPTH));
    }

    #[test]
    fn json_parse_traps_before_building_past_the_memory_limit() {
        let program = IrProgram::default();
        let mut runtime = IrRuntime::new(&program, limits(10_000_000, 4096));
        let text = Value::str(format!("[{}]", vec!["0"; 50_000].join(",")));
        let result = runtime.call_builtin_item_values(BuiltinItemId::JSON_PARSE, &[text], span());
        assert!(matches!(trap_of(result), Trap::OutOfMemory));
    }

    #[test]
    fn large_input_scans_trap_before_exceeding_fuel() {
        let program = IrProgram::default();
        let big = "x".repeat(100_000);

        let mut runtime = IrRuntime::new(&program, limits(1000, 8 << 20));
        let mut text = Value::str(big.clone());
        let result = runtime.apply_builtin_method(
            BuiltinMethodId::STR_TRIM,
            &mut text,
            CallArgs::new(),
            span(),
        );
        assert!(matches!(trap_of(result), Trap::OutOfFuel), "trim");

        let mut runtime = IrRuntime::new(&program, limits(1000, 8 << 20));
        let mut text = Value::str(big.clone());
        let args: CallArgs = smallvec![Value::str(big.clone()), Value::str("")];
        let result =
            runtime.apply_builtin_method(BuiltinMethodId::STR_REPLACE, &mut text, args, span());
        assert!(matches!(trap_of(result), Trap::OutOfFuel), "replace");

        let mut runtime = IrRuntime::new(&program, limits(1000, 8 << 20));
        let result = runtime.index_value(
            Value::str(big.clone()),
            Value::Range {
                start: 0,
                end: 0,
                inclusive: false,
            },
            span(),
        );
        assert!(matches!(trap_of(result), Trap::OutOfFuel), "slice");

        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("k".into()), Value::Int(1));
        let mut runtime = IrRuntime::new(&program, limits(1000, 8 << 20));
        let mut receiver = Value::map(map);
        let args: CallArgs = smallvec![Value::str(big.clone())];
        let result =
            runtime.apply_builtin_method(BuiltinMethodId::MAP_HAS, &mut receiver, args, span());
        assert!(matches!(trap_of(result), Trap::OutOfFuel), "map key");

        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("k".into()), Value::Int(1));
        let mut runtime = IrRuntime::new(&program, limits(1000, 8 << 20));
        let result = runtime.index_value(Value::map(map), Value::str(big.clone()), span());
        assert!(matches!(trap_of(result), Trap::OutOfFuel), "map index key");

        let items: Vec<Value> = (0..100).map(|_| Value::str("x".repeat(100))).collect();
        let mut runtime = IrRuntime::new(&program, limits(500, 8 << 20));
        let mut receiver = Value::list(items);
        let args: CallArgs = smallvec![Value::str("y".repeat(100))];
        let result = runtime.apply_builtin_method(
            BuiltinMethodId::LIST_CONTAINS,
            &mut receiver,
            args,
            span(),
        );
        assert!(matches!(trap_of(result), Trap::OutOfFuel), "contains");
    }

    #[test]
    fn join_checks_projected_memory_before_building() {
        let program = IrProgram::default();
        let mut runtime = IrRuntime::new(&program, limits(1_000_000, 4096));
        let items = (0..10).map(|_| Value::str("x".repeat(1000))).collect();
        let mut receiver = Value::list(items);
        let args: CallArgs = smallvec![Value::str(",")];
        let result =
            runtime.apply_builtin_method(BuiltinMethodId::LIST_JOIN, &mut receiver, args, span());
        assert!(matches!(trap_of(result), Trap::OutOfMemory));
    }

    #[test]
    fn replace_checks_projected_memory_before_building() {
        let program = IrProgram::default();
        let mut runtime = IrRuntime::new(&program, limits(1_000_000, 4096));
        let mut text = Value::str("a".repeat(1000));
        let args: CallArgs = smallvec![Value::str("a"), Value::str("b".repeat(100))];
        let result =
            runtime.apply_builtin_method(BuiltinMethodId::STR_REPLACE, &mut text, args, span());
        assert!(matches!(trap_of(result), Trap::OutOfMemory));
    }

    #[test]
    fn large_input_fuel_scales_with_bytes_scanned() {
        let program = IrProgram::default();

        let mut runtime = IrRuntime::new(&program, limits(1_000_000, 8 << 20));
        let mut text = Value::str(" ".repeat(8000));
        runtime
            .apply_builtin_method(
                BuiltinMethodId::STR_TRIM,
                &mut text,
                CallArgs::new(),
                span(),
            )
            .expect("trim");
        assert_eq!(runtime.fuel_used(), 1000, "trim scans the input");

        let mut runtime = IrRuntime::new(&program, limits(1_000_000, 8 << 20));
        runtime
            .index_value(
                Value::str("x".repeat(8000)),
                Value::Range {
                    start: 0,
                    end: 0,
                    inclusive: false,
                },
                span(),
            )
            .expect("slice");
        assert_eq!(runtime.fuel_used(), 1000, "slice scans the input");

        let mut runtime = IrRuntime::new(&program, limits(1_000_000, 8 << 20));
        let mut text = Value::str("x".repeat(8000));
        let args: CallArgs = smallvec![Value::str("x".repeat(8000)), Value::str("")];
        runtime
            .apply_builtin_method(BuiltinMethodId::STR_REPLACE, &mut text, args, span())
            .expect("replace");
        assert_eq!(runtime.fuel_used(), 1000, "replace scans the input");

        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("k".into()), Value::Int(1));
        let mut runtime = IrRuntime::new(&program, limits(1_000_000, 8 << 20));
        let mut receiver = Value::map(map);
        let args: CallArgs = smallvec![Value::str("x".repeat(8000))];
        runtime
            .apply_builtin_method(BuiltinMethodId::MAP_HAS, &mut receiver, args, span())
            .expect("map has");
        assert_eq!(runtime.fuel_used(), 999, "map keys charge their bytes");

        let items: Vec<Value> = (0..10).map(|_| Value::str("x".repeat(80))).collect();
        let mut runtime = IrRuntime::new(&program, limits(1_000_000, 8 << 20));
        let mut receiver = Value::list(items);
        let args: CallArgs = smallvec![Value::str("y".repeat(80))];
        runtime
            .apply_builtin_method(BuiltinMethodId::LIST_CONTAINS, &mut receiver, args, span())
            .expect("contains");
        assert_eq!(
            runtime.fuel_used(),
            110,
            "contains charges a comparison per element"
        );
    }

    #[test]
    fn small_input_fuel_is_unchanged() {
        let program = IrProgram::default();

        let mut runtime = IrRuntime::new(&program, limits(1000, 8 << 20));
        let mut text = Value::str("  hi  ");
        runtime
            .apply_builtin_method(
                BuiltinMethodId::STR_TRIM,
                &mut text,
                CallArgs::new(),
                span(),
            )
            .expect("small trim");
        assert_eq!(runtime.fuel_used(), 1, "trim scan unit");

        let mut runtime = IrRuntime::new(&program, limits(1000, 8 << 20));
        runtime
            .index_value(
                Value::str("abcdefgh"),
                Value::Range {
                    start: 0,
                    end: 4,
                    inclusive: false,
                },
                span(),
            )
            .expect("small slice");
        assert_eq!(runtime.fuel_used(), 1, "slice scan unit");

        let mut runtime = IrRuntime::new(&program, limits(1000, 8 << 20));
        let mut text = Value::str("Hello");
        let args: CallArgs = smallvec![Value::str("Hello"), Value::str("Bye")];
        runtime
            .apply_builtin_method(BuiltinMethodId::STR_REPLACE, &mut text, args, span())
            .expect("small replace");
        assert_eq!(runtime.fuel_used(), 1, "replace scan unit");

        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("a".into()), Value::Int(1));
        let mut runtime = IrRuntime::new(&program, limits(1000, 8 << 20));
        let mut receiver = Value::map(map);
        let args: CallArgs = smallvec![Value::str("a")];
        runtime
            .apply_builtin_method(BuiltinMethodId::MAP_HAS, &mut receiver, args, span())
            .expect("small map lookup");
        assert_eq!(runtime.fuel_used(), 0, "small keys keep their old fuel");

        let mut runtime = IrRuntime::new(&program, limits(1000, 8 << 20));
        let mut receiver = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let args: CallArgs = smallvec![Value::Int(3)];
        runtime
            .apply_builtin_method(BuiltinMethodId::LIST_CONTAINS, &mut receiver, args, span())
            .expect("small contains");
        assert_eq!(runtime.fuel_used(), 3, "one unit per element");
    }

    fn receiver_for(receiver: BuiltinReceiver) -> Value {
        match receiver {
            BuiltinReceiver::List => Value::list(Vec::new()),
            BuiltinReceiver::Map => Value::map(OrderedMap::new()),
            BuiltinReceiver::Str => Value::str(""),
            BuiltinReceiver::Int => Value::Int(1),
            BuiltinReceiver::Float => Value::Float(1.0),
            BuiltinReceiver::Range => Value::Range {
                start: 0,
                end: 0,
                inclusive: false,
            },
            BuiltinReceiver::Option => Value::Option(None),
            BuiltinReceiver::Result => Value::Result(Ok(Box::new(Value::Int(1)))),
        }
    }

    fn option_int(value: &Value) -> Option<Option<i64>> {
        match value {
            Value::Option(Some(inner)) => Some(inner.as_int()),
            Value::Option(None) => Some(None),
            _ => None,
        }
    }

    #[test]
    fn every_canonical_builtin_method_reaches_a_dispatch_arm() {
        for (index, def) in martite_ir::BUILTIN_METHODS.iter().enumerate() {
            let program = IrProgram::default();
            let mut runtime = IrRuntime::new(&program, limits(1_000_000, 8 << 20));
            let mut receiver = receiver_for(def.receiver);
            let result = runtime.apply_builtin_method(
                BuiltinMethodId::new(index as u32),
                &mut receiver,
                CallArgs::new(),
                span(),
            );
            if let Err(EvalError::Trap(trap)) = result {
                if let Trap::Panic { message, .. } = *trap {
                    assert!(
                        !message.contains("unknown"),
                        "{} {} fell through to the dispatch fallback: {message}",
                        def.receiver.as_str(),
                        def.name
                    );
                }
            }
        }
    }

    #[test]
    fn builtin_method_dispatch_by_id_keeps_results() {
        let program = IrProgram::default();
        let mut runtime = IrRuntime::new(&program, limits(1_000_000, 8 << 20));

        let mut list = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let result = runtime
            .apply_builtin_method(
                BuiltinMethodId::LIST_LEN,
                &mut list,
                CallArgs::new(),
                span(),
            )
            .expect("list len");
        assert!(matches!(result, Value::Int(3)));

        let mut list = Value::list(Vec::new());
        runtime
            .apply_builtin_method(
                BuiltinMethodId::LIST_PUSH,
                &mut list,
                smallvec![Value::Int(7)],
                span(),
            )
            .expect("list push");
        let result = runtime
            .apply_builtin_method(
                BuiltinMethodId::LIST_LEN,
                &mut list,
                CallArgs::new(),
                span(),
            )
            .expect("list len after push");
        assert!(matches!(result, Value::Int(1)));

        let mut float = Value::Float(9.0);
        let result = runtime
            .apply_builtin_method(
                BuiltinMethodId::FLOAT_SQRT,
                &mut float,
                CallArgs::new(),
                span(),
            )
            .expect("float sqrt");
        let Value::Float(value) = result else {
            panic!("expected a float");
        };
        assert_eq!(value, 3.0);

        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("k".into()), Value::Int(7));
        let mut receiver = Value::map(map);
        let result = runtime
            .apply_builtin_method(
                BuiltinMethodId::MAP_GET,
                &mut receiver,
                smallvec![Value::str("k")],
                span(),
            )
            .expect("map get");
        let Value::Option(Some(value)) = result else {
            panic!("expected Some");
        };
        assert!(matches!(*value, Value::Int(7)));
    }

    #[test]
    fn builtin_module_items_dispatch_by_id() {
        let program = IrProgram::default();
        let mut runtime = IrRuntime::new(&program, limits(1_000_000, 8 << 20));

        let result = runtime
            .call_builtin_item_values(
                BuiltinItemId::MATH_MIN,
                &[Value::Int(3), Value::Int(5)],
                span(),
            )
            .expect("math min");
        assert!(matches!(result, Value::Int(3)));

        let result = runtime
            .call_builtin_item_values(
                BuiltinItemId::MATH_MAX,
                &[Value::Int(3), Value::Int(5)],
                span(),
            )
            .expect("math max");
        assert!(matches!(result, Value::Int(5)));

        let result = runtime
            .call_builtin_item_values(
                BuiltinItemId::MATH_CLAMP,
                &[Value::Int(9), Value::Int(0), Value::Int(7)],
                span(),
            )
            .expect("math clamp");
        assert!(matches!(result, Value::Int(7)));

        for item in [
            BuiltinItemId::MATH_SIN,
            BuiltinItemId::MATH_COS,
            BuiltinItemId::MATH_TAN,
            BuiltinItemId::MATH_EXP,
            BuiltinItemId::MATH_LN,
            BuiltinItemId::MATH_LOG10,
        ] {
            let result = runtime
                .call_builtin_item_values(item, &[Value::Float(0.0)], span())
                .unwrap_or_else(|error| panic!("{item:?}: {error:?}"));
            assert!(matches!(result, Value::Float(_)));
        }

        let result = runtime
            .call_builtin_item_values(
                BuiltinItemId::MATH_ATAN2,
                &[Value::Float(0.0), Value::Float(1.0)],
                span(),
            )
            .expect("math atan2");
        assert!(matches!(result, Value::Float(_)));

        let parsed = runtime
            .call_builtin_item_values(BuiltinItemId::JSON_PARSE, &[Value::str("null")], span())
            .expect("json parse");
        let Value::Result(Ok(value)) = parsed else {
            panic!("expected a successful parse");
        };
        let stringified = runtime
            .call_builtin_item_values(BuiltinItemId::JSON_STRINGIFY, &[*value], span())
            .expect("json stringify");
        assert_eq!(stringified.as_str(), Some("null"));
    }

    #[test]
    fn malformed_builtin_ids_trap_defensively() {
        let program = IrProgram::default();
        let mut runtime = IrRuntime::new(&program, limits(1_000_000, 8 << 20));

        let result = runtime.apply_builtin_method(
            BuiltinMethodId::new(999),
            &mut Value::Unit,
            CallArgs::new(),
            span(),
        );
        let Trap::Panic { message, .. } = trap_of(result) else {
            panic!("expected a panic");
        };
        assert!(message.contains("unknown builtin method"), "{message}");

        let result = runtime.call_builtin_item_values(BuiltinItemId::new(999), &[], span());
        let Trap::Panic { message, .. } = trap_of(result) else {
            panic!("expected a panic");
        };
        assert!(message.contains("unknown builtin module item"), "{message}");
    }

    #[test]
    fn map_lookup_methods_accept_borrowed_keys() {
        let program = IrProgram::default();
        let mut runtime = IrRuntime::new(&program, limits(1_000_000, 8 << 20));

        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("b".into()), Value::Int(2));
        map.insert(KeyValue::Str("a".into()), Value::Int(1));
        map.insert(KeyValue::Int(7), Value::Int(3));
        map.insert(KeyValue::Bool(false), Value::Int(4));
        map.insert(
            KeyValue::Tuple(vec![KeyValue::Str("pair".into()), KeyValue::Int(2)]),
            Value::Int(5),
        );
        let mut receiver = Value::map(map);

        let args: CallArgs = smallvec![Value::str("a")];
        let result = runtime
            .apply_builtin_method(BuiltinMethodId::MAP_GET, &mut receiver, args, span())
            .expect("str get");
        assert_eq!(option_int(&result), Some(Some(1)));

        let args: CallArgs = smallvec![Value::Int(7)];
        let result = runtime
            .apply_builtin_method(BuiltinMethodId::MAP_HAS, &mut receiver, args, span())
            .expect("int has");
        assert!(matches!(result, Value::Bool(true)));

        let args: CallArgs = smallvec![Value::Bool(false)];
        let result = runtime
            .apply_builtin_method(BuiltinMethodId::MAP_GET, &mut receiver, args, span())
            .expect("bool get");
        assert_eq!(option_int(&result), Some(Some(4)));

        let pair = Value::tuple(vec![Value::str("pair"), Value::Int(2)]);
        let result = runtime
            .index_value(receiver.clone(), pair.clone(), span())
            .expect("tuple index");
        assert_eq!(result.as_int(), Some(5));

        let args: CallArgs = smallvec![pair];
        let result = runtime
            .apply_builtin_method(BuiltinMethodId::MAP_REMOVE, &mut receiver, args, span())
            .expect("tuple remove");
        assert_eq!(option_int(&result), Some(Some(5)));

        let Value::Map(map) = &receiver else {
            panic!("receiver is a map");
        };
        let order: Vec<KeyValue> = map.entries().iter().map(|(key, _)| key.clone()).collect();
        assert_eq!(
            order,
            vec![
                KeyValue::Str("b".into()),
                KeyValue::Str("a".into()),
                KeyValue::Int(7),
                KeyValue::Bool(false),
            ],
            "borrowed lookup and removal preserve insertion order"
        );

        let args: CallArgs = smallvec![Value::str("b")];
        let result = runtime
            .apply_builtin_method(BuiltinMethodId::MAP_REMOVE, &mut receiver, args, span())
            .expect("str remove");
        assert_eq!(option_int(&result), Some(Some(2)));

        let args: CallArgs = smallvec![Value::str("missing")];
        let result = runtime
            .apply_builtin_method(BuiltinMethodId::MAP_REMOVE, &mut receiver, args, span())
            .expect("missing remove");
        assert_eq!(option_int(&result), Some(None));

        let Value::Map(map) = &receiver else {
            panic!("receiver is a map");
        };
        let order: Vec<KeyValue> = map.entries().iter().map(|(key, _)| key.clone()).collect();
        assert_eq!(
            order,
            vec![
                KeyValue::Str("a".into()),
                KeyValue::Int(7),
                KeyValue::Bool(false),
            ]
        );
    }

    #[test]
    fn map_lookup_methods_still_reject_non_key_values() {
        let program = IrProgram::default();
        let mut map = OrderedMap::new();
        map.insert(KeyValue::Int(1), Value::Int(1));
        let mut receiver = Value::map(map);

        for (method, name) in [
            (BuiltinMethodId::MAP_GET, "get"),
            (BuiltinMethodId::MAP_HAS, "has"),
            (BuiltinMethodId::MAP_REMOVE, "remove"),
        ] {
            let mut runtime = IrRuntime::new(&program, limits(1_000_000, 8 << 20));
            let args: CallArgs = smallvec![Value::list(vec![Value::Int(1)])];
            let trap = trap_of(runtime.apply_builtin_method(method, &mut receiver, args, span()));
            assert!(
                trap.panic_message()
                    .contains("value cannot be used as a map key"),
                "{name} must reject a list key"
            );
        }

        let mut runtime = IrRuntime::new(&program, limits(1_000_000, 8 << 20));
        let trap =
            trap_of(runtime.index_value(receiver.clone(), Value::tuple(vec![Value::Unit]), span()));
        assert!(
            trap.panic_message()
                .contains("value cannot be used as a map key"),
            "indexing must reject a tuple containing a non-key value"
        );
    }
}
