fn scale_in_place(mut values: list[int], factor: int) {
    var index = 0
    while index < values.len() {
        values[index] = values[index] + factor
        index += 1
    }
}

fn bump(mut value: int, amount: int) -> int {
    value += amount
    value
}

fn main() {
    var values = [1, 2, 3, 4]
    var index = 0
    var total = 0
    while index < 500 {
        scale_in_place(mut values, 1)
        total = bump(mut total, 1)
        index += 1
    }
    let _ = values
    let _ = total
}
