struct S { a: int, b: int }

fn main() {
    let xs = [
        // first
        1, // one
        // second
        2, // two
    ]
    let m = {
        // key a
        "a": 1, // value a
        "b": 2,
    }
    let s = S { // fields
        a: 1, // a
        b: 2,
    }
    let p = Pair {
        // base
        ..s,
        a: 3,
    }
    match xs {
        // first arm
        1 => print("one"), // one
        // second arm
        _ => { print("other") } // other
    }
}
