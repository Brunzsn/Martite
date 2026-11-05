fn add(a: int, b: int) -> int { a + b }

test "adds numbers" { assert(add(2, 3) == 5, "2 + 3 should be 5") }

test "lists sort" {
    var xs = [3, 1, 2]
    xs.sort()
    assert(xs == [1, 2, 3], "list was not sorted")
}

fn main() { print("{add(20, 22)}") }
