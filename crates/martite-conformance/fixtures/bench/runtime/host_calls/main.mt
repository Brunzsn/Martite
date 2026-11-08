use app.bench

on run(count: int, seed: int) -> int {
    let token = bench.mint(seed)
    var total = 0
    var index = 0
    while index < count {
        total = bench.mix(token, total + index)
        index += 1
    }
    total
}
