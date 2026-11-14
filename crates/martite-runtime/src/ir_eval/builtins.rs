//! Builtin execution for the IR evaluator (#109).
//!
//! The method bodies mirror `builtins.rs` exactly. That file belongs to the
//! frozen AST interpreter; #116 removes it. Calls to user functions and
//! closures go through `call_closure_value`.

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
                self.charge(fuel::show_cost(value.meta().slots))?;
                let mut text = String::new();
                write_show(&mut text, &value, &self.program.types, true);
                self.charge(fuel::string_cost(text.len() as u64))?;
                self.log.push(text);
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
        let Some(def) = item.item() else {
            return self.trap(self.internal_panic("unknown builtin module item"));
        };
        match (def.module, def.name) {
            ("std.math", "min") | ("std.math", "max") => {
                let a = values.first().cloned().unwrap_or(Value::Unit);
                let b = values.get(1).cloned().unwrap_or(Value::Unit);
                let ord = value_ord(&a, &b);
                Ok(
                    if (def.name == "min" && ord.is_le()) || (def.name == "max" && ord.is_ge()) {
                        a
                    } else {
                        b
                    },
                )
            }
            ("std.math", "clamp") => {
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
            ("std.math", name @ ("sin" | "cos" | "tan" | "exp" | "ln" | "log10")) => {
                let Value::Float(x) = values.first().cloned().unwrap_or(Value::Float(0.0)) else {
                    return self.trap(self.internal_panic("expected float"));
                };
                Ok(Value::Float(match name {
                    "sin" => libm::sin(x),
                    "cos" => libm::cos(x),
                    "tan" => libm::tan(x),
                    "exp" => libm::exp(x),
                    "ln" => libm::log(x),
                    _ => libm::log10(x),
                }))
            }
            ("std.math", "atan2") => {
                let Value::Float(y) = values.first().cloned().unwrap_or(Value::Float(0.0)) else {
                    return self.trap(self.internal_panic("expected float"));
                };
                let Value::Float(x) = values.get(1).cloned().unwrap_or(Value::Float(0.0)) else {
                    return self.trap(self.internal_panic("expected float"));
                };
                Ok(Value::Float(libm::atan2(y, x)))
            }
            ("std.json", "parse") => {
                let text = values
                    .first()
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                self.charge(fuel::string_cost(text.len() as u64))?;
                if text.len() as u64 > self.limits.memory {
                    return Err(EvalError::Trap(Trap::OutOfMemory));
                }
                let json_type = self.program.entries.json_type;
                match crate::json::parse(&text, json_type) {
                    Ok(value) => {
                        self.check_value_depth(&value, span)?;
                        self.note_value(&value);
                        self.note_alloc(16);
                        Ok(Value::Result(Ok(Box::new(value))))
                    }
                    Err(message) => {
                        self.note_alloc(16 + 16 + message.len() as u64);
                        Ok(Value::Result(Err(Box::new(Value::str(message)))))
                    }
                }
            }
            ("std.json", "stringify") => {
                let value = values.first().cloned().unwrap_or(Value::Unit);
                let text = crate::json::stringify(&value, &self.program.types);
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
        let Some(arity) = method.arity() else {
            return self.trap(self.internal_panic("unknown builtin method"));
        };
        let receiver_kind = def.receiver;
        let name = def.name;
        let mut_self = def.mut_self;
        let function = self.current_function();
        let Some(receiver_id) = function.children.get(first_child as usize).copied() else {
            return self.trap(self.internal_panic("builtin method has no receiver"));
        };

        if !mut_self {
            let mut receiver = self.eval(receiver_id)?;
            let args = self.builtin_method_args(first_child, arity)?;
            let result =
                self.apply_builtin_method(receiver_kind, name, &mut receiver, args, span)?;
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
        self.check_builtin_mutation_depth(name, &args, place.accessors.len())?;

        let mut root = self.take_place_root(&place)?;
        let (result, root) = if place.accessors.is_empty() {
            drop(root);
            let result =
                self.apply_builtin_method(receiver_kind, name, &mut receiver, args, span)?;
            (result, receiver)
        } else {
            let replaced = self.take_path_value(&mut root, &place.accessors)?;
            drop(replaced);
            let result =
                self.apply_builtin_method(receiver_kind, name, &mut receiver, args, span)?;
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
                        .map(|value| Value::Option(Some(Box::new(value))))
                        .unwrap_or(Value::Option(None)),
                    _ => Value::Option(None),
                })
            }
            "first" => Ok(items
                .first()
                .cloned()
                .map(|value| Value::Option(Some(Box::new(value))))
                .unwrap_or(Value::Option(None))),
            "last" => Ok(items
                .last()
                .cloned()
                .map(|value| Value::Option(Some(Box::new(value))))
                .unwrap_or(Value::Option(None))),
            "contains" => {
                let needle = args.into_iter().next().unwrap_or(Value::Unit);
                self.charge(fuel::slot_cost(items.len() as u64))?;
                Ok(Value::Bool(
                    items.iter().any(|value| value_eq(value, &needle)),
                ))
            }
            "map" => {
                let closure = self.closure_arg(&args, 0)?;
                let snapshot = items.clone();
                self.charge(fuel::slot_cost(snapshot.len() as u64))?;
                let mut out = Vec::with_capacity(snapshot.len());
                for item in snapshot.iter() {
                    out.push(
                        self.call_closure_value(closure, smallvec![item.clone()])
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
                        .call_closure_value(closure, smallvec![item.clone()])
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
                        .call_closure_value(closure, smallvec![init, item.clone()])
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
                        self.call_closure_value(closure, smallvec![item.clone()])
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
                        self.call_closure_value(closure, smallvec![item.clone()])
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
                    .map(|(index, value)| {
                        Value::tuple(vec![Value::Int(index as i64), value.clone()])
                    })
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
                        .call_closure_value(closure, smallvec![item.clone()])
                        .map_err(EvalError::Trap)?;
                    keyed.push((key, item.clone()));
                }
                keyed.sort_by(|a, b| value_ord(&a.0, &b.0));
                self.ensure_deadline()?;
                Ok(Value::list(
                    keyed.into_iter().map(|(_, value)| value).collect(),
                ))
            }
            "join" => {
                let separator = args
                    .first()
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let mut out = String::new();
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push_str(separator);
                    }
                    if let Some(text) = item.as_str() {
                        out.push_str(text);
                    }
                }
                self.charge(fuel::slot_cost(items.len() as u64))?;
                self.charge(fuel::string_cost(out.len() as u64))?;
                Ok(Value::str(out))
            }
            "push" => {
                let value = args.into_iter().next().unwrap_or(Value::Unit);
                let len = items.len();
                self.charge(fuel::SLOT_COST)?;
                self.note_alloc(8);
                let list = self.cow_vec_mut(items, len)?;
                list.push(value);
                Ok(Value::Unit)
            }
            "pop" => {
                let len = items.len();
                self.charge(fuel::SLOT_COST)?;
                let list = self.cow_vec_mut(items, len)?;
                Ok(list
                    .pop()
                    .map(|value| Value::Option(Some(Box::new(value))))
                    .unwrap_or(Value::Option(None)))
            }
            "sort" => {
                let len = items.len();
                self.charge(fuel::sort_cost(len as u64))?;
                let list = self.cow_vec_mut(items, len)?;
                list.sort_by(value_ord);
                self.ensure_deadline()?;
                Ok(Value::Unit)
            }
            "insert" => {
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
            "remove_at" => {
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
            "extend" => {
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
                    .map_err(|message| EvalError::Trap(self.panic(message, span)))?;
                Ok(map
                    .get(&key)
                    .cloned()
                    .map(|value| Value::Option(Some(Box::new(value))))
                    .unwrap_or(Value::Option(None)))
            }
            "has" => {
                let key = args.first().cloned().unwrap_or(Value::Unit);
                let key = key_from_value(&key)
                    .map_err(|message| EvalError::Trap(self.panic(message, span)))?;
                Ok(Value::Bool(map.get(&key).is_some()))
            }
            "keys" => {
                self.charge(fuel::slot_cost(map.len() as u64))?;
                Ok(Value::list(map.keys().iter().map(key_to_value).collect()))
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
                        .map(|(key, value)| Value::tuple(vec![key_to_value(key), value.clone()]))
                        .collect(),
                ))
            }
            "remove" => {
                let key = args.first().cloned().unwrap_or(Value::Unit);
                let key = key_from_value(&key)
                    .map_err(|message| EvalError::Trap(self.panic(message, span)))?;
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
            "clear" => {
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
                        .map(|character| Value::str(character.to_string()))
                        .collect(),
                ))
            }
            "split" => {
                let separator = args
                    .first()
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let parts: Vec<Value> = if separator.is_empty() {
                    text.chars()
                        .map(|character| Value::str(character.to_string()))
                        .collect()
                } else {
                    text.split(separator)
                        .map(|part| Value::Str(Rc::from(part)))
                        .collect()
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
                let needle = args
                    .first()
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(Value::Bool(text.contains(needle)))
            }
            "find" => {
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
            "starts_with" => {
                let needle = args
                    .first()
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(Value::Bool(text.starts_with(needle)))
            }
            "ends_with" => {
                let needle = args
                    .first()
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(Value::Bool(text.ends_with(needle)))
            }
            "replace" => {
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
                    Some(value) => Value::Option(Some(Box::new(Value::Int(value)))),
                    None => Value::Option(None),
                })
            }
            "parse_float" => {
                self.charge(fuel::string_cost(text.len() as u64))?;
                Ok(match parse_float(text) {
                    Some(value) => Value::Option(Some(Box::new(Value::Float(value)))),
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
                        let exponent = u32::try_from(exponent)
                            .map_err(|_| EvalError::Trap(self.panic("integer overflow", span)))?;
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
                    .and_then(|value| match value {
                        Value::Float(value) => Some(*value),
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
                    && if *inclusive {
                        value <= *end
                    } else {
                        value < *end
                    };
                Ok(Value::Bool(inside))
            }
            "to_list" => {
                self.charge(fuel::slot_cost(len.max(0) as u64))?;
                let mut out = Vec::new();
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
                        self.call_closure_value(closure, smallvec![*value])
                            .map_err(EvalError::Trap)?,
                    )))),
                    None => Ok(Value::Option(None)),
                }
            }
            "and_then" => {
                let closure = self.closure_arg(&args, 0)?;
                match option {
                    Some(value) => self
                        .call_closure_value(closure, smallvec![*value])
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
                        .and_then(|value| value.as_str().map(str::to_string))
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
                        self.call_closure_value(closure, smallvec![*value])
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
                        self.call_closure_value(closure, smallvec![*error])
                            .map_err(EvalError::Trap)?,
                    )))),
                }
            }
            "and_then" => {
                let closure = self.closure_arg(&args, 0)?;
                match result {
                    Ok(value) => self
                        .call_closure_value(closure, smallvec![*value])
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
                        .and_then(|value| value.as_str().map(str::to_string))
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
