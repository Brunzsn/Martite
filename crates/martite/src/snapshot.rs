//! Versioned binary codec for plugin state snapshots.
//!
//! One tag table drives both the encoder and the decoder. Map keys are encoded
//! as ordinary values and converted through `key_to_value` / `key_from_value`,
//! so there is no separate key-only schema to keep in sync.
//!
//! Encoding and decoding are budgeted and validate values against the checked
//! state type. The wire format expands shared `Rc` payloads, so encoding
//! rejects a value whose expanded form exceeds the budget instead of
//! allocating without bound.

use std::rc::Rc;

use martite_ir::{Ty, TyId, TypeDefKind, TypeId, TypeTable};
use martite_runtime::{
    EnumValue, HandleValue, MAX_VALUE_DEPTH, OrderedMap, StructValue, Value, key_from_value,
    key_to_value,
};

/// Snapshot wire-format version.
///
/// The language is pre-0.1.0 and snapshots are session-scoped, so the internal
/// map-key layout was unified with the value tags without a version bump.
/// `Snapshot::format_version` remains 1.
pub(crate) const SNAPSHOT_FORMAT_VERSION: u32 = 1;

/// Wire tags shared by the encoder and decoder.
///
/// Tags are part of the snapshot layout: do not renumber them. `Function` is a
/// sentinel because function values cannot be plugin state (E506).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tag {
    Unit = 0,
    Int = 1,
    Float = 2,
    Bool = 3,
    Str = 4,
    List = 5,
    Tuple = 6,
    Map = 7,
    Range = 8,
    Struct = 9,
    Enum = 10,
    Option = 11,
    Result = 12,
    Handle = 13,
    Function = 14,
}

impl Tag {
    fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0 => Self::Unit,
            1 => Self::Int,
            2 => Self::Float,
            3 => Self::Bool,
            4 => Self::Str,
            5 => Self::List,
            6 => Self::Tuple,
            7 => Self::Map,
            8 => Self::Range,
            9 => Self::Struct,
            10 => Self::Enum,
            11 => Self::Option,
            12 => Self::Result,
            13 => Self::Handle,
            14 => Self::Function,
            _ => return None,
        })
    }
}

/// The single codec used to encode and decode both snapshot values and map
/// keys. `types` is the IR type table the snapshot is validated against.
pub(crate) struct ValueCodec<'a> {
    types: &'a TypeTable,
    handle_validator: Option<&'a dyn Fn(&str, &str) -> bool>,
}

impl<'a> ValueCodec<'a> {
    pub(crate) fn new(types: &'a TypeTable) -> Self {
        Self {
            types,
            handle_validator: None,
        }
    }

    /// Install the host validator called for every decoded opaque handle.
    ///
    /// Without a validator, decoding a handle fails: a token is only valid for
    /// the host session that issued it, and the codec cannot tell a current
    /// token from a replayed one.
    pub(crate) fn with_handle_validator(
        mut self,
        validator: Option<&'a dyn Fn(&str, &str) -> bool>,
    ) -> Self {
        self.handle_validator = validator;
        self
    }

    /// Encode `value`, checking it against `expected` and stopping at `max_bytes`.
    pub(crate) fn encode(
        &self,
        value: &Value,
        expected: &Ty,
        max_bytes: u64,
    ) -> Result<Vec<u8>, String> {
        let mut encoder = Encoder {
            types: self.types,
            out: Vec::new(),
            limit: max_bytes,
        };
        encoder.write_value(value, expected, &[], 0)?;
        Ok(encoder.out)
    }

    /// Decode `bytes`, checking every tag against `expected` and bounding both
    /// the input and the decoded allocations by `max_bytes`.
    pub(crate) fn decode(
        &self,
        bytes: &[u8],
        expected: &Ty,
        max_bytes: u64,
    ) -> Result<Value, String> {
        if bytes.len() as u64 > max_bytes {
            return Err(format!(
                "snapshot is {} bytes but the restore budget is {max_bytes} bytes",
                bytes.len()
            ));
        }
        let mut reader = Reader::new(bytes, max_bytes);
        let value = self.read_value(&mut reader, expected, &[], 0)?;
        if reader.pos != bytes.len() {
            return Err("trailing bytes in snapshot".into());
        }
        Ok(value)
    }

    fn read_value(
        &self,
        reader: &mut Reader<'_>,
        expected: &Ty,
        args: &[TyId],
        depth: usize,
    ) -> Result<Value, String> {
        if depth > MAX_VALUE_DEPTH {
            return Err("snapshot value nesting is too deep".into());
        }
        if matches!(expected, Ty::Never | Ty::Param(_)) {
            return self.read_unchecked(reader, depth);
        }
        let tag = Tag::from_byte(reader.read_u8()?).ok_or("snapshot contains a non-data value")?;
        Ok(match tag {
            Tag::Unit => {
                self.expect_tag(expected, tag, matches!(expected, Ty::Unit))?;
                Value::Unit
            }
            Tag::Int => {
                self.expect_tag(expected, tag, matches!(expected, Ty::Int))?;
                Value::Int(reader.read_i64()?)
            }
            Tag::Float => {
                self.expect_tag(expected, tag, matches!(expected, Ty::Float))?;
                Value::Float(f64::from_bits(reader.read_u64()?))
            }
            Tag::Bool => {
                self.expect_tag(expected, tag, matches!(expected, Ty::Bool))?;
                Value::Bool(reader.read_u8()? != 0)
            }
            Tag::Str => {
                self.expect_tag(expected, tag, matches!(expected, Ty::Str))?;
                Value::str(reader.read_string()?)
            }
            Tag::List => {
                let Ty::List(item) = expected else {
                    return Err(self.mismatch(expected, tag));
                };
                let len = reader.read_collection_len(1)?;
                reader.charge(container_bytes(8, len))?;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(self.read_child(reader, *item, args, depth + 1)?);
                }
                Value::list(items)
            }
            Tag::Tuple => {
                let Ty::Tuple(items) = expected else {
                    return Err(self.mismatch(expected, tag));
                };
                let len = reader.read_collection_len(1)?;
                if items.len() != len {
                    return Err("snapshot tuple length does not match its type".into());
                }
                reader.charge(container_bytes(8, len))?;
                let mut values = Vec::with_capacity(len);
                for item in items {
                    values.push(self.read_child(reader, *item, args, depth + 1)?);
                }
                Value::tuple(values)
            }
            Tag::Map => {
                let Ty::Map(key_ty, value_ty) = expected else {
                    return Err(self.mismatch(expected, tag));
                };
                let len = reader.read_collection_len(2)?;
                reader.charge(container_bytes(16, len))?;
                let mut map = OrderedMap::new();
                for _ in 0..len {
                    // A map key is an ordinary value on the wire. `key_from_value`
                    // enforces the key-eligible subset, so there is no second tag
                    // ordering to keep in sync with the value tags.
                    let key_value = self.read_child(reader, *key_ty, args, depth + 1)?;
                    let key = key_from_value(&key_value)
                        .map_err(|_| "invalid map key in snapshot".to_string())?;
                    let value = self.read_child(reader, *value_ty, args, depth + 1)?;
                    map.insert(key, value);
                }
                Value::map(map)
            }
            Tag::Range => {
                self.expect_tag(expected, tag, matches!(expected, Ty::Range))?;
                Value::Range {
                    start: reader.read_i64()?,
                    end: reader.read_i64()?,
                    inclusive: reader.read_u8()? != 0,
                }
            }
            Tag::Struct => {
                let Ty::Named(expected_id, type_args) = expected else {
                    return Err(self.mismatch(expected, tag));
                };
                let type_id = TypeId(reader.read_type_id()?);
                if type_id != *expected_id {
                    return Err(format!(
                        "snapshot struct type `{}` does not match the declared type `{}`",
                        def_name(self.types, type_id),
                        def_name(self.types, *expected_id)
                    ));
                }
                let len = reader.read_collection_len(1)?;
                let Some(def) = self.types.get(type_id) else {
                    return Err("snapshot references an unknown struct type".into());
                };
                let TypeDefKind::Struct { fields } = &def.kind else {
                    return Err("snapshot struct type is not a struct".into());
                };
                if fields.len() != len {
                    return Err("snapshot struct field count does not match its type".into());
                }
                reader.charge(container_bytes(8, len))?;
                let named_args = resolve_args(self.types, type_args, args)?;
                let mut values = Vec::with_capacity(len);
                for field in fields {
                    values.push(self.read_child(reader, field.ty, &named_args, depth + 1)?);
                }
                Value::Struct(Rc::new(StructValue::new(type_id, values)))
            }
            Tag::Enum => {
                let Ty::Named(expected_id, type_args) = expected else {
                    return Err(self.mismatch(expected, tag));
                };
                let type_id = TypeId(reader.read_type_id()?);
                if type_id != *expected_id {
                    return Err(format!(
                        "snapshot enum type `{}` does not match the declared type `{}`",
                        def_name(self.types, type_id),
                        def_name(self.types, *expected_id)
                    ));
                }
                let variant = reader.read_len()?;
                let len = reader.read_collection_len(1)?;
                let Some(def) = self.types.get(type_id) else {
                    return Err("snapshot references an unknown enum type".into());
                };
                let TypeDefKind::Enum { variants } = &def.kind else {
                    return Err("snapshot enum type is not an enum".into());
                };
                let Some(variant_def) = variants.get(variant) else {
                    return Err("snapshot enum variant is out of range".into());
                };
                if variant_def.payload.len() != len {
                    return Err("snapshot enum payload count does not match its variant".into());
                }
                reader.charge(container_bytes(8, len))?;
                let named_args = resolve_args(self.types, type_args, args)?;
                let mut payload = Vec::with_capacity(len);
                for field in &variant_def.payload {
                    payload.push(self.read_child(reader, *field, &named_args, depth + 1)?);
                }
                Value::Enum(Rc::new(EnumValue::new(type_id, variant, payload)))
            }
            Tag::Option => {
                let Ty::Option(item) = expected else {
                    return Err(self.mismatch(expected, tag));
                };
                if reader.read_u8()? == 0 {
                    Value::Option(None)
                } else {
                    reader.charge(16)?;
                    Value::Option(Some(Box::new(self.read_child(
                        reader,
                        *item,
                        args,
                        depth + 1,
                    )?)))
                }
            }
            Tag::Result => {
                let Ty::Result(ok, err) = expected else {
                    return Err(self.mismatch(expected, tag));
                };
                reader.charge(16)?;
                if reader.read_u8()? == 0 {
                    Value::Result(Ok(Box::new(self.read_child(
                        reader,
                        *ok,
                        args,
                        depth + 1,
                    )?)))
                } else {
                    Value::Result(Err(Box::new(self.read_child(
                        reader,
                        *err,
                        args,
                        depth + 1,
                    )?)))
                }
            }
            Tag::Handle => {
                let Ty::Named(expected_id, _) = expected else {
                    return Err(self.mismatch(expected, tag));
                };
                let type_id = TypeId(reader.read_type_id()?);
                if type_id != *expected_id {
                    return Err(format!(
                        "snapshot handle type `{}` does not match the declared type `{}`",
                        def_name(self.types, type_id),
                        def_name(self.types, *expected_id)
                    ));
                }
                let Some(def) = self.types.get(type_id) else {
                    return Err("snapshot references an unknown handle type".into());
                };
                if !matches!(&def.kind, TypeDefKind::Opaque) {
                    return Err("snapshot handle type is not opaque".into());
                }
                let token = reader.read_string()?;
                let Some(validate) = self.handle_validator else {
                    return Err(
                        "snapshot contains an opaque handle but no handle validator is installed"
                            .into(),
                    );
                };
                if !validate(def.name.as_ref(), &token) {
                    return Err(format!(
                        "snapshot handle `{}` was rejected by the host",
                        def.name
                    ));
                }
                Value::Handle(Rc::new(HandleValue { type_id, token }))
            }
            Tag::Function => return Err("snapshot contains a non-data value".into()),
        })
    }

    fn expect_tag(&self, expected: &Ty, tag: Tag, ok: bool) -> Result<(), String> {
        if ok {
            Ok(())
        } else {
            Err(self.mismatch(expected, tag))
        }
    }

    fn mismatch(&self, expected: &Ty, tag: Tag) -> String {
        format!(
            "snapshot value has tag `{}` but the declared type is `{}`",
            tag_name(tag),
            ty_name(self.types, expected)
        )
    }

    fn read_child(
        &self,
        reader: &mut Reader<'_>,
        ty_id: TyId,
        args: &[TyId],
        depth: usize,
    ) -> Result<Value, String> {
        let actual = match self.types.get_ty(ty_id) {
            Some(Ty::Param(index)) => match args.get(*index as usize) {
                Some(actual) => *actual,
                None => return self.read_unchecked(reader, depth),
            },
            Some(ty) => {
                let ty = ty.clone();
                return self.read_value(reader, &ty, args, depth);
            }
            None => return Err("snapshot references an unknown type".into()),
        };
        match self.types.get_ty(actual) {
            Some(Ty::Param(_)) | None => self.read_unchecked(reader, depth),
            Some(ty) => {
                let ty = ty.clone();
                self.read_value(reader, &ty, args, depth)
            }
        }
    }

    /// Decode without a declared type. Used only for a generic parameter whose
    /// concrete argument was erased; nominal headers and handles are still
    /// checked.
    fn read_unchecked(&self, reader: &mut Reader<'_>, depth: usize) -> Result<Value, String> {
        if depth > MAX_VALUE_DEPTH {
            return Err("snapshot value nesting is too deep".into());
        }
        let tag = Tag::from_byte(reader.read_u8()?).ok_or("snapshot contains a non-data value")?;
        Ok(match tag {
            Tag::Unit => Value::Unit,
            Tag::Int => Value::Int(reader.read_i64()?),
            Tag::Float => Value::Float(f64::from_bits(reader.read_u64()?)),
            Tag::Bool => Value::Bool(reader.read_u8()? != 0),
            Tag::Str => Value::str(reader.read_string()?),
            Tag::List => {
                let len = reader.read_collection_len(1)?;
                reader.charge(container_bytes(8, len))?;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(self.read_unchecked(reader, depth + 1)?);
                }
                Value::list(items)
            }
            Tag::Tuple => {
                let len = reader.read_collection_len(1)?;
                reader.charge(container_bytes(8, len))?;
                let mut items = Vec::with_capacity(len);
                for _ in 0..len {
                    items.push(self.read_unchecked(reader, depth + 1)?);
                }
                Value::tuple(items)
            }
            Tag::Map => {
                let len = reader.read_collection_len(2)?;
                reader.charge(container_bytes(16, len))?;
                let mut map = OrderedMap::new();
                for _ in 0..len {
                    let key_value = self.read_unchecked(reader, depth + 1)?;
                    let key = key_from_value(&key_value)
                        .map_err(|_| "invalid map key in snapshot".to_string())?;
                    let value = self.read_unchecked(reader, depth + 1)?;
                    map.insert(key, value);
                }
                Value::map(map)
            }
            Tag::Range => Value::Range {
                start: reader.read_i64()?,
                end: reader.read_i64()?,
                inclusive: reader.read_u8()? != 0,
            },
            Tag::Struct => {
                let type_id = TypeId(reader.read_type_id()?);
                let len = reader.read_collection_len(1)?;
                let Some(def) = self.types.get(type_id) else {
                    return Err("snapshot references an unknown struct type".into());
                };
                let TypeDefKind::Struct { fields } = &def.kind else {
                    return Err("snapshot struct type is not a struct".into());
                };
                if fields.len() != len {
                    return Err("snapshot struct field count does not match its type".into());
                }
                reader.charge(container_bytes(8, len))?;
                let mut values = Vec::with_capacity(len);
                for _ in 0..len {
                    values.push(self.read_unchecked(reader, depth + 1)?);
                }
                Value::Struct(Rc::new(StructValue::new(type_id, values)))
            }
            Tag::Enum => {
                let type_id = TypeId(reader.read_type_id()?);
                let variant = reader.read_len()?;
                let len = reader.read_collection_len(1)?;
                let Some(def) = self.types.get(type_id) else {
                    return Err("snapshot references an unknown enum type".into());
                };
                let TypeDefKind::Enum { variants } = &def.kind else {
                    return Err("snapshot enum type is not an enum".into());
                };
                let Some(variant_def) = variants.get(variant) else {
                    return Err("snapshot enum variant is out of range".into());
                };
                if variant_def.payload.len() != len {
                    return Err("snapshot enum payload count does not match its variant".into());
                }
                reader.charge(container_bytes(8, len))?;
                let mut payload = Vec::with_capacity(len);
                for _ in 0..len {
                    payload.push(self.read_unchecked(reader, depth + 1)?);
                }
                Value::Enum(Rc::new(EnumValue::new(type_id, variant, payload)))
            }
            Tag::Option => {
                if reader.read_u8()? == 0 {
                    Value::Option(None)
                } else {
                    reader.charge(16)?;
                    Value::Option(Some(Box::new(self.read_unchecked(reader, depth + 1)?)))
                }
            }
            Tag::Result => {
                reader.charge(16)?;
                if reader.read_u8()? == 0 {
                    Value::Result(Ok(Box::new(self.read_unchecked(reader, depth + 1)?)))
                } else {
                    Value::Result(Err(Box::new(self.read_unchecked(reader, depth + 1)?)))
                }
            }
            Tag::Handle => {
                let type_id = TypeId(reader.read_type_id()?);
                let Some(def) = self.types.get(type_id) else {
                    return Err("snapshot references an unknown handle type".into());
                };
                if !matches!(&def.kind, TypeDefKind::Opaque) {
                    return Err("snapshot handle type is not opaque".into());
                }
                let token = reader.read_string()?;
                let Some(validate) = self.handle_validator else {
                    return Err(
                        "snapshot contains an opaque handle but no handle validator is installed"
                            .into(),
                    );
                };
                if !validate(def.name.as_ref(), &token) {
                    return Err(format!(
                        "snapshot handle `{}` was rejected by the host",
                        def.name
                    ));
                }
                Value::Handle(Rc::new(HandleValue { type_id, token }))
            }
            Tag::Function => return Err("snapshot contains a non-data value".into()),
        })
    }
}

/// Budgeted encoder. Every write goes through [`Self::reserve`], so a shared
/// `Rc` DAG that expands exponentially stops at `limit` instead of allocating.
struct Encoder<'a> {
    types: &'a TypeTable,
    out: Vec<u8>,
    limit: u64,
}

impl Encoder<'_> {
    fn write_value(
        &mut self,
        value: &Value,
        expected: &Ty,
        args: &[TyId],
        depth: usize,
    ) -> Result<(), String> {
        if depth > MAX_VALUE_DEPTH {
            return Err("snapshot value nesting is too deep".into());
        }
        if matches!(expected, Ty::Never | Ty::Param(_)) {
            return self.write_unchecked(value, depth);
        }
        let types = self.types;
        match expected {
            Ty::Unit => {
                self.expect(expected, value, matches!(value, Value::Unit))?;
                self.push(Tag::Unit as u8)
            }
            Ty::Int => {
                let Value::Int(number) = value else {
                    return Err(self.mismatch(expected, value));
                };
                self.push(Tag::Int as u8)?;
                self.extend(&number.to_le_bytes())
            }
            Ty::Float => {
                let Value::Float(number) = value else {
                    return Err(self.mismatch(expected, value));
                };
                self.push(Tag::Float as u8)?;
                self.extend(&number.to_bits().to_le_bytes())
            }
            Ty::Bool => {
                let Value::Bool(flag) = value else {
                    return Err(self.mismatch(expected, value));
                };
                self.push(Tag::Bool as u8)?;
                self.push(*flag as u8)
            }
            Ty::Str => {
                let Value::Str(text) = value else {
                    return Err(self.mismatch(expected, value));
                };
                self.push(Tag::Str as u8)?;
                self.write_bytes(text.as_bytes())
            }
            Ty::Range => {
                let Value::Range {
                    start,
                    end,
                    inclusive,
                } = value
                else {
                    return Err(self.mismatch(expected, value));
                };
                self.push(Tag::Range as u8)?;
                self.extend(&start.to_le_bytes())?;
                self.extend(&end.to_le_bytes())?;
                self.push(*inclusive as u8)
            }
            Ty::List(item) => {
                let Value::List(items) = value else {
                    return Err(self.mismatch(expected, value));
                };
                self.push(Tag::List as u8)?;
                self.write_len(items.len())?;
                for item_value in items.iter() {
                    self.write_child(item_value, *item, args, depth + 1)?;
                }
                Ok(())
            }
            Ty::Tuple(items) => {
                let Value::Tuple(values) = value else {
                    return Err(self.mismatch(expected, value));
                };
                if values.len() != items.len() {
                    return Err("state tuple length does not match its type".into());
                }
                self.push(Tag::Tuple as u8)?;
                self.write_len(items.len())?;
                for (item_value, item) in values.iter().zip(items) {
                    self.write_child(item_value, *item, args, depth + 1)?;
                }
                Ok(())
            }
            Ty::Map(key_ty, value_ty) => {
                let Value::Map(map) = value else {
                    return Err(self.mismatch(expected, value));
                };
                self.push(Tag::Map as u8)?;
                self.write_len(map.len())?;
                for (key, item) in map.entries() {
                    let key_value = key_to_value(key);
                    self.write_child(&key_value, *key_ty, args, depth + 1)?;
                    self.write_child(item, *value_ty, args, depth + 1)?;
                }
                Ok(())
            }
            Ty::Option(item) => {
                let Value::Option(inner) = value else {
                    return Err(self.mismatch(expected, value));
                };
                self.push(Tag::Option as u8)?;
                match inner {
                    None => self.push(0),
                    Some(inner) => {
                        self.push(1)?;
                        self.write_child(inner, *item, args, depth + 1)
                    }
                }
            }
            Ty::Result(ok, err) => {
                let Value::Result(result) = value else {
                    return Err(self.mismatch(expected, value));
                };
                self.push(Tag::Result as u8)?;
                match result {
                    Ok(value) => {
                        self.push(0)?;
                        self.write_child(value, *ok, args, depth + 1)
                    }
                    Err(value) => {
                        self.push(1)?;
                        self.write_child(value, *err, args, depth + 1)
                    }
                }
            }
            Ty::Named(type_id, type_args) => {
                let Some(def) = types.get(*type_id) else {
                    return Err("state references an unknown type".into());
                };
                match &def.kind {
                    TypeDefKind::Struct { fields } => {
                        let Value::Struct(struct_value) = value else {
                            return Err(self.mismatch(expected, value));
                        };
                        if struct_value.type_id != *type_id {
                            return Err(format!(
                                "state struct type `{}` does not match the declared type `{}`",
                                def_name(types, struct_value.type_id),
                                def_name(types, *type_id)
                            ));
                        }
                        if struct_value.fields.len() != fields.len() {
                            return Err("state struct field count does not match its type".into());
                        }
                        self.push(Tag::Struct as u8)?;
                        self.write_len(type_id.0 as usize)?;
                        self.write_len(fields.len())?;
                        let named_args = resolve_args(types, type_args, args)?;
                        for (field_value, field) in struct_value.fields.iter().zip(fields) {
                            self.write_child(field_value, field.ty, &named_args, depth + 1)?;
                        }
                        Ok(())
                    }
                    TypeDefKind::Enum { variants } => {
                        let Value::Enum(enum_value) = value else {
                            return Err(self.mismatch(expected, value));
                        };
                        if enum_value.type_id != *type_id {
                            return Err(format!(
                                "state enum type `{}` does not match the declared type `{}`",
                                def_name(types, enum_value.type_id),
                                def_name(types, *type_id)
                            ));
                        }
                        let Some(variant) = variants.get(enum_value.variant) else {
                            return Err("state enum variant is out of range".into());
                        };
                        if enum_value.payload.len() != variant.payload.len() {
                            return Err(
                                "state enum payload count does not match its variant".into()
                            );
                        }
                        self.push(Tag::Enum as u8)?;
                        self.write_len(type_id.0 as usize)?;
                        self.write_len(enum_value.variant)?;
                        self.write_len(variant.payload.len())?;
                        let named_args = resolve_args(types, type_args, args)?;
                        for (field_value, field) in enum_value.payload.iter().zip(&variant.payload)
                        {
                            self.write_child(field_value, *field, &named_args, depth + 1)?;
                        }
                        Ok(())
                    }
                    TypeDefKind::Opaque => {
                        let Value::Handle(handle) = value else {
                            return Err(self.mismatch(expected, value));
                        };
                        if handle.type_id != *type_id {
                            return Err(format!(
                                "state handle type `{}` does not match the declared type `{}`",
                                def_name(types, handle.type_id),
                                def_name(types, *type_id)
                            ));
                        }
                        self.push(Tag::Handle as u8)?;
                        self.write_len(type_id.0 as usize)?;
                        self.write_bytes(handle.token.as_bytes())
                    }
                }
            }
            Ty::Fn(_, _) => Err("state cannot contain function values".into()),
            Ty::Never | Ty::Param(_) => unreachable!(),
        }
    }

    fn write_child(
        &mut self,
        value: &Value,
        ty_id: TyId,
        args: &[TyId],
        depth: usize,
    ) -> Result<(), String> {
        let ty = match self.types.get_ty(ty_id) {
            Some(Ty::Param(index)) => match args.get(*index as usize) {
                Some(actual) => match self.types.get_ty(*actual) {
                    Some(Ty::Param(_)) | None => return self.write_unchecked(value, depth),
                    Some(ty) => ty.clone(),
                },
                None => return self.write_unchecked(value, depth),
            },
            Some(ty) => ty.clone(),
            None => return Err("state references an unknown type".into()),
        };
        self.write_value(value, &ty, args, depth)
    }

    /// Write without a declared type. Used only for a generic parameter whose
    /// concrete argument was erased; non-data values are still rejected.
    fn write_unchecked(&mut self, value: &Value, depth: usize) -> Result<(), String> {
        if depth > MAX_VALUE_DEPTH {
            return Err("snapshot value nesting is too deep".into());
        }
        use Value::*;
        match value {
            Unit => self.push(Tag::Unit as u8),
            Int(v) => {
                self.push(Tag::Int as u8)?;
                self.extend(&v.to_le_bytes())
            }
            Float(v) => {
                self.push(Tag::Float as u8)?;
                self.extend(&v.to_bits().to_le_bytes())
            }
            Bool(v) => {
                self.push(Tag::Bool as u8)?;
                self.push(*v as u8)
            }
            Str(v) => {
                self.push(Tag::Str as u8)?;
                self.write_bytes(v.as_bytes())
            }
            List(items) => {
                self.push(Tag::List as u8)?;
                self.write_len(items.len())?;
                for item in items.iter() {
                    self.write_unchecked(item, depth + 1)?;
                }
                Ok(())
            }
            Tuple(items) => {
                self.push(Tag::Tuple as u8)?;
                self.write_len(items.len())?;
                for item in items.iter() {
                    self.write_unchecked(item, depth + 1)?;
                }
                Ok(())
            }
            Map(map) => {
                self.push(Tag::Map as u8)?;
                self.write_len(map.len())?;
                for (key, value) in map.entries() {
                    self.write_unchecked(&key_to_value(key), depth + 1)?;
                    self.write_unchecked(value, depth + 1)?;
                }
                Ok(())
            }
            Range {
                start,
                end,
                inclusive,
            } => {
                self.push(Tag::Range as u8)?;
                self.extend(&start.to_le_bytes())?;
                self.extend(&end.to_le_bytes())?;
                self.push(*inclusive as u8)
            }
            Struct(value) => {
                self.push(Tag::Struct as u8)?;
                self.write_len(value.type_id.0 as usize)?;
                self.write_len(value.fields.len())?;
                for field in &value.fields {
                    self.write_unchecked(field, depth + 1)?;
                }
                Ok(())
            }
            Enum(value) => {
                self.push(Tag::Enum as u8)?;
                self.write_len(value.type_id.0 as usize)?;
                self.write_len(value.variant)?;
                self.write_len(value.payload.len())?;
                for field in &value.payload {
                    self.write_unchecked(field, depth + 1)?;
                }
                Ok(())
            }
            Option(value) => {
                self.push(Tag::Option as u8)?;
                match value {
                    None => self.push(0),
                    Some(value) => {
                        self.push(1)?;
                        self.write_unchecked(value, depth + 1)
                    }
                }
            }
            Result(result) => {
                self.push(Tag::Result as u8)?;
                match result {
                    Ok(value) => {
                        self.push(0)?;
                        self.write_unchecked(value, depth + 1)
                    }
                    Err(value) => {
                        self.push(1)?;
                        self.write_unchecked(value, depth + 1)
                    }
                }
            }
            Handle(value) => {
                self.push(Tag::Handle as u8)?;
                self.write_len(value.type_id.0 as usize)?;
                self.write_bytes(value.token.as_bytes())
            }
            Fn(_) | Builtin(_) | BuiltinModuleFn(_) => {
                Err("snapshot contains a non-data value".into())
            }
        }
    }

    fn expect(&self, expected: &Ty, value: &Value, ok: bool) -> Result<(), String> {
        if ok {
            Ok(())
        } else {
            Err(self.mismatch(expected, value))
        }
    }

    fn mismatch(&self, expected: &Ty, value: &Value) -> String {
        format!(
            "state value `{}` does not match the declared type `{}`",
            value_kind(value),
            ty_name(self.types, expected)
        )
    }

    fn reserve(&self, additional: usize) -> Result<(), String> {
        let next = self
            .out
            .len()
            .checked_add(additional)
            .ok_or_else(|| self.budget_error())?;
        if next as u64 > self.limit {
            Err(self.budget_error())
        } else {
            Ok(())
        }
    }

    fn budget_error(&self) -> String {
        format!("snapshot exceeds the encode budget of {} bytes", self.limit)
    }

    fn push(&mut self, byte: u8) -> Result<(), String> {
        self.reserve(1)?;
        self.out.push(byte);
        Ok(())
    }

    fn extend(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.reserve(bytes.len())?;
        self.out.extend_from_slice(bytes);
        Ok(())
    }

    fn write_len(&mut self, value: usize) -> Result<(), String> {
        self.extend(&(value as u64).to_le_bytes())
    }

    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.write_len(bytes.len())?;
        self.extend(bytes)
    }
}

fn container_bytes(slot: u64, len: usize) -> u64 {
    16u64.saturating_add(slot.saturating_mul(len as u64))
}

fn resolve_args(types: &TypeTable, ids: &[TyId], args: &[TyId]) -> Result<Vec<TyId>, String> {
    let mut resolved = Vec::with_capacity(ids.len());
    for id in ids {
        match types.get_ty(*id) {
            Some(Ty::Param(index)) => {
                resolved.push(args.get(*index as usize).copied().unwrap_or(*id));
            }
            Some(_) => resolved.push(*id),
            None => return Err("state references an unknown type argument".into()),
        }
    }
    Ok(resolved)
}

fn tag_name(tag: Tag) -> &'static str {
    match tag {
        Tag::Unit => "unit",
        Tag::Int => "int",
        Tag::Float => "float",
        Tag::Bool => "bool",
        Tag::Str => "str",
        Tag::List => "list",
        Tag::Tuple => "tuple",
        Tag::Map => "map",
        Tag::Range => "range",
        Tag::Struct => "struct",
        Tag::Enum => "enum",
        Tag::Option => "option",
        Tag::Result => "result",
        Tag::Handle => "handle",
        Tag::Function => "function",
    }
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Unit => "unit",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Bool(_) => "bool",
        Value::Str(_) => "str",
        Value::List(_) => "list",
        Value::Tuple(_) => "tuple",
        Value::Map(_) => "map",
        Value::Range { .. } => "range",
        Value::Struct(_) => "struct",
        Value::Enum(_) => "enum",
        Value::Option(_) => "option",
        Value::Result(_) => "result",
        Value::Fn(_) | Value::Builtin(_) | Value::BuiltinModuleFn(_) => "function",
        Value::Handle(_) => "handle",
    }
}

fn def_name(types: &TypeTable, id: TypeId) -> String {
    types
        .get(id)
        .map(|def| def.name.to_string())
        .unwrap_or_else(|| format!("type #{}", id.0))
}

fn ty_name(types: &TypeTable, ty: &Ty) -> String {
    match ty {
        Ty::Int => "int".into(),
        Ty::Float => "float".into(),
        Ty::Bool => "bool".into(),
        Ty::Str => "str".into(),
        Ty::Unit => "unit".into(),
        Ty::Range => "range".into(),
        Ty::Never => "never".into(),
        Ty::Tuple(items) => format!("tuple of {}", items.len()),
        Ty::List(_) => "list".into(),
        Ty::Map(_, _) => "map".into(),
        Ty::Option(_) => "option".into(),
        Ty::Result(_, _) => "result".into(),
        Ty::Fn(_, _) => "fn".into(),
        Ty::Named(id, _) => def_name(types, *id),
        Ty::Param(index) => format!("T{index}"),
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    budget: u64,
    allocated: u64,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8], budget: u64) -> Self {
        Self {
            bytes,
            pos: 0,
            budget,
            allocated: 0,
        }
    }

    /// Account for the heap a decoded value will occupy before allocating it.
    ///
    /// The accounting mirrors the runtime's nominal value sizes (`8` bytes per
    /// sequence slot, `16` per map entry or wrapper) rather than exact Rust
    /// layout, so a state that fit `Limits::memory` at runtime also fits the
    /// same budget when restored.
    fn charge(&mut self, bytes: u64) -> Result<(), String> {
        self.allocated = self.allocated.saturating_add(bytes);
        if self.allocated > self.budget {
            return Err(format!(
                "snapshot exceeds the restore memory budget of {} bytes",
                self.budget
            ));
        }
        Ok(())
    }

    fn read_u8(&mut self) -> Result<u8, String> {
        let byte = *self.bytes.get(self.pos).ok_or("truncated snapshot")?;
        self.pos += 1;
        Ok(byte)
    }

    fn read_u64(&mut self) -> Result<u64, String> {
        let rest = self.bytes.get(self.pos..).ok_or("truncated snapshot")?;
        let slice = rest.get(..8).ok_or("truncated snapshot")?;
        self.pos += 8;
        Ok(u64::from_le_bytes(
            slice.try_into().map_err(|_| "truncated snapshot")?,
        ))
    }

    fn read_i64(&mut self) -> Result<i64, String> {
        Ok(self.read_u64()? as i64)
    }

    fn read_len(&mut self) -> Result<usize, String> {
        usize::try_from(self.read_u64()?).map_err(|_| "snapshot length is too large".into())
    }

    fn read_type_id(&mut self) -> Result<u32, String> {
        u32::try_from(self.read_len()?).map_err(|_| "snapshot type id is too large".into())
    }

    fn read_collection_len(&mut self, min_item_bytes: usize) -> Result<usize, String> {
        let len = self.read_len()?;
        let remaining = self.bytes.len().saturating_sub(self.pos);
        if len > remaining / min_item_bytes.max(1) {
            return Err("snapshot container length exceeds remaining bytes".into());
        }
        Ok(len)
    }

    fn read_string(&mut self) -> Result<String, String> {
        let len = self.read_len()?;
        let remaining = self.bytes.len().saturating_sub(self.pos);
        if len > remaining {
            return Err("truncated snapshot string".into());
        }
        self.charge(16u64.saturating_add(len as u64))?;
        let slice = &self.bytes[self.pos..self.pos + len];
        self.pos += len;
        String::from_utf8(slice.to_vec()).map_err(|_| "invalid UTF-8 in snapshot".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use martite_ir::{BuiltinFnId, FieldDef, Ty, TypeDef, VariantDef};
    use martite_runtime::KeyValue;

    struct TestTypes {
        table: TypeTable,
        point: TypeId,
        other: TypeId,
        choice: TypeId,
        doc: TypeId,
        state: TypeId,
    }

    fn test_types() -> TestTypes {
        let mut table = TypeTable::default();
        let int = table.add_ty(Ty::Int);
        let point = table.add_def(TypeDef {
            name: "Point".into(),
            kind: TypeDefKind::Struct {
                fields: vec![
                    FieldDef {
                        name: "x".into(),
                        ty: int,
                    },
                    FieldDef {
                        name: "y".into(),
                        ty: int,
                    },
                ],
            },
            params: 0,
            is_interface: false,
            is_builtin: false,
        });
        let other = table.add_def(TypeDef {
            name: "Other".into(),
            kind: TypeDefKind::Struct {
                fields: vec![
                    FieldDef {
                        name: "a".into(),
                        ty: int,
                    },
                    FieldDef {
                        name: "b".into(),
                        ty: int,
                    },
                    FieldDef {
                        name: "c".into(),
                        ty: int,
                    },
                ],
            },
            params: 0,
            is_interface: false,
            is_builtin: false,
        });
        let choice = table.add_def(TypeDef {
            name: "Choice".into(),
            kind: TypeDefKind::Enum {
                variants: vec![
                    VariantDef {
                        name: "Empty".into(),
                        payload: Vec::new(),
                    },
                    VariantDef {
                        name: "One".into(),
                        payload: vec![int],
                    },
                ],
            },
            params: 0,
            is_interface: false,
            is_builtin: false,
        });
        let doc = table.add_def(TypeDef {
            name: "Doc".into(),
            kind: TypeDefKind::Opaque,
            params: 0,
            is_interface: true,
            is_builtin: false,
        });
        let state = table.add_def(TypeDef {
            name: "State".into(),
            kind: TypeDefKind::Struct {
                fields: vec![
                    FieldDef {
                        name: "saves".into(),
                        ty: int,
                    },
                    FieldDef {
                        name: "last_len".into(),
                        ty: int,
                    },
                    FieldDef {
                        name: "rest".into(),
                        ty: int,
                    },
                ],
            },
            params: 0,
            is_interface: false,
            is_builtin: false,
        });
        TestTypes {
            table,
            point,
            other,
            choice,
            doc,
            state,
        }
    }

    fn codec(types: &TestTypes) -> ValueCodec<'_> {
        ValueCodec::new(&types.table)
    }

    fn accept_handles(_: &str, _: &str) -> bool {
        true
    }

    fn codec_with_handles(types: &TestTypes) -> ValueCodec<'_> {
        let validator: &dyn Fn(&str, &str) -> bool = &accept_handles;
        ValueCodec::new(&types.table).with_handle_validator(Some(validator))
    }

    fn wildcard() -> Ty {
        Ty::Param(0)
    }

    fn state_ty(types: &TestTypes) -> Ty {
        Ty::Named(types.state, Vec::new())
    }

    fn encode_wild(types: &TestTypes, value: &Value) -> Vec<u8> {
        codec(types)
            .encode(value, &wildcard(), u64::MAX)
            .expect("encode")
    }

    fn decode_wild(types: &TestTypes, bytes: &[u8]) -> Result<Value, String> {
        codec_with_handles(types).decode(bytes, &wildcard(), u64::MAX)
    }

    fn complex_value(types: &TestTypes) -> Value {
        let mut map = OrderedMap::new();
        map.insert(KeyValue::Int(1), Value::str("one"));
        map.insert(KeyValue::Bool(true), Value::list(vec![Value::Unit]));
        map.insert(
            KeyValue::Str("s".into()),
            Value::tuple(vec![Value::Int(2), Value::Bool(false)]),
        );
        map.insert(
            KeyValue::Tuple(vec![KeyValue::Int(3), KeyValue::Str("t".into())]),
            Value::Range {
                start: -2,
                end: 7,
                inclusive: true,
            },
        );
        map.insert(
            KeyValue::Struct(types.point, vec![KeyValue::Int(1), KeyValue::Int(2)]),
            Value::Option(Some(Box::new(Value::str("some")))),
        );
        map.insert(
            KeyValue::Enum(types.choice, 1, vec![KeyValue::Int(4)]),
            Value::Result(Err(Box::new(Value::str("err")))),
        );
        map.insert(
            KeyValue::Option(Some(Box::new(KeyValue::Int(5)))),
            Value::Bool(true),
        );
        map.insert(
            KeyValue::Result(Ok(Box::new(KeyValue::Int(6)))),
            Value::Bool(false),
        );
        map.insert(
            KeyValue::Handle(types.doc, "token".into()),
            Value::Float(1.25),
        );

        let nested = vec![
            Value::Unit,
            Value::Int(-9),
            Value::Float(1.5),
            Value::Bool(true),
            Value::str("héllo"),
            Value::Struct(Rc::new(StructValue::new(
                types.point,
                vec![Value::Int(1), Value::Int(2)],
            ))),
            Value::Enum(Rc::new(EnumValue::new(
                types.choice,
                1,
                vec![Value::Int(3)],
            ))),
            Value::Option(None),
            Value::Option(Some(Box::new(Value::Int(4)))),
            Value::Result(Ok(Box::new(Value::Int(5)))),
            Value::Result(Err(Box::new(Value::str("bad")))),
            Value::Handle(Rc::new(HandleValue {
                type_id: types.doc,
                token: "nested".into(),
            })),
            Value::map(map.clone()),
            Value::Range {
                start: 0,
                end: 3,
                inclusive: false,
            },
            Value::tuple(vec![Value::Int(6), Value::str("t")]),
        ];

        Value::Struct(Rc::new(StructValue::new(
            types.state,
            vec![
                Value::list(nested),
                Value::map(map),
                Value::tuple(vec![
                    Value::Option(None),
                    Value::Result(Ok(Box::new(Value::Unit))),
                ]),
            ],
        )))
    }

    fn state_value(types: &TestTypes) -> Value {
        Value::Struct(Rc::new(StructValue::new(
            types.state,
            vec![Value::Int(1), Value::Int(2), Value::Int(3)],
        )))
    }

    #[test]
    fn wire_tags_are_stable() {
        let tags = [
            (0, Tag::Unit),
            (1, Tag::Int),
            (2, Tag::Float),
            (3, Tag::Bool),
            (4, Tag::Str),
            (5, Tag::List),
            (6, Tag::Tuple),
            (7, Tag::Map),
            (8, Tag::Range),
            (9, Tag::Struct),
            (10, Tag::Enum),
            (11, Tag::Option),
            (12, Tag::Result),
            (13, Tag::Handle),
            (14, Tag::Function),
        ];
        for (byte, tag) in tags {
            assert_eq!(tag as u8, byte);
            assert_eq!(Tag::from_byte(byte), Some(tag));
        }
        assert_eq!(Tag::from_byte(15), None);
        assert_eq!(Tag::from_byte(255), None);
    }

    #[test]
    fn complex_value_round_trips() {
        let types = test_types();
        let value = complex_value(&types);
        let bytes = codec_with_handles(&types)
            .encode(&value, &wildcard(), u64::MAX)
            .expect("encode");
        let decoded = codec_with_handles(&types)
            .decode(&bytes, &wildcard(), u64::MAX)
            .expect("decode");
        assert_eq!(decoded, value);
    }

    #[test]
    fn map_key_order_is_preserved() {
        let types = test_types();
        let mut map = OrderedMap::new();
        for key in [
            KeyValue::Str("b".into()),
            KeyValue::Str("a".into()),
            KeyValue::Int(1),
        ] {
            map.insert(key, Value::Unit);
        }
        let bytes = codec(&types)
            .encode(&Value::map(map), &wildcard(), u64::MAX)
            .expect("encode");
        let decoded = codec(&types)
            .decode(&bytes, &wildcard(), u64::MAX)
            .expect("decode");
        let Value::Map(decoded) = decoded else {
            panic!("expected map");
        };
        assert_eq!(
            decoded.keys(),
            vec![
                KeyValue::Str("b".into()),
                KeyValue::Str("a".into()),
                KeyValue::Int(1)
            ]
        );
    }

    #[test]
    fn float_bits_round_trip() {
        let types = test_types();
        for bits in [
            f64::NAN.to_bits(),
            f64::INFINITY.to_bits(),
            (-0.0f64).to_bits(),
            1.25f64.to_bits(),
        ] {
            let bytes = codec(&types)
                .encode(&Value::Float(f64::from_bits(bits)), &Ty::Float, u64::MAX)
                .expect("encode");
            let Value::Float(value) = codec(&types)
                .decode(&bytes, &Ty::Float, u64::MAX)
                .expect("decode")
            else {
                panic!("expected float");
            };
            assert_eq!(value.to_bits(), bits);
        }
    }

    #[test]
    fn every_truncated_prefix_is_rejected() {
        let types = test_types();
        let bytes = encode_wild(&types, &complex_value(&types));
        for end in 0..bytes.len() {
            assert!(
                decode_wild(&types, &bytes[..end]).is_err(),
                "prefix length {end}"
            );
        }
        assert!(decode_wild(&types, &bytes).is_ok());
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let types = test_types();
        let bytes = codec(&types)
            .encode(&Value::Int(1), &Ty::Int, u64::MAX)
            .expect("encode");
        let mut bytes = bytes;
        bytes.push(0);
        assert!(codec(&types).decode(&bytes, &Ty::Int, u64::MAX).is_err());
    }

    #[test]
    fn non_data_tags_are_rejected() {
        let types = test_types();
        assert!(decode_wild(&types, &[Tag::Function as u8]).is_err());
        assert!(decode_wild(&types, &[15]).is_err());
        assert!(decode_wild(&types, &[255]).is_err());

        let function = Value::Builtin(BuiltinFnId::new(0));
        assert!(
            codec(&types)
                .encode(&function, &wildcard(), u64::MAX)
                .is_err()
        );
    }

    #[test]
    fn oversized_and_malformed_payloads_are_rejected() {
        let types = test_types();

        let mut string = vec![Tag::Str as u8];
        string.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode_wild(&types, &string).is_err());

        let mut list = vec![Tag::List as u8];
        list.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode_wild(&types, &list).is_err());

        let mut map = vec![Tag::Map as u8];
        map.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode_wild(&types, &map).is_err());

        let mut utf8 = vec![Tag::Str as u8];
        utf8.extend_from_slice(&1u64.to_le_bytes());
        utf8.push(0xFF);
        assert!(decode_wild(&types, &utf8).is_err());

        let mut type_id = vec![Tag::Struct as u8];
        type_id.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode_wild(&types, &type_id).is_err());
    }

    fn raw_struct(type_id: u64, len: u64) -> Vec<u8> {
        let mut bytes = vec![Tag::Struct as u8];
        bytes.extend_from_slice(&type_id.to_le_bytes());
        bytes.extend_from_slice(&len.to_le_bytes());
        bytes
    }

    fn raw_enum(type_id: u64, variant: u64, len: u64) -> Vec<u8> {
        let mut bytes = vec![Tag::Enum as u8];
        bytes.extend_from_slice(&type_id.to_le_bytes());
        bytes.extend_from_slice(&variant.to_le_bytes());
        bytes.extend_from_slice(&len.to_le_bytes());
        bytes
    }

    fn raw_handle(type_id: u64) -> Vec<u8> {
        let mut bytes = vec![Tag::Handle as u8];
        bytes.extend_from_slice(&type_id.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes
    }

    #[test]
    fn container_headers_are_validated() {
        let types = test_types();

        assert!(decode_wild(&types, &raw_struct(999, 0)).is_err());
        assert!(decode_wild(&types, &raw_struct(types.choice.0 as u64, 0)).is_err());
        assert!(decode_wild(&types, &raw_struct(types.point.0 as u64, 0)).is_err());
        assert!(decode_wild(&types, &raw_enum(999, 0, 0)).is_err());
        assert!(decode_wild(&types, &raw_enum(types.choice.0 as u64, 9, 0)).is_err());
        assert!(decode_wild(&types, &raw_enum(types.choice.0 as u64, 1, 0)).is_err());
        assert!(decode_wild(&types, &raw_handle(999)).is_err());
        assert!(decode_wild(&types, &raw_handle(types.point.0 as u64)).is_err());
    }

    #[test]
    fn map_keys_use_value_tags_and_reject_non_key_values() {
        let types = test_types();
        let mut bytes = vec![Tag::Map as u8];
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(&encode_wild(&types, &Value::list(vec![Value::Unit])));
        bytes.push(Tag::Unit as u8);
        assert!(decode_wild(&types, &bytes).is_err());
    }

    #[test]
    fn depth_limit_is_enforced() {
        let types = test_types();
        let mut value = Value::Unit;
        for _ in 0..MAX_VALUE_DEPTH {
            value = Value::list(vec![value]);
        }
        let bytes = codec(&types)
            .encode(&value, &wildcard(), u64::MAX)
            .expect("encode at the depth limit");
        assert!(codec(&types).decode(&bytes, &wildcard(), u64::MAX).is_ok());

        let too_deep = Value::list(vec![value]);
        assert!(
            codec(&types)
                .encode(&too_deep, &wildcard(), u64::MAX)
                .is_err()
        );
    }

    #[test]
    fn handle_tokens_round_trip_and_require_a_host_validator() {
        let types = test_types();
        let value = Value::Handle(Rc::new(HandleValue {
            type_id: types.doc,
            token: "stale".into(),
        }));
        let bytes = codec(&types)
            .encode(&value, &wildcard(), u64::MAX)
            .expect("encode");
        assert!(
            codec(&types).decode(&bytes, &wildcard(), u64::MAX).is_err(),
            "handles must be rejected without a validator"
        );
        let decoded = codec_with_handles(&types)
            .decode(&bytes, &wildcard(), u64::MAX)
            .expect("decode");
        let Value::Handle(decoded) = decoded else {
            panic!("expected handle");
        };
        assert_eq!(decoded.token, "stale");

        let bad = Value::Handle(Rc::new(HandleValue {
            type_id: types.point,
            token: "x".into(),
        }));
        assert!(
            codec(&types)
                .encode(&bad, &Ty::Named(types.point, Vec::new()), u64::MAX)
                .is_err()
        );
        assert!(decode_wild(&types, &raw_handle(types.point.0 as u64)).is_err());
    }

    #[test]
    fn length_overflow_is_rejected() {
        let bytes = u64::MAX.to_le_bytes();
        let mut reader = Reader::new(&bytes, u64::MAX);
        assert!(reader.read_collection_len(1).is_err());
    }

    #[test]
    fn string_length_is_bounded_by_remaining_bytes() {
        let mut bytes = 100u64.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"short");
        let mut reader = Reader::new(&bytes, u64::MAX);
        assert!(reader.read_string().is_err());
    }

    #[test]
    fn arbitrary_bytes_do_not_panic() {
        let types = test_types();
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for len in 0..256 {
            let mut bytes = Vec::with_capacity(len);
            for _ in 0..len {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                bytes.push((state >> 33) as u8);
            }
            let _ = codec(&types).decode(&bytes, &wildcard(), u64::MAX);
        }
    }

    #[test]
    fn encoded_state_matches_the_wire_layout() {
        let types = test_types();
        let value = state_value(&types);
        let bytes = codec(&types)
            .encode(&value, &state_ty(&types), u64::MAX)
            .expect("encode");

        let mut expected = vec![Tag::Struct as u8];
        expected.extend_from_slice(&(types.state.0 as u64).to_le_bytes());
        expected.extend_from_slice(&3u64.to_le_bytes());
        for field in [1i64, 2, 3] {
            expected.push(Tag::Int as u8);
            expected.extend_from_slice(&field.to_le_bytes());
        }
        assert_eq!(bytes, expected);

        let decoded = codec(&types)
            .decode(&bytes, &state_ty(&types), u64::MAX)
            .expect("decode");
        assert_eq!(decoded, value);
    }

    #[test]
    fn encoding_rejects_values_that_do_not_match_the_state_type() {
        let types = test_types();
        let forged = Value::Struct(Rc::new(StructValue::new(
            types.state,
            vec![Value::str("not an int"), Value::Int(2), Value::Int(3)],
        )));
        let error = codec(&types)
            .encode(&forged, &state_ty(&types), u64::MAX)
            .expect_err("a forged field type must be rejected");
        assert!(error.contains("declared type"), "{error}");
    }

    #[test]
    fn encoding_rejects_an_unrelated_struct_type() {
        let types = test_types();
        let other = Value::Struct(Rc::new(StructValue::new(
            types.other,
            vec![Value::Int(1), Value::Int(2), Value::Int(3)],
        )));
        let error = codec(&types)
            .encode(&other, &state_ty(&types), u64::MAX)
            .expect_err("an unrelated struct type must be rejected");
        assert!(error.contains("does not match"), "{error}");

        let point = Value::Struct(Rc::new(StructValue::new(
            types.point,
            vec![Value::Int(1), Value::Int(2)],
        )));
        assert!(
            codec(&types)
                .encode(&point, &state_ty(&types), u64::MAX)
                .is_err()
        );
    }

    #[test]
    fn decoding_rejects_forged_field_types() {
        let types = test_types();
        let forged = Value::Struct(Rc::new(StructValue::new(
            types.state,
            vec![Value::str("not an int"), Value::Int(2), Value::Int(3)],
        )));
        let bytes = encode_wild(&types, &forged);
        let error = codec(&types)
            .decode(&bytes, &state_ty(&types), u64::MAX)
            .expect_err("a forged field type must be rejected");
        assert!(error.contains("declared type"), "{error}");
    }

    #[test]
    fn decoding_rejects_a_struct_of_an_unrelated_type() {
        let types = test_types();
        let other = Value::Struct(Rc::new(StructValue::new(
            types.other,
            vec![Value::Int(1), Value::Int(2), Value::Int(3)],
        )));
        let bytes = encode_wild(&types, &other);
        let error = codec(&types)
            .decode(&bytes, &state_ty(&types), u64::MAX)
            .expect_err("an unrelated struct type must be rejected");
        assert!(error.contains("does not match"), "{error}");
    }

    #[test]
    fn decode_budget_bounds_oversized_collections() {
        let types = test_types();
        let mut bytes = vec![Tag::List as u8];
        bytes.extend_from_slice(&10_000u64.to_le_bytes());
        bytes.extend_from_slice(&vec![Tag::Unit as u8; 10_000]);

        let error = codec(&types)
            .decode(&bytes, &wildcard(), 20_000)
            .expect_err("the decoded allocation must be bounded");
        assert!(error.contains("memory budget"), "{error}");
        assert!(
            codec(&types).decode(&bytes, &wildcard(), 100).is_err(),
            "the input bytes must be capped"
        );
    }

    #[test]
    fn encode_budget_rejects_expanding_shared_values() {
        let types = test_types();
        let mut value = Value::Enum(Rc::new(EnumValue::new(types.choice, 0, vec![])));
        for _ in 0..40 {
            value = Value::Enum(Rc::new(EnumValue::new(
                types.choice,
                1,
                vec![value.clone(), value],
            )));
        }
        let error = codec(&types)
            .encode(&value, &wildcard(), 4 * 1024)
            .expect_err("the expanded DAG must exceed the encode budget");
        assert!(error.contains("encode budget"), "{error}");
    }
}
