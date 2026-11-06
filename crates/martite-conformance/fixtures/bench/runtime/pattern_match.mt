enum Token { Number(int), Word(str), End }

fn score(token: Token) -> int {
    match token {
        Number(value) => value,
        Word(text) => text.len(),
        End => 0,
    }
}

fn main() {
    let tokens = [Token.Number(1), Token.Word("ab"), Token.End]
    var total = 0
    var index = 0
    while index < 500 {
        for token in tokens {
            total += score(token)
        }
        index += 1
    }
    let _ = total
}
