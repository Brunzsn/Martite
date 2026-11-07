//! Fuel cost helpers shared by the interpreter and builtin methods.

use crate::value::{KeyValue, Value};

/// Cost of sorting `n` items: `n * ceil(log2(n + 1))`.
pub(crate) fn sort_cost(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let lg = (n + 1).next_power_of_two().trailing_zeros() as u64;
    n.saturating_mul(lg)
}

/// Number of list/tuple/map slots reachable from `value`, including nested
/// containers. Map entries count as two slots (key and value).
pub(crate) fn slots(value: &Value) -> u64 {
    match value {
        Value::List(items) | Value::Tuple(items) => {
            items.len() as u64 + items.iter().map(slots).sum::<u64>()
        }
        Value::Map(map) => {
            let entries = map.len() as u64 * 2;
            entries
                + map
                    .iter()
                    .map(|(key, value)| key_slots(key) + slots(value))
                    .sum::<u64>()
        }
        Value::Struct(value) => {
            value.fields.len() as u64 + value.fields.iter().map(slots).sum::<u64>()
        }
        Value::Enum(value) => {
            value.payload.len() as u64 + value.payload.iter().map(slots).sum::<u64>()
        }
        Value::Option(Some(inner)) | Value::Result(Ok(inner)) | Value::Result(Err(inner)) => {
            1 + slots(inner)
        }
        Value::Str(text) => {
            let _ = text;
            0
        }
        _ => 0,
    }
}

/// Cost of comparing `value`: all container slots plus the bytes of every
/// nested string that a comparison would touch.
pub(crate) fn comparison_cost(value: &Value) -> u64 {
    slots(value).saturating_add(string_cost(value))
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

fn string_cost(value: &Value) -> u64 {
    match value {
        Value::Str(text) => (text.len() as u64).div_ceil(8),
        Value::List(items) | Value::Tuple(items) => items.iter().map(string_cost).sum(),
        Value::Map(map) => map
            .iter()
            .map(|(key, value)| key_string_cost(key) + string_cost(value))
            .sum(),
        Value::Struct(value) => value.fields.iter().map(string_cost).sum(),
        Value::Enum(value) => value.payload.iter().map(string_cost).sum(),
        Value::Option(Some(inner)) | Value::Result(Ok(inner)) | Value::Result(Err(inner)) => {
            string_cost(inner)
        }
        _ => 0,
    }
}

fn key_string_cost(key: &KeyValue) -> u64 {
    match key {
        KeyValue::Str(text) => (text.len() as u64).div_ceil(8),
        KeyValue::Tuple(items) => items.iter().map(key_string_cost).sum(),
        KeyValue::Struct(_, fields) | KeyValue::Enum(_, _, fields) => {
            fields.iter().map(key_string_cost).sum()
        }
        KeyValue::Option(Some(inner))
        | KeyValue::Result(Ok(inner))
        | KeyValue::Result(Err(inner)) => key_string_cost(inner),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::OrderedMap;

    #[test]
    fn sorting_matches_the_fuel_table() {
        assert_eq!(sort_cost(0), 0);
        assert_eq!(sort_cost(1), 1);
        assert_eq!(sort_cost(2), 4);
        assert_eq!(sort_cost(3), 6);
        assert_eq!(sort_cost(4), 12);
        assert_eq!(sort_cost(5), 15);
        assert_eq!(sort_cost(6), 18);
        assert_eq!(sort_cost(7), 21);
        assert_eq!(sort_cost(8), 32);
    }

    #[test]
    fn nested_slots_count_every_container() {
        let nested = Value::list(vec![
            Value::list(vec![Value::Int(1), Value::Int(2)]),
            Value::list(vec![Value::Int(3)]),
        ]);
        assert_eq!(slots(&nested), 2 + 2 + 1);

        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("a".into()), Value::Int(1));
        map.insert(KeyValue::Str("b".into()), Value::Int(2));
        assert_eq!(slots(&Value::map(map)), 4);
    }

    #[test]
    fn comparison_cost_includes_nested_strings() {
        let value = Value::list(vec![Value::str("abcd"), Value::str("efgh")]);
        assert_eq!(comparison_cost(&value), 2 + 1 + 1);
    }
}
