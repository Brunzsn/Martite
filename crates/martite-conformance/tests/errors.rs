use std::collections::HashSet;

use martite_base::{Code, LoadLimits};
use martite_conformance::{Case, assert_clean, assert_diagnostic};

struct ErrorRow {
    code: Code,
    fail: &'static str,
    pass: &'static str,
    span: Option<&'static str>,
    message: &'static str,
}

const ROWS: &[ErrorRow] = &[
    ErrorRow {
        code: Code::E001,
        fail: r#"fn main() { let x = "\q" }"#,
        pass: r#"fn main() { let x = "\\q" }"#,
        span: Some("\\"),
        message: "escape",
    },
    ErrorRow {
        code: Code::E003,
        fail: "fn main( { }",
        pass: "fn main() { }",
        span: Some("{"),
        message: "expected",
    },
    ErrorRow {
        code: Code::E101,
        fail: "fn main() { missing() }",
        pass: "fn main() { print(\"ok\") }",
        span: Some("missing"),
        message: "missing",
    },
    ErrorRow {
        code: Code::E102,
        fail: "fn main() {}\nfn main() {}\n",
        pass: "fn main() {}\nfn other() {}\n",
        span: Some("fn main() {}"),
        message: "duplicate",
    },
    ErrorRow {
        code: Code::E103,
        fail: "fn main() { let x = 1\nlet x = 2 }",
        pass: "fn main() { let x = 1\nlet y = 2\nprint(\"{x}{y}\") }",
        span: Some("x"),
        message: "shadow",
    },
    ErrorRow {
        code: Code::E104,
        fail: "fn main() { let fooBar = 1 }",
        pass: "fn main() { let foo_bar = 1\nprint(foo_bar) }",
        span: Some("fooBar"),
        message: "snake",
    },
    ErrorRow {
        code: Code::E105,
        fail: "const A: int = B\nconst B: int = A\nfn main() {}",
        pass: "const MAX: int = 1\nconst MIN: int = MAX + 1\nfn main() { print(MIN) }",
        span: Some("const A: int = B"),
        message: "cycle",
    },
    ErrorRow {
        code: Code::E106,
        fail: "use nowhere\nfn main() {}",
        pass: "fn main() {}",
        span: Some("use nowhere"),
        message: "unknown module",
    },
    ErrorRow {
        code: Code::E201,
        fail: r#"fn main() { let x: int = "no" }"#,
        pass: "fn main() { let x: int = 1\nprint(x) }",
        span: Some("\"no\""),
        message: "expected",
    },
    ErrorRow {
        code: Code::E202,
        fail: "fn main() { let x = [] }",
        pass: "fn main() { let x: list[int] = []\nprint(x) }",
        span: Some(""),
        message: "infer",
    },
    ErrorRow {
        code: Code::E203,
        fail: r#"fn main() { let x = 1 + "a" }"#,
        pass: "fn main() { let x = 1 + 2\nprint(x) }",
        span: Some("1 + \"a\""),
        message: "operator",
    },
    ErrorRow {
        code: Code::E204,
        fail: "fn f(x: int) {}\nfn main() { f() }",
        pass: "fn f(x: int) {}\nfn main() { f(1) }",
        span: Some("f()"),
        message: "argument",
    },
    ErrorRow {
        code: Code::E205,
        fail: "fn main() { let m = {1.0: 2} }",
        pass: "fn main() { let m = {\"a\": 2}\nprint(m) }",
        span: Some("1.0"),
        message: "map key",
    },
    ErrorRow {
        code: Code::E206,
        fail: "fn main() { 1 }",
        pass: "fn main() { let _ = 1 }",
        span: Some("1"),
        message: "unit",
    },
    ErrorRow {
        code: Code::E207,
        fail: "fn main() { let x: int? = Some(1)\nlet y = x? }",
        pass: "fn try_it(x: int?) -> int? {\n    let y = x?\n    Some(y)\n}\nfn main() { print(try_it(None) ?? 0) }",
        span: Some("x?"),
        message: "Option",
    },
    ErrorRow {
        code: Code::E208,
        fail: "fn main() { let x = 1\nx.nope() }",
        pass: "fn main() { let x = 1\nprint(x) }",
        span: Some("x"),
        message: "method",
    },
    ErrorRow {
        code: Code::E209,
        fail: "fn main() { let x: list[int, str] = [] }",
        pass: "fn main() { let x: list[int] = []\nprint(x) }",
        span: Some("list[int, str]"),
        message: "type argument",
    },
    ErrorRow {
        code: Code::E301,
        fail: "fn main() { match true { true => print(\"x\") } }",
        pass: "fn main() { match true { true => print(\"x\"), false => print(\"y\") } }",
        span: Some("match true { true => print(\"x\") }"),
        message: "exhaustive",
    },
    ErrorRow {
        code: Code::E302,
        fail: "fn main() { let Some(x) = Some(1) }",
        pass: "fn main() { match Some(1) { Some(x) => print(\"{x}\"), None => print(\"none\") } }",
        span: Some("Some(x)"),
        message: "refutable",
    },
    ErrorRow {
        code: Code::E303,
        fail: "fn main() { match 1 { 1 | x => print(\"x\"), _ => print(\"y\") } }",
        pass: "fn main() { match 1 { 1 => print(\"x\"), _ => print(\"y\") } }",
        span: Some("x"),
        message: "bind",
    },
    ErrorRow {
        code: Code::E304,
        fail: "fn main() { match 1 { true => print(\"x\"), _ => print(\"y\") } }",
        pass: "fn main() { match 1 { 1 => print(\"x\"), _ => print(\"y\") } }",
        span: Some("true"),
        message: "pattern",
    },
    ErrorRow {
        code: Code::E401,
        fail: "fn main() { let x = 1\nx = 2 }",
        pass: "fn main() { var x = 1\nx = 2\nprint(x) }",
        span: Some("x"),
        message: "assign",
    },
    ErrorRow {
        code: Code::E402,
        fail: "fn f(mut x: int) {}\nfn main() { let x = 1\nf(x) }",
        pass: "fn f(mut x: int) {}\nfn main() { var x = 1\nf(mut x) }",
        span: Some("x"),
        message: "mut",
    },
    ErrorRow {
        code: Code::E403,
        fail: "fn f(mut a: int, b: int) {}\nfn main() { var x = 1\nf(mut x, x) }",
        pass: "fn f(mut a: int, b: int) {}\nfn main() { var x = 1\nvar y = 2\nf(mut x, y) }",
        span: Some("f(mut x, x)"),
        message: "same variable",
    },
    ErrorRow {
        code: Code::E404,
        fail: "fn main() { var x = 1\nlet f = fn() { x = 2 }\nlet _ = f }",
        pass: "fn main() { let x = 1\nlet f = fn() { x + 1 }\nprint(f()) }",
        span: Some("x"),
        message: "captured",
    },
    ErrorRow {
        code: Code::E405,
        fail: "fn main() { break }",
        pass: "fn main() { while false { break } }",
        span: Some("break"),
        message: "loop",
    },
    ErrorRow {
        code: Code::E406,
        fail: "const XX: int = 1 / 0\nfn main() {}",
        pass: "const XX: int = 1 + 0\nfn main() { print(XX) }",
        span: Some("1 / 0"),
        message: "constant",
    },
    ErrorRow {
        code: Code::E407,
        fail: "fn main(x: int) {}",
        pass: "fn main() {}",
        span: Some("fn main(x: int) {}"),
        message: "main",
    },
];

#[test]
fn load_limit_variants_are_e002() {
    let mut nested_source = String::from("fn main() { let x = ");
    for _ in 0..200 {
        nested_source.push('(');
    }
    nested_source.push('1');
    for _ in 0..200 {
        nested_source.push(')');
    }
    nested_source.push_str(" }");
    let nested = Case::single(&nested_source).limits(LoadLimits {
        file_size: 1 << 20,
        nesting_depth: 32,
        file_count: 256,
    });
    assert_diagnostic(&nested, Code::E002, None, Some("nesting"), None);

    let oversized =
        Case::single("fn main() { print(\"a long enough source\") }").limits(LoadLimits {
            file_size: 8,
            nesting_depth: 128,
            file_count: 256,
        });
    assert_diagnostic(&oversized, Code::E002, None, Some("exceeds"), None);

    let too_many = Case::single("use a\nfn main() { a.work() }")
        .file("a.mt", "pub fn work() {}\n")
        .limits(LoadLimits {
            file_size: 1 << 20,
            nesting_depth: 128,
            file_count: 1,
        });
    assert_diagnostic(&too_many, Code::E002, None, Some("too many"), None);
    assert_clean(&Case::single("fn main() {}"));
}

#[test]
fn every_e_code_is_covered_by_a_fixture() {
    let covered: HashSet<Code> = ROWS.iter().map(|row| row.code).collect();
    let all = [
        Code::E001,
        Code::E002,
        Code::E003,
        Code::E101,
        Code::E102,
        Code::E103,
        Code::E104,
        Code::E105,
        Code::E106,
        Code::E201,
        Code::E202,
        Code::E203,
        Code::E204,
        Code::E205,
        Code::E206,
        Code::E207,
        Code::E208,
        Code::E209,
        Code::E301,
        Code::E302,
        Code::E303,
        Code::E304,
        Code::E401,
        Code::E402,
        Code::E403,
        Code::E404,
        Code::E405,
        Code::E406,
        Code::E407,
        Code::E501,
        Code::E502,
        Code::E503,
        Code::E504,
        Code::E505,
        Code::E506,
        Code::E507,
        Code::E508,
        Code::E509,
    ];
    for code in all {
        // E002 and E501-E509 need dedicated multi-file/manifest/host fixtures.
        let dedicated = code == Code::E002 || (Code::E501..=Code::E509).contains(&code);
        assert!(
            covered.contains(&code) || dedicated,
            "{code} has no conformance fixture"
        );
    }
}

#[test]
fn every_single_file_compile_error_has_a_pass_and_fail_fixture() {
    let mut failures = Vec::new();
    for row in ROWS {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_diagnostic(
                &Case::single(row.fail),
                row.code,
                row.span,
                Some(row.message),
                None,
            );
            assert_clean(&Case::single(row.pass));
        }));
        if let Err(payload) = result {
            let message = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown panic".into());
            failures.push(format!("{}: {message}", row.code));
        }
    }
    assert!(
        failures.is_empty(),
        "compile-error fixtures failed:\n{}",
        failures.join("\n\n")
    );
}
