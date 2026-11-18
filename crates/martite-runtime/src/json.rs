use std::rc::Rc;

use martite_ir::{TypeDefKind, TypeId, TypeTable};
use serde_json::{Number, Value as JsonValue};

use crate::value::{EnumValue, KeyValue, MAX_VALUE_DEPTH, OrderedMap, Value};

/// JSON conversion rejects a nesting level that would produce a runtime value
/// deeper than [`MAX_VALUE_DEPTH`]. Each JSON array or object adds two runtime
/// levels (the `Json` variant plus its list or map), so the conversion depth is
/// capped at half the runtime ceiling and reported as a recoverable `Err`.
const MAX_DEPTH: usize = MAX_VALUE_DEPTH / 2;

#[derive(Debug)]
pub enum ParseError {
    /// Malformed JSON or invalid nesting; surfaces as `Err(message)`.
    Invalid(String),
    /// The converted value would exceed the caller's memory budget.
    TooLarge,
}

/// Parse JSON with no explicit memory budget.
pub fn parse(text: &str, json_type: Option<TypeId>) -> Result<Value, String> {
    match parse_bounded(text, json_type, u64::MAX) {
        Ok(value) => Ok(value),
        Err(ParseError::Invalid(message)) => Err(message),
        Err(ParseError::TooLarge) => Err("JSON value exceeds the memory budget".into()),
    }
}

/// Parse JSON while bounding the nominal size of the converted value.
pub fn parse_bounded(
    text: &str,
    json_type: Option<TypeId>,
    budget: u64,
) -> Result<Value, ParseError> {
    let type_id = json_type.unwrap_or(TypeId(0));
    let parsed: JsonValue =
        serde_json::from_str(text).map_err(|error| ParseError::Invalid(error.to_string()))?;
    let mut budget = budget;
    convert(parsed, type_id, 0, &mut budget)
}

fn take(budget: &mut u64, bytes: u64) -> Result<(), ParseError> {
    if bytes > *budget {
        return Err(ParseError::TooLarge);
    }
    *budget -= bytes;
    Ok(())
}

fn convert(
    json: JsonValue,
    json_type: TypeId,
    depth: usize,
    budget: &mut u64,
) -> Result<Value, ParseError> {
    if depth >= MAX_DEPTH {
        return Err(ParseError::Invalid("JSON nesting is too deep".into()));
    }
    let variant = |index: usize, payload: Vec<Value>| {
        Value::Enum(Rc::new(EnumValue::new(json_type, index, payload)))
    };
    Ok(match json {
        JsonValue::Null => {
            take(budget, 16 + 8)?;
            variant(0, Vec::new())
        }
        JsonValue::Bool(value) => {
            take(budget, 16 + 8)?;
            variant(1, vec![Value::Bool(value)])
        }
        JsonValue::Number(number) => {
            let value = number
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| ParseError::Invalid("JSON number is out of range".to_string()))?;
            take(budget, 16 + 8)?;
            variant(2, vec![Value::Float(value)])
        }
        JsonValue::String(value) => {
            take(budget, 16 + 8 + 16 + value.len() as u64)?;
            variant(3, vec![Value::str(value)])
        }
        JsonValue::Array(items) => {
            let len = items.len();
            take(
                budget,
                16u64
                    .saturating_add(8)
                    .saturating_add(16)
                    .saturating_add(8u64.saturating_mul(len as u64)),
            )?;
            let mut values = Vec::new();
            values
                .try_reserve_exact(len)
                .map_err(|_| ParseError::TooLarge)?;
            for item in items {
                values.push(convert(item, json_type, depth + 1, budget)?);
            }
            variant(4, vec![Value::list(values)])
        }
        JsonValue::Object(object) => {
            let len = object.len();
            let key_bytes = object.keys().fold(0u64, |total, key| {
                total.saturating_add(16 + key.len() as u64)
            });
            take(
                budget,
                16u64
                    .saturating_add(8)
                    .saturating_add(16)
                    .saturating_add(16u64.saturating_mul(len as u64))
                    .saturating_add(key_bytes),
            )?;
            let mut map = OrderedMap::new();
            map.try_reserve(len).map_err(|_| ParseError::TooLarge)?;
            for (key, value) in object {
                map.insert(
                    KeyValue::Str(key),
                    convert(value, json_type, depth + 1, budget)?,
                );
            }
            variant(5, vec![Value::map(map)])
        }
    })
}

pub fn stringify(value: &Value, types: &TypeTable) -> String {
    let mut out = String::new();
    let _ = write_json(&mut out, value, types, 0, usize::MAX);
    out
}

/// Serialize a `Json` value without ever growing the output past `limit`
/// bytes. Report `Err(())` so the caller can trap `OutOfMemory`.
pub fn stringify_limited(value: &Value, types: &TypeTable, limit: u64) -> Result<String, ()> {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut out = String::new();
    write_json(&mut out, value, types, 0, limit)?;
    Ok(out)
}

fn push(out: &mut String, limit: usize, text: &str) -> Result<(), ()> {
    if out.len().saturating_add(text.len()) > limit {
        return Err(());
    }
    if out.try_reserve(text.len()).is_err() {
        return Err(());
    }
    out.push_str(text);
    Ok(())
}

fn push_char(out: &mut String, limit: usize, ch: char) -> Result<(), ()> {
    let mut buffer = [0u8; 4];
    push(out, limit, ch.encode_utf8(&mut buffer))
}

fn write_json_string(out: &mut String, limit: usize, text: &str) -> Result<(), ()> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    push(out, limit, "\"")?;
    for ch in text.chars() {
        match ch {
            '"' => push(out, limit, "\\\"")?,
            '\\' => push(out, limit, "\\\\")?,
            '\u{08}' => push(out, limit, "\\b")?,
            '\u{0c}' => push(out, limit, "\\f")?,
            '\n' => push(out, limit, "\\n")?,
            '\r' => push(out, limit, "\\r")?,
            '\t' => push(out, limit, "\\t")?,
            ch if (ch as u32) < 0x20 => {
                let code = ch as u32;
                let escaped = [
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    HEX[((code >> 4) & 0xf) as usize],
                    HEX[(code & 0xf) as usize],
                ];
                push(out, limit, std::str::from_utf8(&escaped).expect("ascii"))?;
            }
            ch => push_char(out, limit, ch)?,
        }
    }
    push(out, limit, "\"")
}

fn write_json(
    out: &mut String,
    value: &Value,
    types: &TypeTable,
    depth: usize,
    limit: usize,
) -> Result<(), ()> {
    if depth > MAX_VALUE_DEPTH {
        return push(out, limit, "null");
    }
    let Value::Enum(enum_value) = value else {
        return push(out, limit, "null");
    };
    let Some(def) = types.get(enum_value.type_id) else {
        return push(out, limit, "null");
    };
    let TypeDefKind::Enum { variants } = &def.kind else {
        return push(out, limit, "null");
    };
    let Some(variant) = variants.get(enum_value.variant) else {
        return push(out, limit, "null");
    };
    match variant.name.as_ref() {
        "Null" => push(out, limit, "null"),
        "Bool" => match enum_value.payload.first() {
            Some(Value::Bool(true)) => push(out, limit, "true"),
            Some(Value::Bool(false)) => push(out, limit, "false"),
            _ => push(out, limit, "null"),
        },
        "Num" => match enum_value.payload.first() {
            Some(Value::Float(value)) if value.is_finite() => {
                let text = Number::from_f64(*value)
                    .map(|number| number.to_string())
                    .unwrap_or_else(|| "null".into());
                push(out, limit, &text)
            }
            _ => push(out, limit, "null"),
        },
        "Str" => match enum_value.payload.first() {
            Some(Value::Str(value)) => write_json_string(out, limit, value),
            _ => push(out, limit, "null"),
        },
        "Arr" => match enum_value.payload.first() {
            Some(Value::List(items)) => {
                push(out, limit, "[")?;
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        push(out, limit, ",")?;
                    }
                    write_json(out, item, types, depth + 1, limit)?;
                }
                push(out, limit, "]")
            }
            _ => push(out, limit, "[]"),
        },
        "Obj" => match enum_value.payload.first() {
            Some(Value::Map(map)) => {
                push(out, limit, "{")?;
                for (index, (key, value)) in map.entries().iter().enumerate() {
                    if index > 0 {
                        push(out, limit, ",")?;
                    }
                    match key {
                        KeyValue::Str(key) => write_json_string(out, limit, key)?,
                        other => write_json_string(
                            out,
                            limit,
                            &crate::ops::show(&crate::value::key_to_value(other), types, true),
                        )?,
                    }
                    push(out, limit, ":")?;
                    write_json(out, value, types, depth + 1, limit)?;
                }
                push(out, limit, "}")
            }
            _ => push(out, limit, "{}"),
        },
        _ => push(out, limit, "null"),
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_DEPTH, ParseError, parse_bounded, write_json_string};

    #[test]
    fn json_string_escaping_matches_serde_json() {
        for text in [
            "",
            "a\"b",
            "a\\b",
            "line\nbreak\tand\rreturn",
            "\u{08}\u{0c}\u{00}\u{1f}",
            "café 😀",
        ] {
            let mut out = String::new();
            write_json_string(&mut out, usize::MAX, text).expect("unbounded escape");
            assert_eq!(
                out,
                serde_json::to_string(text).expect("serde_json escape"),
                "escaping {text:?}"
            );
        }
    }

    #[test]
    fn bounded_json_strings_stop_at_the_limit() {
        let mut out = String::new();
        assert!(write_json_string(&mut out, 3, "abcdef").is_err());
        assert!(out.len() <= 3);
    }

    #[test]
    fn conversion_rejects_nesting_before_the_value_depth_ceiling() {
        let nested = format!("{}0{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        match parse_bounded(&nested, None, u64::MAX) {
            Err(ParseError::Invalid(message)) => {
                assert_eq!(message, "JSON nesting is too deep");
            }
            Err(ParseError::TooLarge) => panic!("unexpected budget error"),
            Ok(_) => panic!("nesting {MAX_DEPTH} must be rejected"),
        }
    }
}
