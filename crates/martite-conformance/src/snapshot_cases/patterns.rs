use crate::snapshot::SnapshotCase;
use crate::snapshot_cases::{default_caps, main_case};

pub fn cases() -> Vec<SnapshotCase> {
    let caps = default_caps();
    vec![
        main_case(
            "patterns/struct_literals_preserve_declaration_order_and_updates",
            r#"
            struct Point { x: int, y: int, z: int }

            fn main() {
                let base = Point { x: 1, y: 2, z: 3 }
                let updated = Point { ..base, y: 9 }
                print("{updated}")

                let reordered = Point { z: 30, x: 10, y: 20 }
                print("{reordered}")

                let x = 7
                let shorthand = Point { x, y: 8, z: 9 }
                print("{shorthand}")
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "patterns/struct_literal_fields_evaluate_after_base_in_source_order",
            r#"
            struct Point { x: int, y: int, z: int }

            fn note(mut log: list[int], value: int) -> int {
                log.push(value)
                value
            }

            fn base(mut log: list[int]) -> Point {
                log.push(0)
                Point { x: 0, y: 0, z: 0 }
            }

            fn main() {
                var log: list[int] = []
                let point = Point { ..base(mut log), z: note(mut log, 3), x: note(mut log, 1) }
                print("{log}")
                print("{point}")
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "patterns/struct_patterns_use_declaration_slots",
            r#"
            struct Point { x: int, y: int, z: int }
            struct Segment { start: Point, end: Point }

            fn main() {
                let point = Point { x: 1, y: 2, z: 3 }
                match point {
                    Point { z, x, .. } => print("{x}:{z}"),
                }

                let Point { y, .. } = point
                print("{y}")

                let segment = Segment {
                    start: Point { x: 4, y: 5, z: 6 },
                    end: Point { x: 7, y: 8, z: 9 },
                }
                match segment {
                    Segment { end: Point { y: end_y, .. }, .. } => print("{end_y}"),
                }
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "patterns/variant_patterns_bind_payloads",
            r#"
            enum Shape { Circle(float), Rect(int, int), Empty }

            fn describe(shape: Shape) -> str {
                match shape {
                    Circle(radius) => "circle {radius}",
                    Rect(width, height) => "rect {width}x{height}",
                    Empty => "empty",
                }
            }

            fn main() {
                print("{describe(Shape.Circle(1.5))}")
                print("{describe(Shape.Rect(2, 3))}")
                print("{describe(Shape.Empty)}")

                let some: int? = Some(4)
                print("{some ?? 0}")
                let result: Result[int, str] = Ok(5)
                print("{result.unwrap_or(0)}")
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "patterns/match_guards_and_or_patterns_fall_through_cleanly",
            r#"
            enum Value { A(int, int), B(int), C }

            fn main() {
                let value = Value.A(2, 3)
                match value {
                    A(x, 1) | A(x, 2) => print("early {x}"),
                    A(x, _) => print("late {x}"),
                    B(x) => print("b {x}"),
                    C => print("c"),
                }

                match Some(1) {
                    Some(x) if x > 5 => print("big"),
                    Some(x) => print("{x}"),
                    None => print("none"),
                }
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "patterns/failed_guards_keep_side_effects",
            r#"
            fn bump(mut value: int) -> bool {
                value += 1
                false
            }

            fn main() {
                var value = 0
                match 1 {
                    1 if bump(mut value) => print("yes"),
                    _ => print("{value}"),
                }
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "patterns/nested_or_patterns_and_failed_guards_restore_bindings",
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

                let nested: (int?, int) = (Some(4), 5)
                match nested {
                    (Some(x), y) if x > 10 => print("big {x}:{y}"),
                    (Some(x), y) => print("nested {x}:{y}"),
                    (None, _) => print("none"),
                }

                match Some([1, 2, 3]) {
                    Some(items) if items.len() > 5 => print("big"),
                    Some(items) => print("{items.len()}"),
                    None => print("none"),
                }
            }
            "#,
            None,
            caps,
        ),
    ]
}
