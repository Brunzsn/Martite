use martite_base::{Code, LoadLimits};
use martite_syntax::{parse_interface, parse_source};

fn parse_ok(source: &str) -> bool {
    let parsed = parse_source(source, LoadLimits::default());
    assert!(
        parsed.diagnostics.is_empty(),
        "expected a parse, got {:?}\n{source}",
        parsed.diagnostics
    );
    true
}

fn parse_error(source: &str, expected: Option<Code>) {
    let parsed = parse_source(source, LoadLimits::default());
    assert!(
        !parsed.diagnostics.is_empty(),
        "expected a parse error\n{source}"
    );
    if let Some(code) = expected {
        assert!(
            parsed.diagnostics.iter().any(|d| d.code == code),
            "expected {code}, got {:?}",
            parsed.diagnostics
        );
    }
}

#[test]
fn newlines_are_significant_but_ignored_after_operators_and_brackets() {
    parse_ok("fn main() {\n    let x = 1\n    let y = 2\n}\n");
    parse_error("fn main() { let x = 1 let y = 2 }\n", Some(Code::E003));
    parse_ok(
        "fn main() {\n    let x = 1 +\n        2\n    let y = (1\n        + 2)\n    let z = [1,\n        2]\n}\n",
    );
    parse_ok("fn main() {\n    let value = Some(1)\n        .unwrap_or(0)\n}\n");
}

#[test]
fn a_bare_brace_in_expression_position_is_a_map_literal() {
    parse_ok("fn main() {\n    let table = {\"a\": 1}\n}\n");
    parse_ok("fn choose(x: int) -> int {\n    match x {\n        _ => { 1 }\n    }\n}\n");
    parse_ok("fn choose(x: int) -> int {\n    match x {\n        _ => ({\"a\": 1}),\n    }\n}\n");
    parse_error(
        "fn choose(x: int) -> int {\n    match x {\n        _ => {\"a\": 1}\n    }\n}\n",
        Some(Code::E003),
    );
}

#[test]
fn struct_literals_are_restricted_in_control_flow_heads() {
    let pred = "struct Point { x: int }\n";
    parse_error(
        &format!("{pred}fn main() {{ if Point {{ x: 1 }}.x == 1 {{ print(\"x\") }} }}\n"),
        Some(Code::E003),
    );
    parse_ok(&format!(
        "{pred}fn main() {{ if (Point {{ x: 1 }}).x == 1 {{ print(\"x\") }} }}\n"
    ));
    parse_ok(&format!(
        "{pred}fn main() {{ while (Point {{ x: 1 }}).x == 0 {{ break }} }}\n"
    ));
    parse_ok(&format!(
        "{pred}fn main() {{ for value in [(Point {{ x: 1 }})] {{ print(value.x) }} }}\n"
    ));
    parse_ok(&format!(
        "{pred}fn main() {{ match (Point {{ x: 1 }}).x {{ _ => print(\"x\") }} }}\n"
    ));
}

#[test]
fn comparison_and_range_are_non_associative() {
    parse_ok("fn main(a: int, b: int, c: int) { let x = a < b }\n");
    parse_error(
        "fn main(a: int, b: int, c: int) { let x = a < b < c }\n",
        Some(Code::E003),
    );
    parse_error(
        "fn main(a: int, b: int, c: int) { let x = a..b..c }\n",
        Some(Code::E003),
    );
    parse_ok("fn main(a: int, b: int) { let x = a..b }\n");
    parse_ok("fn main(a: int, b: int) { let x = a..=b }\n");
}

#[test]
fn trailing_commas_are_allowed_in_every_list_form() {
    parse_ok(
        r#"
        struct Pair[A, B,] { first: A, second: B, }
        enum Choice[A,] { One(A,), Two, }
        fn call[A, B,](a: A, b: B,) -> (A, B,) { (a, b,) }
        fn main() {
            let items = [1, 2,]
            let table = {"a": 1, "b": 2,}
            let tuple = (1, 2,)
            let pair = Pair { first: 1, second: 2, }
            let one = (1,)
            let value = call(1, 2,)
            match value.0 {
                1 => print("one"),
                _ => print("other"),
            }
        }
        "#,
    );
    parse_ok(
        r#"
        fn main() {
            let pair = (1, 2)
            match pair {
                (1, 2,) => print("yes"),
                _ => print("no"),
            }
        }
        "#,
    );
}

#[test]
fn comments_and_doc_comments_do_not_change_the_parse() {
    parse_ok(
        r#"
        // file comment
        /// docs for f
        fn f(
            // parameter
            x: int, // trailing
        ) -> int {
            // before let
            let y = x + // after operator
                1
            y // trailing result
        }

        fn main() { print(f(1)) }
        "#,
    );
}

#[test]
fn interface_mode_accepts_signatures_and_opaque_handles() {
    let parsed = parse_interface(
        "struct Doc\nstruct Range { start: int, end: int }\nconst LIMIT: int = 10\nfn text(doc: Doc) -> str\non saved(doc: Doc) -> Result[(), str]\n",
        LoadLimits::default(),
    );
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);

    let parsed = parse_interface("impl Doc {}\n", LoadLimits::default());
    assert!(parsed.diagnostics.iter().any(|d| d.code == Code::E003));

    let parsed = parse_interface(
        "fn text(doc: Doc) -> str { doc.len() }\n",
        LoadLimits::default(),
    );
    assert!(parsed.diagnostics.iter().any(|d| d.code == Code::E003));
}

#[test]
fn negative_literals_are_part_of_patterns_but_unary_in_expressions() {
    parse_ok(
        "fn main(n: int) {\n    match n {\n        -1 => print(\"minus one\"),\n        _ => print(\"other\"),\n    }\n}\n",
    );
    parse_ok("fn main() { let x = -1\nprint(x) }\n");
}

#[test]
fn match_arm_comma_rules_are_enforced() {
    parse_ok(
        "fn main(x: int) {\n    match x {\n        1 => print(\"one\"),\n        2 => { print(\"two\") }\n        _ => { print(\"other\") },\n    }\n}\n",
    );
    parse_error(
        "fn main(x: int) {\n    match x {\n        1 => print(\"one\")\n        2 => print(\"two\"),\n    }\n}\n",
        Some(Code::E003),
    );
}

#[test]
fn string_interpolation_is_parsed_as_a_nested_expression() {
    parse_ok("fn main() { let name = \"world\"\nprint(\"hello {name}\") }\n");
    parse_ok("fn main() { print(\"{1 + 2}\") }\n");
    parse_error("fn main() { print(\"{1 + }\") }\n", Some(Code::E003));
}
