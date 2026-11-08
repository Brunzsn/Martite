fn main() {
    let values = [
        5, 8, 13, 21, 34, 55, 89, 144,
        233, 377, 610, 987, 1597, 2584, 4181, 6765,
    ]
    var checksum = 0
    var round = 0
    while round < 200 {
        var index = 0
        while index <= values.len() {
            match values.get(index) {
                Some(value) => {
                    if value % 3 == 0 {
                        checksum += value
                    }
                }
                None => {
                    checksum = checksum - 1
                }
            }
            index += 1
        }
        round += 1
    }
    let _ = checksum
}
