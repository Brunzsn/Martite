//! AST-vs-IR differential for the evaluator slices (#108, #109, #110).
//!
//! The cases compare result, log, exact fuel used and, for the match
//! rollback regression, the minimum memory budget.

use std::cell::Cell;
use std::rc::Rc;

use martite_base::Severity;
use martite_conformance::{Case, min_memory};
use martite_ir::{IrProgram, TypeTable, lower, verify};
use martite_runtime::{
    ExecLimits, HostContext, HostDispatcher, HostFault, IrHostDispatcher, IrRuntime, Runtime, Trap,
    Value, show,
};

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

fn lowered_ir(case: &Case) -> IrProgram {
    let (program, sources) = checked(case);
    let ir = lower(&program, &sources).expect("fixture lowers");
    verify(&ir).expect("fixture verifies");
    ir
}

fn run_ir(ir: &IrProgram, limits: ExecLimits) -> Result<(), Trap> {
    let mut runtime = IrRuntime::new(ir, limits);
    runtime.run_main().map(|_| ())
}

fn min_memory_ir(case: &Case) -> u64 {
    let ir = lowered_ir(case);
    let mut low = 0u64;
    let mut high = 8 << 20;
    let limits = |memory| ExecLimits {
        fuel: 1_000_000,
        memory,
        depth: 200,
        deadline: None,
    };
    assert!(
        run_ir(&ir, limits(high)).is_ok(),
        "fixture needs more than the probe cap"
    );
    while low < high {
        let mid = low + (high - low) / 2;
        match run_ir(&ir, limits(mid)) {
            Ok(()) => high = mid,
            Err(Trap::OutOfMemory) => low = mid + 1,
            Err(trap) => panic!("unexpected trap while probing memory: {trap:?}"),
        }
    }
    low
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
fn match_patterns_and_guards_match_the_ast_interpreter() {
    for (id, source) in [
        (
            "enum_or_patterns",
            r#"
            enum Shape { Pair(int, int), Single(int), Empty }
            fn main() {
                let value = Shape.Pair(2, 3)
                match value {
                    Pair(x, 1) | Pair(x, 2) => print("early {x}"),
                    Pair(x, y) => print("pair {x}:{y}"),
                    Single(x) => print("single {x}"),
                    Empty => print("empty"),
                }
                match Shape.Pair(2, 2) {
                    Pair(x, 1) | Pair(x, 2) => print("or {x}"),
                    _ => print("other"),
                }
            }
            "#,
        ),
        (
            "nested_prelude_and_failed_guard",
            r#"
            fn main() {
                let nested: (int?, int) = (Some(4), 5)
                match nested {
                    (Some(x), y) if x > 10 => print("big {x}:{y}"),
                    (Some(x), y) => print("nested {x}:{y}"),
                    (None, _) => print("none"),
                }
            }
            "#,
        ),
        (
            "struct_patterns",
            r#"
            struct Point { x: int, y: int }
            fn main() {
                let point = Point { x: 1, y: 2 }
                match point {
                    Point { x, y } => print("{x}:{y}"),
                }
            }
            "#,
        ),
        (
            "literal_patterns_and_wildcards",
            r#"
            fn main() {
                match "b" {
                    "a" => print("a"),
                    "b" => print("b"),
                    _ => print("other"),
                }
                match true {
                    true => print("yes"),
                    false => print("no"),
                }
                match 3 {
                    1 | 2 => print("small"),
                    n if n > 2 => print("big {n}"),
                    _ => print("other"),
                }
                let none: int? = None
                match none {
                    None => print("none"),
                    Some(value) => print("{value}"),
                }
            }
            "#,
        ),
        (
            "match_block_arms",
            r#"
            fn main() {
                match 1 {
                    1 => { print("one") },
                    _ => {},
                }
                match 2 {
                    value if value > 1 => { print("{value}") },
                    _ => {},
                }
            }
            "#,
        ),
    ] {
        assert_matches(id, source);
    }
}

#[test]
fn failed_guards_restore_the_previous_binding_for_copy_on_write() {
    let case = Case::single(
        r#"
        fn main() {
            var items = [1, 2, 3]
            for i in 0..2 {
                match Some(items) {
                    Some(bound) if i == 0 => print("{bound[0]}"),
                    _ => {}
                }
            }
            items[0] = 9
            print("{items[0]}")
        }
        "#,
    );
    let limits = ExecLimits::default();
    let ast = observe_ast(&case, limits);
    let ir = observe_ir(&case, limits);
    assert_eq!(ir.outcome, ast.outcome, "outcome");
    assert_eq!(ir.log, ast.log, "log");
    assert_eq!(ir.fuel, ast.fuel, "fuel used");
    assert_eq!(
        min_memory_ir(&case),
        min_memory(&case),
        "a failed guard must restore the previous binding, not clear the slot"
    );
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

/// Snapshot cases whose call, builtin and host surface is executable by the
/// #109 evaluator. Closures, captures and `match` cases stay out until #110.
const IR_READY_CASES: &[&str] = &[
    "bench/load/multi",
    "bench/runtime/field_index_ops.mt",
    "bench/runtime/fib.mt",
    "bench/runtime/function_calls.mt",
    "bench/runtime/host_calls",
    "bench/runtime/map_ops.mt",
    "bench/runtime/method_dispatch.mt",
    "bench/runtime/mut_params.mt",
    "bench/runtime/plugin",
    "e2e/generic_stack",
    "host/host_functions_can_construct_handles",
    "host/make_handle_without_an_opaque_return_is_a_host_fault",
    "host/plugin_init_hook_and_state",
    "limits/depth/recursive-down",
    "limits/fuel/join-slots-and-bytes",
    "limits/fuel/list-extend-slots",
    "limits/fuel/shared-insert-copies-and-shifts",
    "limits/fuel/shared-push-copies-the-list",
    "limits/fuel/shared-remove-copies-and-shifts",
    "limits/fuel/sorting",
    "limits/fuel/unique-insert-shifts-only",
    "limits/fuel/unique-push-has-no-copy",
    "limits/fuel/unique-remove-shifts-only",
    "limits/memory/arguments-and-locals",
    "limits/memory/copy-on-write",
    "plugins/a_trap_rolls_back_the_state_and_an_err_commits_it",
    "plugins/handler_parameters_are_copy_in_and_copy_out",
    "plugins/host_functions_can_create_handles_during_a_call",
    "plugins/init_state_and_handler_copy_out_work",
    "plugins/settings_are_delivered_to_init_as_json",
    "plugins/shutdown_and_stateless_plugins_work",
    "plugins/stateless_plugin_handlers",
    "traps/panic/abs-overflow",
    "traps/panic/call-trace",
    "traps/panic/insert-bounds",
    "traps/panic/option-expect",
    "traps/panic/pow-negative",
    "traps/panic/pow-overflow",
    "traps/panic/remove-bounds",
    "traps/panic/replace-empty",
    "traps/panic/result-expect",
    "traps/panic/round-negative",
];

#[test]
fn call_and_host_fixtures_match_the_ast_interpreter() {
    let cases = martite_conformance::snapshot_cases::all_cases();
    for id in IR_READY_CASES {
        let case = cases
            .iter()
            .find(|case| case.id() == *id)
            .unwrap_or_else(|| panic!("missing snapshot case {id}"));
        assert_eq!(case.observe_ir(), case.observe(), "{id}");
    }
}

#[test]
fn call_dispatch_and_try_match_the_ast_interpreter() {
    for (id, source) in [
        (
            "try_across_calls",
            r#"
            fn ok(x: int) -> Result[int, str] { Ok(x) }
            fn fail() -> Result[int, str] { Err("bad") }
            fn chain() -> Result[int, str] {
                let a = ok(1)?
                let b = ok(2)?
                Ok(a + b)
            }
            fn chain_err() -> Result[int, str] {
                let a = ok(1)?
                let b = fail()?
                Ok(a + b)
            }
            fn main() {
                print("{chain()}")
                print("{chain_err()}")
            }
            "#,
        ),
        (
            "call_values",
            r#"
            use std.math.sin

            fn double(x: int) -> int { x * 2 }

            fn main() {
                let function = double
                print("{function(3)}")
                let sine = sin
                print("{sine(0.0)}")
                let check = assert
                check(true, "ok")
            }
            "#,
        ),
        (
            "panic_builtin_value",
            r#"
            fn main() {
                let fail = panic
                fail("boom")
            }
            "#,
        ),
        (
            "json_and_try_across_calls",
            r#"
            use std.json

            fn main() -> Result[(), str] {
                let value = json.parse("[1, 2, 3]")?
                print(json.stringify(value))
                print("12".parse_int())
                print("12.5".parse_float())
                Ok(())
            }
            "#,
        ),
        (
            "builtin_list_methods",
            r#"
            fn inc(x: int) -> int { x + 1 }
            fn even(x: int) -> bool { x % 2 == 0 }
            fn add(acc: int, x: int) -> int { acc + x }
            fn negate(x: int) -> int { -x }

            fn main() {
                var xs = [3, 1, 2]
                xs.push(4)
                xs.insert(1, 9)
                let popped = xs.pop()
                let first = xs.first()
                let last = xs.last()
                let removed = xs.remove_at(0)
                xs.sort()
                let got = xs.get(0)
                let mapped = xs.map(inc)
                let filtered = mapped.filter(even)
                let folded = filtered.fold(0, add)
                let any = xs.any(even)
                let all = xs.all(even)
                let enumerated = xs.enumerate()
                let reversed = xs.reversed()
                let sorted = xs.sorted_by(negate)
                let joined = ["a", "b"].join("-")
                let contains = xs.contains(4)
                xs.extend([5, 6])
                let length = xs.len()
                xs.clear()
                print("{got}:{popped}:{first}:{last}:{removed}:{mapped}:{filtered}:{folded}")
                print("{any}:{all}:{enumerated}:{reversed}:{sorted}")
                print("{joined}:{contains}:{length}:{xs.len()}")
            }
            "#,
        ),
        (
            "builtin_map_methods",
            r#"
            fn main() {
                var table = {"a": 1, "b": 2}
                let got = table.get("a")
                let has = table.has("b")
                let keys = table.keys()
                let values = table.values()
                let entries = table.entries()
                let removed = table.remove("a")
                let length = table.len()
                table.clear()
                print("{got}:{has}:{keys}:{values}:{entries}:{removed}:{length}:{table.len()}")
            }
            "#,
        ),
        (
            "builtin_str_methods",
            r#"
            fn main() {
                let text = "  a,b  "
                let chars = text.chars()
                let parts = text.split(",")
                let trimmed = text.trim()
                let contains = text.contains("a")
                let found = text.find("a")
                let starts = text.starts_with(" ")
                let ends = text.ends_with(" ")
                let replaced = text.replace("a", "A")
                let upper = text.to_upper()
                let lower = text.to_lower()
                let integer = trimmed.parse_int()
                let decimal_trimmed = trimmed.parse_float()
                let small = "12".parse_int()
                let decimal = "12.5".parse_float()
                print("{text.len()}:{chars}:{parts}")
                print("{trimmed}:{contains}:{found}:{starts}:{ends}")
                print("{replaced}:{upper}:{lower}")
                print("{integer}:{decimal_trimmed}:{small}:{decimal}")
            }
            "#,
        ),
        (
            "builtin_scalar_and_range_methods",
            r#"
            fn main() {
                let negative = -3
                let base = 2
                let negative_float = -4.5
                let four = 4.0
                let fraction = 4.9
                let two = 2.0
                let rounded = 1.55
                let zero = 0.0
                let one = 1.0
                let numbers = 1..3
                let inclusive_numbers = 1..=3
                print("{negative.abs()}:{base.pow(3)}")
                print("{negative_float.abs()}:{four.floor()}:{four.ceil()}:{fraction.trunc()}")
                print("{four.sqrt()}:{two.pow(3.0)}:{rounded.round(1)}")
                print("{zero.is_nan()}:{one.is_finite()}")
                print("{numbers.len()}:{numbers.contains(2)}:{inclusive_numbers.to_list()}")
            }
            "#,
        ),
        (
            "builtin_option_result_methods",
            r#"
            fn inc(x: int) -> int { x + 1 }
            fn maybe(x: int) -> int? { Some(x + 1) }
            fn checked(x: int) -> Result[int, str] {
                if x > 0 { Ok(x) } else { Err("negative") }
            }
            fn shout(x: str) -> str { x.to_upper() }

            fn main() {
                let some: int? = Some(3)
                let none: int? = None
                let ok_or_some = some.ok_or("missing")
                let ok_or_none = none.ok_or("missing")
                let expect_some = some.expect("some")
                print("{some.is_some()}:{none.is_none()}")
                print("{some.map(inc)}:{some.and_then(maybe)}:{some.unwrap_or(0)}:{none.unwrap_or(7)}")
                print("{ok_or_some}:{ok_or_none}:{expect_some}")

                let ok: Result[int, str] = Ok(3)
                let err: Result[int, str] = Err("bad")
                let expect_ok = ok.expect("ok")
                print("{ok.is_ok()}:{err.is_err()}")
                print("{ok.map(inc)}:{err.map(inc)}:{err.map_err(shout)}")
                print("{ok.and_then(checked)}:{err.and_then(checked)}")
                print("{ok.unwrap_or(0)}:{err.unwrap_or(7)}:{ok.ok()}:{err.ok()}:{expect_ok}")
            }
            "#,
        ),
        (
            "mut_writeback_places",
            r#"
            struct Box { value: int }

            impl Box {
                fn bump(mut self, amount: int) { self.value += amount }
                fn swap(mut self, mut other: int) {
                    let temp = self.value
                    self.value = other
                    other = temp
                }
            }

            fn add_one(mut value: int) { value += 1 }

            fn main() {
                var box = Box { value: 1 }
                let keep = box
                box.bump(2)
                print("{keep.value}:{box.value}")
                var other = 9
                box.swap(mut other)
                print("{box.value}:{other}")
                add_one(mut box.value)
                print("{box.value}")

                var numbers = [3, 1]
                let numbers_keep = numbers
                numbers.sort()
                numbers.push(2)
                print("{numbers_keep}:{numbers}")

                var table = {"a": [3, 1]}
                table["a"].sort()
                print("{table}")
            }
            "#,
        ),
    ] {
        assert_matches(id, source);
    }
}

#[derive(Clone, Copy)]
enum HostOutcome {
    Unit,
    Fault,
    HandleResult,
}

struct TestHost {
    charge: u64,
    calls: Rc<Cell<usize>>,
    outcome: HostOutcome,
}

impl TestHost {
    fn new(charge: u64, outcome: HostOutcome) -> (Self, Rc<Cell<usize>>) {
        let calls = Rc::new(Cell::new(0));
        (
            Self {
                charge,
                calls: calls.clone(),
                outcome,
            },
            calls,
        )
    }

    fn dispatch(&mut self, cx: &mut HostContext) -> Result<Value, HostFault> {
        self.calls.set(self.calls.get() + 1);
        match self.outcome {
            HostOutcome::Unit => {
                cx.charge(self.charge);
                Ok(Value::Unit)
            }
            HostOutcome::Fault => {
                cx.charge(self.charge);
                Err(HostFault::new("host failed"))
            }
            HostOutcome::HandleResult => {
                let handle = cx.make_handle("token")?;
                Ok(Value::Result(Ok(Box::new(handle))))
            }
        }
    }
}

impl HostDispatcher for TestHost {
    fn call(
        &mut self,
        _def: martite_check::DefId,
        _args: &[Value],
        cx: &mut HostContext,
    ) -> Result<Value, HostFault> {
        self.dispatch(cx)
    }
}

impl IrHostDispatcher for TestHost {
    fn call(
        &mut self,
        _host: martite_ir::HostId,
        _args: &[Value],
        cx: &mut HostContext,
    ) -> Result<Value, HostFault> {
        self.dispatch(cx)
    }
}

fn lowered(
    case: &Case,
) -> (
    martite_check::CheckedProgram,
    martite_base::SourceMap,
    martite_ir::IrProgram,
) {
    let (program, sources) = checked(case);
    let ir = lower(&program, &sources).expect("fixture lowers");
    verify(&ir).expect("fixture verifies");
    (program, sources, ir)
}

fn host_case() -> Case {
    Case::single("use app.api\nfn main() { api.ping() }")
        .interface("app.api", "fn ping()\n")
        .grant("app.api")
}

fn limits(fuel: u64, depth: u32) -> ExecLimits {
    ExecLimits {
        fuel,
        memory: 8 << 20,
        depth,
        deadline: None,
    }
}

#[test]
fn host_fuel_is_charged_after_the_call() {
    let case = host_case();
    let (program, _sources, ir) = lowered(&case);

    for (fuel, completes) in [(7, false), (8, true)] {
        let (mut ast_host, ast_calls) = TestHost::new(5, HostOutcome::Unit);
        let mut ast = Runtime::with_host(&program, limits(fuel, 200), &mut ast_host);
        let ast_result = ast.run_main();
        assert_eq!(ast_calls.get(), 1, "AST host ran before the fuel trap");
        assert_eq!(ast_result.is_ok(), completes, "AST result at fuel {fuel}");

        let (mut ir_host, ir_calls) = TestHost::new(5, HostOutcome::Unit);
        let mut ir_runtime = IrRuntime::with_host(&ir, limits(fuel, 200), &mut ir_host);
        let ir_result = ir_runtime.run_main();
        assert_eq!(ir_calls.get(), 1, "IR host ran before the fuel trap");
        assert_eq!(ir_result.is_ok(), completes, "IR result at fuel {fuel}");
        assert_eq!(
            format!("{ast_result:?}"),
            format!("{ir_result:?}"),
            "host fuel trap at {fuel}"
        );
        assert_eq!(ast.fuel_used(), ir_runtime.fuel_used());
    }
}

#[test]
fn host_depth_is_checked_before_dispatch() {
    let case = host_case();
    let (program, _sources, ir) = lowered(&case);

    for (depth, calls, completes) in [(1, 0, false), (2, 1, true)] {
        let (mut ast_host, ast_calls) = TestHost::new(0, HostOutcome::Unit);
        let mut ast = Runtime::with_host(&program, limits(100, depth), &mut ast_host);
        let ast_result = ast.run_main();
        assert_eq!(ast_calls.get(), calls, "AST host calls at depth {depth}");
        assert_eq!(ast_result.is_ok(), completes, "AST result at depth {depth}");

        let (mut ir_host, ir_calls) = TestHost::new(0, HostOutcome::Unit);
        let mut ir_runtime = IrRuntime::with_host(&ir, limits(100, depth), &mut ir_host);
        let ir_result = ir_runtime.run_main();
        assert_eq!(ir_calls.get(), calls, "IR host calls at depth {depth}");
        assert_eq!(ir_result.is_ok(), completes, "IR result at depth {depth}");
        assert_eq!(
            format!("{ast_result:?}"),
            format!("{ir_result:?}"),
            "host depth trap at {depth}"
        );
    }
}

#[test]
fn host_faults_and_missing_dispatchers_match() {
    let case = host_case();
    let (program, _sources, ir) = lowered(&case);

    let (mut ast_host, _) = TestHost::new(0, HostOutcome::Fault);
    let ast = Runtime::with_host(&program, limits(100, 200), &mut ast_host).run_main();
    let (mut ir_host, _) = TestHost::new(0, HostOutcome::Fault);
    let ir_result = IrRuntime::with_host(&ir, limits(100, 200), &mut ir_host).run_main();
    match (ast, ir_result) {
        (Err(Trap::HostFault(left)), Err(Trap::HostFault(right))) => {
            assert_eq!(left, right);
            assert_eq!(left, "host failed");
        }
        (left, right) => panic!("expected host faults, got {left:?} and {right:?}"),
    }

    let ast = Runtime::new(&program, limits(100, 200)).run_main();
    let ir_result = IrRuntime::new(&ir, limits(100, 200)).run_main();
    assert_eq!(format!("{ast:?}"), format!("{ir_result:?}"));
    assert!(matches!(
        ir_result,
        Err(Trap::Panic { ref message, .. }) if message == "no test double"
    ));
}

#[test]
fn opaque_handle_return_types_match() {
    let interface = "struct Doc\nfn good() -> Result[Doc, str]\nfn bad() -> Result[str, Doc]\n";
    for (function, succeeds) in [("good", true), ("bad", false)] {
        let case = Case::single(format!(
            "use app.api\nfn main() {{ let value = api.{function}()\nlet _ = value }}"
        ))
        .interface("app.api", interface)
        .grant("app.api");
        let (program, _sources, ir) = lowered(&case);

        let (mut ast_host, _) = TestHost::new(0, HostOutcome::HandleResult);
        let ast = Runtime::with_host(&program, limits(100, 200), &mut ast_host).run_main();
        let (mut ir_host, _) = TestHost::new(0, HostOutcome::HandleResult);
        let ir_result = IrRuntime::with_host(&ir, limits(100, 200), &mut ir_host).run_main();

        assert_eq!(ast.is_ok(), succeeds, "AST handle result for {function}");
        assert_eq!(
            ir_result.is_ok(),
            succeeds,
            "IR handle result for {function}"
        );
        match (ast, ir_result) {
            (Ok(ast), Ok(ir_result)) => assert_eq!(
                show(&ast.value, &TypeTable::from_checker(&program), true),
                show(&ir_result.value, &ir.types, true)
            ),
            (Err(ast), Err(ir_result)) => assert_eq!(format!("{ast:?}"), format!("{ir_result:?}")),
            (ast, ir_result) => {
                panic!("handle results differ for {function}: {ast:?} and {ir_result:?}")
            }
        }
    }
}

#[test]
fn invoke_returns_mutable_parameters_in_declaration_order() {
    let case = Case::single("fn bump(mut x: int) -> int { x += 1\nx }");
    let (program, _sources, ir) = lowered(&case);
    let ast_def = program
        .functions
        .iter()
        .enumerate()
        .find_map(|(index, sig)| {
            sig.as_ref()
                .filter(|sig| sig.name == "bump")
                .map(|_| martite_check::DefId(index as u32))
        })
        .expect("bump definition");
    let ir_fn = ir
        .fns
        .iter()
        .find(|function| function.name.as_ref() == "bump")
        .expect("bump IR function")
        .id;

    let ast = Runtime::new(&program, limits(100, 200))
        .invoke(ast_def, vec![Value::Int(1)])
        .expect("AST bump");
    let ir_result = IrRuntime::new(&ir, limits(100, 200))
        .invoke(ir_fn, vec![Value::Int(1)])
        .expect("IR bump");
    let ast_types = TypeTable::from_checker(&program);
    assert_eq!(show(&ast.value, &ast_types, true), "2");
    assert_eq!(show(&ir_result.value, &ir.types, true), "2");
    assert_eq!(ast.params.len(), 1);
    assert_eq!(ir_result.params.len(), 1);
    assert_eq!(
        show(&ast.params[0], &ast_types, true),
        show(&ir_result.params[0], &ir.types, true)
    );
    assert_eq!(show(&ir_result.params[0], &ir.types, true), "2");
}
