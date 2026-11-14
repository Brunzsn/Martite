//! Focused IR evaluator regression tests.
//!
//! The full-corpus gate lives in `tests/ir_differential.rs` and the checked-in
//! snapshots. The cases here cover invariants that are not represented by a
//! snapshot fixture: host dispatch ordering, exact fuel boundaries, per-site
//! literal accounting and copy-on-write memory behavior.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use martite_base::Severity;
use martite_conformance::{Case, lower_case_ir, min_depth, min_fuel, min_memory};
use martite_ir::{HostId, IrProgram, lower, verify};
use martite_runtime::{
    ExecLimits, HostContext, HostFault, IrHostDispatcher, IrRuntime, Trap, Value, show,
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

fn observe(case: &Case, limits: ExecLimits) -> Observation {
    let (program, sources) = checked(case);
    let ir = lower(&program, &sources).expect("fixture lowers");
    verify(&ir).expect("fixture verifies");
    observe_ir(&ir, limits)
}

fn observe_ir(ir: &IrProgram, limits: ExecLimits) -> Observation {
    let mut runtime = IrRuntime::new(ir, limits);
    let result = runtime.run_main();
    let log = match &result {
        Ok(main) => main.log.clone(),
        Err(_) => runtime.take_log(),
    };
    let fuel = runtime.fuel_used();
    let outcome = match result {
        Ok(main) => Ok(show(&main.value, &ir.types, true)),
        Err(trap) => Err(format!("{trap:?}")),
    };
    Observation { outcome, log, fuel }
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
fn zero_payload_variant_value_keeps_the_tick_cadence() {
    // Sweep enough loop iterations to cross the 1024-tick deadline check.
    let mut first_deadline = None;
    for iterations in 0..=260 {
        let source = format!(
            "enum Shape {{ Empty }}\nfn main() {{\n    var i = 0\n    while i < {iterations} {{ i += 1 }}\n    let _ = Shape.Empty\n    let _ = Shape.Empty()\n}}"
        );
        let case = Case::single(source);
        let observation = observe(
            &case,
            ExecLimits {
                fuel: 1_000_000,
                memory: 8 << 20,
                depth: 200,
                deadline: Some(Duration::ZERO),
            },
        );
        let deadline = matches!(&observation.outcome, Err(message) if message.contains("Deadline"));
        if deadline {
            first_deadline.get_or_insert(iterations);
        }
    }
    assert_eq!(first_deadline, Some(170), "deadline boundary changed");
}

#[test]
fn identical_static_literals_at_distinct_nodes_account_separately() {
    let case = Case::single(
        "fn main() {\n    let a = \"abcd\"\n    let b = \"abcd\"\n    let _ = (a, b)\n}",
    );
    assert_eq!(
        min_memory(&case),
        40,
        "each literal site must have its own first-use cache"
    );
}

#[test]
fn trailing_expression_statement_skips_its_memory_check() {
    let case =
        Case::single("fn main() {\n    var xs = [1, 2, 3]\n    let keep = xs\n    xs.push(4)\n}");
    assert_eq!(
        min_memory(&case),
        40,
        "the evaluator must skip the memory check after a block's trailing expression statement"
    );
}

#[test]
fn value_depth_checks_match_the_snapshot_oracle() {
    let build = r#"
        enum Nested { Leaf, Next(Nested) }
        fn build(n: int) -> Nested {
            var value = Nested.Leaf
            var i = 0
            while i < n {
                value = Nested.Next(value)
                i += 1
            }
            value
        }
    "#;
    for (source, expected) in [
        (
            format!("{build}\nfn main() {{ print(\"{{build(129)}}\") }}"),
            Err("DepthExceeded".into()),
        ),
        (
            format!("{build}\nfn main() {{ var xs: list[Nested] = []\n    xs.push(build(127))\n}}"),
            Err("DepthExceeded".into()),
        ),
        (
            format!(
                "{build}\nfn main() {{ var xs: list[Nested] = []\n    xs.push(build(126))\n    xs.insert(1, build(126))\n}}"
            ),
            Ok("()".into()),
        ),
    ] {
        let observation = observe(&Case::single(&source), ExecLimits::default());
        assert_eq!(
            observation.outcome, expected,
            "value-depth boundary changed"
        );
    }
}

#[test]
fn closure_call_depth_through_builtins_matches() {
    let case = Case::single(
        "fn main() {\n    let xs = [1, 2, 3]\n    let ys = xs.map(fn(x) => x + 1)\n    print(\"{ys}\")\n}",
    );
    assert_eq!(min_depth(&case), 2);
}

#[test]
fn fuel_limit_edges_are_exact() {
    for source in [
        "fn main() {}",
        "fn main() { let x = 1 }",
        "fn main() { let xs = [1, 2, 3] }",
        "fn main() { var i = 0\nwhile i < 3 { i += 1 } }",
    ] {
        let case = Case::single(source);
        let fuel = min_fuel(&case);
        let at = observe(
            &case,
            ExecLimits {
                fuel,
                memory: 8 << 20,
                depth: 200,
                deadline: None,
            },
        );
        assert!(
            at.outcome.is_ok(),
            "{source:?} must complete at fuel {fuel}: {at:?}"
        );
        if fuel > 1 {
            let below = observe(
                &case,
                ExecLimits {
                    fuel: fuel - 1,
                    memory: 8 << 20,
                    depth: 200,
                    deadline: None,
                },
            );
            assert!(
                matches!(&below.outcome, Err(message) if message.contains("OutOfFuel")),
                "{source:?} must trap OutOfFuel at fuel {}: {below:?}",
                fuel - 1
            );
        }
    }
}

#[test]
fn compound_assignment_costs_less_than_the_written_out_form() {
    let compound = min_fuel(&Case::single("fn main() { var value = 1\nvalue += 2 }"));
    let written = min_fuel(&Case::single(
        "fn main() { var value = 1\nvalue = value + 2 }",
    ));
    assert!(
        compound < written,
        "compound assignment {compound} must cost less than the written-out form {written}"
    );
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
    let observation = observe(&case, ExecLimits::default());
    assert_eq!(observation.outcome, Ok("()".into()));
    assert_eq!(observation.log, ["1", "9"]);
    assert_eq!(
        min_memory(&case),
        80,
        "a failed guard must restore the previous binding, not clear the slot"
    );
}

#[test]
fn failed_guard_bindings_do_not_leak_a_reference_in_ir() {
    let baseline = Case::single(
        "fn main() {\n    var items = [1, 2, 3]\n    items.push(4)\n    print(\"{items.len()}\")\n}",
    );
    let with_failed_guard = Case::single(
        "fn main() {\n    var items = [1, 2, 3]\n    match Some(items) {\n        Some(bound) if false => print(\"{bound.len()}\"),\n        _ => {}\n    }\n    items.push(4)\n    print(\"{items.len()}\")\n}",
    );
    let observation = observe(&with_failed_guard, ExecLimits::default());
    assert_eq!(observation.outcome, Ok("()".into()));
    assert_eq!(observation.log, ["4"]);
    assert_eq!(
        min_memory(&with_failed_guard),
        min_memory(&baseline),
        "a failed guard must not keep its binding slot live into later copy-on-write"
    );
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

impl IrHostDispatcher for TestHost {
    fn call(
        &mut self,
        _host: HostId,
        _args: &[Value],
        cx: &mut HostContext,
    ) -> Result<Value, HostFault> {
        self.dispatch(cx)
    }
}

fn lowered(case: &Case) -> IrProgram {
    lower_case_ir(case)
}

fn host_case() -> Case {
    Case::single("use app.api\nfn main() { api.ping() }")
        .interface("app.api", "fn ping()\n")
        .grant("app.api")
}

#[test]
fn host_fuel_is_charged_after_the_call() {
    let case = host_case();
    let ir = lowered(&case);

    for (fuel, completes) in [(7, false), (8, true)] {
        let (mut host, calls) = TestHost::new(5, HostOutcome::Unit);
        let mut runtime = IrRuntime::with_host(&ir, limits(fuel, 200), &mut host);
        let result = runtime.run_main();
        assert_eq!(calls.get(), 1, "host ran before the fuel trap");
        assert_eq!(result.is_ok(), completes, "result at fuel {fuel}");
    }
}

#[test]
fn host_depth_is_checked_before_dispatch() {
    let case = host_case();
    let ir = lowered(&case);

    for (depth, calls, completes) in [(1, 0, false), (2, 1, true)] {
        let (mut host, call_count) = TestHost::new(0, HostOutcome::Unit);
        let mut runtime = IrRuntime::with_host(&ir, limits(100, depth), &mut host);
        let result = runtime.run_main();
        assert_eq!(call_count.get(), calls, "host calls at depth {depth}");
        assert_eq!(result.is_ok(), completes, "result at depth {depth}");
    }
}

#[test]
fn host_faults_and_missing_dispatchers_are_reported() {
    let case = host_case();
    let ir = lowered(&case);

    let (mut host, _) = TestHost::new(0, HostOutcome::Fault);
    let result = IrRuntime::with_host(&ir, limits(100, 200), &mut host).run_main();
    match result {
        Err(Trap::HostFault(message)) => assert_eq!(message, "host failed"),
        other => panic!("expected a host fault, got {other:?}"),
    }

    let result = IrRuntime::new(&ir, limits(100, 200)).run_main();
    assert!(matches!(
        result,
        Err(Trap::Panic { ref message, .. }) if message == "no test double"
    ));
}

#[test]
fn opaque_handle_return_types_are_enforced() {
    let interface = "struct Doc\nfn good() -> Result[Doc, str]\nfn bad() -> Result[str, Doc]\n";
    for (function, succeeds) in [("good", true), ("bad", false)] {
        let case = Case::single(format!(
            "use app.api\nfn main() {{ let value = api.{function}()\nlet _ = value }}"
        ))
        .interface("app.api", interface)
        .grant("app.api");
        let ir = lowered(&case);

        let (mut host, _) = TestHost::new(0, HostOutcome::HandleResult);
        let result = IrRuntime::with_host(&ir, limits(100, 200), &mut host).run_main();
        assert_eq!(result.is_ok(), succeeds, "handle result for {function}");
    }
}

#[test]
fn invoke_returns_mutable_parameters_in_declaration_order() {
    let case = Case::single("fn bump(mut x: int) -> int { x += 1\nx }");
    let ir = lowered(&case);
    let function = ir
        .fns
        .iter()
        .find(|function| function.name.as_ref() == "bump")
        .expect("bump IR function")
        .id;

    let result = IrRuntime::new(&ir, limits(100, 200))
        .invoke(function, vec![Value::Int(1)])
        .expect("bump");
    assert_eq!(show(&result.value, &ir.types, true), "2");
    assert_eq!(result.params.len(), 1);
    assert_eq!(show(&result.params[0], &ir.types, true), "2");
}
