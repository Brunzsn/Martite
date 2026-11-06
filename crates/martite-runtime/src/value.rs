use std::collections::HashMap;
use std::rc::Rc;

use indexmap::IndexMap;
use martite_check::{BindingId, BuiltinFn, DefId, TypeId};
use martite_syntax::NodeId;

#[derive(Debug, Clone)]
pub enum Value {
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(Rc<str>),
    List(Rc<Vec<Value>>),
    Map(Rc<OrderedMap>),
    Tuple(Rc<Vec<Value>>),
    Range {
        start: i64,
        end: i64,
        inclusive: bool,
    },
    Struct(Rc<StructValue>),
    Enum(Rc<EnumValue>),
    Option(Option<Box<Value>>),
    Result(Result<Box<Value>, Box<Value>>),
    Fn(Rc<Closure>),
    Builtin(BuiltinFn),
    BuiltinModuleFn {
        module: String,
        name: String,
    },
    Handle(Rc<HandleValue>),
}

#[derive(Debug, Clone)]
pub struct StructValue {
    pub type_id: TypeId,
    pub fields: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct EnumValue {
    pub type_id: TypeId,
    pub variant: usize,
    pub payload: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct HandleValue {
    pub type_id: TypeId,
    pub token: String,
}

#[derive(Debug, Clone)]
pub struct Closure {
    pub target: ClosureTarget,
    /// Captured free values. `None` for free-function values, which cannot
    /// capture locals, and for lambdas with no free variables.
    pub captures: Option<Rc<HashMap<BindingId, Value>>>,
}

#[derive(Debug, Clone)]
pub enum ClosureTarget {
    Def(DefId),
    Lambda { node: NodeId },
}

#[derive(Debug, Clone, Default)]
pub struct OrderedMap {
    entries: IndexMap<Key, Value>,
}

impl OrderedMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, key: &Key) -> Option<&Value> {
        self.entries.get(key)
    }

    pub fn get_mut(&mut self, key: &Key) -> Option<&mut Value> {
        self.entries.get_mut(key)
    }

    /// Insert a key, keeping its position if it already exists.
    pub fn insert(&mut self, key: Key, value: Value) {
        self.entries.insert(key, value);
    }

    /// Remove a key in O(n), preserving the order of the remaining keys.
    pub fn remove(&mut self, key: &Key) -> Option<Value> {
        self.entries.shift_remove(key)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn entries(&self) -> &indexmap::map::Slice<Key, Value> {
        self.entries.as_slice()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&Key, &Value)> {
        self.entries.iter()
    }

    pub fn keys(&self) -> Vec<Key> {
        self.entries.keys().cloned().collect()
    }

    pub fn values(&self) -> Vec<Value> {
        self.entries.values().cloned().collect()
    }

    pub fn eq_as_set(&self, other: &OrderedMap) -> bool {
        if self.entries.len() != other.entries.len() {
            return false;
        }
        self.entries.iter().all(|(key, value)| {
            other
                .entries
                .get(key)
                .map(|other_value| other_value == value)
                .unwrap_or(false)
        })
    }
}

/// Maximum nesting of runtime values. This is independent of syntactic
/// nesting; it protects recursive traversals and drop glue from hostile values
/// built at runtime.
pub const MAX_VALUE_DEPTH: usize = 128;

/// True when `value` contains a container nested deeper than `limit`.
///
/// This is deliberately iterative: it is used to reject values before
/// recursive consumers (`Show`, equality, snapshots, drop glue) can see them.
pub fn value_depth_exceeds(value: &Value, limit: usize) -> bool {
    let mut stack = vec![(value, 1usize)];
    while let Some((value, depth)) = stack.pop() {
        if depth > limit {
            return true;
        }
        match value {
            Value::List(items) | Value::Tuple(items) => {
                for item in items.iter() {
                    stack.push((item, depth + 1));
                }
            }
            Value::Map(map) => {
                for (key, value) in map.entries() {
                    if key_depth_exceeds(key, limit.saturating_sub(depth)) {
                        return true;
                    }
                    stack.push((value, depth + 1));
                }
            }
            Value::Struct(value) => {
                for field in &value.fields {
                    stack.push((field, depth + 1));
                }
            }
            Value::Enum(value) => {
                for field in &value.payload {
                    stack.push((field, depth + 1));
                }
            }
            Value::Option(Some(value)) | Value::Result(Ok(value)) | Value::Result(Err(value)) => {
                stack.push((value, depth + 1));
            }
            Value::Fn(closure) => {
                if let Some(captures) = &closure.captures {
                    for value in captures.values() {
                        stack.push((value, depth + 1));
                    }
                }
            }
            Value::Unit
            | Value::Int(_)
            | Value::Float(_)
            | Value::Bool(_)
            | Value::Str(_)
            | Value::Range { .. }
            | Value::Option(None)
            | Value::Builtin(_)
            | Value::BuiltinModuleFn { .. }
            | Value::Handle(_) => {}
        }
    }
    false
}

fn key_depth_exceeds(key: &KeyValue, remaining: usize) -> bool {
    match key {
        KeyValue::Tuple(items) => {
            remaining == 0
                || items
                    .iter()
                    .any(|item| key_depth_exceeds(item, remaining - 1))
        }
        KeyValue::Struct(_, fields) | KeyValue::Enum(_, _, fields) => {
            remaining == 0
                || fields
                    .iter()
                    .any(|field| key_depth_exceeds(field, remaining - 1))
        }
        KeyValue::Option(Some(value))
        | KeyValue::Result(Ok(value))
        | KeyValue::Result(Err(value)) => remaining == 0 || key_depth_exceeds(value, remaining - 1),
        _ => false,
    }
}

pub type Key = KeyValue;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KeyValue {
    Int(i64),
    Bool(bool),
    Str(String),
    Tuple(Vec<KeyValue>),
    Struct(TypeId, Vec<KeyValue>),
    Enum(TypeId, usize, Vec<KeyValue>),
    Option(Option<Box<KeyValue>>),
    Result(Result<Box<KeyValue>, Box<KeyValue>>),
    Handle(TypeId, String),
}

pub fn key_from_value(value: &Value) -> Result<KeyValue, String> {
    Ok(match value {
        Value::Int(v) => KeyValue::Int(*v),
        Value::Bool(v) => KeyValue::Bool(*v),
        Value::Str(v) => KeyValue::Str(v.to_string()),
        Value::Tuple(items) => {
            KeyValue::Tuple(items.iter().map(key_from_value).collect::<Result<_, _>>()?)
        }
        Value::Struct(value) => KeyValue::Struct(
            value.type_id,
            value
                .fields
                .iter()
                .map(key_from_value)
                .collect::<Result<_, _>>()?,
        ),
        Value::Enum(value) => KeyValue::Enum(
            value.type_id,
            value.variant,
            value
                .payload
                .iter()
                .map(key_from_value)
                .collect::<Result<_, _>>()?,
        ),
        Value::Option(inner) => KeyValue::Option(
            inner
                .as_ref()
                .map(|v| key_from_value(v).map(Box::new))
                .transpose()?,
        ),
        Value::Result(result) => KeyValue::Result(match result {
            Ok(v) => Ok(Box::new(key_from_value(v)?)),
            Err(v) => Err(Box::new(key_from_value(v)?)),
        }),
        Value::Handle(handle) => KeyValue::Handle(handle.type_id, handle.token.clone()),
        _ => return Err("value cannot be used as a map key".into()),
    })
}

pub fn key_to_value(key: &KeyValue) -> Value {
    match key {
        KeyValue::Int(v) => Value::Int(*v),
        KeyValue::Bool(v) => Value::Bool(*v),
        KeyValue::Str(v) => Value::Str(Rc::from(v.as_str())),
        KeyValue::Tuple(items) => Value::Tuple(Rc::new(items.iter().map(key_to_value).collect())),
        KeyValue::Struct(id, fields) => Value::Struct(Rc::new(StructValue {
            type_id: *id,
            fields: fields.iter().map(key_to_value).collect(),
        })),
        KeyValue::Enum(id, variant, payload) => Value::Enum(Rc::new(EnumValue {
            type_id: *id,
            variant: *variant,
            payload: payload.iter().map(key_to_value).collect(),
        })),
        KeyValue::Option(inner) => Value::Option(inner.as_ref().map(|v| Box::new(key_to_value(v)))),
        KeyValue::Result(result) => Value::Result(match result {
            Ok(v) => Ok(Box::new(key_to_value(v))),
            Err(v) => Err(Box::new(key_to_value(v))),
        }),
        KeyValue::Handle(id, token) => Value::Handle(Rc::new(HandleValue {
            type_id: *id,
            token: token.clone(),
        })),
    }
}

impl Value {
    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(Rc::from(s.into()))
    }

    pub fn list(items: Vec<Value>) -> Value {
        Value::List(Rc::new(items))
    }

    pub fn tuple(items: Vec<Value>) -> Value {
        Value::Tuple(Rc::new(items))
    }

    pub fn map(map: OrderedMap) -> Value {
        Value::Map(Rc::new(map))
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(v) => Some(v),
            _ => None,
        }
    }

    /// Nominal live bytes of heap payloads, counting shared payloads once.
    pub fn size_bytes(&self, seen: &mut std::collections::HashSet<usize>) -> u64 {
        match self {
            Value::Unit | Value::Int(_) | Value::Float(_) | Value::Bool(_) => 0,
            Value::Str(s) => {
                let ptr = Rc::as_ptr(s) as *const () as usize;
                if !seen.insert(ptr) {
                    return 0;
                }
                16 + s.len() as u64
            }
            Value::List(items) | Value::Tuple(items) => {
                let ptr = Rc::as_ptr(items) as *const () as usize;
                if !seen.insert(ptr) {
                    return 0;
                }
                let mut total = 16 + 8 * items.len() as u64;
                for item in items.iter() {
                    total += item.size_bytes(seen);
                }
                total
            }
            Value::Map(map) => {
                let ptr = Rc::as_ptr(map) as *const () as usize;
                if !seen.insert(ptr) {
                    return 0;
                }
                let mut total = 16 + 16 * map.len() as u64;
                for (key, value) in map.entries() {
                    total += key_value_size(key);
                    total += value.size_bytes(seen);
                }
                total
            }
            Value::Range { .. } => 0,
            Value::Struct(value) => {
                let ptr = Rc::as_ptr(value) as *const () as usize;
                if !seen.insert(ptr) {
                    return 0;
                }
                let mut total = 16 + 8 * value.fields.len() as u64;
                for field in &value.fields {
                    total += field.size_bytes(seen);
                }
                total
            }
            Value::Enum(value) => {
                let ptr = Rc::as_ptr(value) as *const () as usize;
                if !seen.insert(ptr) {
                    return 0;
                }
                let mut total = 16 + 8 * value.payload.len() as u64;
                for field in &value.payload {
                    total += field.size_bytes(seen);
                }
                total
            }
            Value::Option(inner) => match inner {
                Some(v) => 16 + v.size_bytes(seen),
                None => 0,
            },
            Value::Result(result) => match result {
                Ok(v) | Err(v) => 16 + v.size_bytes(seen),
            },
            Value::Fn(closure) => {
                let ptr = Rc::as_ptr(closure) as *const () as usize;
                if !seen.insert(ptr) {
                    return 0;
                }
                let mut total = 16;
                if let Some(captures) = &closure.captures {
                    for value in captures.values() {
                        total += value.size_bytes(seen);
                    }
                }
                total
            }
            Value::Builtin(_) | Value::BuiltinModuleFn { .. } => 0,
            Value::Handle(handle) => {
                let ptr = Rc::as_ptr(handle) as *const () as usize;
                if !seen.insert(ptr) {
                    return 0;
                }
                16 + handle.token.len() as u64
            }
        }
    }
}

fn key_value_size(key: &KeyValue) -> u64 {
    match key {
        KeyValue::Int(_) | KeyValue::Bool(_) => 0,
        KeyValue::Str(s) => 16 + s.len() as u64,
        KeyValue::Tuple(items) => {
            16 + 8 * items.len() as u64 + items.iter().map(key_value_size).sum::<u64>()
        }
        KeyValue::Struct(_, fields) => {
            16 + 8 * fields.len() as u64 + fields.iter().map(key_value_size).sum::<u64>()
        }
        KeyValue::Enum(_, _, fields) => {
            16 + 8 * fields.len() as u64 + fields.iter().map(key_value_size).sum::<u64>()
        }
        KeyValue::Option(inner) => inner.as_ref().map(|k| 16 + key_value_size(k)).unwrap_or(0),
        KeyValue::Result(result) => match result {
            Ok(k) | Err(k) => 16 + key_value_size(k),
        },
        KeyValue::Handle(_, token) => 16 + token.len() as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordered_map_keeps_position_on_overwrite_and_reinsert() {
        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("a".into()), Value::Int(1));
        map.insert(KeyValue::Str("b".into()), Value::Int(2));
        map.insert(KeyValue::Str("a".into()), Value::Int(3));
        let entries: Vec<_> = map.entries().iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(
            entries,
            vec![KeyValue::Str("a".into()), KeyValue::Str("b".into())]
        );
        assert_eq!(map.get(&KeyValue::Str("a".into())), Some(&Value::Int(3)));
        map.remove(&KeyValue::Str("a".into()));
        map.insert(KeyValue::Str("a".into()), Value::Int(4));
        let entries: Vec<_> = map.entries().iter().map(|(k, _)| k.clone()).collect();
        assert_eq!(
            entries,
            vec![KeyValue::Str("b".into()), KeyValue::Str("a".into())]
        );
    }

    #[test]
    fn map_equality_ignores_order() {
        let mut a = OrderedMap::new();
        a.insert(KeyValue::Str("x".into()), Value::Int(1));
        a.insert(KeyValue::Str("y".into()), Value::Int(2));
        let mut b = OrderedMap::new();
        b.insert(KeyValue::Str("y".into()), Value::Int(2));
        b.insert(KeyValue::Str("x".into()), Value::Int(1));
        assert!(a.eq_as_set(&b));
    }

    #[test]
    fn value_depth_is_bounded() {
        let mut value = Value::Unit;
        for _ in 0..MAX_VALUE_DEPTH + 1 {
            value = Value::list(vec![value]);
        }
        assert!(value_depth_exceeds(&value, MAX_VALUE_DEPTH));
    }
}
