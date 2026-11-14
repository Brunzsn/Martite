use crate::snapshot::SnapshotCase;
use crate::snapshot_cases::{default_caps, main_case, main_case_files};

const PANIC_CASES: &[(&str, &str)] = &[
    (
        "list-bounds",
        "fn main() { let xs = [1]\nprint(\"{xs[3]}\") }",
    ),
    (
        "string-slice",
        "fn main() { let text = \"abc\"\nprint(text[1..9]) }",
    ),
    ("divide-by-zero", "fn main() { let x = 1 / 0\nprint(x) }"),
    ("modulo-by-zero", "fn main() { let x = 1 % 0\nprint(x) }"),
    (
        "add-overflow",
        "fn main() { let x = 9223372036854775807 + 1\nprint(x) }",
    ),
    (
        "sub-overflow",
        "fn main() { let x = -9223372036854775807 - 2\nprint(x) }",
    ),
    (
        "negate-overflow",
        "fn main() { let min = -9223372036854775807 - 1\nprint(-min) }",
    ),
    (
        "abs-overflow",
        "fn main() { let min = -9223372036854775807 - 1\nprint(min.abs()) }",
    ),
    (
        "divide-overflow",
        "fn main() { let min = -9223372036854775807 - 1\nprint(min / -1) }",
    ),
    ("pow-overflow", "fn main() { print(2.pow(63)) }"),
    ("pow-negative", "fn main() { print(2.pow(-1)) }"),
    ("float-to-int-nan", "fn main() { print(int(0.0 / 0.0)) }"),
    ("float-to-int-range", "fn main() { print(int(1e300)) }"),
    ("round-negative", "fn main() { print(1.5.round(-1)) }"),
    (
        "replace-empty",
        "fn main() { print(\"abc\".replace(\"\", \"x\")) }",
    ),
    (
        "option-expect",
        "fn main() { let value: int? = None\nprint(value.expect(\"none\")) }",
    ),
    (
        "result-expect",
        "fn main() { let value: Result[int, str] = Err(\"bad\")\nprint(value.expect(\"result\")) }",
    ),
    ("assert", "fn main() { assert(false, \"boom\") }"),
    ("panic", "fn main() { panic(\"explode\") }"),
    (
        "map-key",
        "fn main() { let table = {\"a\": 1}\nlet value = table[\"b\"]\nprint(value) }",
    ),
    (
        "insert-bounds",
        "fn main() { var xs = [1]\nxs.insert(5, 2) }",
    ),
    (
        "remove-bounds",
        "fn main() { var xs = [1]\nlet _ = xs.remove_at(4) }",
    ),
];

pub fn cases() -> Vec<SnapshotCase> {
    let caps = default_caps();
    let mut cases = Vec::new();
    for (name, source) in PANIC_CASES {
        cases.push(main_case(
            format!("traps/panic/{name}"),
            *source,
            None,
            caps,
        ));
    }
    cases.push(main_case(
        "traps/panic/call-trace",
        "fn inner() {\n    panic(\"boom\")\n}\nfn outer() {\n    inner()\n}\nfn main() {\n    outer()\n}\n",
        None,
        caps,
    ));
    // The lambda is defined in `util.mt` and called from `main.mt`; a trap
    // inside it names the defining module (#118).
    cases.push(main_case_files(
        "traps/panic/lambda-across-modules",
        vec![
            (
                "main.mt".into(),
                "use util\nfn main() {\n    let fail = util.make()\n    let _ = fail()\n}\n".into(),
            ),
            (
                "util.mt".into(),
                "pub fn make() -> fn() -> int {\n    fn() { panic(\"boom\") }\n}\n".into(),
            ),
        ],
        "main.mt",
        None,
        caps,
    ));
    cases
}
