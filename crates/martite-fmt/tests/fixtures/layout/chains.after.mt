fn main() {
    let values = [1, 2, 3]
    let text = editor.text(doc)
        .split(" ")
        .filter(fn(w) => w != "")
        .map(fn(w) => w.trim())
        .filter(fn(w) => w.len() > 0)
        .len()
    values
}

fn logic() -> bool {
    return first_condition_name and second_condition_name
        or third_condition_name and fourth_condition_name
}

fn coalesce() -> int {
    return first_value_name ?? second_value_name ?? third_value_name ?? fourth_value_name
}
