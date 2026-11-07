use std::cmp::Ordering;

use martite_check::TypeDefKind;
use martite_check::TypeTable;

use crate::value::{Value, key_from_value};

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        value_eq(self, other)
    }
}

pub fn value_eq(a: &Value, b: &Value) -> bool {
    use Value::*;
    match (a, b) {
        (Unit, Unit) => true,
        (Int(a), Int(b)) => a == b,
        (Float(a), Float(b)) => a == b,
        (Bool(a), Bool(b)) => a == b,
        (Str(a), Str(b)) => a == b,
        (List(a), List(b)) | (Tuple(a), Tuple(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| value_eq(x, y))
        }
        (Map(a), Map(b)) => a.eq_as_set(b),
        (
            Range {
                start: a1,
                end: a2,
                inclusive: a3,
            },
            Range {
                start: b1,
                end: b2,
                inclusive: b3,
            },
        ) => a1 == b1 && a2 == b2 && a3 == b3,
        (Struct(a), Struct(b)) => {
            a.type_id == b.type_id
                && a.fields.len() == b.fields.len()
                && a.fields
                    .iter()
                    .zip(b.fields.iter())
                    .all(|(x, y)| value_eq(x, y))
        }
        (Enum(a), Enum(b)) => {
            a.type_id == b.type_id
                && a.variant == b.variant
                && a.payload.len() == b.payload.len()
                && a.payload
                    .iter()
                    .zip(b.payload.iter())
                    .all(|(x, y)| value_eq(x, y))
        }
        (Option(a), Option(b)) => match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => value_eq(a, b),
            _ => false,
        },
        (Result(a), Result(b)) => match (a, b) {
            (Ok(a), Ok(b)) | (Err(a), Err(b)) => value_eq(a, b),
            _ => false,
        },
        (Handle(a), Handle(b)) => a.type_id == b.type_id && a.token == b.token,
        (Builtin(a), Builtin(b)) => a == b,
        (
            BuiltinModuleFn {
                module: am,
                name: an,
            },
            BuiltinModuleFn {
                module: bm,
                name: bn,
            },
        ) => am == bm && an == bn,
        _ => false,
    }
}

/// The total order used by sorting and `std.math.min/max/clamp`. NaN sorts
/// after every other float and compares equal to itself.
pub fn value_ord(a: &Value, b: &Value) -> Ordering {
    use Value::*;
    match (a, b) {
        (Bool(a), Bool(b)) => a.cmp(b),
        (Int(a), Int(b)) => a.cmp(b),
        (Float(a), Float(b)) => total_float_cmp(*a, *b),
        (Str(a), Str(b)) => a.cmp(b),
        (Tuple(a), Tuple(b)) | (List(a), List(b)) => sequence_cmp(a, b),
        (Option(a), Option(b)) => match (a, b) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(a), Some(b)) => value_ord(a, b),
        },
        (Result(a), Result(b)) => match (a, b) {
            (Ok(a), Ok(b)) | (Err(a), Err(b)) => value_ord(a, b),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
        },
        (Struct(a), Struct(b)) => a
            .type_id
            .0
            .cmp(&b.type_id.0)
            .then_with(|| sequence_cmp(&a.fields, &b.fields)),
        (Enum(a), Enum(b)) => a
            .type_id
            .0
            .cmp(&b.type_id.0)
            .then_with(|| a.variant.cmp(&b.variant))
            .then_with(|| sequence_cmp(&a.payload, &b.payload)),
        (
            Range {
                start: a1,
                end: a2,
                inclusive: a3,
            },
            Range {
                start: b1,
                end: b2,
                inclusive: b3,
            },
        ) => a1.cmp(b1).then_with(|| a2.cmp(b2)).then_with(|| a3.cmp(b3)),
        (Handle(a), Handle(b)) => a
            .type_id
            .0
            .cmp(&b.type_id.0)
            .then_with(|| a.token.cmp(&b.token)),
        _ => type_rank(a).cmp(&type_rank(b)),
    }
}

fn total_float_cmp(a: f64, b: f64) -> Ordering {
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Greater,
        (false, true) => Ordering::Less,
        (false, false) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
    }
}

fn sequence_cmp(a: &[Value], b: &[Value]) -> Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let ord = value_ord(x, y);
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

fn type_rank(value: &Value) -> u8 {
    match value {
        Value::Unit => 0,
        Value::Bool(_) => 1,
        Value::Int(_) => 2,
        Value::Float(_) => 3,
        Value::Str(_) => 4,
        Value::Tuple(_) => 5,
        Value::List(_) => 6,
        Value::Map(_) => 7,
        Value::Range { .. } => 8,
        Value::Struct(_) => 9,
        Value::Enum(_) => 10,
        Value::Option(_) => 11,
        Value::Result(_) => 12,
        Value::Fn(_) => 13,
        Value::Builtin(_) => 14,
        Value::BuiltinModuleFn { .. } => 15,
        Value::Handle(_) => 16,
    }
}

/// The normative `Show` format. `top` is true for `{x}`, `str(x)`, and
/// `print(x)`; strings are raw at the top and quoted inside containers.
pub fn show(value: &Value, types: &TypeTable, top: bool) -> String {
    let mut out = String::new();
    write_show(&mut out, value, types, top);
    out
}

/// Appends the normative `Show` format and returns the number of container
/// slots traversed. Callers that charge `Show` fuel use the count directly
/// instead of walking the value twice.
pub(crate) fn write_show(out: &mut String, value: &Value, types: &TypeTable, top: bool) -> u64 {
    use std::fmt::Write;
    match value {
        Value::Unit => {
            out.push_str("()");
            0
        }
        Value::Int(v) => {
            let _ = write!(out, "{v}");
            0
        }
        Value::Float(v) => {
            let _ = write!(out, "{v:?}");
            0
        }
        Value::Bool(v) => {
            let _ = write!(out, "{v}");
            0
        }
        Value::Str(s) => {
            if top {
                out.push_str(s);
            } else {
                out.push('"');
                for ch in s.chars() {
                    match ch {
                        '\\' => out.push_str("\\\\"),
                        '"' => out.push_str("\\\""),
                        '\n' => out.push_str("\\n"),
                        '\t' => out.push_str("\\t"),
                        '\r' => out.push_str("\\r"),
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            0
        }
        Value::List(items) => {
            out.push('[');
            let mut slots = items.len() as u64;
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                slots += write_show(out, item, types, false);
            }
            out.push(']');
            slots
        }
        Value::Tuple(items) => {
            out.push('(');
            let mut slots = items.len() as u64;
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                slots += write_show(out, item, types, false);
            }
            if items.len() == 1 {
                out.push(',');
            }
            out.push(')');
            slots
        }
        Value::Map(map) => {
            out.push('{');
            let mut slots = map.len() as u64 * 2;
            for (i, (key, value)) in map.entries().iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                slots += write_show(out, &crate::value::key_to_value(key), types, false);
                out.push_str(": ");
                slots += write_show(out, value, types, false);
            }
            out.push('}');
            slots
        }
        Value::Range {
            start,
            end,
            inclusive,
        } => {
            let _ = write!(out, "{start}..{}{end}", if *inclusive { "=" } else { "" });
            0
        }
        Value::Struct(value) => {
            let def = types.get(value.type_id);
            let name = def.map(|d| d.name.as_str()).unwrap_or("<struct>");
            let _ = write!(out, "{name} {{ ");
            let mut slots = value.fields.len() as u64;
            if let Some(TypeDefKind::Struct { fields }) = def.map(|d| &d.kind) {
                for (i, (field, field_value)) in fields.iter().zip(value.fields.iter()).enumerate()
                {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    let _ = write!(out, "{}: ", field.name);
                    slots += write_show(out, field_value, types, false);
                }
            }
            out.push_str(" }");
            slots
        }
        Value::Enum(value) => {
            let def = types.get(value.type_id);
            let name = def.map(|d| d.name.as_str()).unwrap_or("<enum>");
            let variant = def
                .and_then(|d| match &d.kind {
                    TypeDefKind::Enum { variants } => variants.get(value.variant),
                    _ => None,
                })
                .map(|v| v.name.as_str())
                .unwrap_or("<variant>");
            let _ = write!(out, "{name}.{variant}");
            let mut slots = value.payload.len() as u64;
            if !value.payload.is_empty() {
                out.push('(');
                for (i, item) in value.payload.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    slots += write_show(out, item, types, false);
                }
                out.push(')');
            }
            slots
        }
        Value::Option(inner) => match inner {
            None => {
                out.push_str("None");
                0
            }
            Some(v) => {
                out.push_str("Some(");
                let slots = 1 + write_show(out, v, types, false);
                out.push(')');
                slots
            }
        },
        Value::Result(result) => match result {
            Ok(v) => {
                out.push_str("Ok(");
                let slots = 1 + write_show(out, v, types, false);
                out.push(')');
                slots
            }
            Err(v) => {
                out.push_str("Err(");
                let slots = 1 + write_show(out, v, types, false);
                out.push(')');
                slots
            }
        },
        Value::Handle(handle) => {
            let name = types
                .get(handle.type_id)
                .map(|d| d.name.as_str())
                .unwrap_or("Handle");
            let _ = write!(out, "<{name}>");
            0
        }
        Value::Fn(_) | Value::Builtin(_) | Value::BuiltinModuleFn { .. } => {
            out.push_str("<fn>");
            0
        }
    }
}

pub fn map_key(value: &Value) -> Result<crate::value::KeyValue, String> {
    key_from_value(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cost::slots;
    use crate::value::{KeyValue, OrderedMap, Value};

    #[test]
    fn write_show_reports_the_slots_it_traverses() {
        let types = TypeTable::default();
        let mut map = OrderedMap::new();
        map.insert(
            KeyValue::Str("a".into()),
            Value::list(vec![Value::Int(1), Value::Int(2)]),
        );
        let value = Value::list(vec![
            Value::Int(1),
            Value::map(map),
            Value::Option(Some(Box::new(Value::list(vec![Value::Int(3)])))),
            Value::Result(Ok(Box::new(Value::Int(4)))),
        ]);
        let mut out = String::new();
        let counted = write_show(&mut out, &value, &types, true);
        assert_eq!(counted, slots(&value));
        assert_eq!(out, "[1, {\"a\": [1, 2]}, Some([3]), Ok(4)]");
    }
}
