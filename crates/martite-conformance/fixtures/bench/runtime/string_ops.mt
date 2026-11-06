fn main() {
    var text = ""
    var index = 0
    while index < 100 {
        text = text + "abc,"
        index += 1
    }
    let parts = text.split(",")
    let trimmed = parts.map(fn(part) => part.trim()).filter(fn(part) => part != "")
    let joined = trimmed.join("|")
    let replaced = joined.replace("a", "A")
    let upper = replaced.to_upper()
    let _ = upper.len()
}
