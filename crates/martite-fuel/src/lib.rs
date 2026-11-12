//! Edition-1 fuel table shared by the runtime and the IR.
//!
//! Every fuel charge in `martite-runtime` maps to exactly one row here. Static
//! costs are constants; costs that depend on a value's size are pure functions
//! over counts. The runtime extracts the counts from `ValueMeta::slots` and
//! `ValueMeta::string_slots` and passes them in, so this crate has no
//! dependency on the runtime.
//!
//! Function and lambda entry add no fuel charge of their own: the call
//! expression's entry charge is the only charge. Statements charge nothing
//! themselves; their child expressions charge.
//!
//! ## Static costs
//!
//! | Event | Cost | Constant |
//! | --- | --- | --- |
//! | Each evaluated expression node | 1 | [`EXPRESSION_COST`] |
//! | Each `while` condition check and `for` element | 1 | [`ITERATION_COST`] |
//! | Each `any`/`all` callback item | 1 | [`ITERATION_COST`] |
//! | `push`/`pop` on a list | 1 | [`SLOT_COST`] |
//! | One map entry | 2 slots | [`MAP_ENTRY_SLOTS`] |
//! | One string fuel unit | 8 bytes | [`STRING_BYTES_PER_UNIT`] |
//! | Host call | 1 plus the host's charge | [`HOST_CALL_COST`] |
//!
//! ## Dynamic costs
//!
//! | Event | Formula | Helper |
//! | --- | --- | --- |
//! | List/tuple literal, struct literal, variant payload, list slice, copy-on-write list/struct | `n` slots | [`slot_cost`] |
//! | Map literal, copy-on-write map, map `entries`/`clear` | `2n` | [`map_cost`] |
//! | String bytes (concat, slice, static literal, interpolation, `print`, `std.json`, `str` methods, `join`) | `ceil(bytes / 8)` | [`string_cost`] |
//! | `sorted`, `sorted_by`, `sort` | `n * ceil(log2(n + 1))` | [`sort_cost`] |
//! | `==`, `!=`, `<`, `<=`, `>`, `>=` | `slots + string_slots` | [`comparison_cost`] |
//! | `Show`, `print`/interpolation show part, conversion to `str` | `slots` | [`show_cost`] |
//! | List `contains`/`map`/`filter`/`fold`/`enumerate`/`reversed`/`join` item count/`clear`/`extend`; map `keys`/`values`; range `to_list` | `n` | [`slot_cost`] |
//! | List `insert` | `len - index + 1` | [`list_insert_cost`] |
//! | List `remove_at` | `len - index` | [`list_remove_cost`] |
//! | Map `remove` (after the key lookup) | `2 * (entries - index)` | [`map_remove_cost`] |
//! | Host call | `1 + host_charged` | [`host_call_cost`] |
//!
//! `ValueMeta::slots` counts list/tuple/struct/variant slots, with every map
//! entry counting as two slots. `ValueMeta::string_slots` is
//! [`string_slots`]`(bytes)`, used by [`comparison_cost`].

/// Fuel for evaluating one expression node.
pub const EXPRESSION_COST: u64 = 1;

/// Fuel for one `while` condition check, one `for` element, or one `any`/`all`
/// callback item.
pub const ITERATION_COST: u64 = 1;

/// Fuel for touching one list, tuple, struct or variant slot.
pub const SLOT_COST: u64 = 1;

/// Number of slots in one map entry: the key and the value.
pub const MAP_ENTRY_SLOTS: u64 = 2;

/// Number of string bytes covered by one fuel unit.
pub const STRING_BYTES_PER_UNIT: u64 = 8;

/// Base fuel for a host call, before the host's own charge is added.
pub const HOST_CALL_COST: u64 = 1;

/// Fuel for touching `slots` list, tuple, struct or variant slots.
#[inline]
pub fn slot_cost(slots: u64) -> u64 {
    slots
}

/// Fuel for touching a map with `entries` entries (two slots each).
#[inline]
pub fn map_cost(entries: u64) -> u64 {
    entries.saturating_mul(MAP_ENTRY_SLOTS)
}

/// Number of 8-byte units in `bytes`.
#[inline]
pub fn string_slots(bytes: u64) -> u64 {
    bytes.div_ceil(STRING_BYTES_PER_UNIT)
}

/// Fuel for touching a string of `bytes` bytes.
#[inline]
pub fn string_cost(bytes: u64) -> u64 {
    string_slots(bytes)
}

/// Cost of sorting `n` items: `n * ceil(log2(n + 1))`.
#[inline]
pub fn sort_cost(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let lg = (n + 1).next_power_of_two().trailing_zeros() as u64;
    n.saturating_mul(lg)
}

/// Cost of comparing two values: their container slots plus the bytes of every
/// nested string that a comparison would touch.
#[inline]
pub fn comparison_cost(slots: u64, string_slots: u64) -> u64 {
    slot_cost(slots).saturating_add(string_slots)
}

/// Number of slots traversed by `Show`.
#[inline]
pub fn show_cost(slots: u64) -> u64 {
    slot_cost(slots)
}

/// Fuel for inserting into a list of `len` items at `index`.
#[inline]
pub fn list_insert_cost(len: u64, index: u64) -> u64 {
    len.saturating_sub(index).saturating_add(1)
}

/// Fuel for removing from a list of `len` items at `index`.
#[inline]
pub fn list_remove_cost(len: u64, index: u64) -> u64 {
    len.saturating_sub(index)
}

/// Fuel for removing a map entry and shifting the entries after it.
#[inline]
pub fn map_remove_cost(entries: u64, index: u64) -> u64 {
    map_cost(entries.saturating_sub(index))
}

/// Fuel for a host call: the base charge plus whatever the host charged.
#[inline]
pub fn host_call_cost(host_charged: u64) -> u64 {
    host_charged.saturating_add(HOST_CALL_COST)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn strings_round_up_to_eight_byte_units() {
        assert_eq!(string_cost(0), 0);
        assert_eq!(string_cost(1), 1);
        assert_eq!(string_cost(7), 1);
        assert_eq!(string_cost(8), 1);
        assert_eq!(string_cost(9), 2);
        assert_eq!(string_cost(16), 2);
        assert_eq!(string_cost(17), 3);
        assert_eq!(string_slots(9), string_cost(9));
    }

    #[test]
    fn map_entries_count_two_slots() {
        assert_eq!(map_cost(0), 0);
        assert_eq!(map_cost(1), 2);
        assert_eq!(map_cost(3), 6);
    }

    #[test]
    fn slot_show_and_comparison_costs() {
        assert_eq!(slot_cost(0), 0);
        assert_eq!(slot_cost(3), 3);
        assert_eq!(show_cost(3), 3);
        assert_eq!(comparison_cost(2, 1), 3);
        assert_eq!(comparison_cost(0, 0), 0);
    }

    #[test]
    fn list_and_map_mutations_charge_shifted_slots() {
        assert_eq!(list_insert_cost(4, 0), 5);
        assert_eq!(list_insert_cost(4, 4), 1);
        assert_eq!(list_remove_cost(4, 0), 4);
        assert_eq!(list_remove_cost(4, 3), 1);
        assert_eq!(map_remove_cost(4, 0), 8);
        assert_eq!(map_remove_cost(4, 3), 2);
    }

    #[test]
    fn host_calls_add_the_base_charge() {
        assert_eq!(host_call_cost(0), 1);
        assert_eq!(host_call_cost(5), 6);
    }

    #[test]
    fn costs_saturate() {
        assert_eq!(map_cost(u64::MAX), u64::MAX);
        assert_eq!(comparison_cost(u64::MAX, 1), u64::MAX);
        assert_eq!(list_insert_cost(u64::MAX, 0), u64::MAX);
        assert_eq!(map_remove_cost(u64::MAX, 1), u64::MAX);
        assert_eq!(host_call_cost(u64::MAX), u64::MAX);
    }
}
