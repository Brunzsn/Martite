use std::cmp::Ordering;

use martite_ir::{TypeDefKind, TypeTable};

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
        (BuiltinModuleFn(a), BuiltinModuleFn(b)) => a == b,
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
        Value::BuiltinModuleFn(_) => 15,
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

/// Appends the normative `Show` format to `out`.
pub(crate) fn write_show(out: &mut String, value: &Value, types: &TypeTable, top: bool) {
    use std::fmt::Write;
    match value {
        Value::Unit => out.push_str("()"),
        Value::Int(v) => {
            let _ = write!(out, "{v}");
        }
        Value::Float(v) => {
            let _ = write!(out, "{v:?}");
        }
        Value::Bool(v) => {
            let _ = write!(out, "{v}");
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
        }
        Value::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_show(out, item, types, false);
            }
            out.push(']');
        }
        Value::Tuple(items) => {
            out.push('(');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_show(out, item, types, false);
            }
            if items.len() == 1 {
                out.push(',');
            }
            out.push(')');
        }
        Value::Map(map) => {
            out.push('{');
            for (i, (key, value)) in map.entries().iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_show(out, &crate::value::key_to_value(key), types, false);
                out.push_str(": ");
                write_show(out, value, types, false);
            }
            out.push('}');
        }
        Value::Range {
            start,
            end,
            inclusive,
        } => {
            let _ = write!(out, "{start}..{}{end}", if *inclusive { "=" } else { "" });
        }
        Value::Struct(value) => {
            let def = types.get(value.type_id);
            let name = def.map(|d| d.name.as_ref()).unwrap_or("<struct>");
            let _ = write!(out, "{name} {{ ");
            if let Some(TypeDefKind::Struct { fields }) = def.map(|d| &d.kind) {
                for (i, (field, field_value)) in fields.iter().zip(value.fields.iter()).enumerate()
                {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    let _ = write!(out, "{}: ", field.name);
                    write_show(out, field_value, types, false);
                }
            }
            out.push_str(" }");
        }
        Value::Enum(value) => {
            let def = types.get(value.type_id);
            let name = def.map(|d| d.name.as_ref()).unwrap_or("<enum>");
            let variant = def
                .and_then(|d| match &d.kind {
                    TypeDefKind::Enum { variants } => variants.get(value.variant),
                    _ => None,
                })
                .map(|v| v.name.as_ref())
                .unwrap_or("<variant>");
            let _ = write!(out, "{name}.{variant}");
            if !value.payload.is_empty() {
                out.push('(');
                for (i, item) in value.payload.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_show(out, item, types, false);
                }
                out.push(')');
            }
        }
        Value::Option(inner) => match inner {
            None => out.push_str("None"),
            Some(v) => {
                out.push_str("Some(");
                write_show(out, v, types, false);
                out.push(')');
            }
        },
        Value::Result(result) => match result {
            Ok(v) => {
                out.push_str("Ok(");
                write_show(out, v, types, false);
                out.push(')');
            }
            Err(v) => {
                out.push_str("Err(");
                write_show(out, v, types, false);
                out.push(')');
            }
        },
        Value::Handle(handle) => {
            let name = types
                .get(handle.type_id)
                .map(|d| d.name.as_ref())
                .unwrap_or("Handle");
            let _ = write!(out, "<{name}>");
        }
        Value::Fn(_) | Value::Builtin(_) | Value::BuiltinModuleFn(_) => {
            out.push_str("<fn>");
        }
    }
}

pub fn map_key(value: &Value) -> Result<crate::value::KeyValue, String> {
    key_from_value(value)
}

pub(crate) fn int_arg(args: &[Value], index: usize) -> Option<i64> {
    match args.get(index) {
        Some(Value::Int(v)) => Some(*v),
        _ => None,
    }
}

pub(crate) fn range_len(start: i64, end: i64, inclusive: bool) -> i64 {
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

pub(crate) fn parse_int(text: &str) -> Option<i64> {
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

pub(crate) fn parse_float(text: &str) -> Option<f64> {
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
