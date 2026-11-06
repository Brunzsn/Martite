struct Counter { value: int }

impl Counter {
    fn new() -> Counter { Counter { value: 0 } }
    fn bump(mut self, amount: int) { self.value += amount }
    fn get(self) -> int { self.value }
}

fn main() {
    var counter = Counter.new()
    var index = 0
    while index < 500 {
        counter.bump(1)
        index += 1
    }
    let _ = counter.get()
}
