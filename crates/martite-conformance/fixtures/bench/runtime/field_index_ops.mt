struct Body {
    x: float,
    y: float,
    z: float,
    vx: float,
    vy: float,
    vz: float,
}

fn main() {
    var bodies = [
        Body { x: 0.0, y: 0.0, z: 0.0, vx: 0.01, vy: 0.02, vz: 0.03 },
        Body { x: 1.0, y: 2.0, z: 3.0, vx: 0.04, vy: 0.05, vz: 0.06 },
        Body { x: 2.0, y: 4.0, z: 6.0, vx: 0.07, vy: 0.08, vz: 0.09 },
        Body { x: 3.0, y: 6.0, z: 9.0, vx: 0.10, vy: 0.11, vz: 0.12 },
        Body { x: 4.0, y: 8.0, z: 12.0, vx: 0.13, vy: 0.14, vz: 0.15 },
        Body { x: 5.0, y: 10.0, z: 15.0, vx: 0.16, vy: 0.17, vz: 0.18 },
        Body { x: 6.0, y: 12.0, z: 18.0, vx: 0.19, vy: 0.20, vz: 0.21 },
        Body { x: 7.0, y: 14.0, z: 21.0, vx: 0.22, vy: 0.23, vz: 0.24 },
    ]
    var matrix: list[list[float]] = []
    var row = 0
    while row < bodies.len() {
        matrix.push([0.0, 0.0, 0.0])
        row += 1
    }
    var step = 0
    while step < 500 {
        var index = 0
        while index < bodies.len() {
            bodies[index].x += bodies[index].vx
            bodies[index].y += bodies[index].vy
            bodies[index].z += bodies[index].vz
            matrix[index][0] += bodies[index].x
            matrix[index][1] += bodies[index].y
            matrix[index][2] += bodies[index].z
            index += 1
        }
        step += 1
    }
    let _ = matrix
}
