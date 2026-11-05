use std::path::{Path, PathBuf};

use martite_base::LoadLimits;
use martite_config::{FmtConfig, ToolConfig};
use martite_fmt::{format_source, structurally_equal};
use martite_syntax::parse_source;

use martite_conformance::{Case, lint_case};

fn format(source: &str) -> String {
    let output = format_source(source, &FmtConfig::default());
    assert!(
        output.diagnostics.is_empty(),
        "formatter diagnostics: {:?}\n{source}",
        output.diagnostics
    );
    output.text.expect("formatter text")
}

fn parse(source: &str) -> martite_syntax::Program {
    let parsed = parse_source(source, LoadLimits::default());
    assert!(
        parsed.diagnostics.is_empty(),
        "parse diagnostics: {:?}\n{source}",
        parsed.diagnostics
    );
    parsed.program
}

fn walk(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(path) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

fn format_any(source: &str, interface: bool) -> String {
    if interface {
        let output = martite_fmt::format_interface(source, &FmtConfig::default());
        assert!(
            output.diagnostics.is_empty(),
            "formatter diagnostics: {:?}\n{source}",
            output.diagnostics
        );
        output.text.expect("formatted interface")
    } else {
        format(source)
    }
}

#[test]
fn every_golden_fixture_is_idempotent_and_ast_preserving() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../martite-fmt/tests/fixtures");
    for before in walk(&root) {
        let name = before
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if !name.ends_with(".before.mt") && !name.ends_with(".before.mti") {
            continue;
        }
        let interface = name.ends_with(".mti");
        let after = before.with_file_name(name.replace(".before.", ".after."));
        let source = std::fs::read_to_string(&before).expect("read fixture");
        let expected = std::fs::read_to_string(&after).expect("read expected fixture");
        let actual = if interface {
            let output = martite_fmt::format_interface(&source, &FmtConfig::default());
            assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
            output.text.expect("formatted interface")
        } else {
            format(&source)
        };
        assert_eq!(actual, expected, "fixture {}", before.display());
        assert_eq!(
            format_any(&actual, interface),
            actual,
            "idempotence for {}",
            before.display()
        );
        if !interface {
            assert!(
                structurally_equal(&parse(&source), &parse(&actual)),
                "AST changed for {}",
                before.display()
            );
        }
    }
}

#[test]
fn repository_examples_remain_idempotent() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    for path in walk(&root) {
        let extension = path.extension().and_then(|ext| ext.to_str());
        if extension != Some("mt") && extension != Some("mti") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read example");
        let first = if extension == Some("mti") {
            martite_fmt::format_interface(&source, &FmtConfig::default())
                .text
                .expect("formatted interface")
        } else {
            format(&source)
        };
        assert_eq!(
            format_any(&first, extension == Some("mti")),
            first,
            "idempotence for {}",
            path.display()
        );
    }
}

#[test]
fn comments_survive_every_grammar_position() {
    let cases: &[(&str, &[&str])] = &[
        (
            "// top\nuse app.mod\n\n/// doc\nfn f(\n    // parameter list comment\n) {}\n",
            &["// top", "/// doc", "// parameter list comment"],
        ),
        (
            "fn main() {\n    // before let\n    let x = 1 // trailing\n}\n",
            &["// before let", "// trailing"],
        ),
        (
            "fn main() {\n    let xs = [\n        // list\n        1,\n        2, // second\n    ]\n}\n",
            &["// list", "// second"],
        ),
        (
            "fn main() {\n    let table = {\n        // key\n        \"a\": 1,\n    }\n}\n",
            &["// key"],
        ),
        (
            "struct S {\n    // field\n    x: int,\n}\nfn main() { print(S { x: 1 }) }\n",
            &["// field"],
        ),
        (
            "fn main(x: int) {\n    match x {\n        // arm\n        1 => print(\"one\"),\n        _ => print(\"other\"),\n    }\n}\n",
            &["// arm"],
        ),
        (
            "fn main() {\n    let value = 1 + // operator\n        2\n    print(value)\n}\n",
            &["// operator"],
        ),
        (
            "fn main() {\n    let f = fn(x) { // lambda\n        x + 1\n    }\n}\n",
            &["// lambda"],
        ),
        (
            "test \"name\" {\n    // test\n    assert(true, \"x\")\n}\n",
            &["// test"],
        ),
        (
            "enum E {\n    // variant\n    A,\n}\nfn main() { print(E.A) }\n",
            &["// variant"],
        ),
        (
            "struct S { x: int }\nimpl S {\n    // method\n    fn get(self) -> int { self.x }\n}\nfn main() { print(S { x: 1 }.get()) }\n",
            &["// method"],
        ),
        (
            "fn main() {\n    // while\n    while false { // body\n        break\n    }\n    // for\n    for value in [1] { // loop body\n        print(value)\n    }\n}\n",
            &["// while", "// body", "// for", "// loop body"],
        ),
        (
            "fn main(flag: bool) {\n    // if\n    if flag { // then\n        print(\"a\")\n    } else { // else\n        print(\"b\")\n    }\n}\n",
            &["// if", "// then", "// else"],
        ),
        (
            "fn main() {\n    let pair = (1, // first\n        2)\n    print(pair)\n}\n",
            &["// first"],
        ),
    ];
    for (source, comments) in cases {
        let formatted = format(source);
        for comment in *comments {
            assert!(
                formatted.contains(comment),
                "comment {comment:?} was lost\nsource:\n{source}\nformatted:\n{formatted}"
            );
        }
        assert!(
            structurally_equal(&parse(source), &parse(&formatted)),
            "AST changed for:\n{source}"
        );
    }
}

fn lint_codes(source: &str) -> Vec<martite_base::Code> {
    let run = lint_case(
        &Case::single(source),
        &ToolConfig {
            lint_preset: Some("recommended".into()),
            ..ToolConfig::default()
        },
        None,
        None,
    );
    let mut codes: Vec<_> = run.codes();
    codes.sort();
    codes.dedup();
    codes
}

#[test]
fn formatting_never_changes_which_diagnostics_are_suppressed() {
    let sources = [
        "fn main() {\n    // allow(L005)\n    if true {\n        print(\"x\")\n    }\n}\n",
        "fn main() {\n    // allow(L016)\n    if true {\n    }\n}\n",
        "// allow(L001)\nfn unused_parameter(unused: int) {}\nfn main() {}\n",
        "fn main() {\n    // allow(L002)\n    var value = 1\n    print(value)\n}\n",
    ];
    for source in sources {
        let formatted = format(source);
        assert_eq!(
            lint_codes(source),
            lint_codes(&formatted),
            "suppression changed by formatting\nbefore:\n{source}\nafter:\n{formatted}"
        );
    }
}
