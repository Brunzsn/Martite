//! Versioned binary codec for plugin state snapshots.
//!
//! One tag table drives both the encoder and the decoder. Map keys are encoded
//! as ordinary values and converted through `key_to_value` / `key_from_value`,
//! so there is no separate key-only schema to keep in sync.

use std::rc::Rc;

use martite_ir::{TypeDefKind, TypeId, TypeTable};
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
}

impl<'a> ValueCodec<'a> {
    pub(crate) fn new(types: &'a TypeTable) -> Self {
        Self { types }
    }

    pub(crate) fn encode(&self, value: &Value) -> Vec<u8> {
        let mut out = Vec::new();
        write_value(&mut out, value);
        out
    }

    pub(crate) fn decode(&self, bytes: &[u8]) -> Result<Value, String> {
        let mut reader = Reader::new(bytes);
        let value = self.read_value(&mut reader, 0)?;
        if reader.pos != bytes.len() {
            return Err("trailing bytes in snapshot".into());
        }
        Ok(value)
    }

    fn read_value(&self, reader: &mut Reader<'_>, depth: usize) -> Result<Value, String> {
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
                let mut items = Vec::new();
                for _ in 0..len {
                    items.push(self.read_value(reader, depth + 1)?);
                }
                Value::list(items)
            }
            Tag::Tuple => {
                let len = reader.read_collection_len(1)?;
                let mut items = Vec::new();
                for _ in 0..len {
                    items.push(self.read_value(reader, depth + 1)?);
                }
                Value::tuple(items)
            }
            Tag::Map => {
                let len = reader.read_collection_len(2)?;
                let mut map = OrderedMap::new();
                for _ in 0..len {
                    // A map key is an ordinary value on the wire. `key_from_value`
                    // enforces the key-eligible subset, so there is no second tag
                    // ordering to keep in sync with the value tags.
                    let key_value = self.read_value(reader, depth + 1)?;
                    let key = key_from_value(&key_value)
                        .map_err(|_| "invalid map key in snapshot".to_string())?;
                    let value = self.read_value(reader, depth + 1)?;
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
                let mut values = Vec::new();
                for _ in 0..len {
                    values.push(self.read_value(reader, depth + 1)?);
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
                let mut payload = Vec::new();
                for _ in 0..len {
                    payload.push(self.read_value(reader, depth + 1)?);
                }
                Value::Enum(Rc::new(EnumValue::new(type_id, variant, payload)))
            }
            Tag::Option => {
                if reader.read_u8()? == 0 {
                    Value::Option(None)
                } else {
                    Value::Option(Some(Box::new(self.read_value(reader, depth + 1)?)))
                }
            }
            Tag::Result => {
                if reader.read_u8()? == 0 {
                    Value::Result(Ok(Box::new(self.read_value(reader, depth + 1)?)))
                } else {
                    Value::Result(Err(Box::new(self.read_value(reader, depth + 1)?)))
                }
            }
            Tag::Handle => {
                let type_id = TypeId(reader.read_type_id()?);
                let token = reader.read_string()?;
                let Some(def) = self.types.get(type_id) else {
                    return Err("snapshot references an unknown handle type".into());
                };
                if !matches!(&def.kind, TypeDefKind::Opaque) {
                    return Err("snapshot handle type is not opaque".into());
                }
                // The token is host-defined and intentionally opaque here. The
                // codec checks the type, not the session the token belongs to;
                // the host decides whether a restored token is still valid.
                Value::Handle(Rc::new(HandleValue { type_id, token }))
            }
            Tag::Function => return Err("snapshot contains a non-data value".into()),
        })
    }
}

fn write_value(out: &mut Vec<u8>, value: &Value) {
    use Value::*;
    match value {
        Unit => out.push(Tag::Unit as u8),
        Int(v) => {
            out.push(Tag::Int as u8);
            out.extend_from_slice(&v.to_le_bytes());
        }
        Float(v) => {
            out.push(Tag::Float as u8);
            out.extend_from_slice(&v.to_bits().to_le_bytes());
        }
        Bool(v) => {
            out.push(Tag::Bool as u8);
            out.push(*v as u8);
        }
        Str(v) => {
            out.push(Tag::Str as u8);
            write_bytes(out, v.as_bytes());
        }
        List(items) => {
            out.push(Tag::List as u8);
            write_len(out, items.len());
            for item in items.iter() {
                write_value(out, item);
            }
        }
        Tuple(items) => {
            out.push(Tag::Tuple as u8);
            write_len(out, items.len());
            for item in items.iter() {
                write_value(out, item);
            }
        }
        Map(map) => {
            out.push(Tag::Map as u8);
            write_len(out, map.len());
            for (key, value) in map.entries() {
                write_value(out, &key_to_value(key));
                write_value(out, value);
            }
        }
        Range {
            start,
            end,
            inclusive,
        } => {
            out.push(Tag::Range as u8);
            out.extend_from_slice(&start.to_le_bytes());
            out.extend_from_slice(&end.to_le_bytes());
            out.push(*inclusive as u8);
        }
        Struct(value) => {
            out.push(Tag::Struct as u8);
            write_len(out, value.type_id.0 as usize);
            write_len(out, value.fields.len());
            for field in &value.fields {
                write_value(out, field);
            }
        }
        Enum(value) => {
            out.push(Tag::Enum as u8);
            write_len(out, value.type_id.0 as usize);
            write_len(out, value.variant);
            write_len(out, value.payload.len());
            for field in &value.payload {
                write_value(out, field);
            }
        }
        Option(value) => {
            out.push(Tag::Option as u8);
            match value {
                None => out.push(0),
                Some(value) => {
                    out.push(1);
                    write_value(out, value);
                }
            }
        }
        Result(result) => {
            out.push(Tag::Result as u8);
            match result {
                Ok(value) => {
                    out.push(0);
                    write_value(out, value);
                }
                Err(value) => {
                    out.push(1);
                    write_value(out, value);
                }
            }
        }
        Handle(value) => {
            out.push(Tag::Handle as u8);
            write_len(out, value.type_id.0 as usize);
            write_bytes(out, value.token.as_bytes());
        }
        Fn(_) | Builtin(_) | BuiltinModuleFn(_) => out.push(Tag::Function as u8),
    }
}

fn write_len(out: &mut Vec<u8>, value: usize) {
    out.extend_from_slice(&(value as u64).to_le_bytes());
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    write_len(out, bytes.len());
    out.extend_from_slice(bytes);
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
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
                        name: "values".into(),
                        ty: int,
                    },
                    FieldDef {
                        name: "map".into(),
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
            choice,
            doc,
            state,
        }
    }

    fn codec(types: &TestTypes) -> ValueCodec<'_> {
        ValueCodec::new(&types.table)
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
        let bytes = codec(&types).encode(&value);
        let decoded = codec(&types).decode(&bytes).expect("decode");
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
        let bytes = codec(&types).encode(&Value::map(map));
        let decoded = codec(&types).decode(&bytes).expect("decode");
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
            let bytes = codec(&types).encode(&Value::Float(f64::from_bits(bits)));
            let Value::Float(value) = codec(&types).decode(&bytes).expect("decode") else {
                panic!("expected float");
            };
            assert_eq!(value.to_bits(), bits);
        }
    }

    #[test]
    fn every_truncated_prefix_is_rejected() {
        let types = test_types();
        let bytes = codec(&types).encode(&complex_value(&types));
        for end in 0..bytes.len() {
            assert!(
                codec(&types).decode(&bytes[..end]).is_err(),
                "prefix length {end}"
            );
        }
        assert!(codec(&types).decode(&bytes).is_ok());
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let types = test_types();
        let mut bytes = codec(&types).encode(&Value::Int(1));
        bytes.push(0);
        assert!(codec(&types).decode(&bytes).is_err());
    }

    #[test]
    fn non_data_tags_are_rejected() {
        let types = test_types();
        assert!(codec(&types).decode(&[Tag::Function as u8]).is_err());
        assert!(codec(&types).decode(&[15]).is_err());
        assert!(codec(&types).decode(&[255]).is_err());

        let function = Value::Builtin(BuiltinFnId::new(0));
        assert!(
            codec(&types)
                .decode(&codec(&types).encode(&function))
                .is_err()
        );
    }

    #[test]
    fn oversized_and_malformed_payloads_are_rejected() {
        let types = test_types();

        let mut string = vec![Tag::Str as u8];
        string.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(codec(&types).decode(&string).is_err());

        let mut list = vec![Tag::List as u8];
        list.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(codec(&types).decode(&list).is_err());

        let mut map = vec![Tag::Map as u8];
        map.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(codec(&types).decode(&map).is_err());

        let mut utf8 = vec![Tag::Str as u8];
        utf8.extend_from_slice(&1u64.to_le_bytes());
        utf8.push(0xFF);
        assert!(codec(&types).decode(&utf8).is_err());

        let mut type_id = vec![Tag::Struct as u8];
        type_id.extend_from_slice(&u64::MAX.to_le_bytes());
        assert!(codec(&types).decode(&type_id).is_err());
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

        assert!(codec(&types).decode(&raw_struct(999, 0)).is_err());
        assert!(
            codec(&types)
                .decode(&raw_struct(types.choice.0 as u64, 0))
                .is_err()
        );
        assert!(
            codec(&types)
                .decode(&raw_struct(types.point.0 as u64, 0))
                .is_err()
        );
        assert!(codec(&types).decode(&raw_enum(999, 0, 0)).is_err());
        assert!(
            codec(&types)
                .decode(&raw_enum(types.choice.0 as u64, 9, 0))
                .is_err()
        );
        assert!(
            codec(&types)
                .decode(&raw_enum(types.choice.0 as u64, 1, 0))
                .is_err()
        );
        assert!(codec(&types).decode(&raw_handle(999)).is_err());
        assert!(
            codec(&types)
                .decode(&raw_handle(types.point.0 as u64))
                .is_err()
        );
    }

    #[test]
    fn map_keys_use_value_tags_and_reject_non_key_values() {
        let types = test_types();
        let mut bytes = vec![Tag::Map as u8];
        bytes.extend_from_slice(&1u64.to_le_bytes());
        write_value(&mut bytes, &Value::list(vec![Value::Unit]));
        write_value(&mut bytes, &Value::Unit);
        assert!(codec(&types).decode(&bytes).is_err());
    }

    #[test]
    fn depth_limit_is_enforced() {
        let types = test_types();
        let mut value = Value::Unit;
        for _ in 0..MAX_VALUE_DEPTH {
            value = Value::list(vec![value]);
        }
        assert!(codec(&types).decode(&codec(&types).encode(&value)).is_ok());

        let too_deep = Value::list(vec![value]);
        assert!(
            codec(&types)
                .decode(&codec(&types).encode(&too_deep))
                .is_err()
        );
    }

    #[test]
    fn handle_tokens_round_trip_and_require_opaque_types() {
        let types = test_types();
        let value = Value::Handle(Rc::new(HandleValue {
            type_id: types.doc,
            token: "stale".into(),
        }));
        let decoded = codec(&types)
            .decode(&codec(&types).encode(&value))
            .expect("decode");
        let Value::Handle(decoded) = decoded else {
            panic!("expected handle");
        };
        assert_eq!(decoded.token, "stale");

        let bad = Value::Handle(Rc::new(HandleValue {
            type_id: types.point,
            token: "x".into(),
        }));
        assert!(codec(&types).decode(&codec(&types).encode(&bad)).is_err());
    }

    #[test]
    fn length_overflow_is_rejected() {
        let bytes = u64::MAX.to_le_bytes();
        let mut reader = Reader::new(&bytes);
        assert!(reader.read_collection_len(1).is_err());
    }

    #[test]
    fn string_length_is_bounded_by_remaining_bytes() {
        let mut bytes = 100u64.to_le_bytes().to_vec();
        bytes.extend_from_slice(b"short");
        let mut reader = Reader::new(&bytes);
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
            let _ = codec(&types).decode(&bytes);
        }
    }
}
