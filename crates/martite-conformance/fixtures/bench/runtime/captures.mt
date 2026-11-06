fn main() {
    let scale = 3
    let offset = 7
    let values = [1, 2, 3, 4, 5]
    var total = 0
    var index = 0
    while index < 200 {
        let mapped = values.map(fn(value) => value * scale + offset)
        total += mapped.fold(0, fn(acc, value) => acc + value)
        index += 1
    }
    let _ = total
}
