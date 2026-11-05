fn main() {
    if true { print("a") } else { print("b") }
    while false { break }
    for x in [1, 2] { print("{x}") }
    match 1 {
        1 => print("one"),
        _ => { print("other") }
    }
}
