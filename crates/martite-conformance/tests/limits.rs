use std::time::Duration;

use martite_base::Code;
use martite_conformance::{Case, min_fuel, min_memory, run_case, run_case_with_limits};
use martite_runtime::{
    ExecLimits, HostContext, HostDispatcher, HostFault, MAX_VALUE_DEPTH, Trap, Value,
    value_depth_exceeds,
};

struct FuelRow {
    name: &'static str,
    source: &'static str,
    expected: u64,
}

const FUEL_ROWS: &[FuelRow] = &[
    FuelRow {
        name: "empty main",
        source: "fn main() {}",
        expected: 1,
    },
    FuelRow {
        name: "one binding",
        source: "fn main() { let x = 1 }",
        expected: 2,
    },
    FuelRow {
        name: "while loop",
        source: "fn main() { var i = 0\nwhile i < 3 { i += 1 } }",
        expected: 21,
    },
    FuelRow {
        name: "list literal slots",
        source: "fn main() { let xs = [1, 2, 3] }",
        expected: 8,
    },
    FuelRow {
        name: "list equality slots",
        source: "fn main() { let a = [1, 2, 3]\nlet b = [1, 2, 3]\nlet same = a == b\nprint(\"{same}\") }",
        expected: 26,
    },
    FuelRow {
        name: "list show slots",
        source: "fn main() { let xs = [1, 2, 3]\nprint(\"{xs}\") }",
        expected: 15,
    },
    FuelRow {
        name: "map literal slots",
        source: "fn main() { let table = {\"a\": 1, \"b\": 2}\nprint(\"{table}\") }",
        expected: 19,
    },
    FuelRow {
        name: "string concatenation",
        source: "fn main() { let text = \"ab\" + \"cd\"\nprint(text) }",
        expected: 10,
    },
    FuelRow {
        name: "sorting",
        source: "fn main() { let xs = [3, 1, 2]\nlet ys = xs.sorted()\nprint(\"{ys}\") }",
        expected: 23,
    },
    FuelRow {
        name: "print bytes",
        source: "fn main() { print(\"abcdefgh\") }",
        expected: 5,
    },
    FuelRow {
        name: "string slicing",
        source: "fn main() { let text = \"abcdefgh\"\nlet part = text[0..4]\nprint(part) }",
        expected: 12,
    },
    FuelRow {
        name: "tuple literal slots",
        source: "fn main() { let pair = (1, 2, 3) }",
        expected: 8,
    },
    FuelRow {
        name: "tuple equality slots",
        source: "fn main() { let a = (1, 2, 3)\nlet b = (1, 2, 3)\nlet same = a == b\nprint(\"{same}\") }",
        expected: 26,
    },
    FuelRow {
        name: "map equality slots",
        source: "fn main() { let a = {\"x\": 1, \"y\": 2}\nlet b = {\"x\": 1, \"y\": 2}\nlet same = a == b\nprint(\"{same}\") }",
        expected: 35,
    },
    FuelRow {
        name: "list map slots",
        source: "fn main() { let ys = [1, 2, 3].map(fn(x) => x + 1)\nprint(\"{ys}\") }",
        expected: 29,
    },
    FuelRow {
        name: "list extend slots",
        source: "fn main() { var xs = [1, 2]\nxs.extend([3, 4])\nprint(\"{xs}\") }",
        expected: 21,
    },
];

#[test]
fn fuel_table_costs_are_golden() {
    for row in FUEL_ROWS {
        assert_eq!(
            min_fuel(&Case::single(row.source)),
            row.expected,
            "fuel for {}",
            row.name
        );
    }
}

#[test]
fn memory_costs_are_golden() {
    for (name, source, expected) in [
        ("string", "fn main() { let text = \"abcd\" }", 20),
        ("list", "fn main() { let xs = [1, 2, 3] }", 40),
        ("map", "fn main() { let table = {\"a\": 1, \"b\": 2} }", 82),
        (
            "arguments and locals",
            "fn use_it(x: int) -> int {\n    let values = [x, x]\n    values.len()\n}\nfn main() { print(use_it(1)) }",
            32,
        ),
        (
            "copy on write",
            "fn use_it(x: int) -> int {\n    var values = [x, x]\n    var copy = values\n    copy.push(x)\n    values.len() + copy.len()\n}\nfn main() { print(use_it(1)) }",
            72,
        ),
    ] {
        assert_eq!(
            min_memory(&Case::single(source)),
            expected,
            "memory for {name}"
        );
    }
}

#[test]
fn memory_payload_sizes_are_nominal() {
    use martite_runtime::KeyValue;
    use martite_runtime::OrderedMap;
    use std::collections::HashSet;

    let list = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    assert_eq!(list.size_bytes(&mut HashSet::new()), 16 + 8 * 3);
    let text = Value::str("abcd");
    assert_eq!(text.size_bytes(&mut HashSet::new()), 16 + 4);
    let mut map = OrderedMap::new();
    map.insert(KeyValue::Str("a".into()), Value::Int(1));
    map.insert(KeyValue::Str("b".into()), Value::Int(2));
    assert_eq!(
        Value::map(map).size_bytes(&mut HashSet::new()),
        16 + 16 * 2 + 2 * (16 + 1)
    );
}

#[test]
fn shared_rc_payloads_are_counted_once() {
    use std::collections::HashSet;

    let inner = Value::list(vec![Value::Int(1), Value::Int(2)]);
    let outer = Value::list(vec![inner.clone(), inner]);
    // Outer slots: 16 + 2 * 8. The shared inner list contributes 16 + 2 * 8 once.
    assert_eq!(outer.size_bytes(&mut HashSet::new()), (16 + 16) + (16 + 16));
}

#[test]
fn memory_limits_include_locals_arguments_and_copy_on_write() {
    let baseline = min_memory(&Case::single(
        "fn use_it(x: int) -> int {\n    let values = [x, x]\n    values.len()\n}\nfn main() { print(use_it(1)) }",
    ));
    let copied = min_memory(&Case::single(
        "fn use_it(x: int) -> int {\n    var values = [x, x]\n    var copy = values\n    copy.push(x)\n    values.len() + copy.len()\n}\nfn main() { print(use_it(1)) }",
    ));
    assert!(
        copied > baseline,
        "copy-on-write must add the copied payload: baseline {baseline}, copied {copied}"
    );
}

#[test]
fn depth_and_deadline_limits_are_enforced() {
    let recursive = "fn down(n: int) -> int {\n    if n == 0 { 0 } else { down(n - 1) }\n}\nfn main() { print(down(10)) }";
    let limits = ExecLimits {
        fuel: 1_000_000,
        memory: 8 << 20,
        depth: 3,
        deadline: None,
    };
    assert!(matches!(
        run_case_with_limits(&Case::single(recursive), limits),
        Err(Trap::DepthExceeded)
    ));

    let deadline = ExecLimits {
        fuel: 1_000_000,
        memory: 8 << 20,
        depth: 200,
        deadline: Some(Duration::ZERO),
    };
    assert!(matches!(
        run_case_with_limits(
            &Case::single("fn main() { var i = 0\nwhile true { i += 1 } }"),
            deadline
        ),
        Err(Trap::Deadline)
    ));
}

#[test]
fn runtime_value_nesting_is_capped_at_128() {
    // Build a value deeper than the runtime ceiling with a recursive enum.
    let source = r#"
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
        fn main() { print("{build(129)}") }
        "#;
    let trap = run_case(Case::single(source)).expect_err("depth trap");
    assert!(matches!(trap, Trap::DepthExceeded), "{trap:?}");
    assert!(
        value_depth_exceeds(
            &Value::List(std::rc::Rc::new(vec![Value::Int(1)])),
            MAX_VALUE_DEPTH
        ) == false
    );
}

struct ChargingHost;

impl HostDispatcher for ChargingHost {
    fn call(
        &mut self,
        _def: martite_check::DefId,
        _args: &[Value],
        cx: &mut HostContext,
    ) -> Result<Value, HostFault> {
        cx.charge(5);
        Ok(Value::Unit)
    }
}

#[test]
fn host_calls_charge_one_plus_the_host_amount() {
    let case = Case::single("use app.api\nfn main() { api.work() }")
        .interface("app.api", "fn work()\n")
        .grant("app.api");
    let loaded = case.load();
    assert!(!loaded.diagnostics.iter().any(|d| d.code == Code::E508));
    let program = loaded.program.expect("program");

    let mut minimum = None;
    for fuel in 1..=20 {
        let mut host = ChargingHost;
        let mut runtime = martite_runtime::Runtime::with_host(
            &program,
            ExecLimits {
                fuel,
                memory: 8 << 20,
                depth: 200,
                deadline: None,
            },
            &mut host,
        );
        match runtime.run_main() {
            Ok(_) => {
                minimum = Some(fuel);
                break;
            }
            Err(Trap::OutOfFuel) => {}
            Err(trap) => panic!("unexpected trap: {trap:?}"),
        }
    }
    // One expression node, one host-call charge, and the host's five units.
    assert_eq!(minimum, Some(8));
}
