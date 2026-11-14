fn main() {
    let scale = 3
    let offset = 1
    let values = [1, 2, 3, 4]
    let mapped = values.map(fn(value) => value * scale + offset)
    let total = mapped.fold(0, fn(acc, value) => acc + value)
    print("{mapped}:{total}")
}
