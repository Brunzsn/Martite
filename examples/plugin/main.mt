use app.editor
use app.editor.Doc

struct Stats { saves: int, last_len: int }

on init() -> Stats {
    Stats { saves: 0, last_len: 0 }
}

on saved(mut self, doc: Doc) -> Result[(), str] {
    let words = editor.text(doc).len()
    self.saves += 1
    self.last_len = words
    Ok(())
}
