use std::rc::Rc;

use martite_check::{TypeDefKind, TypeId, TypeTable};
use serde_json::{Map, Number, Value as JsonValue};

use crate::value::{EnumValue, KeyValue, OrderedMap, Value};

const MAX_DEPTH: usize = 128;

pub fn parse(text: &str, json_type: Option<TypeId>) -> Result<Value, String> {
    let type_id = json_type.unwrap_or(TypeId(0));
    let parsed: JsonValue = serde_json::from_str(text).map_err(|e| e.to_string())?;
    convert(parsed, type_id, 0)
}

fn convert(json: JsonValue, json_type: TypeId, depth: usize) -> Result<Value, String> {
    if depth > MAX_DEPTH {
        return Err("JSON nesting is too deep".into());
    }
    let variant = |index: usize, payload: Vec<Value>| {
        Value::Enum(Rc::new(EnumValue::new(json_type, index, payload)))
    };
    Ok(match json {
        JsonValue::Null => variant(0, Vec::new()),
        JsonValue::Bool(value) => variant(1, vec![Value::Bool(value)]),
        JsonValue::Number(number) => {
            let value = number
                .as_f64()
                .filter(|value| value.is_finite())
                .ok_or_else(|| "JSON number is out of range".to_string())?;
            variant(2, vec![Value::Float(value)])
        }
        JsonValue::String(value) => variant(3, vec![Value::str(value)]),
        JsonValue::Array(items) => {
            let mut values = Vec::with_capacity(items.len());
            for item in items {
                values.push(convert(item, json_type, depth + 1)?);
            }
            variant(4, vec![Value::list(values)])
        }
        JsonValue::Object(object) => {
            let mut map = OrderedMap::new();
            for (key, value) in object {
                map.insert(KeyValue::Str(key), convert(value, json_type, depth + 1)?);
            }
            variant(5, vec![Value::map(map)])
        }
    })
}

pub fn stringify(value: &Value, types: &TypeTable) -> String {
    let json = to_json(value, types, 0);
    serde_json::to_string(&json).unwrap_or_else(|_| "null".into())
}

fn to_json(value: &Value, types: &TypeTable, depth: usize) -> JsonValue {
    if depth > MAX_DEPTH {
        return JsonValue::Null;
    }
    let Value::Enum(enum_value) = value else {
        return JsonValue::Null;
    };
    let Some(def) = types.get(enum_value.type_id) else {
        return JsonValue::Null;
    };
    let TypeDefKind::Enum { variants } = &def.kind else {
        return JsonValue::Null;
    };
    let Some(variant) = variants.get(enum_value.variant) else {
        return JsonValue::Null;
    };
    match variant.name.as_str() {
        "Null" => JsonValue::Null,
        "Bool" => match enum_value.payload.first() {
            Some(Value::Bool(true)) => JsonValue::Bool(true),
            Some(Value::Bool(false)) => JsonValue::Bool(false),
            _ => JsonValue::Null,
        },
        "Num" => match enum_value.payload.first() {
            Some(Value::Float(value)) if value.is_finite() => Number::from_f64(*value)
                .map(JsonValue::Number)
                .unwrap_or(JsonValue::Null),
            _ => JsonValue::Null,
        },
        "Str" => match enum_value.payload.first() {
            Some(Value::Str(value)) => JsonValue::String(value.to_string()),
            _ => JsonValue::Null,
        },
        "Arr" => match enum_value.payload.first() {
            Some(Value::List(items)) => JsonValue::Array(
                items
                    .iter()
                    .map(|item| to_json(item, types, depth + 1))
                    .collect(),
            ),
            _ => JsonValue::Array(Vec::new()),
        },
        "Obj" => match enum_value.payload.first() {
            Some(Value::Map(map)) => {
                let mut object = Map::new();
                for (key, value) in map.entries() {
                    let key = match key {
                        KeyValue::Str(key) => key.clone(),
                        other => crate::ops::show(&crate::value::key_to_value(other), types, true),
                    };
                    object.insert(key, to_json(value, types, depth + 1));
                }
                JsonValue::Object(object)
            }
            _ => JsonValue::Object(Map::new()),
        },
        _ => JsonValue::Null,
    }
}
