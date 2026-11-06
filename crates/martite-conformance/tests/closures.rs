use martite_conformance::{Case, run};

#[test]
fn lambdas_capture_by_value() {
    assert_eq!(
        run(r#"
            fn main() {
                var value = 1
                let read = fn() => value
                value = 2
                print("{read()}")
            }
            "#),
        ["1"]
    );
}

#[test]
fn nested_lambdas_capture_transitively() {
    assert_eq!(
        run(r#"
            fn main() {
                let base = 10
                let add = fn(a: int) => fn(b: int) => base + a + b
                let add_five = add(5)
                print("{add_five(2)}")
            }
            "#),
        ["17"]
    );
}

#[test]
fn loop_lambdas_capture_each_iteration() {
    assert_eq!(
        run(r#"
            fn main() {
                var readers: list[fn() -> int] = []
                for i in 0..3 {
                    readers.push(fn() => i)
                }
                print("{readers[0]()}{readers[1]()}{readers[2]()}")
            }
            "#),
        ["012"]
    );
}

#[test]
fn free_function_values_work_in_higher_order_builtins() {
    assert_eq!(
        run(r#"
            fn double(value: int) -> int { value * 2 }
            fn main() {
                let function = double
                let values = [1, 2, 3]
                print("{values.map(function)}")
            }
            "#),
        ["[2, 4, 6]"]
    );
}

#[test]
fn higher_order_builtins_accept_borrowed_closures() {
    assert_eq!(
        run(r#"
            fn main() {
                let values = ["bbb", "a", "cc"]
                print("{values.sorted_by(fn(value) => value.len())}")
                let some: int? = Some(2)
                print("{some.map(fn(value) => value + 1)}")
                let result: Result[int, str] = Ok(3)
                print("{result.map(fn(value) => value * 2)}")
            }
            "#),
        [r#"["a", "cc", "bbb"]"#, "Some(3)", "Ok(6)"]
    );
}

#[test]
fn lambdas_record_only_free_variables() {
    let loaded = Case::single(
        r#"
        fn main() {
            let used = 3
            let unused = 4
            let add = fn(value: int) => value + used
            print("{add(1)}")
            let _ = unused
        }
        "#,
    )
    .load();
    let program = loaded.program.expect("program");
    let captures: Vec<Vec<String>> = program
        .lambdas
        .values()
        .map(|lambda| {
            let mut names: Vec<String> = lambda
                .captures
                .iter()
                .map(|id| {
                    program
                        .bindings
                        .get(id)
                        .expect("capture must have a binding")
                        .name
                        .clone()
                })
                .collect();
            names.sort();
            names
        })
        .collect();
    assert_eq!(captures, vec![vec!["used".to_string()]]);
}

#[test]
fn nested_lambdas_record_transitive_captures() {
    let loaded = Case::single(
        r#"
        fn main() {
            let base = 10
            let add = fn(a: int) => fn(b: int) => base + a + b
            let add_five = add(5)
            print("{add_five(2)}")
        }
        "#,
    )
    .load();
    let program = loaded.program.expect("program");
    let mut captures: Vec<Vec<String>> = program
        .lambdas
        .values()
        .map(|lambda| {
            let mut names: Vec<String> = lambda
                .captures
                .iter()
                .map(|id| {
                    program
                        .bindings
                        .get(id)
                        .expect("capture must have a binding")
                        .name
                        .clone()
                })
                .collect();
            names.sort();
            names
        })
        .collect();
    captures.sort();
    assert_eq!(
        captures,
        vec![
            vec!["a".to_string(), "base".to_string()],
            vec!["base".to_string()],
        ]
    );
}

#[test]
fn calls_copy_out_only_mutable_parameters() {
    assert_eq!(
        run(r#"
            fn bump(mut value: int, amount: int) -> int {
                value += amount
                value
            }
            fn main() {
                var value = 1
                let result = bump(mut value, 2)
                print("{value}:{result}")
            }
            "#),
        ["3:3"]
    );
}
