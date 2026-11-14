use martite_base::Code;
use martite_conformance::{Case, assert_code, codes, run, run_case_with_limits, trap};
use martite_runtime::{ExecLimits, Trap, Value};

#[test]
fn lexical_errors() {
    assert_code(r#"fn main() { let x = "\q" }"#, Code::E001);
    assert_code("fn main() { let fooBar = 1 }", Code::E104);
    assert_code("fn main( { }", Code::E003);
}

#[test]
fn load_limit_e002() {
    let mut source = String::from("fn main() { let x = ");
    for _ in 0..200 {
        source.push('(');
    }
    source.push('1');
    for _ in 0..200 {
        source.push(')');
    }
    source.push_str(" }");
    assert_code(&source, Code::E002);
}

#[test]
fn name_errors() {
    assert_code("fn main() { missing() }", Code::E101);
    assert_code("fn main() {}\nfn main() {}\n", Code::E102);
    assert_code("fn main() { let x = 1\nlet x = 2 }", Code::E103);
    assert_code(
        "const A: int = B\nconst B: int = A\nfn main() {}",
        Code::E105,
    );
    assert_code("use nowhere\nfn main() {}", Code::E106);
}

#[test]
fn type_errors() {
    assert_code(r#"fn main() { let x: int = "no" }"#, Code::E201);
    assert_code("fn main() { let x = [] }", Code::E202);
    assert_code(r#"fn main() { let x = 1 + "a" }"#, Code::E203);
    assert_code("fn f(x: int) {}\nfn main() { f() }", Code::E204);
    assert_code("fn main() { let m = {1.0: 2} }", Code::E205);
    assert_code("fn main() { 1 }", Code::E206);
    assert_code(
        "fn main() { let x: int? = Some(1)\nlet y = x? }",
        Code::E207,
    );
    assert_code("fn main() { let x = 1\nx.nope() }", Code::E208);
    assert_code("fn main() { let x: list[int, str] = [] }", Code::E209);
}

#[test]
fn pattern_errors() {
    assert_code(
        "fn main() { match true { true => print(\"x\") } }",
        Code::E301,
    );
    assert_code("fn main() { let Some(x) = Some(1) }", Code::E302);
    assert_code(
        "fn main() { match 1 { 1 | x => print(\"x\"), _ => print(\"y\") } }",
        Code::E303,
    );
    assert_code(
        "fn main() { match 1 { true => print(\"x\"), _ => print(\"y\") } }",
        Code::E304,
    );
}

#[test]
fn mutability_errors() {
    assert_code("fn main() { let x = 1\nx = 2 }", Code::E401);
    assert_code(
        "fn f(mut x: int) {}\nfn main() { let x = 1\nf(x) }",
        Code::E402,
    );
    assert_code(
        "fn f(mut a: int, b: int) {}\nfn main() { var x = 1\nf(mut x, x) }",
        Code::E403,
    );
    assert_code(
        "fn main() { var x = 1\nlet f = fn() { x = 2 }\nlet _ = f }",
        Code::E404,
    );
    assert_code("fn main() { break }", Code::E405);
    assert_code("const XX: int = 1 / 0\nfn main() {}", Code::E406);
    assert_code("fn main(x: int) {}", Code::E407);
}

#[test]
fn plugin_errors() {
    let interface = r#"struct Doc
fn text(doc: Doc) -> str
on saved(doc: Doc) -> Result[(), str]
"#;
    let base = Case::single("use app.editor\nfn main() {}").interface("app.editor", interface);
    assert!(codes(base).contains(&Code::E501));

    let base = Case::single("on nope() {}\n")
        .interface("app.editor", interface)
        .grant("app.editor");
    assert!(codes(base).contains(&Code::E502));

    let base = Case::single("on saved() -> Result[(), str] { Ok(()) }\n")
        .interface("app.editor", interface)
        .grant("app.editor");
    assert!(codes(base).contains(&Code::E503));

    let base = Case::single("on saved(mut self) -> Result[(), str] { Ok(()) }\n")
        .interface("app.editor", interface)
        .grant("app.editor");
    assert!(codes(base).contains(&Code::E504));

    let base = Case::single("on init() -> fn() -> int { fn() => 1 }\n")
        .interface("app.editor", interface)
        .grant("app.editor");
    assert!(codes(base).contains(&Code::E506));

    let calls_handler = Case::single("on a() { b() }\non b() {}\n")
        .interface("app.hooks", "on a()\non b()\n")
        .grant("app.hooks");
    assert!(codes(calls_handler).contains(&Code::E505));

    let base = Case::single("fn main() {}").interface("bad.iface", "on init() -> int\n");
    assert!(codes(base).contains(&Code::E507));
}

#[test]
fn show_format_golden() {
    let logs = run(r#"
        enum Color { Red, Green }
        struct Point { x: int, y: float }
        fn main() {
            print("{1}")
            print("{2.0}")
            let text = "a"
            print("{text}")
            let items = [1, 2]
            print("{items}")
            let table = {"a": 1}
            print("{table}")
            print("{Point { x: 1, y: 2.5 }}")
            print("{Color.Red}")
            print("{Some(1)}")
            print("{0..3}")
        }
        "#);
    assert_eq!(
        logs,
        vec![
            "1",
            "2.0",
            "a",
            "[1, 2]",
            "{\"a\": 1}",
            "Point { x: 1, y: 2.5 }",
            "Color.Red",
            "Some(1)",
            "0..3",
        ]
    );
}

#[test]
fn const_values_are_shared_but_stay_immutable() {
    let logs = run(r#"
        struct Point { x: int, y: int }

        const VALUES: list[int] = [1, 2, 3]
        const TABLE: map[str, int] = {"a": 1, "b": 2}
        const ORIGIN: Point = Point { x: 4, y: 5 }
        const NESTED: map[str, list[int]] = {"a": [1]}

        fn main() {
            var copy = VALUES
            copy.push(4)
            var table = TABLE
            table["c"] = 3
            var nested = NESTED
            nested["a"].push(2)
            print("{VALUES}")
            print("{copy}")
            print("{TABLE}")
            print("{table}")
            print("{NESTED}")
            print("{nested}")
            print("{ORIGIN.x + ORIGIN.y}")
        }
        "#);
    assert_eq!(
        logs,
        vec![
            "[1, 2, 3]",
            "[1, 2, 3, 4]",
            "{\"a\": 1, \"b\": 2}",
            "{\"a\": 1, \"b\": 2, \"c\": 3}",
            "{\"a\": [1]}",
            "{\"a\": [1, 2]}",
            "9",
        ]
    );
}

#[test]
fn const_containers_are_reused_in_loops() {
    let logs = run(r#"
        const VALUES: list[int] = [1, 2, 3, 4]
        const TABLE: map[str, int] = {"a": 1, "b": 2}

        fn main() {
            var total = 0
            var index = 0
            while index < 3 {
                total += VALUES[0] + VALUES[3]
                total += TABLE["a"] + TABLE["b"]
                index += 1
            }
            print("{total}")
        }
        "#);
    assert_eq!(logs, ["24"]);
}

#[test]
fn std_math_surface_uses_euler() {
    let logs = run(r#"
        use std.math
        use std.math.EULER
        fn main() {
            print("{EULER}")
            print("{math.EULER}")
            print("{math.PI}")
            print("{math.min(3, 1)}")
            print("{math.clamp(5, 0, 3)}")
        }
        "#);
    assert_eq!(
        logs,
        vec![
            "2.718281828459045",
            "2.718281828459045",
            "3.141592653589793",
            "1",
            "3",
        ]
    );
}

#[test]
fn runtime_panics() {
    assert!(matches!(
        trap("fn main() { let xs = [1]\nprint(\"{xs[3]}\") }"),
        Trap::Panic { .. }
    ));
    assert!(matches!(
        trap("fn main() { let x = 1 / 0\nprint(\"{x}\") }"),
        Trap::Panic { .. }
    ));
    assert!(matches!(
        trap("fn main() { assert(false, \"boom\") }"),
        Trap::Panic { .. }
    ));
}

#[test]
fn fuel_limit() {
    let result = run_case_with_limits(
        &Case::single("fn main() { var i = 0\nwhile i < 1000 { i += 1 } }"),
        ExecLimits {
            fuel: 10,
            memory: 8 << 20,
            depth: 50,
            deadline: None,
        },
    );
    assert!(matches!(result, Err(Trap::OutOfFuel)));
}

#[test]
fn memory_limit_is_enforced() {
    let result = run_case_with_limits(
        &Case::single("fn main() { let xs = [1, 2, 3, 4, 5, 6, 7, 8] }"),
        ExecLimits {
            fuel: 1_000_000,
            memory: 4,
            depth: 50,
            deadline: None,
        },
    );
    assert!(matches!(result, Err(Trap::OutOfMemory)));
}

#[test]
fn result_and_options_work() {
    let logs = run(r#"
        fn main() {
            let x: int? = Some(2)
            print("{x ?? 0}")
            let y: int? = None
            print("{y ?? 7}")
            let r: Result[int, str] = Ok(3)
            print("{r.unwrap_or(0)}")
            let e: Result[int, str] = Err("bad")
            print("{e.unwrap_or(9)}")
        }
        "#);
    assert_eq!(logs, ["2", "7", "3", "9"]);
}

#[test]
fn option_and_result_methods_move_payloads() {
    assert_eq!(
        run(r#"
        fn main() {
            let values = [10, 20]
            let some: list[int]? = Some([1, 2, 3])
            let none: list[int]? = None
            print("{some.is_some()}")
            print("{none.is_none()}")
            print("{some.map(fn(items) => items.len())}")
            print("{none.map(fn(items) => items.len())}")
            print("{some.and_then(fn(items) => Some(items.len() * 2))}")
            print("{none.and_then(fn(items) => Some(items.len()))}")
            print("{some.ok_or("missing")}")
            print("{none.ok_or("missing")}")
            print("{some.unwrap_or([9])}")
            print("{none.unwrap_or([9])}")
            print("{some.expect("some")}")
            print("{some}")
            print("{values.get(0).map(fn(value) => value + 1)}")
            print("{values.get(7).and_then(fn(value) => Some(value + 1))}")

            let ok: Result[list[int], str] = Ok([1, 2])
            let err: Result[list[int], str] = Err("bad")
            print("{ok.is_ok()}")
            print("{err.is_err()}")
            print("{ok.map(fn(items) => items.len())}")
            print("{err.map(fn(items) => items.len())}")
            print("{ok.map_err(fn(message) => message.len())}")
            print("{err.map_err(fn(message) => message.len())}")
            print("{ok.and_then(fn(items) => Ok(items.len()))}")
            print("{err.and_then(fn(items) => Ok(items.len()))}")
            print("{ok.ok()}")
            print("{err.ok()}")
            print("{ok.unwrap_or([9])}")
            print("{err.unwrap_or([9])}")
            print("{ok.expect("ok")}")
            print("{err}")
        }
        "#),
        [
            "true",
            "true",
            "Some(3)",
            "None",
            "Some(6)",
            "None",
            "Ok([1, 2, 3])",
            "Err(\"missing\")",
            "[1, 2, 3]",
            "[9]",
            "[1, 2, 3]",
            "Some([1, 2, 3])",
            "Some(11)",
            "None",
            "true",
            "true",
            "Ok(2)",
            "Err(\"bad\")",
            "Ok([1, 2])",
            "Err(3)",
            "Ok(2)",
            "Err(\"bad\")",
            "Some([1, 2])",
            "None",
            "[1, 2]",
            "[9]",
            "[1, 2]",
            "Err(\"bad\")",
        ]
    );
}

#[test]
fn host_value_helpers_are_stable() {
    // A tiny sanity check that Value equality follows the specified rules.
    assert!(Value::Int(1) != Value::Float(1.0));
    assert!(matches!(Value::Option(None), Value::Option(None)));
}
