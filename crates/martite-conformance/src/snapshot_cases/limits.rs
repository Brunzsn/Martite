use crate::snapshot::SnapshotCase;
use crate::snapshot_cases::{default_caps, main_case};

const FUEL_CASES: &[(&str, &str)] = &[
    ("empty-main", "fn main() {}"),
    ("one-binding", "fn main() { let x = 1 }"),
    (
        "while-loop",
        "fn main() { var i = 0\nwhile i < 3 { i += 1 } }",
    ),
    ("list-literal-slots", "fn main() { let xs = [1, 2, 3] }"),
    (
        "list-equality-slots",
        "fn main() { let a = [1, 2, 3]\nlet b = [1, 2, 3]\nlet same = a == b\nprint(\"{same}\") }",
    ),
    (
        "list-show-slots",
        "fn main() { let xs = [1, 2, 3]\nprint(\"{xs}\") }",
    ),
    (
        "map-literal-slots",
        "fn main() { let table = {\"a\": 1, \"b\": 2}\nprint(\"{table}\") }",
    ),
    (
        "string-concatenation",
        "fn main() { let text = \"ab\" + \"cd\"\nprint(text) }",
    ),
    (
        "sorting",
        "fn main() { let xs = [3, 1, 2]\nlet ys = xs.sorted()\nprint(\"{ys}\") }",
    ),
    ("print-bytes", "fn main() { print(\"abcdefgh\") }"),
    (
        "string-slicing",
        "fn main() { let text = \"abcdefgh\"\nlet part = text[0..4]\nprint(part) }",
    ),
    ("tuple-literal-slots", "fn main() { let pair = (1, 2, 3) }"),
    (
        "tuple-equality-slots",
        "fn main() { let a = (1, 2, 3)\nlet b = (1, 2, 3)\nlet same = a == b\nprint(\"{same}\") }",
    ),
    (
        "map-equality-slots",
        "fn main() { let a = {\"x\": 1, \"y\": 2}\nlet b = {\"x\": 1, \"y\": 2}\nlet same = a == b\nprint(\"{same}\") }",
    ),
    (
        "list-map-slots",
        "fn main() { let ys = [1, 2, 3].map(fn(x) => x + 1)\nprint(\"{ys}\") }",
    ),
    (
        "list-extend-slots",
        "fn main() { var xs = [1, 2]\nxs.extend([3, 4])\nprint(\"{xs}\") }",
    ),
    (
        "sorting-two-items",
        "fn main() { let xs = [2, 1]\nlet ys = xs.sorted() }",
    ),
    (
        "sorting-four-items",
        "fn main() { let xs = [4, 3, 2, 1]\nlet ys = xs.sorted() }",
    ),
    (
        "sorting-eight-items",
        "fn main() { let xs = [8, 7, 6, 5, 4, 3, 2, 1]\nlet ys = xs.sorted() }",
    ),
    (
        "nested-show-slots",
        "fn main() { let xs = [[1, 2], [3]]\nprint(\"{xs}\") }",
    ),
    (
        "join-slots-and-bytes",
        "fn main() { let xs = [\"ab\", \"cd\"]\nprint(xs.join(\",\")) }",
    ),
    (
        "unique-push-has-no-copy",
        "fn main() { var xs = [1, 2, 3]\nxs.push(4) }",
    ),
    (
        "shared-push-copies-the-list",
        "fn main() { var xs = [1, 2, 3]\nlet keep = xs\nxs.push(4) }",
    ),
    (
        "unique-insert-shifts-only",
        "fn main() { var xs = [1, 2, 3, 4]\nxs.insert(1, 9) }",
    ),
    (
        "shared-insert-copies-and-shifts",
        "fn main() { var xs = [1, 2, 3, 4]\nlet keep = xs\nxs.insert(1, 9) }",
    ),
    (
        "unique-remove-shifts-only",
        "fn main() { var xs = [1, 2, 3, 4]\nlet _removed = xs.remove_at(1) }",
    ),
    (
        "shared-remove-copies-and-shifts",
        "fn main() { var xs = [1, 2, 3, 4]\nlet keep = xs\nlet _removed = xs.remove_at(1) }",
    ),
    (
        "unique-nested-assignment-has-no-copy",
        "struct Box { xs: list[int] }\nfn main() { var box = Box { xs: [1, 2] }\nbox.xs[0] = 9 }",
    ),
    (
        "shared-nested-assignment-copies-parents",
        "struct Box { xs: list[int] }\nfn main() { var box = Box { xs: [1, 2] }\nlet keep = box\nbox.xs[0] = 9 }",
    ),
    (
        "nested-equality-slots",
        "fn main() { let a = [[1, 2], [3]]\nlet b = [[1, 2], [3]]\nlet same = a == b\nprint(\"{same}\") }",
    ),
    (
        "repeated-string-literal-bytes",
        "fn main() { var i = 0\nwhile i < 3 { let text = \"abcd\"\ni += 1 } }",
    ),
];

const MEMORY_CASES: &[(&str, &str)] = &[
    ("string", "fn main() { let text = \"abcd\" }"),
    ("list", "fn main() { let xs = [1, 2, 3] }"),
    ("map", "fn main() { let table = {\"a\": 1, \"b\": 2} }"),
    (
        "arguments-and-locals",
        "fn use_it(x: int) -> int {\n    let values = [x, x]\n    values.len()\n}\nfn main() { print(use_it(1)) }",
    ),
    (
        "copy-on-write",
        "fn use_it(x: int) -> int {\n    var values = [x, x]\n    var copy = values\n    copy.push(x)\n    values.len() + copy.len()\n}\nfn main() { print(use_it(1)) }",
    ),
];

pub fn cases() -> Vec<SnapshotCase> {
    let caps = default_caps();
    let mut cases = Vec::new();
    for (name, source) in FUEL_CASES {
        cases.push(main_case(
            format!("limits/fuel/{name}"),
            *source,
            None,
            caps,
        ));
    }
    for (name, source) in MEMORY_CASES {
        cases.push(main_case(
            format!("limits/memory/{name}"),
            *source,
            None,
            caps,
        ));
    }
    cases.push(main_case(
        "limits/depth/recursive-down",
        "fn down(n: int) -> int {\n    if n == 0 { 0 } else { down(n - 1) }\n}\nfn main() { print(down(10)) }",
        None,
        caps,
    ));
    cases
}
