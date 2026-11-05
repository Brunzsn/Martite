fn produce() -> int { 1 }

fn extremely_long_function_name_with_many_words(
    first_argument: int,
    second_argument: int,
    third_argument: int,
) -> int { first_argument + second_argument + third_argument }

fn main() {
    var total = 0
    total = total + 1
    let value = produce()
    let long_variable_name = extremely_long_function_name_with_many_words(
        produce(),
        produce(),
        produce(),
    )
}
