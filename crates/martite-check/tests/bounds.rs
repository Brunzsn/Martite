//! Regression tests for recursive type walks and generic bound enforcement.
//!
//! These cover issues #161 (recursive type walks), #165 (exponential walks over
//! type diamonds), and #172 (bounds through nested type parameters, map keys,
//! and first-class generic functions).

use std::time::{Duration, Instant};

use martite_base::{Code, LoadLimits, MemoryProvider, SourceMap};
use martite_check::check;
use martite_resolve::{ResolveOptions, resolve};

fn codes_for(source: &str) -> Vec<Code> {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
    let mut sources = SourceMap::new();
    let resolved = resolve(
        &provider,
        &ResolveOptions {
            entry: "main.mt".into(),
            interfaces: Vec::new(),
            grants: Vec::new(),
            limits: LoadLimits::default(),
        },
        &mut sources,
    );
    let program = resolved
        .program
        .unwrap_or_else(|| panic!("resolve failed: {:?}", resolved.diagnostics));
    check(program).diagnostics.iter().map(|d| d.code).collect()
}

fn assert_code(source: &str, expected: Code) {
    let codes = codes_for(source);
    assert!(
        codes.contains(&expected),
        "expected {expected}, got {codes:?}"
    );
}

fn assert_clean(source: &str) {
    let codes = codes_for(source);
    assert!(codes.is_empty(), "expected a clean check, got {codes:?}");
}

fn assert_clean_and_fast(source: &str) {
    let started = Instant::now();
    let codes = codes_for(source);
    let elapsed = started.elapsed();
    assert!(codes.is_empty(), "expected a clean check, got {codes:?}");
    assert!(
        elapsed < Duration::from_secs(5),
        "bound walk took {elapsed:?}; memoization regressed"
    );
}

/// A shared-subtree graph: every level mentions the previous one twice.
fn diamond(levels: usize, preamble: &str, root: &str) -> String {
    let mut source = String::new();
    if !preamble.is_empty() {
        source.push_str(preamble);
        source.push('\n');
    }
    source.push_str(root);
    source.push('\n');
    for level in 1..=levels {
        source.push_str(&format!(
            "struct Node{level} {{ a: Node{prev}, b: Node{prev} }}\n",
            prev = level - 1
        ));
    }
    source
}

// ----------------------------------------------------------------------
// #161: recursive type walks
// ----------------------------------------------------------------------

#[test]
fn recursive_main_error_type_is_a_data_type() {
    assert_clean("enum E { Leaf, Next(E) }\nfn main() -> Result[(), E] { Ok(()) }");
}

#[test]
fn recursive_plugin_state_is_a_data_type() {
    assert_clean("enum Tree { Leaf, Node(Tree) }\non init() -> Tree { Tree.Leaf }");
}

#[test]
fn recursive_builtin_json_state_is_a_data_type() {
    assert_clean("use std.json.Json\non init() -> Json { panic(\"no state\") }");
}

#[test]
fn recursive_builtin_json_main_error_is_showable() {
    assert_clean("use std.json.Json\nfn main() -> Result[(), Json] { panic(\"no error\") }");
}

#[test]
fn recursive_plugin_state_with_a_function_field_is_rejected() {
    assert_code(
        "enum E { Leaf, Next(E), Callback(fn()) }\non init() -> E { E.Leaf }",
        Code::E506,
    );
}

#[test]
fn recursive_main_error_type_with_a_function_payload_is_rejected() {
    assert_code(
        "enum E { Leaf, Next(E), Callback(fn()) }\nfn main() -> Result[(), E] { Ok(()) }",
        Code::E407,
    );
}

// ----------------------------------------------------------------------
// #165: shared type graphs stay linear
// ----------------------------------------------------------------------

#[test]
fn diamond_show_is_memoized() {
    let source = diamond(
        40,
        "fn show_all(s: Node40) { print(\"{s}\") }\nfn main() { }",
        "struct Node0 { a: int, b: int }",
    );
    assert_clean_and_fast(&source);
}

#[test]
fn diamond_ord_is_memoized() {
    let source = diamond(
        40,
        "fn less(s: Node40) { let _ = s < s }\nfn main() { }",
        "struct Node0 { a: int, b: int }",
    );
    assert_clean_and_fast(&source);
}

#[test]
fn diamond_map_key_is_memoized() {
    let source = diamond(
        40,
        "fn key() -> Node40 { panic(\"no value\") }\nfn main() { let m: map[Node40, int] = { key(): 1 }\nlet _ = m.len() }",
        "struct Node0 { a: int, b: int }",
    );
    assert_clean_and_fast(&source);
}

#[test]
fn diamond_with_a_function_leaf_is_still_rejected() {
    let source = diamond(
        40,
        "fn show_all(s: Node40) { print(\"{s}\") }\nfn main() { }",
        "struct Node0 { a: fn(), b: fn() }",
    );
    assert_code(&source, Code::E205);
}

// ----------------------------------------------------------------------
// #172: nested generic parameters and map keys
// ----------------------------------------------------------------------

#[test]
fn generic_map_key_is_rejected() {
    assert_code(
        "fn add_one_key[T](k: T) -> int {\n    let m: map[T, int] = { k: 1 }\n    m.len()\n}\nfn main() { print(\"{add_one_key([1, 2])}\") }",
        Code::E205,
    );
}

#[test]
fn map_key_through_a_nested_generic_parameter_is_rejected() {
    assert_code(
        "struct Box[T] { value: T }\nfn key[T](x: T) -> int {\n    let m: map[Box[T], int] = { Box { value: x }: 1 }\n    m.len()\n}\nfn main() { }",
        Code::E205,
    );
}

#[test]
fn concrete_map_key_is_unchanged() {
    assert_clean(
        "struct Key { a: int, b: (str, bool) }\nfn key() -> Key { panic(\"no value\") }\nfn main() { let m: map[Key, int] = { key(): 1 }\nlet _ = m.len() }",
    );
}

#[test]
fn instantiated_generic_map_key_is_accepted() {
    assert_clean(
        "struct Box[T] { value: T }\nfn key() -> Box[int] { panic(\"no value\") }\nfn main() { let m: map[Box[int], int] = { key(): 1 }\nlet _ = m.len() }",
    );
}

#[test]
fn eq_through_a_nested_parameter_is_rejected() {
    assert_code(
        "fn same[T](a: list[T], b: list[T]) -> bool { a == b }\nfn main() {\n    let f = fn() { }\n    print(\"{same([f], [f])}\")\n}",
        Code::E205,
    );
}

#[test]
fn eq_through_a_nested_parameter_with_a_bound_is_accepted() {
    assert_clean(
        "fn same[T: Eq](a: list[T], b: list[T]) -> bool { a == b }\nfn main() { let _ = same([1], [2]) }",
    );
}

#[test]
fn ord_through_a_nested_parameter_is_rejected() {
    assert_code(
        "fn greater[T](a: list[T], b: list[T]) -> bool { a < b }\nfn main() { let _ = greater([1], [2]) }",
        Code::E205,
    );
}

#[test]
fn ord_through_a_nested_parameter_with_a_bound_is_accepted() {
    assert_clean(
        "fn greater[T: Ord](a: list[T], b: list[T]) -> bool { a < b }\nfn main() { let _ = greater([1], [2]) }",
    );
}

#[test]
fn show_through_a_nested_parameter_is_rejected() {
    assert_code(
        "fn render[T](xs: list[T]) -> str { \"{xs}\" }\nfn main() { }",
        Code::E205,
    );
}

#[test]
fn show_through_a_nested_parameter_with_a_bound_is_accepted() {
    assert_clean(
        "fn render[T: Show](xs: list[T]) -> str { \"{xs}\" }\nfn main() { let _ = render([1]) }",
    );
}

// ----------------------------------------------------------------------
// #172: first-class generic functions and indirect calls
// ----------------------------------------------------------------------

#[test]
fn direct_generic_call_rejects_a_closure() {
    assert_code(
        "fn max_of[T: Ord](a: T, b: T) -> T { if a > b { a } else { b } }\nfn main() {\n    let g = fn(x: int) { x }\n    let _ = max_of(g, g)\n}",
        Code::E205,
    );
}

#[test]
fn generic_function_value_enforces_bounds_on_an_indirect_call() {
    assert_code(
        "fn max_of[T: Ord](a: T, b: T) -> T { if a > b { a } else { b } }\nfn wrap[U](a: U, b: U) -> U {\n    let f = max_of\n    f(a, b)\n}\nfn main() {\n    let g = fn(x: int) { x }\n    let _ = wrap(g, g)\n}",
        Code::E205,
    );
}

#[test]
fn generic_function_value_with_a_bound_is_accepted() {
    assert_clean(
        "fn max_of[T: Ord](a: T, b: T) -> T { if a > b { a } else { b } }\nfn wrap[U: Ord](a: U, b: U) -> U {\n    let f = max_of\n    f(a, b)\n}\nfn main() { let _ = wrap(1, 2) }",
    );
}

#[test]
fn generic_function_value_with_concrete_arguments_is_accepted() {
    assert_clean(
        "fn max_of[T: Ord](a: T, b: T) -> T { if a > b { a } else { b } }\nfn main() {\n    let f = max_of\n    print(\"{f(1, 2)}\")\n}",
    );
}

#[test]
fn aliasing_a_generic_function_value_preserves_bounds() {
    assert_code(
        "fn max_of[T: Ord](a: T, b: T) -> T { if a > b { a } else { b } }\nfn main() {\n    let f = max_of\n    let g = f\n    let h = fn(x: int) { x }\n    let _ = g(h, h)\n}",
        Code::E205,
    );
}
