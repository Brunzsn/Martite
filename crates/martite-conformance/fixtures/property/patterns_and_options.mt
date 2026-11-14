enum Token { Number(int), Word(str), Empty }

fn score(token: Token) -> int {
    match token {
        Number(value) if value > 0 => value,
        Number(value) => -value,
        Word(text) => text.len(),
        Empty => 0,
    }
}

fn main() {
    let tokens = [Token.Number(2), Token.Number(-1), Token.Word("ab"), Token.Empty]
    var total = 0
    for token in tokens {
        total += score(token)
    }
    let maybe: int? = Some(total)
    let value = maybe ?? 7
    let result: Result[int, str] = Ok(value)
    match result {
        Ok(number) => print("{number}"),
        Err(error) => print(error),
    }
}
