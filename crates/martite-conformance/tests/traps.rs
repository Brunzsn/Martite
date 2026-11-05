use std::time::Duration;

use martite_base::Code;
use martite_conformance::{Case, run_case_with_limits, trap};
use martite_runtime::{ExecLimits, HostContext, HostDispatcher, HostFault, Runtime, Trap, Value};

fn assert_panic(source: &str, expected: &str) {
    match trap(source) {
        Trap::Panic { message, .. } => assert!(
            message.contains(expected),
            "expected panic containing {expected:?}, got {message:?}"
        ),
        other => panic!("expected a panic containing {expected:?}, got {other:?}"),
    }
}

#[test]
fn every_panic_site_is_a_panic_trap() {
    let rows: &[(&str, &str)] = &[
        ("fn main() { let xs = [1]\nprint(\"{xs[3]}\") }", "bounds"),
        (
            "fn main() { let text = \"abc\"\nprint(text[1..9]) }",
            "slice",
        ),
        ("fn main() { let x = 1 / 0\nprint(x) }", "zero"),
        ("fn main() { let x = 1 % 0\nprint(x) }", "zero"),
        (
            "fn main() { let x = 9223372036854775807 + 1\nprint(x) }",
            "overflow",
        ),
        (
            "fn main() { let x = -9223372036854775807 - 2\nprint(x) }",
            "overflow",
        ),
        (
            "fn main() { let min = -9223372036854775807 - 1\nprint(-min) }",
            "overflow",
        ),
        (
            "fn main() { let min = -9223372036854775807 - 1\nprint(min.abs()) }",
            "overflow",
        ),
        (
            "fn main() { let min = -9223372036854775807 - 1\nprint(min / -1) }",
            "overflow",
        ),
        ("fn main() { print(2.pow(63)) }", "overflow"),
        ("fn main() { print(2.pow(-1)) }", "negative"),
        ("fn main() { print(int(0.0 / 0.0)) }", "range"),
        ("fn main() { print(int(1e300)) }", "range"),
        ("fn main() { print(1.5.round(-1)) }", "digit"),
        ("fn main() { print(\"abc\".replace(\"\", \"x\")) }", "empty"),
        (
            "fn main() { let value: int? = None\nprint(value.expect(\"none\")) }",
            "none",
        ),
        (
            "fn main() { let value: Result[int, str] = Err(\"bad\")\nprint(value.expect(\"result\")) }",
            "result",
        ),
        ("fn main() { assert(false, \"boom\") }", "boom"),
        ("fn main() { panic(\"explode\") }", "explode"),
        (
            "fn main() { let table = {\"a\": 1}\nlet value = table[\"b\"]\nprint(value) }",
            "map key",
        ),
        ("fn main() { var xs = [1]\nxs.insert(5, 2) }", "insert"),
        (
            "fn main() { var xs = [1]\nlet _ = xs.remove_at(4) }",
            "bounds",
        ),
    ];
    for (source, expected) in rows {
        assert_panic(source, expected);
    }
}

#[test]
fn panic_traps_carry_a_source_span_and_call_trace() {
    let source = "fn inner() {\n    panic(\"boom\")\n}\nfn outer() {\n    inner()\n}\nfn main() {\n    outer()\n}\n";
    match trap(source) {
        Trap::Panic {
            message,
            source,
            span,
            trace,
        } => {
            assert_eq!(message, "boom");
            assert!(source.is_some());
            assert!(span.is_some());
            assert!(trace.iter().any(|frame| frame == "inner"), "{trace:?}");
            assert!(trace.iter().any(|frame| frame == "outer"), "{trace:?}");
        }
        other => panic!("expected panic, got {other:?}"),
    }
}

#[test]
fn limits_and_host_faults_are_distinct_traps() {
    let out_of_fuel = run_case_with_limits(
        &Case::single("fn main() { var i = 0\nwhile true { i += 1 } }"),
        ExecLimits {
            fuel: 10,
            memory: 8 << 20,
            depth: 200,
            deadline: None,
        },
    );
    assert!(matches!(out_of_fuel, Err(Trap::OutOfFuel)));

    let out_of_memory = run_case_with_limits(
        &Case::single("fn main() { let xs = [1, 2, 3, 4, 5, 6, 7, 8] }"),
        ExecLimits {
            fuel: 1_000_000,
            memory: 4,
            depth: 200,
            deadline: None,
        },
    );
    assert!(matches!(out_of_memory, Err(Trap::OutOfMemory)));

    let depth = run_case_with_limits(
        &Case::single(
            "fn down(n: int) -> int { if n == 0 { 0 } else { down(n - 1) } }\nfn main() { print(down(5)) }",
        ),
        ExecLimits {
            fuel: 1_000_000,
            memory: 8 << 20,
            depth: 2,
            deadline: None,
        },
    );
    assert!(matches!(depth, Err(Trap::DepthExceeded)));

    let deadline = run_case_with_limits(
        &Case::single("fn main() { var i = 0\nwhile true { i += 1 } }"),
        ExecLimits {
            fuel: 1_000_000,
            memory: 8 << 20,
            depth: 200,
            deadline: Some(Duration::ZERO),
        },
    );
    assert!(matches!(deadline, Err(Trap::Deadline)));
}

struct FaultyHost;

impl HostDispatcher for FaultyHost {
    fn call(
        &mut self,
        _def: martite_check::DefId,
        _args: &[Value],
        _cx: &mut HostContext,
    ) -> Result<Value, HostFault> {
        Err(HostFault::new("host failed"))
    }
}

#[test]
fn host_errors_become_host_fault_traps() {
    let case = Case::single("use app.api\nfn main() { api.work() }")
        .interface("app.api", "fn work()\n")
        .grant("app.api");
    let loaded = case.load();
    assert!(!loaded.diagnostics.iter().any(|d| d.code == Code::E508));
    let program = loaded.program.expect("program");
    let mut host = FaultyHost;
    let mut runtime = Runtime::with_host(
        &program,
        ExecLimits {
            fuel: 1_000_000,
            memory: 8 << 20,
            depth: 200,
            deadline: None,
        },
        &mut host,
    );
    match runtime.run_main() {
        Err(Trap::HostFault(message)) => assert_eq!(message, "host failed"),
        other => panic!("expected HostFault, got {other:?}"),
    }
}
