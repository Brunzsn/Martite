use crate::snapshot::SnapshotCase;
use crate::snapshot_cases::{default_caps, main_case};

pub fn cases() -> Vec<SnapshotCase> {
    let caps = default_caps();
    vec![
        main_case(
            "closures/lambdas_capture_by_value",
            r#"
            fn main() {
                var value = 1
                let read = fn() => value
                value = 2
                print("{read()}")
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "closures/nested_lambdas_capture_transitively",
            r#"
            fn main() {
                let base = 10
                let add = fn(a: int) => fn(b: int) => base + a + b
                let add_five = add(5)
                print("{add_five(2)}")
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "closures/loop_lambdas_capture_each_iteration",
            r#"
            fn main() {
                var readers: list[fn() -> int] = []
                for i in 0..3 {
                    readers.push(fn() => i)
                }
                print("{readers[0]()}{readers[1]()}{readers[2]()}")
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "closures/free_function_values_work_in_higher_order_builtins",
            r#"
            fn double(value: int) -> int { value * 2 }
            fn main() {
                let function = double
                let values = [1, 2, 3]
                print("{values.map(function)}")
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "closures/builtin_module_function_values_are_callable",
            r#"
            use std.math.sin
            fn main() {
                let function = sin
                print("{function(0.0)}")
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "closures/higher_order_builtins_accept_borrowed_closures",
            r#"
            fn main() {
                let values = ["bbb", "a", "cc"]
                print("{values.sorted_by(fn(value) => value.len())}")
                let some: int? = Some(2)
                print("{some.map(fn(value) => value + 1)}")
                let result: Result[int, str] = Ok(3)
                print("{result.map(fn(value) => value * 2)}")
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "closures/lambdas_record_only_free_variables",
            r#"
            fn main() {
                let used = 3
                let unused = 4
                let add = fn(value: int) => value + used
                print("{add(1)}")
                let _ = unused
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "closures/nested_lambdas_record_transitive_captures",
            r#"
            fn main() {
                let base = 10
                let add = fn(a: int) => fn(b: int) => base + a + b
                let add_five = add(5)
                print("{add_five(2)}")
            }
            "#,
            None,
            caps,
        ),
        main_case(
            "closures/calls_copy_out_only_mutable_parameters",
            r#"
            fn bump(mut value: int, amount: int) -> int {
                value += amount
                value
            }
            fn main() {
                var value = 1
                let result = bump(mut value, 2)
                print("{value}:{result}")
            }
            "#,
            None,
            caps,
        ),
    ]
}
