fn main() {
    var table = {"a": 1, "b": 2, "c": 3, "key": 0}
    var index = 0
    while index < 100 {
        table["key"] = index
        let value = table.get("key")
        let present = table.has("a")
        let _ = value
        let _ = present
        index += 1
    }
    let keys = table.keys()
    let values = table.values()
    let entries = table.entries()
    let _ = keys.len() + values.len() + entries.len()
}
