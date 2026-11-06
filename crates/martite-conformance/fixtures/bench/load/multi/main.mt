use app.util
use config.MAX_SIZE
use geometry.shapes
use geometry.shapes.Circle

fn main() {
    let circle: Circle = shapes.make()
    let size = util.clamp(circle.r, MAX_SIZE)
    print("{size}")
}
