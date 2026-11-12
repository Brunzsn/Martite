use crate::snapshot::SnapshotCase;
use crate::snapshot_cases::{default_caps, main_case, main_case_files};

pub fn cases() -> Vec<SnapshotCase> {
    let caps = default_caps();
    let mut cases = vec![
        main_case(
            "e2e/hello_world",
            r#"fn main() { print("hello") }"#,
            None,
            caps,
        ),
        main_case(
            "e2e/arithmetic_and_interpolation",
            r#"fn main() { print("one plus two = {1 + 2}") }"#,
            None,
            caps,
        ),
        main_case(
            "e2e/let_and_if",
            r#"
            fn main() {
                let x = 4
                if x > 3 {
                    print("big")
                } else {
                    print("small")
                }
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "e2e/lists_and_loops",
            r#"
            fn main() {
                var xs = [1, 2, 3]
                xs.push(4)
                var total = 0
                for x in xs { total += x }
                print("{total}")
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "e2e/loops_iterate_a_snapshot",
            r#"
            fn main() {
                var xs = [1, 2, 3]
                var total = 0
                for x in xs {
                    total += x
                    xs.push(x)
                }
                print("{total}:{xs.len()}")

                var table = {"a": 1, "b": 2}
                var keys = ""
                for (key, value) in table {
                    keys += key
                    table["c"] = value
                }
                print("{keys}:{table.len()}")

                var text = "ab"
                var seen = ""
                for ch in text {
                    seen += ch
                    text = text + "c"
                }
                print("{seen}:{text.len()}")
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "e2e/mut_receiver_is_copied_in_before_arguments",
            r#"
            fn add_marker(mut xs: list[int]) -> int {
                xs.push(9)
                0
            }
            fn main() {
                var xs = [1, 2, 3]
                xs.push(add_marker(mut xs))
                print("{xs}")
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "e2e/option_match",
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
            "#,
            None,
            caps,
        ),
        main_case(
            "e2e/functions_and_closures",
            r#"
            fn double(x: int) -> int { x * 2 }
            fn main() {
                let ys = [1, 2, 3].map(double)
                let zs = ys.map(fn(x) => x + 1)
                print("{zs}")
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "e2e/enums_and_match",
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
            "#,
            None,
            caps,
        ),
        main_case(
            "e2e/generic_stack",
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
            "#,
            None,
            caps,
        ),
        main_case(
            "e2e/result_and_try",
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
            "#,
            None,
            caps,
        ),
        main_case(
            "e2e/std_json_round_trip",
            r#"
            use std.json
            fn main() {
                match json.parse("\{\"a\": [1, 2], \"b\": true\}") {
                    Ok(value) => print(json.stringify(value)),
                    Err(e) => print("error: {e}")
                }
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "e2e/std_json_duplicate_keys_keep_first_position_and_last_value",
            r#"
            use std.json
            fn main() {
                match json.parse("\{\"a\": 1, \"b\": 2, \"a\": 3\}") {
                    Ok(value) => print(json.stringify(value)),
                    Err(e) => print("error: {e}")
                }
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "e2e/structs_and_show",
            r#"
            struct Point { x: int, y: int }
            fn main() {
                let p = Point { x: 1, y: 2 }
                print("{p}")
            }
            "#,
            None,
            caps,
        ),
    ];
    cases.push(main_case_files(
        "e2e/modules_and_consts",
        vec![
            (
                "main.mt".into(),
                r#"
            use util.MAX
            use util.twice
            fn main() { print("{twice(MAX)}") }
            "#
                .into(),
            ),
            (
                "util.mt".into(),
                "pub const MAX: int = 5\npub fn twice(x: int) -> int { x * 2 }\n".into(),
            ),
        ],
        "main.mt",
        None,
        caps,
    ));
    cases
}
