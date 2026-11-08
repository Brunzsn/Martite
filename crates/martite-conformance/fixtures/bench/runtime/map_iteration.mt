fn main() {
    var table: map[int, int] = {}
    var index = 0
    while index < 128 {
        table[index] = index * 3
        index += 1
    }
    var total = 0
    var round = 0
    while round < 256 {
        for (_, value) in table {
            total += value
        }
        round += 1
    }
    let _ = total
}
