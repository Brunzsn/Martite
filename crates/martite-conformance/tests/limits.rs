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
        expected: 18,
    },
    FuelRow {
        name: "map literal slots",
        source: "fn main() { let table = {\"a\": 1, \"b\": 2}\nprint(\"{table}\") }",
        expected: 23,
    },
    FuelRow {
        name: "string concatenation",
        source: "fn main() { let text = \"ab\" + \"cd\"\nprint(text) }",
        expected: 10,
    },
    FuelRow {
        name: "sorting",
        source: "fn main() { let xs = [3, 1, 2]\nlet ys = xs.sorted()\nprint(\"{ys}\") }",
        expected: 26,
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
        expected: 37,
    },
    FuelRow {
        name: "list map slots",
        source: "fn main() { let ys = [1, 2, 3].map(fn(x) => x + 1)\nprint(\"{ys}\") }",
        expected: 32,
    },
    FuelRow {
        name: "list extend slots",
        source: "fn main() { var xs = [1, 2]\nxs.extend([3, 4])\nprint(\"{xs}\") }",
        expected: 25,
    },
    FuelRow {
        name: "sorting two items",
        source: "fn main() { let xs = [2, 1]\nlet ys = xs.sorted() }",
        expected: 12,
    },
    FuelRow {
        name: "sorting four items",
        source: "fn main() { let xs = [4, 3, 2, 1]\nlet ys = xs.sorted() }",
        expected: 24,
    },
    FuelRow {
        name: "sorting eight items",
        source: "fn main() { let xs = [8, 7, 6, 5, 4, 3, 2, 1]\nlet ys = xs.sorted() }",
        expected: 52,
    },
    FuelRow {
        name: "nested show slots",
        source: "fn main() { let xs = [[1, 2], [3]]\nprint(\"{xs}\") }",
        expected: 24,
    },
    FuelRow {
        name: "join slots and bytes",
        source: "fn main() { let xs = [\"ab\", \"cd\"]\nprint(xs.join(\",\")) }",
        expected: 17,
    },
    FuelRow {
        name: "unique push has no copy",
        source: "fn main() { var xs = [1, 2, 3]\nxs.push(4) }",
        expected: 11,
    },
    FuelRow {
        name: "shared push copies the list",
        source: "fn main() { var xs = [1, 2, 3]\nlet keep = xs\nxs.push(4) }",
        expected: 15,
    },
    FuelRow {
        name: "unique insert shifts only",
        source: "fn main() { var xs = [1, 2, 3, 4]\nxs.insert(1, 9) }",
        expected: 17,
    },
    FuelRow {
        name: "shared insert copies and shifts",
        source: "fn main() { var xs = [1, 2, 3, 4]\nlet keep = xs\nxs.insert(1, 9) }",
        expected: 22,
    },
    FuelRow {
        name: "unique remove shifts only",
        source: "fn main() { var xs = [1, 2, 3, 4]\nlet _removed = xs.remove_at(1) }",
        expected: 15,
    },
    FuelRow {
        name: "shared remove copies and shifts",
        source: "fn main() { var xs = [1, 2, 3, 4]\nlet keep = xs\nlet _removed = xs.remove_at(1) }",
        expected: 20,
    },
    FuelRow {
        name: "unique nested assignment has no copy",
        source: "struct Box { xs: list[int] }\nfn main() { var box = Box { xs: [1, 2] }\nbox.xs[0] = 9 }",
        expected: 10,
    },
    FuelRow {
        name: "shared nested assignment copies parents",
        source: "struct Box { xs: list[int] }\nfn main() { var box = Box { xs: [1, 2] }\nlet keep = box\nbox.xs[0] = 9 }",
        expected: 14,
    },
    FuelRow {
        name: "nested equality slots",
        source: "fn main() { let a = [[1, 2], [3]]\nlet b = [[1, 2], [3]]\nlet same = a == b\nprint(\"{same}\") }",
        expected: 36,
    },
    FuelRow {
        name: "repeated string literal bytes",
        source: "fn main() { var i = 0\nwhile i < 3 { let text = \"abcd\"\ni += 1 } }",
        expected: 27,
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
fn repeated_small_allocations_do_not_inflate_memory() {
    let short = min_memory(&Case::single(
        "fn main() { var i = 0\nwhile i < 10 { let xs = [i, i]\n i += 1 } }",
    ));
    let long = min_memory(&Case::single(
        "fn main() { var i = 0\nwhile i < 1000 { let xs = [i, i]\n i += 1 } }",
    ));
    assert_eq!(long, short);
    assert_eq!(long, 32);
}

#[test]
fn shared_const_values_are_counted_once() {
    let shared = min_memory(&Case::single(
        "const VALUES: list[int] = [1, 2, 3]\nfn main() { var all: list[list[int]] = []\nvar i = 0\nwhile i < 10 { all.push(VALUES)\ni += 1 } }",
    ));
    let rebuilt = min_memory(&Case::single(
        "fn main() { var all: list[list[int]] = []\nvar i = 0\nwhile i < 10 { all.push([1, 2, 3])\ni += 1 } }",
    ));
    assert!(
        shared < rebuilt,
        "cached constants must share one payload: shared {shared}, rebuilt {rebuilt}"
    );
}

#[test]
fn shared_string_literals_are_counted_once() {
    let shared = min_memory(&Case::single(
        "fn main() { var all: list[str] = []\nvar i = 0\nwhile i < 10 { all.push(\"abc\")\ni += 1 } }",
    ));
    let rebuilt = min_memory(&Case::single(
        "fn main() { var all: list[str] = []\nvar i = 0\nwhile i < 10 { all.push(\"a\" + \"bc\")\ni += 1 } }",
    ));
    assert!(
        shared < rebuilt,
        "cached literals must share one payload: shared {shared}, rebuilt {rebuilt}"
    );
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
fn container_mutation_rechecks_value_depth() {
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
    for mutation in [
        "var xs: list[Nested] = []\n    xs.push(build(127))",
        "var xs: list[Nested] = []\n    xs.insert(0, build(127))",
        "var table: map[str, Nested] = {}\n    table[\"key\"] = build(127)",
    ] {
        let source = format!("{build}\nfn main() {{\n    {mutation}\n}}");
        let trap = run_case(Case::single(&source)).expect_err("depth trap");
        assert!(matches!(trap, Trap::DepthExceeded), "{trap:?}");
    }

    let source = format!(
        "{build}\nfn main() {{\n    var xs: list[Nested] = []\n    xs.push(build(126))\n    xs.insert(1, build(126))\n    var table: map[str, Nested] = {{}}\n    table[\"key\"] = build(126)\n}}"
    );
    assert!(run_case(Case::single(&source)).is_ok());
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
