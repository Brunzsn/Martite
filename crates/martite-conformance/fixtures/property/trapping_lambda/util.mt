pub fn make() -> fn() -> int {
    fn() { panic("boom") }
}
