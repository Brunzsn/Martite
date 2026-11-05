use std::collections::HashSet;

use martite_base::Code;
use martite_config::ToolConfig;
use martite_conformance::{Case, LintRun, lint_case};

fn recommended() -> ToolConfig {
    ToolConfig {
        lint_preset: Some("recommended".into()),
        ..ToolConfig::default()
    }
}

fn lint_source(source: &str) -> LintRun {
    lint_case(&Case::single(source), &recommended(), None, None)
}

fn run_selected(source: &str, selected: &[Code]) -> LintRun {
    lint_case(
        &Case::single(source),
        &recommended(),
        Some(HashSet::from_iter(selected.iter().copied())),
        None,
    )
}

#[test]
fn a_suppression_above_a_function_covers_the_signature_but_not_the_body() {
    let run = lint_source("// allow(L001)\nfn unused_parameter(unused: int) {}\nfn main() {}\n");
    run.assert_not(Code::L001);
    run.assert_not(Code::L022);

    let run = lint_source(
        "// allow(L005)\nfn body() {\n    if true {\n        print(\"x\")\n    }\n}\nfn main() { body() }\n",
    );
    run.assert_has(Code::L005, None, None);
}

#[test]
fn suppression_above_a_statement_covers_the_whole_statement_head() {
    let run = lint_source(
        "fn main() {\n    // allow(L024)\n    if true and false or true {\n        print(\"x\")\n    }\n}\n",
    );
    run.assert_not(Code::L024);
    run.assert_not(Code::L022);
}

#[test]
fn blank_lines_do_not_detach_a_suppression() {
    let run = lint_source(
        "fn main() {\n    // allow(L005)\n\n    if true {\n        print(\"x\")\n    }\n}\n",
    );
    run.assert_not(Code::L005);
    run.assert_not(Code::L022);
}

#[test]
fn several_suppressions_above_one_element_combine() {
    let run = lint_source(
        "fn main() {\n    // allow(L005)\n    // allow(L016)\n    if true {\n    }\n}\n",
    );
    run.assert_not(Code::L005);
    run.assert_not(Code::L016);
    run.assert_not(Code::L022);
}

#[test]
fn a_suppression_above_a_match_arm_covers_that_arm() {
    let source = "fn choose(flag: bool) {\n    match flag {\n        true => print(\"a\"),\n        false => print(\"b\"),\n        // allow(L003)\n        _ => print(\"c\"),\n    }\n}\nfn main() { choose(true) }\n";
    let run = lint_source(source);
    run.assert_not(Code::L003);
    run.assert_not(Code::L022);
}

#[test]
fn a_suppression_above_a_public_item_is_attached_to_the_whole_item() {
    let run = lint_source("// allow(L001)\nfn dead_private() {}\nfn main() {}\n");
    run.assert_not(Code::L001);
    run.assert_not(Code::L022);
}

#[test]
fn e_codes_and_unknown_ids_cannot_suppress() {
    // An E-code name in a suppression is itself L022, and E201 is not hidden.
    let run = lint_source("// allow(E201)\nfn main() {}\n");
    run.assert_has(Code::L022, None, None);

    let e201 = Case::single("// allow(E201)\nfn main() {\n    let x: int = \"no\"\n}\n");
    martite_conformance::assert_diagnostic(&e201, Code::E201, None, Some("expected int"), None);

    let run = lint_source("// allow(L999)\nfn main() {}\n");
    run.assert_has(Code::L022, None, None);
}

#[test]
fn a_known_rule_that_is_off_or_excluded_does_not_suppress_nothing() {
    // L002 is off under `minimal`, so the comment names a known rule.
    let config = ToolConfig {
        lint_preset: Some("minimal".into()),
        ..ToolConfig::default()
    };
    let run = lint_case(
        &Case::single("fn main() {\n    // allow(L002)\n    var value = 1\n    print(value)\n}\n"),
        &config,
        None,
        None,
    );
    run.assert_not(Code::L022);

    let run = run_selected(
        "fn main() {\n    // allow(L002)\n    var value = 1\n    print(value)\n}\n",
        &[Code::L005, Code::L022],
    );
    run.assert_not(Code::L022);
}

#[test]
fn l022_and_l032_are_not_suppressible() {
    let run = lint_source("// allow(L022)\n// allow(L999)\nfn main() {}\n");
    run.assert_has(Code::L022, None, None);

    let run = lint_source(
        "// allow(L032)\nfn main() {\n    /// stray\n    let value = 1\n    print(value)\n}\n",
    );
    run.assert_has(Code::L032, None, None);
}

#[test]
fn l020_in_the_manifest_is_not_suppressed_by_a_comment() {
    let manifest = r#"
[plugin]
name = "lint-fixture"
version = "0.1.0"
lang = "1"
api = "1.0"
entry = "main.mt"

# allow(L020)
[permissions]
grant = ["app.api", "std.math"]
"#;
    let case = Case::single("use app.api\non check() {}\n")
        .interface("app.api", "on check()\n")
        .grant("app.api")
        .grant("std.math");
    let run = lint_case(&case, &recommended(), None, Some(manifest));
    run.assert_has(Code::L020, None, None);
}

#[test]
fn suppressed_diagnostics_still_count_for_l022_usage() {
    let run = lint_source(
        "fn main() {\n    // allow(L005)\n    if true {\n        print(\"x\")\n    }\n}\n",
    );
    run.assert_not(Code::L005);
    run.assert_not(Code::L022);
}

#[test]
fn a_multiline_call_head_is_covered_by_the_suppression() {
    let source = "fn check(a: bool, b: bool, c: bool) {\n    // allow(L024)\n    if a and\n        b or c {\n        print(\"x\")\n    }\n}\nfn main() { check(true, true, false) }\n";
    let run = lint_source(source);
    run.assert_not(Code::L024);
    run.assert_not(Code::L022);
}
