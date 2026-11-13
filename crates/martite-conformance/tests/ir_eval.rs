//! AST-vs-IR differential for the first evaluator slice (#108).
//!
//! Calls, builtin methods, hosts and closures are #109/#110, so this file
//! deliberately exercises only the core expression, aggregate and control-flow
//! surface. The cases compare result, log and exact fuel used.

use martite_base::Severity;
use martite_conformance::Case;
use martite_ir::{TypeTable, lower, verify};
use martite_runtime::{ExecLimits, IrRuntime, Runtime, show};

#[derive(Debug, PartialEq, Eq)]
struct Observation {
    outcome: Result<String, String>,
    log: Vec<String>,
    fuel: u64,
}

fn checked(case: &Case) -> (martite_check::CheckedProgram, martite_base::SourceMap) {
    let loaded = case.load();
    assert!(
        !loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error),
        "fixture must check:\n{}",
        loaded
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.render(&loaded.sources))
            .collect::<Vec<_>>()
            .join("\n")
    );
    (loaded.program.expect("checked program"), loaded.sources)
}

fn observe_ast(case: &Case, limits: ExecLimits) -> Observation {
    let (program, _sources) = checked(case);
    let types = TypeTable::from_checker(&program);
    let mut runtime = Runtime::new(&program, limits);
    let result = runtime.run_main();
    let log = match &result {
        Ok(main) => main.log.clone(),
        Err(_) => runtime.take_log(),
    };
    let fuel = runtime.fuel_used();
    let outcome = match result {
        Ok(main) => Ok(show(&main.value, &types, true)),
        Err(trap) => Err(format!("{trap:?}")),
    };
    Observation { outcome, log, fuel }
}

fn observe_ir(case: &Case, limits: ExecLimits) -> Observation {
    let (program, sources) = checked(case);
    let ir = lower(&program, &sources).expect("fixture lowers");
    verify(&ir).expect("fixture verifies");
    let types = &ir.types;
    let mut runtime = IrRuntime::new(&ir, limits);
    let result = runtime.run_main();
    let log = match &result {
        Ok(main) => main.log.clone(),
        Err(_) => runtime.take_log(),
    };
    let fuel = runtime.fuel_used();
    let outcome = match result {
        Ok(main) => Ok(show(&main.value, types, true)),
        Err(trap) => Err(format!("{trap:?}")),
    };
    Observation { outcome, log, fuel }
}

fn assert_matches(id: &str, source: &str) {
    let case = Case::single(source);
    let limits = ExecLimits::default();
    let ast = observe_ast(&case, limits);
    let ir = observe_ir(&case, limits);
    assert_eq!(ir.outcome, ast.outcome, "{id}: outcome");
    assert_eq!(ir.log, ast.log, "{id}: log");
    assert_eq!(ir.fuel, ast.fuel, "{id}: fuel used");
}

#[test]
fn core_fixtures_match_the_ast_interpreter() {
    for (id, source) in [
        ("hello", r#"fn main() { print("hello") }"#),
        (
            "arithmetic_and_interpolation",
            r#"fn main() { print("one plus two = {1 + 2}") }"#,
        ),
        (
            "let_and_if",
            r#"
            fn main() {
                let x = 4
                if x > 3 { print("big") } else { print("small") }
            }
            "#,
        ),
        (
            "while_break_continue",
            r#"
            fn main() {
                var i = 0
                var total = 0
                while i < 5 {
                    i += 1
                    if i == 2 { continue }
                    if i == 4 { break }
                    total += i
                }
                print("{total}")
            }
            "#,
        ),
        (
            "for_list",
            r#"
            fn main() {
                var total = 0
                for x in [1, 2, 3] { total += x }
                print("{total}")
            }
            "#,
        ),
        (
            "for_map_str_and_ranges",
            r#"
            fn main() {
                var total = 0
                for (key, value) in {"a": 1, "b": 2} {
                    if key == "a" { continue }
                    total += value
                }
                for ch in "ab" { total += 1 }
                for i in 0..3 { total += i }
                for i in 1..=2 { total += i }
                print("{total}")
            }
            "#,
        ),
        (
            "tuples_lists_maps_ranges",
            r#"
            fn main() {
                let xs = [1, 2, 3, 4]
                let ys = xs[1..3]
                let pair = (1, "x", true)
                let table = {"a": 1, "b": 2}
                let span = 1..4
                let a = table["a"]
                print("{ys}:{pair.1}:{a}:{span}")
            }
            "#,
        ),
        (
            "structs_and_nested_assignment",
            r#"
            struct Box { xs: list[int] }
            struct Point { xs: Box, y: int }
            fn main() {
                var point = Point { xs: Box { xs: [1, 2] }, y: 3 }
                let keep = point
                point.xs.xs[0] = 9
                point.y = 7
                print("{keep.xs.xs[0]}:{point.xs.xs[0]}:{point.y}")
            }
            "#,
        ),
        (
            "struct_update",
            r#"
            struct Point { x: int, y: int }
            fn main() {
                let base = Point { x: 1, y: 2 }
                let updated = Point { ..base, y: 9 }
                print("{updated}")
            }
            "#,
        ),
        (
            "enum_variants",
            r#"
            enum Shape { Circle(float), Empty }
            fn main() {
                let circle = Shape.Circle(2.5)
                let empty = Shape.Empty
                print("{circle}:{empty}")
            }
            "#,
        ),
        (
            "prelude_and_coalesce",
            r#"
            fn main() {
                let some: int? = Some(1)
                let none: int? = None
                print("{some ?? 2}:{none ?? 3}")
            }
            "#,
        ),
        (
            "try_success",
            r#"
            fn main() -> Result[(), str] {
                let result: Result[int, str] = Ok(3)
                let value: int = result?
                print("{value + 1}")
                Ok(())
            }
            "#,
        ),
        (
            "try_error",
            r#"
            fn main() -> Result[(), str] {
                let result: Result[int, str] = Err("bad")
                let value: int = result?
                print("{value}")
                Ok(())
            }
            "#,
        ),
        (
            "conversions",
            r#"
            fn main() {
                print("{int(2.9)}:{float(2)}:{str(true)}")
            }
            "#,
        ),
        (
            "constants_and_repeated_literal",
            r#"
            const BASE: int = 7
            fn main() {
                var i = 0
                var text = ""
                while i < 3 {
                    let literal = "abcd"
                    text = text + literal
                    i += 1
                }
                print("{BASE + 1}:{text}")
            }
            "#,
        ),
    ] {
        assert_matches(id, source);
    }
}

#[test]
fn panic_and_assert_traps_match_the_ast_interpreter() {
    for (id, source) in [
        ("panic", r#"fn main() { panic("boom") }"#),
        ("assert", r#"fn main() { assert(false, "boom") }"#),
        (
            "integer_overflow",
            r#"fn main() { let big = 9223372036854775807
print("{big + 1}") }"#,
        ),
        ("division_by_zero", r#"fn main() { let value = 1 / 0 }"#),
        (
            "list_index_out_of_bounds",
            r#"fn main() { let xs = [1, 2]
let value = xs[5] }"#,
        ),
        (
            "string_slice_out_of_bounds",
            r#"fn main() { let text = "abc"
let part = text[1..9] }"#,
        ),
    ] {
        assert_matches(id, source);
    }
}
