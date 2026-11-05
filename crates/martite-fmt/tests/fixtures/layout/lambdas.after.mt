fn main() {
    let f = fn(x) => x + 1
    let g = fn(x) { x + 1 }
    let h = fn(x, y) {
        let z = x + y
        z
    }
    let result = items.map(fn(item) {
        let doubled = item * 2
        doubled
    })
}
