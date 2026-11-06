fn main() {
    let values = [1, 2, 3, 4, 5, 6, 7, 8]
    var total = 0
    var index = 0
    while index < 200 {
        let mapped = values.map(fn(value) => value * 2)
        let filtered = mapped.filter(fn(value) => value % 3 == 0)
        total += filtered.fold(0, fn(acc, value) => acc + value)
        index += 1
    }
    let _ = total
}
