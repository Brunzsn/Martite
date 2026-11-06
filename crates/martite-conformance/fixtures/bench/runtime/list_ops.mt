fn main() {
    var values: list[int] = []
    var index = 0
    while index < 200 {
        values.push(index)
        index += 1
    }
    values.sort()
    let doubled = values.map(fn(value) => value * 2)
    let evens = doubled.filter(fn(value) => value % 2 == 0)
    let total = evens.fold(0, fn(acc, value) => acc + value)
    let _ = total
}
