use std::collections::HashMap;
use std::ops::Deref;
use std::rc::Rc;

use indexmap::IndexMap;
use martite_check::{BindingId, BuiltinFn, BuiltinItemId, DefId, TypeId};
use martite_syntax::NodeId;

#[derive(Debug, Clone)]
pub enum Value {
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(Rc<str>),
    List(Rc<SeqValue>),
    Map(Rc<MapValue>),
    Tuple(Rc<SeqValue>),
    Range {
        start: i64,
        end: i64,
        inclusive: bool,
    },
    Struct(Rc<StructValue>),
    Enum(Rc<EnumValue>),
    Option(Option<Box<Value>>),
    Result(Result<Box<Value>, Box<Value>>),
    Fn(FnValue),
    Builtin(BuiltinFn),
    BuiltinModuleFn(BuiltinItemId),
    Handle(Rc<HandleValue>),
}

/// Cached, nominal metadata for a value.
///
/// `depth` mirrors the runtime value-nesting cap (a leaf is level 1).
/// `slots` and `string_slots` mirror the fuel table. `bytes` is nominal and
/// counts shared payloads at each occurrence; the authoritative memory census
/// still uses `Value::size_bytes` with its de-duplicating set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueMeta {
    pub depth: u16,
    pub slots: u64,
    pub string_slots: u64,
    pub bytes: u64,
}

impl ValueMeta {
    const fn leaf() -> Self {
        Self {
            depth: 1,
            slots: 0,
            string_slots: 0,
            bytes: 0,
        }
    }

    const fn empty_container() -> Self {
        Self {
            depth: 1,
            slots: 0,
            string_slots: 0,
            bytes: 16,
        }
    }
}

fn wrapped_meta(inner: ValueMeta) -> ValueMeta {
    ValueMeta {
        depth: 1 + inner.depth,
        slots: 1 + inner.slots,
        string_slots: inner.string_slots,
        bytes: 16 + inner.bytes,
    }
}

fn add_slot_meta(meta: &mut ValueMeta, deep_count: &mut u32, child: ValueMeta) {
    meta.slots = meta.slots.saturating_add(1).saturating_add(child.slots);
    meta.string_slots = meta.string_slots.saturating_add(child.string_slots);
    meta.bytes = meta.bytes.saturating_add(8).saturating_add(child.bytes);
    add_depth_meta(meta, deep_count, 1 + child.depth);
}

fn add_depth_meta(meta: &mut ValueMeta, deep_count: &mut u32, depth: u16) {
    if depth > meta.depth {
        meta.depth = depth;
        *deep_count = 1;
    } else if depth == meta.depth {
        *deep_count = deep_count.saturating_add(1);
    }
}

/// A list or tuple payload with cached metadata.
#[derive(Debug, Clone)]
pub struct SeqValue {
    items: Vec<Value>,
    meta: ValueMeta,
    deep_count: u32,
}

impl SeqValue {
    /// Build a sequence that adopts `items` and computes its metadata in place.
    pub fn new(items: Vec<Value>) -> Self {
        let mut meta = ValueMeta::empty_container();
        let mut deep_count = 0;
        for item in &items {
            add_slot_meta(&mut meta, &mut deep_count, item.meta());
        }
        Self {
            items,
            meta,
            deep_count,
        }
    }

    pub(crate) fn push(&mut self, value: Value) {
        let child = value.meta();
        self.items.push(value);
        self.add_slot(child);
    }

    pub(crate) fn pop(&mut self) -> Option<Value> {
        let value = self.items.pop()?;
        self.remove_slot(value.meta());
        Some(value)
    }

    pub(crate) fn insert(&mut self, index: usize, value: Value) {
        let child = value.meta();
        self.items.insert(index, value);
        self.add_slot(child);
    }

    pub(crate) fn remove(&mut self, index: usize) -> Value {
        let value = self.items.remove(index);
        self.remove_slot(value.meta());
        value
    }

    pub(crate) fn clear(&mut self) {
        self.items.clear();
        self.meta = ValueMeta::empty_container();
        self.deep_count = 0;
    }

    pub(crate) fn sort_by<F>(&mut self, compare: F)
    where
        F: FnMut(&Value, &Value) -> std::cmp::Ordering,
    {
        self.items.sort_by(compare);
    }

    pub(crate) fn extend_values<I: IntoIterator<Item = Value>>(&mut self, values: I) {
        for value in values {
            self.push(value);
        }
    }

    pub(crate) fn slot_mut(&mut self, index: usize) -> Option<&mut Value> {
        self.items.get_mut(index)
    }

    pub(crate) fn note_child_replaced(&mut self, old: ValueMeta, new: ValueMeta) {
        if old == new {
            return;
        }
        self.meta.slots = self
            .meta
            .slots
            .saturating_sub(old.slots)
            .saturating_add(new.slots);
        self.meta.string_slots = self
            .meta
            .string_slots
            .saturating_sub(old.string_slots)
            .saturating_add(new.string_slots);
        self.meta.bytes = self
            .meta
            .bytes
            .saturating_sub(old.bytes)
            .saturating_add(new.bytes);
        self.replace_depth(1 + old.depth, 1 + new.depth);
    }

    fn add_slot(&mut self, child: ValueMeta) {
        add_slot_meta(&mut self.meta, &mut self.deep_count, child);
    }

    fn remove_slot(&mut self, child: ValueMeta) {
        self.meta.slots = self
            .meta
            .slots
            .saturating_sub(1)
            .saturating_sub(child.slots);
        self.meta.string_slots = self.meta.string_slots.saturating_sub(child.string_slots);
        self.meta.bytes = self
            .meta
            .bytes
            .saturating_sub(8)
            .saturating_sub(child.bytes);
        self.remove_depth(1 + child.depth);
    }

    fn add_depth(&mut self, depth: u16) {
        add_depth_meta(&mut self.meta, &mut self.deep_count, depth);
    }

    fn remove_depth(&mut self, depth: u16) {
        if depth != self.meta.depth {
            return;
        }
        self.deep_count = self.deep_count.saturating_sub(1);
        if self.deep_count == 0 {
            self.recompute_depth();
        }
    }

    fn replace_depth(&mut self, old: u16, new: u16) {
        if old == self.meta.depth {
            if self.deep_count > 1 {
                self.deep_count -= 1;
            } else {
                // The replacement is already in `items`, so recomputing picks
                // up its contribution and we must not add it again.
                self.recompute_depth();
                return;
            }
        }
        self.add_depth(new);
    }

    fn recompute_depth(&mut self) {
        let mut depth = 1;
        let mut count = 0;
        for item in &self.items {
            let child = 1 + item.meta().depth;
            if child > depth {
                depth = child;
                count = 1;
            } else if child == depth {
                count += 1;
            }
        }
        self.meta.depth = depth;
        self.deep_count = count;
    }
}

impl Deref for SeqValue {
    type Target = Vec<Value>;

    fn deref(&self) -> &Self::Target {
        &self.items
    }
}

/// A map payload with cached metadata.
#[derive(Debug, Clone)]
pub struct MapValue {
    map: OrderedMap,
    meta: ValueMeta,
    deep_count: u32,
}

impl MapValue {
    pub fn new(map: OrderedMap) -> Self {
        let mut meta = ValueMeta::empty_container();
        let mut deep_count = 0u32;
        for (key, item) in map.entries() {
            let stats = KeyStats::new(key);
            let child = item.meta();
            meta.slots = meta.slots.saturating_add(2 + stats.slots + child.slots);
            meta.string_slots = meta
                .string_slots
                .saturating_add(stats.string_slots + child.string_slots);
            meta.bytes = meta.bytes.saturating_add(16 + stats.bytes + child.bytes);
            let depth = stats.entry_depth(child);
            if depth > meta.depth {
                meta.depth = depth;
                deep_count = 1;
            } else if depth == meta.depth {
                deep_count = deep_count.saturating_add(1);
            }
        }
        Self {
            map,
            meta,
            deep_count,
        }
    }

    pub(crate) fn insert(&mut self, key: Key, value: Value) -> Option<Value> {
        let stats = KeyStats::new(&key);
        let child = value.meta();
        let old = self.map.insert(key, value);
        if let Some(old_value) = &old {
            let old_meta = old_value.meta();
            self.meta.slots = self
                .meta
                .slots
                .saturating_sub(old_meta.slots)
                .saturating_add(child.slots);
            self.meta.string_slots = self
                .meta
                .string_slots
                .saturating_sub(old_meta.string_slots)
                .saturating_add(child.string_slots);
            self.meta.bytes = self
                .meta
                .bytes
                .saturating_sub(old_meta.bytes)
                .saturating_add(child.bytes);
            self.replace_depth(stats.entry_depth(old_meta), stats.entry_depth(child));
        } else {
            self.add_entry(stats, child);
        }
        old
    }

    pub(crate) fn remove_with_index(&mut self, key: &Key) -> Option<(usize, Value)> {
        let stats = KeyStats::new(key);
        let (index, value) = self.map.remove_with_index(key)?;
        let child = value.meta();
        self.meta.slots = self
            .meta
            .slots
            .saturating_sub(2 + stats.slots + child.slots);
        self.meta.string_slots = self
            .meta
            .string_slots
            .saturating_sub(stats.string_slots + child.string_slots);
        self.meta.bytes = self
            .meta
            .bytes
            .saturating_sub(16 + stats.bytes + child.bytes);
        self.remove_depth(stats.entry_depth(child));
        Some((index, value))
    }

    pub(crate) fn clear(&mut self) {
        self.map.clear();
        self.meta = ValueMeta::empty_container();
        self.deep_count = 0;
    }

    pub(crate) fn value_mut(&mut self, key: &Key) -> Option<&mut Value> {
        self.map.get_mut(key)
    }

    pub(crate) fn note_value_replaced(&mut self, key: &Key, old: ValueMeta, new: ValueMeta) {
        if old == new {
            return;
        }
        let stats = KeyStats::new(key);
        self.meta.slots = self
            .meta
            .slots
            .saturating_sub(old.slots)
            .saturating_add(new.slots);
        self.meta.string_slots = self
            .meta
            .string_slots
            .saturating_sub(old.string_slots)
            .saturating_add(new.string_slots);
        self.meta.bytes = self
            .meta
            .bytes
            .saturating_sub(old.bytes)
            .saturating_add(new.bytes);
        self.replace_depth(stats.entry_depth(old), stats.entry_depth(new));
    }

    fn add_entry(&mut self, stats: KeyStats, child: ValueMeta) {
        self.meta.slots = self
            .meta
            .slots
            .saturating_add(2 + stats.slots + child.slots);
        self.meta.string_slots = self
            .meta
            .string_slots
            .saturating_add(stats.string_slots + child.string_slots);
        self.meta.bytes = self
            .meta
            .bytes
            .saturating_add(16 + stats.bytes + child.bytes);
        self.add_depth(stats.entry_depth(child));
    }

    fn add_depth(&mut self, depth: u16) {
        if depth > self.meta.depth {
            self.meta.depth = depth;
            self.deep_count = 1;
        } else if depth == self.meta.depth {
            self.deep_count = self.deep_count.saturating_add(1);
        }
    }

    fn remove_depth(&mut self, depth: u16) {
        if depth != self.meta.depth {
            return;
        }
        self.deep_count = self.deep_count.saturating_sub(1);
        if self.deep_count == 0 {
            self.recompute_depth();
        }
    }

    fn replace_depth(&mut self, old: u16, new: u16) {
        if old == self.meta.depth {
            if self.deep_count > 1 {
                self.deep_count -= 1;
            } else {
                // The replacement is already in the map, so recomputing picks
                // up its contribution and we must not add it again.
                self.recompute_depth();
                return;
            }
        }
        self.add_depth(new);
    }

    fn recompute_depth(&mut self) {
        let mut depth = 1;
        let mut count = 0;
        for (key, value) in self.map.entries() {
            let child = KeyStats::new(key).entry_depth(value.meta());
            if child > depth {
                depth = child;
                count = 1;
            } else if child == depth {
                count += 1;
            }
        }
        self.meta.depth = depth;
        self.deep_count = count;
    }
}

impl Deref for MapValue {
    type Target = OrderedMap;

    fn deref(&self) -> &Self::Target {
        &self.map
    }
}

#[derive(Debug, Clone, Copy)]
struct KeyStats {
    nesting: u16,
    slots: u64,
    string_slots: u64,
    bytes: u64,
}

impl KeyStats {
    fn new(key: &Key) -> Self {
        Self {
            nesting: key_nesting(key),
            slots: key_slots(key),
            string_slots: key_string_slots(key),
            bytes: key_value_size(key),
        }
    }

    fn entry_depth(self, value: ValueMeta) -> u16 {
        1 + self.nesting.max(value.depth)
    }
}

#[derive(Debug, Clone)]
pub struct StructValue {
    pub type_id: TypeId,
    pub fields: Vec<Value>,
    meta: ValueMeta,
    deep_count: u32,
}

impl StructValue {
    pub fn new(type_id: TypeId, fields: Vec<Value>) -> Self {
        let mut value = Self {
            type_id,
            fields,
            meta: ValueMeta::empty_container(),
            deep_count: 0,
        };
        value.meta.slots = value.fields.len() as u64;
        value.meta.bytes = value
            .meta
            .bytes
            .saturating_add(8 * value.fields.len() as u64);
        for field in &value.fields {
            let child = field.meta();
            value.meta.string_slots = value.meta.string_slots.saturating_add(child.string_slots);
            value.meta.bytes = value.meta.bytes.saturating_add(child.bytes);
            value.meta.slots = value.meta.slots.saturating_add(child.slots);
            let depth = 1 + child.depth;
            if depth > value.meta.depth {
                value.meta.depth = depth;
                value.deep_count = 1;
            } else if depth == value.meta.depth {
                value.deep_count = value.deep_count.saturating_add(1);
            }
        }
        value
    }

    pub(crate) fn field_mut(&mut self, index: usize) -> Option<&mut Value> {
        self.fields.get_mut(index)
    }

    pub(crate) fn note_field_replaced(&mut self, old: ValueMeta, new: ValueMeta) {
        if old == new {
            return;
        }
        self.meta.slots = self
            .meta
            .slots
            .saturating_sub(old.slots)
            .saturating_add(new.slots);
        self.meta.string_slots = self
            .meta
            .string_slots
            .saturating_sub(old.string_slots)
            .saturating_add(new.string_slots);
        self.meta.bytes = self
            .meta
            .bytes
            .saturating_sub(old.bytes)
            .saturating_add(new.bytes);
        self.replace_depth(1 + old.depth, 1 + new.depth);
    }

    fn replace_depth(&mut self, old: u16, new: u16) {
        if old == self.meta.depth {
            if self.deep_count > 1 {
                self.deep_count -= 1;
            } else {
                // The replacement is already in `fields`, so recomputing picks
                // up its contribution and we must not add it again.
                self.recompute_depth();
                return;
            }
        }
        let depth = new;
        if depth > self.meta.depth {
            self.meta.depth = depth;
            self.deep_count = 1;
        } else if depth == self.meta.depth {
            self.deep_count = self.deep_count.saturating_add(1);
        }
    }

    fn recompute_depth(&mut self) {
        self.meta.depth = 1;
        self.deep_count = 0;
        for field in &self.fields {
            let depth = 1 + field.meta().depth;
            if depth > self.meta.depth {
                self.meta.depth = depth;
                self.deep_count = 1;
            } else if depth == self.meta.depth {
                self.deep_count = self.deep_count.saturating_add(1);
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct EnumValue {
    pub type_id: TypeId,
    pub variant: usize,
    pub payload: Vec<Value>,
    meta: ValueMeta,
}

impl EnumValue {
    pub fn new(type_id: TypeId, variant: usize, payload: Vec<Value>) -> Self {
        let mut meta = ValueMeta::empty_container();
        meta.slots = payload.len() as u64;
        meta.bytes = meta.bytes.saturating_add(8 * payload.len() as u64);
        for field in &payload {
            let child = field.meta();
            meta.slots = meta.slots.saturating_add(child.slots);
            meta.string_slots = meta.string_slots.saturating_add(child.string_slots);
            meta.bytes = meta.bytes.saturating_add(child.bytes);
            meta.depth = meta.depth.max(1 + child.depth);
        }
        Self {
            type_id,
            variant,
            payload,
            meta,
        }
    }
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
    meta: ValueMeta,
}

impl Closure {
    pub fn new(target: ClosureTarget, captures: Option<Rc<HashMap<BindingId, Value>>>) -> Self {
        let mut meta = ValueMeta {
            depth: 1,
            slots: 0,
            string_slots: 0,
            bytes: 16,
        };
        if let Some(captures) = &captures {
            for value in captures.values() {
                meta.bytes = meta.bytes.saturating_add(value.meta().bytes);
                meta.depth = meta.depth.max(1 + value.meta().depth);
            }
        }
        Self {
            target,
            captures,
            meta,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ClosureTarget {
    Def(DefId),
    Lambda { node: NodeId },
}

/// A callable function value.
///
/// Free functions hold only their `DefId`; the `Rc<Closure>` allocation is
/// reserved for lambdas, which may capture a local environment.
#[derive(Debug, Clone)]
pub enum FnValue {
    Def(DefId),
    Closure(Rc<Closure>),
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
    pub fn insert(&mut self, key: Key, value: Value) -> Option<Value> {
        self.entries.insert(key, value)
    }

    /// Remove a key in O(n), preserving the order of the remaining keys.
    pub fn remove(&mut self, key: &Key) -> Option<Value> {
        self.entries.shift_remove(key)
    }

    /// Remove a key in O(n), returning its former index for fuel accounting.
    pub fn remove_with_index(&mut self, key: &Key) -> Option<(usize, Value)> {
        self.entries
            .shift_remove_full(key)
            .map(|(index, _, value)| (index, value))
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
/// Depth is cached in every container payload, so this check does not walk the
/// value. The `limit == 0` case matches the historical rule that any value is
/// rejected at a zero ceiling.
pub fn value_depth_exceeds(value: &Value, limit: usize) -> bool {
    limit == 0 || value.meta().depth as usize > limit
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
        KeyValue::Tuple(items) => Value::tuple(items.iter().map(key_to_value).collect()),
        KeyValue::Struct(id, fields) => Value::Struct(Rc::new(StructValue::new(
            *id,
            fields.iter().map(key_to_value).collect(),
        ))),
        KeyValue::Enum(id, variant, payload) => Value::Enum(Rc::new(EnumValue::new(
            *id,
            *variant,
            payload.iter().map(key_to_value).collect(),
        ))),
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
        Value::List(Rc::new(SeqValue::new(items)))
    }

    pub fn tuple(items: Vec<Value>) -> Value {
        Value::Tuple(Rc::new(SeqValue::new(items)))
    }

    pub fn map(map: OrderedMap) -> Value {
        Value::Map(Rc::new(MapValue::new(map)))
    }

    /// Cached depth, fuel slots, and nominal bytes for this value.
    pub fn meta(&self) -> ValueMeta {
        match self {
            Value::List(items) | Value::Tuple(items) => items.meta,
            Value::Map(map) => map.meta,
            Value::Struct(value) => value.meta,
            Value::Enum(value) => value.meta,
            Value::Fn(FnValue::Def(_)) => ValueMeta::leaf(),
            Value::Fn(FnValue::Closure(closure)) => closure.meta,
            Value::Str(text) => ValueMeta {
                depth: 1,
                slots: 0,
                string_slots: (text.len() as u64).div_ceil(8),
                bytes: 16 + text.len() as u64,
            },
            Value::Handle(handle) => ValueMeta {
                depth: 1,
                slots: 0,
                string_slots: 0,
                bytes: 16 + handle.token.len() as u64,
            },
            Value::Option(Some(inner)) => wrapped_meta(inner.meta()),
            Value::Result(Ok(inner)) | Value::Result(Err(inner)) => wrapped_meta(inner.meta()),
            _ => ValueMeta::leaf(),
        }
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
            Value::Fn(FnValue::Def(_)) => 0,
            Value::Fn(FnValue::Closure(closure)) => {
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
            Value::Builtin(_) | Value::BuiltinModuleFn(_) => 0,
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

pub(crate) fn key_value_size(key: &KeyValue) -> u64 {
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

fn key_slots(key: &KeyValue) -> u64 {
    match key {
        KeyValue::Tuple(items) => items.len() as u64 + items.iter().map(key_slots).sum::<u64>(),
        KeyValue::Struct(_, fields) | KeyValue::Enum(_, _, fields) => {
            fields.len() as u64 + fields.iter().map(key_slots).sum::<u64>()
        }
        KeyValue::Option(Some(inner))
        | KeyValue::Result(Ok(inner))
        | KeyValue::Result(Err(inner)) => 1 + key_slots(inner),
        _ => 0,
    }
}

fn key_string_slots(key: &KeyValue) -> u64 {
    match key {
        KeyValue::Str(text) => (text.len() as u64).div_ceil(8),
        KeyValue::Tuple(items) => items.iter().map(key_string_slots).sum(),
        KeyValue::Struct(_, fields) | KeyValue::Enum(_, _, fields) => {
            fields.iter().map(key_string_slots).sum()
        }
        KeyValue::Option(Some(inner))
        | KeyValue::Result(Ok(inner))
        | KeyValue::Result(Err(inner)) => key_string_slots(inner),
        _ => 0,
    }
}

fn key_nesting(key: &KeyValue) -> u16 {
    match key {
        KeyValue::Tuple(items) => 1 + items.iter().map(key_nesting).max().unwrap_or(0),
        KeyValue::Struct(_, fields) | KeyValue::Enum(_, _, fields) => {
            1 + fields.iter().map(key_nesting).max().unwrap_or(0)
        }
        KeyValue::Option(Some(inner))
        | KeyValue::Result(Ok(inner))
        | KeyValue::Result(Err(inner)) => 1 + key_nesting(inner),
        _ => 0,
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

    #[test]
    fn value_stays_compact() {
        if cfg!(target_pointer_width = "64") {
            // `Range` is the largest payload, and rustc uses the `inclusive`
            // bool's niche for the enum tag. Keep this assertion so
            // value-representation regressions are visible in tests instead
            // of only in benchmarks.
            assert_eq!(std::mem::size_of::<Value>(), 24);
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    struct RefMeta {
        depth: u16,
        slots: u64,
        string_slots: u64,
        bytes: u64,
    }

    impl RefMeta {
        fn leaf() -> Self {
            Self {
                depth: 1,
                slots: 0,
                string_slots: 0,
                bytes: 0,
            }
        }

        fn container() -> Self {
            Self {
                depth: 1,
                slots: 0,
                string_slots: 0,
                bytes: 16,
            }
        }
    }

    fn reference_meta(value: &Value) -> RefMeta {
        match value {
            Value::List(items) | Value::Tuple(items) => {
                let mut meta = RefMeta::container();
                for item in items.iter() {
                    let child = reference_meta(item);
                    meta.slots += 1 + child.slots;
                    meta.string_slots += child.string_slots;
                    meta.bytes += 8 + child.bytes;
                    meta.depth = meta.depth.max(1 + child.depth);
                }
                meta
            }
            Value::Map(map) => {
                let mut meta = RefMeta::container();
                for (key, value) in map.entries() {
                    let child = reference_meta(value);
                    meta.slots += 2 + reference_key_slots(key) + child.slots;
                    meta.string_slots += reference_key_string(key) + child.string_slots;
                    meta.bytes += 16 + reference_key_bytes(key) + child.bytes;
                    meta.depth = meta
                        .depth
                        .max(1 + reference_key_nesting(key).max(child.depth));
                }
                meta
            }
            Value::Struct(value) => {
                let mut meta = RefMeta::container();
                for field in &value.fields {
                    let child = reference_meta(field);
                    meta.slots += 1 + child.slots;
                    meta.string_slots += child.string_slots;
                    meta.bytes += 8 + child.bytes;
                    meta.depth = meta.depth.max(1 + child.depth);
                }
                meta
            }
            Value::Enum(value) => {
                let mut meta = RefMeta::container();
                for field in &value.payload {
                    let child = reference_meta(field);
                    meta.slots += 1 + child.slots;
                    meta.string_slots += child.string_slots;
                    meta.bytes += 8 + child.bytes;
                    meta.depth = meta.depth.max(1 + child.depth);
                }
                meta
            }
            Value::Option(Some(inner)) | Value::Result(Ok(inner)) | Value::Result(Err(inner)) => {
                let child = reference_meta(inner);
                RefMeta {
                    depth: 1 + child.depth,
                    slots: 1 + child.slots,
                    string_slots: child.string_slots,
                    bytes: 16 + child.bytes,
                }
            }
            Value::Fn(FnValue::Def(_)) => RefMeta::leaf(),
            Value::Fn(FnValue::Closure(closure)) => {
                let mut meta = RefMeta::container();
                if let Some(captures) = &closure.captures {
                    for value in captures.values() {
                        let child = reference_meta(value);
                        meta.bytes += child.bytes;
                        meta.depth = meta.depth.max(1 + child.depth);
                    }
                }
                meta
            }
            Value::Str(text) => RefMeta {
                depth: 1,
                slots: 0,
                string_slots: (text.len() as u64).div_ceil(8),
                bytes: 16 + text.len() as u64,
            },
            Value::Handle(handle) => RefMeta {
                depth: 1,
                slots: 0,
                string_slots: 0,
                bytes: 16 + handle.token.len() as u64,
            },
            _ => RefMeta::leaf(),
        }
    }

    fn reference_key_nesting(key: &KeyValue) -> u16 {
        match key {
            KeyValue::Tuple(items) => {
                1 + items.iter().map(reference_key_nesting).max().unwrap_or(0)
            }
            KeyValue::Struct(_, fields) | KeyValue::Enum(_, _, fields) => {
                1 + fields.iter().map(reference_key_nesting).max().unwrap_or(0)
            }
            KeyValue::Option(Some(inner))
            | KeyValue::Result(Ok(inner))
            | KeyValue::Result(Err(inner)) => 1 + reference_key_nesting(inner),
            _ => 0,
        }
    }

    fn reference_key_slots(key: &KeyValue) -> u64 {
        match key {
            KeyValue::Tuple(items) => {
                items.len() as u64 + items.iter().map(reference_key_slots).sum::<u64>()
            }
            KeyValue::Struct(_, fields) | KeyValue::Enum(_, _, fields) => {
                fields.len() as u64 + fields.iter().map(reference_key_slots).sum::<u64>()
            }
            KeyValue::Option(Some(inner))
            | KeyValue::Result(Ok(inner))
            | KeyValue::Result(Err(inner)) => 1 + reference_key_slots(inner),
            _ => 0,
        }
    }

    fn reference_key_string(key: &KeyValue) -> u64 {
        match key {
            KeyValue::Str(text) => (text.len() as u64).div_ceil(8),
            KeyValue::Tuple(items) => items.iter().map(reference_key_string).sum(),
            KeyValue::Struct(_, fields) | KeyValue::Enum(_, _, fields) => {
                fields.iter().map(reference_key_string).sum()
            }
            KeyValue::Option(Some(inner))
            | KeyValue::Result(Ok(inner))
            | KeyValue::Result(Err(inner)) => reference_key_string(inner),
            _ => 0,
        }
    }

    fn reference_key_bytes(key: &KeyValue) -> u64 {
        match key {
            KeyValue::Int(_) | KeyValue::Bool(_) => 0,
            KeyValue::Str(text) => 16 + text.len() as u64,
            KeyValue::Tuple(items) => {
                16 + 8 * items.len() as u64 + items.iter().map(reference_key_bytes).sum::<u64>()
            }
            KeyValue::Struct(_, fields) | KeyValue::Enum(_, _, fields) => {
                16 + 8 * fields.len() as u64 + fields.iter().map(reference_key_bytes).sum::<u64>()
            }
            KeyValue::Option(inner) => inner
                .as_ref()
                .map(|inner| 16 + reference_key_bytes(inner))
                .unwrap_or(0),
            KeyValue::Result(result) => match result {
                Ok(inner) | Err(inner) => 16 + reference_key_bytes(inner),
            },
            KeyValue::Handle(_, token) => 16 + token.len() as u64,
        }
    }

    /// The pre-caching depth walk, kept as an oracle for the metadata test.
    fn legacy_depth_exceeds(value: &Value, limit: usize) -> bool {
        if limit == 0 {
            return true;
        }
        let has_children = match value {
            Value::List(items) | Value::Tuple(items) => !items.is_empty(),
            Value::Map(map) => !map.is_empty(),
            Value::Struct(value) => !value.fields.is_empty(),
            Value::Enum(value) => !value.payload.is_empty(),
            Value::Option(Some(_)) | Value::Result(_) => true,
            Value::Fn(FnValue::Closure(closure)) => closure
                .captures
                .as_ref()
                .is_some_and(|captures| !captures.is_empty()),
            Value::Fn(FnValue::Def(_)) => false,
            _ => false,
        };
        if !has_children {
            return false;
        }
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
                        if legacy_key_depth_exceeds(key, limit.saturating_sub(depth)) {
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
                Value::Option(Some(value))
                | Value::Result(Ok(value))
                | Value::Result(Err(value)) => stack.push((value, depth + 1)),
                Value::Fn(FnValue::Closure(closure)) => {
                    if let Some(captures) = &closure.captures {
                        for value in captures.values() {
                            stack.push((value, depth + 1));
                        }
                    }
                }
                _ => {}
            }
        }
        false
    }

    fn legacy_key_depth_exceeds(key: &KeyValue, remaining: usize) -> bool {
        match key {
            KeyValue::Tuple(items) => {
                remaining == 0
                    || items
                        .iter()
                        .any(|item| legacy_key_depth_exceeds(item, remaining - 1))
            }
            KeyValue::Struct(_, fields) | KeyValue::Enum(_, _, fields) => {
                remaining == 0
                    || fields
                        .iter()
                        .any(|field| legacy_key_depth_exceeds(field, remaining - 1))
            }
            KeyValue::Option(Some(value))
            | KeyValue::Result(Ok(value))
            | KeyValue::Result(Err(value)) => {
                remaining == 0 || legacy_key_depth_exceeds(value, remaining - 1)
            }
            _ => false,
        }
    }

    fn assert_meta(value: &Value) {
        let expected = reference_meta(value);
        let actual = value.meta();
        assert_eq!(
            RefMeta {
                depth: actual.depth,
                slots: actual.slots,
                string_slots: actual.string_slots,
                bytes: actual.bytes,
            },
            expected
        );
    }

    fn assert_seq(seq: &SeqValue) {
        assert_meta(&Value::List(Rc::new(seq.clone())));
    }

    fn assert_map(map: &MapValue) {
        assert_meta(&Value::Map(Rc::new(map.clone())));
    }

    fn compound_value() -> Value {
        let mut map = OrderedMap::new();
        map.insert(
            KeyValue::Tuple(vec![KeyValue::Str("key".into()), KeyValue::Int(1)]),
            Value::list(vec![Value::str("abcd"), Value::Int(2)]),
        );
        map.insert(
            KeyValue::Tuple(vec![
                KeyValue::Tuple(vec![KeyValue::Str("nested".into())]),
                KeyValue::Option(Some(Box::new(KeyValue::Bool(true)))),
            ]),
            Value::Int(4),
        );
        map.insert(
            KeyValue::Str("field".into()),
            Value::Struct(Rc::new(StructValue::new(
                TypeId(7),
                vec![
                    Value::Bool(true),
                    Value::Option(Some(Box::new(Value::str("xyz")))),
                ],
            ))),
        );
        let mut captures = HashMap::new();
        captures.insert(BindingId(1), Value::list(vec![Value::Int(3)]));
        let closure = Value::Fn(FnValue::Closure(Rc::new(Closure::new(
            ClosureTarget::Lambda { node: NodeId(1) },
            Some(Rc::new(captures)),
        ))));
        Value::tuple(vec![
            Value::Int(1),
            Value::list(vec![
                Value::map(map),
                Value::Result(Ok(Box::new(Value::str("hello")))),
            ]),
            Value::Enum(Rc::new(EnumValue::new(
                TypeId(9),
                2,
                vec![
                    closure,
                    Value::Handle(Rc::new(HandleValue {
                        type_id: TypeId(2),
                        token: "tok".into(),
                    })),
                ],
            ))),
        ])
    }

    #[test]
    fn cached_metadata_matches_recursive_reference() {
        let value = compound_value();
        assert_meta(&value);
        let expected = reference_meta(&value);
        for limit in 0..=(expected.depth as usize + 1) {
            assert_eq!(
                value_depth_exceeds(&value, limit),
                limit == 0 || expected.depth as usize > limit
            );
            assert_eq!(
                value_depth_exceeds(&value, limit),
                legacy_depth_exceeds(&value, limit),
                "limit {limit}"
            );
        }
    }

    #[test]
    fn sequence_mutations_keep_metadata_exact() {
        let mut seq = SeqValue::new(vec![Value::Int(1), Value::Int(2)]);
        assert_seq(&seq);
        seq.push(Value::list(vec![Value::Int(3)]));
        assert_seq(&seq);
        seq.insert(1, Value::str("x"));
        assert_seq(&seq);
        seq.remove(1);
        assert_seq(&seq);
        seq.pop();
        assert_seq(&seq);
        seq.sort_by(|a, b| b.meta().depth.cmp(&a.meta().depth));
        assert_seq(&seq);
        seq.clear();
        assert_seq(&seq);
    }

    #[test]
    fn sequence_construction_adopts_input_allocation() {
        let items = vec![Value::Int(1), Value::Int(2), Value::Int(3)];
        let allocation = items.as_ptr();
        let seq = SeqValue::new(items);
        assert_eq!(seq.items.as_ptr(), allocation);
        assert_seq(&seq);
    }

    #[test]
    fn map_mutations_keep_metadata_exact() {
        let mut map = MapValue::new(OrderedMap::new());
        assert_map(&map);
        map.insert(
            KeyValue::Str("deep".into()),
            Value::list(vec![Value::list(vec![Value::Int(1)])]),
        );
        assert_map(&map);
        map.insert(KeyValue::Str("deep".into()), Value::Int(1));
        assert_map(&map);
        map.insert(
            KeyValue::Str("deep".into()),
            Value::list(vec![Value::list(vec![Value::Int(2)])]),
        );
        assert_map(&map);
        map.insert(
            KeyValue::Tuple(vec![KeyValue::Int(1), KeyValue::Str("k".into())]),
            Value::Bool(true),
        );
        assert_map(&map);
        map.remove_with_index(&KeyValue::Str("deep".into()));
        assert_map(&map);
        map.clear();
        assert_map(&map);
    }

    #[test]
    fn replacing_the_deepest_child_does_not_leave_stale_depth() {
        let mut seq = SeqValue::new(vec![Value::list(vec![Value::Int(1)]), Value::Int(2)]);
        assert_seq(&seq);
        let old = seq[0].meta();
        *seq.slot_mut(0).expect("slot") = Value::Int(3);
        let new = seq[0].meta();
        seq.note_child_replaced(old, new);
        assert_seq(&seq);
        seq.pop();
        assert_seq(&seq);
        seq.pop();
        assert_seq(&seq);

        let mut map = MapValue::new(OrderedMap::new());
        map.insert(
            KeyValue::Str("deep".into()),
            Value::list(vec![Value::list(vec![Value::Int(1)])]),
        );
        assert_map(&map);
        map.insert(KeyValue::Str("deep".into()), Value::Int(3));
        assert_map(&map);
        map.remove_with_index(&KeyValue::Str("deep".into()));
        assert_map(&map);
    }

    #[test]
    fn struct_replacement_keeps_metadata_exact() {
        let mut value = StructValue::new(
            TypeId(1),
            vec![
                Value::Int(1),
                Value::list(vec![Value::list(vec![Value::Int(2)])]),
            ],
        );
        assert_meta(&Value::Struct(Rc::new(value.clone())));
        let old = value.fields[1].meta();
        *value.field_mut(1).expect("field") = Value::Int(9);
        let new = value.fields[1].meta();
        value.note_field_replaced(old, new);
        assert_meta(&Value::Struct(Rc::new(value)));
    }

    #[test]
    fn equal_metadata_replacement_leaves_bookkeeping_untouched() {
        let mut seq = SeqValue::new(vec![Value::Int(1), Value::list(vec![Value::Int(2)])]);
        let before = seq.meta;
        let before_deep_count = seq.deep_count;
        let old = seq[0].meta();
        *seq.slot_mut(0).expect("slot") = Value::Int(9);
        let new = seq[0].meta();
        assert_eq!(old, new);
        seq.note_child_replaced(old, new);
        assert_eq!(seq.meta, before);
        assert_eq!(seq.deep_count, before_deep_count);
        assert_seq(&seq);

        let mut map = MapValue::new(OrderedMap::new());
        let key = KeyValue::Str("key".into());
        map.insert(key.clone(), Value::Int(1));
        let before = map.meta;
        let old = map.get(&key).expect("value").meta();
        *map.value_mut(&key).expect("value") = Value::Int(2);
        let new = map.get(&key).expect("value").meta();
        assert_eq!(old, new);
        map.note_value_replaced(&key, old, new);
        assert_eq!(map.meta, before);
        assert_map(&map);

        let mut value = StructValue::new(TypeId(1), vec![Value::Int(1), Value::Int(2)]);
        let before = value.meta;
        let before_deep_count = value.deep_count;
        let old = value.fields[0].meta();
        *value.field_mut(0).expect("field") = Value::Int(9);
        let new = value.fields[0].meta();
        assert_eq!(old, new);
        value.note_field_replaced(old, new);
        assert_eq!(value.meta, before);
        assert_eq!(value.deep_count, before_deep_count);
        assert_meta(&Value::Struct(Rc::new(value)));
    }
}
