fn main() {
    let name = "Martite"
    let count = 3
    let escaped = "quote: \" and slash: \\ and newline: \n"
    let joined = "hello {name}, you have {count} items"
    let nested = "sum = {1 + 2 * 3}, bool = {count > 1}"
    print(escaped)
    print(joined)
    print(nested)
}
