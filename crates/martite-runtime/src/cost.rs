//! Fuel cost helpers shared by the interpreter and builtin methods.

use crate::value::Value;

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
    value.meta().slots
}

/// Cost of comparing `value`: all container slots plus the bytes of every
/// nested string that a comparison would touch.
pub(crate) fn comparison_cost(value: &Value) -> u64 {
    slots(value).saturating_add(value.meta().string_slots)
}

/// Number of slots traversed by `Show`.
pub(crate) fn show_slots(value: &Value) -> u64 {
    value.meta().slots
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{KeyValue, OrderedMap};

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
