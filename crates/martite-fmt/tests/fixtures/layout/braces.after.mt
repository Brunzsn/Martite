struct P { x: int, y: int }

struct Empty {}

fn main() {
    let p = P { x: 1, y: 2 }
    let e = Empty {}
    let m = {"a": 1, "b": 2}
    let t = (1,)
    let u = P { ..p, x: 3 }
}
