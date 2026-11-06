use app.editor
use app.editor.Doc

struct State { saves: int }

on init() -> State { State { saves: 0 } }

on saved(mut self, doc: Doc) -> Result[(), str] {
    let _ = editor.text(doc)
    self.saves += 1
    Ok(())
}
