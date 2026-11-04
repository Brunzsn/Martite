use std::collections::HashMap;
use std::rc::Rc;

use martite_check::{BindingId, BuiltinFn, DefId, TypeId};
use martite_syntax::{LambdaBody, LambdaParam};

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
    pub captures: HashMap<BindingId, Value>,
}

#[derive(Debug, Clone)]
pub enum ClosureTarget {
    Def(DefId),
    Lambda {
        params: Vec<LambdaParam>,
        body: LambdaBody,
    },
}

#[derive(Debug, Clone)]
pub struct OrderedMap {
    entries: Vec<(Key, Value)>,
}

impl Default for OrderedMap {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
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
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    pub fn get_mut(&mut self, key: &Key) -> Option<&mut Value> {
        self.entries
            .iter_mut()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }

    /// Insert a key, keeping its position if it already exists.
    pub fn insert(&mut self, key: Key, value: Value) {
        if let Some((_, existing)) = self.entries.iter_mut().find(|(k, _)| *k == key) {
            *existing = value;
        } else {
            self.entries.push((key, value));
        }
    }

    /// Remove a key in O(n), preserving the order of the remaining keys.
    pub fn remove(&mut self, key: &Key) -> Option<Value> {
        let index = self.entries.iter().position(|(k, _)| k == key)?;
        Some(self.entries.remove(index).1)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn entries(&self) -> &[(Key, Value)] {
        &self.entries
    }

    pub fn keys(&self) -> Vec<Key> {
        self.entries.iter().map(|(k, _)| k.clone()).collect()
    }

    pub fn values(&self) -> Vec<Value> {
        self.entries.iter().map(|(_, v)| v.clone()).collect()
    }

    pub fn eq_as_set(&self, other: &OrderedMap) -> bool {
        if self.entries.len() != other.entries.len() {
            return false;
        }
        self.entries.iter().all(|(key, value)| {
            other
                .entries
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v == value)
                .unwrap_or(false)
        })
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
        Value::Tuple(items) => KeyValue::Tuple(
            items
                .iter()
                .map(key_from_value)
                .collect::<Result<_, _>>()?,
        ),
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
        KeyValue::Tuple(items) => {
            Value::Tuple(Rc::new(items.iter().map(key_to_value).collect()))
        }
        KeyValue::Struct(id, fields) => Value::Struct(Rc::new(StructValue {
            type_id: *id,
            fields: fields.iter().map(key_to_value).collect(),
        })),
        KeyValue::Enum(id, variant, payload) => Value::Enum(Rc::new(EnumValue {
            type_id: *id,
            variant: *variant,
            payload: payload.iter().map(key_to_value).collect(),
        })),
        KeyValue::Option(inner) => {
            Value::Option(inner.as_ref().map(|v| Box::new(key_to_value(v))))
        }
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
                for value in closure.captures.values() {
                    total += value.size_bytes(seen);
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
