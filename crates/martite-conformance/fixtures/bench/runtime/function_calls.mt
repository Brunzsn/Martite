fn step(value: int, amount: int) -> int {
    value + amount
}

fn main() {
    var total = 0
    var index = 0
    while index < 2000 {
        total = step(total, index)
        index += 1
    }
    let _ = total
}
