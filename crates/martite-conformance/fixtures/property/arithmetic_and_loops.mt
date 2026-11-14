fn main() {
    var index = 0
    var total = 0
    while index < 6 {
        index += 1
        total += index * 2
    }
    for value in [1, 2, 3] {
        total += value
    }
    print("{total}")
}
