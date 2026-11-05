use app.status
use app.editor

on saved(mut self, doc: Doc, kind: SaveKind) -> Result[(), str] {
  if kind == SaveKind.Auto {
        return Ok(()) }
    let words = editor.text(doc).split(" ").filter(fn(w) => w != "").map(fn(w) => w.trim()).filter(fn(w) => w.len() > 0).len()
    self.saves += 1
    status.show("{words} words after {self.saves} saves")
}
