struct Pair { left: int, right: int }

impl Pair {
    fn total(self) -> int { self.left + self.right }
}

enum Shape { Circle(float), Empty }

fn main() {
    var pair = Pair { left: 1, right: 2 }
    pair.left += 3
    let updated = Pair { ..pair, right: 9 }
    let shape = Shape.Circle(2.5)
    match shape {
        Circle(radius) => print("{updated.total()}:{radius}"),
        Empty => print("empty"),
    }
}
