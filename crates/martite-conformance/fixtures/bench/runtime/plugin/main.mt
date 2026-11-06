struct State { calls: int, text: str }

on init() -> State { State { calls: 0, text: "" } }

on saved(mut self, item: str) -> Result[(), str] {
    self.calls += 1
    self.text = item
    Ok(())
}

on tick(mut self) {
    self.calls += 1
}
