use martite_base::{LoadLimits, MemoryProvider};
use martite_driver::{load, LoadOptions};
use martite_runtime::{ExecLimits, Runtime};

fn run_source(source: &str) -> Vec<String> {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
    provider.insert(
        "util.mt",
        "pub const MAX: int = 5\npub fn twice(x: int) -> int { x * 2 }\n",
    );
    let loaded = load(
        &provider,
        &LoadOptions {
            entry: "main.mt".into(),
            interfaces: Vec::new(),
            grants: Vec::new(),
            limits: LoadLimits::default(),
            edition: "1".into(),
        },
    );
    if !loaded.diagnostics.is_empty() {
        let rendered: Vec<String> = loaded
            .diagnostics
            .iter()
            .map(|d| d.render(&loaded.sources))
            .collect();
        panic!("diagnostics:\n{}", rendered.join("\n"));
    }
    let program = loaded.program.expect("program");
    let mut runtime = Runtime::new(&program, ExecLimits::default());
    let result = runtime.run_main().expect("runtime");
    result.log
}

#[test]
fn hello_world() {
    assert_eq!(run_source(r#"fn main() { print("hello") }"#), ["hello"]);
}

#[test]
fn arithmetic_and_interpolation() {
    assert_eq!(
        run_source(r#"fn main() { print("one plus two = {1 + 2}") }"#),
        ["one plus two = 3"]
    );
}

#[test]
fn let_and_if() {
    assert_eq!(
        run_source(
            r#"
            fn main() {
                let x = 4
                if x > 3 {
                    print("big")
                } else {
                    print("small")
                }
            }
            "#
        ),
        ["big"]
    );
}

#[test]
fn lists_and_loops() {
    assert_eq!(
        run_source(
            r#"
            fn main() {
                var xs = [1, 2, 3]
                xs.push(4)
                var total = 0
                for x in xs { total += x }
                print("{total}")
            }
            "#
        ),
        ["10"]
    );
}

#[test]
fn option_match() {
    assert_eq!(
        run_source(
            r#"
            fn maybe(flag: bool) -> int? {
                if flag { Some(1) } else { None }
            }
            fn main() {
                match maybe(true) {
                    Some(x) => print("{x}"),
                    None => print("none")
                }
            }
            "#
        ),
        ["1"]
    );
}

#[test]
fn functions_and_closures() {
    assert_eq!(
        run_source(
            r#"
            fn double(x: int) -> int { x * 2 }
            fn main() {
                let ys = [1, 2, 3].map(double)
                let zs = ys.map(fn(x) => x + 1)
                print("{zs}")
            }
            "#
        ),
        ["[3, 5, 7]"]
    );
}

#[test]
fn enums_and_match() {
    assert_eq!(
        run_source(
            r#"
            enum Shape { Circle(float), Rect(float, float), Empty }
            fn area(s: Shape) -> float {
                match s {
                    Shape.Circle(r) => 3.0 * r * r,
                    Shape.Rect(w, h) => w * h,
                    Empty => 0.0
                }
            }
            fn main() {
                print("{area(Shape.Circle(2.0))}")
                print("{area(Shape.Rect(2.0, 3.0))}")
            }
            "#
        ),
        ["12.0", "6.0"]
    );
}

#[test]
fn generic_stack() {
    assert_eq!(
        run_source(
            r#"
            struct Stack[T] { items: list[T] }
            impl[T] Stack[T] {
                fn new() -> Stack[T] { Stack { items: [] } }
                fn push(mut self, x: T) { self.items.push(x) }
                fn peek(self) -> T? { self.items.last() }
            }
            fn main() {
                var s: Stack[int] = Stack.new()
                s.push(1)
                s.push(2)
                print("{s.peek()}")
            }
            "#
        ),
        ["Some(2)"]
    );
}

#[test]
fn modules_and_consts() {
    assert_eq!(
        run_source(
            r#"
            use util.MAX
            use util.twice
            fn main() { print("{twice(MAX)}") }
            "#
        ),
        ["10"]
    );
}

#[test]
fn result_and_try() {
    assert_eq!(
        run_source(
            r#"
            fn parse(s: str) -> Result[int, str] {
                match s.parse_int() {
                    Some(v) => Ok(v),
                    None => Err("bad")
                }
            }
            fn run() -> Result[int, str] {
                let a = parse("3")?
                let b = parse("4")?
                Ok(a + b)
            }
            fn main() {
                match run() {
                    Ok(v) => print("{v}"),
                    Err(e) => print("error: {e}")
                }
            }
            "#
        ),
        ["7"]
    );
}

#[test]
fn std_json_round_trip() {
    assert_eq!(
        run_source(
            r#"
            use std.json
            fn main() {
                match json.parse("\{\"a\": [1, 2], \"b\": true\}") {
                    Ok(value) => print(json.stringify(value)),
                    Err(e) => print("error: {e}")
                }
            }
            "#
        ),
        [r#"{"a":[1.0,2.0],"b":true}"#]
    );
}

#[test]
fn structs_and_show() {
    assert_eq!(
        run_source(
            r#"
            struct Point { x: int, y: int }
            fn main() {
                let p = Point { x: 1, y: 2 }
                print("{p}")
            }
            "#
        ),
        ["Point { x: 1, y: 2 }"]
    );
}
