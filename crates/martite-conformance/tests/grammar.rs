use martite_base::{Code, LoadLimits};
use martite_syntax::{Item, StmtKind, parse_interface, parse_source};

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

#[test]
fn minus_equals_and_arrow_are_distinct_tokens() {
    parse_ok("fn main() {\n    var x = 5\n    x -= 1\n}\n");
    parse_ok("fn f() -> int { 1 }\n");
}

#[test]
fn qualified_unit_patterns_and_pattern_continuations_parse() {
    parse_ok(
        "enum Color { Red, Green }\nfn main(c: Color) {\n    match c {\n        Color.Red => print(\"red\"),\n        Color.Green => print(\"green\"),\n    }\n    let Red |\n        Green = c\n    let Color\n        .Red = c\n}\n",
    );
}

#[test]
fn struct_pattern_rest_allows_a_trailing_comma() {
    parse_ok(
        "struct P { x: int, y: int }\nfn main(p: P) {\n    match p {\n        P { x, .., } => print(\"{x}\"),\n        P { .., } => print(\"p\"),\n    }\n}\n",
    );
}

#[test]
fn statement_errors_recover_at_statement_boundaries() {
    let source = "fn main() {\n    let = 1\n    let = 2\n    let ok = 3\n    print(\"after\")\n    return ok\n}\nfn after() {}\n";
    let parsed = parse_source(source, LoadLimits::default());
    let e003 = parsed
        .diagnostics
        .iter()
        .filter(|d| d.code == Code::E003)
        .count();
    assert_eq!(e003, 2, "{:?}", parsed.diagnostics);
    assert_eq!(parsed.program.items.len(), 2, "{:?}", parsed.program.items);

    let Item::Fn(main) = &parsed.program.items[0] else {
        panic!("expected main");
    };
    let body = main.body.as_ref().expect("function body");
    assert_eq!(body.stmts.len(), 3, "{:?}", body.stmts);
    assert!(matches!(body.stmts[0].kind, StmtKind::Let { .. }));
    assert!(matches!(
        body.stmts[1].kind,
        StmtKind::Expr { assign: None, .. }
    ));
    assert!(matches!(body.stmts[2].kind, StmtKind::Return(Some(_))));
    assert!(matches!(
        &parsed.program.items[1],
        Item::Fn(function) if function.name == "after"
    ));
}

#[test]
fn malformed_items_and_unmatched_openers_still_terminate() {
    let parsed = parse_source(
        "fn broken( {\n    let x = 1\n}\nfn after() {}\n",
        LoadLimits::default(),
    );
    assert!(parsed.diagnostics.iter().any(|d| d.code == Code::E003));
    assert_eq!(parsed.program.items.len(), 1, "{:?}", parsed.program.items);
    assert!(matches!(
        &parsed.program.items[0],
        Item::Fn(function) if function.name == "after"
    ));

    for source in [
        "fn main() {",
        "fn main() {\n    let x = [1, 2\n",
        "fn main() {\n    let x = (1 + 2\n",
        "fn main() {\n    let x = {1: 2\n",
    ] {
        let parsed = parse_source(source, LoadLimits::default());
        assert!(
            parsed.diagnostics.iter().any(|d| d.code == Code::E003),
            "{source:?}: {:?}",
            parsed.diagnostics
        );
    }
}

#[test]
fn malformed_interface_lines_keep_later_signatures() {
    let source = "struct Doc\nfn text(doc: Doc) -> str\nlet broken =\nfn other() -> int\n";
    let parsed = parse_interface(source, LoadLimits::default());
    assert!(parsed.diagnostics.iter().any(|d| d.code == Code::E003));
    assert_eq!(parsed.program.items.len(), 3, "{:?}", parsed.program.items);
    assert!(matches!(
        &parsed.program.items[1],
        Item::Fn(function) if function.name == "text"
    ));
    assert!(matches!(
        &parsed.program.items[2],
        Item::Fn(function) if function.name == "other"
    ));
}
