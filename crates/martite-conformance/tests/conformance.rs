use martite_base::Code;
use martite_conformance::{assert_code, codes, run, trap, Case};
use martite_runtime::{ExecLimits, Runtime, Trap, Value};

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
    assert_code(
        "fn main() {}\nfn main() {}\n",
        Code::E102,
    );
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
    assert_code(
        "fn f(x: int) {}\nfn main() { f() }",
        Code::E204,
    );
    assert_code("fn main() { let m = {1.0: 2} }", Code::E205);
    assert_code("fn main() { 1 }", Code::E206);
    assert_code(
        "fn main() { let x: int? = Some(1)\nlet y = x? }",
        Code::E207,
    );
    assert_code("fn main() { let x = 1\nx.nope() }", Code::E208);
    assert_code(
        "fn main() { let x: list[int, str] = [] }",
        Code::E209,
    );
}

#[test]
fn pattern_errors() {
    assert_code(
        "fn main() { match true { true => print(\"x\") } }",
        Code::E301,
    );
    assert_code(
        "fn main() { let Some(x) = Some(1) }",
        Code::E302,
    );
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
        .interface(
            "app.hooks",
            "on a()\non b()\n",
        )
        .grant("app.hooks");
    assert!(codes(calls_handler).contains(&Code::E505));

    let base = Case::single("fn main() {}")
        .interface("bad.iface", "on init() -> int\n");
    assert!(codes(base).contains(&Code::E507));
}

#[test]
fn show_format_golden() {
    let logs = run(
        r#"
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
        "#,
    );
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
fn std_math_surface_uses_euler() {
    let logs = run(
        r#"
        use std.math
        use std.math.EULER
        fn main() {
            print("{EULER}")
            print("{math.EULER}")
            print("{math.PI}")
            print("{math.min(3, 1)}")
            print("{math.clamp(5, 0, 3)}")
        }
        "#,
    );
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
    let loaded = Case::single("fn main() { var i = 0\nwhile i < 1000 { i += 1 } }").load();
    let program = loaded.program.expect("program");
    let mut runtime = Runtime::new(
        &program,
        ExecLimits {
            fuel: 10,
            memory: 8 << 20,
            depth: 50,
            deadline: None,
        },
    );
    assert!(matches!(runtime.run_main(), Err(Trap::OutOfFuel)));
}

#[test]
fn memory_limit_is_enforced() {
    let loaded = Case::single("fn main() { let xs = [1, 2, 3, 4, 5, 6, 7, 8] }").load();
    let program = loaded.program.expect("program");
    let mut runtime = Runtime::new(
        &program,
        ExecLimits {
            fuel: 1_000_000,
            memory: 4,
            depth: 50,
            deadline: None,
        },
    );
    assert!(matches!(runtime.run_main(), Err(Trap::OutOfMemory)));
}

#[test]
fn result_and_options_work() {
    let logs = run(
        r#"
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
        "#,
    );
    assert_eq!(logs, ["2", "7", "3", "9"]);
}

#[test]
fn host_value_helpers_are_stable() {
    // A tiny sanity check that Value equality follows the specified rules.
    assert!(Value::Int(1) != Value::Float(1.0));
    assert!(matches!(Value::Option(None), Value::Option(None)));
}
