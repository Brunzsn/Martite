fn main() {
    var values = [3, 1, 2]
    values.sort()
    values.push(4)
    let _ = values.pop()
    let doubled = values.map(fn(value) => value * 2)
    let evens = doubled.filter(fn(value) => value % 2 == 0)
    let total = evens.fold(0, fn(acc, value) => acc + value)
    let table = {"a": 1, "b": 2}
    let _ = table.get("a")
    print("{total}:{values.len()}")
}
