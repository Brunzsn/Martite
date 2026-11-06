struct Point { x: int, y: int }

impl Point {
    fn new(x: int, y: int) -> Point { Point { x, y } }
    fn manhattan(self, other: Point) -> int {
        (self.x - other.x).abs() + (self.y - other.y).abs()
    }
}

enum Shape { Circle(float), Rectangle(float, float) }

fn area(shape: Shape) -> float {
    match shape {
        Circle(radius) => 3.14159 * radius * radius,
        Rectangle(width, height) => width * height,
    }
}

fn main() {
    let origin = Point.new(0, 0)
    let point = Point.new(3, 4)
    let distance = origin.manhattan(point)
    let shapes = [Shape.Circle(1.0), Shape.Rectangle(2.0, 3.0)]
    var total = 0.0
    for shape in shapes {
        total += area(shape)
    }
    print("distance = {distance}, area = {total}")
}
