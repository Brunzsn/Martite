fn bump(mut value: int, amount: int) -> int {
    value += amount
    value
}

fn scale(mut values: list[int], factor: int) {
    var index = 0
    while index < values.len() {
        values[index] = values[index] * factor
        index += 1
    }
}

fn main() {
    var total = 1
    total = bump(mut total, 4)
    var values = [1, 2, 3]
    scale(mut values, 2)
    print("{total}:{values}")
}
