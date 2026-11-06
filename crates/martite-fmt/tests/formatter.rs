use std::path::Path;

use martite_base::LoadLimits;
use martite_config::FmtConfig;
use martite_fmt::{format_interface, format_source, structurally_equal};
use martite_syntax::{ParseOptions, parse_interface_with, parse_source_with};

fn format_ok(source: &str, interface: bool) -> String {
    let config = FmtConfig::default();
    let output = if interface {
        format_interface(source, &config)
    } else {
        format_source(source, &config)
    };
    assert!(
        output.diagnostics.is_empty(),
        "unexpected diagnostics: {:?}",
        output.diagnostics
    );
    output.text.expect("formatter output")
}

fn assert_equivalent(before: &str, after: &str, interface: bool) {
    let left = if interface {
        parse_interface_with(before, LoadLimits::default(), ParseOptions::TOOLING).program
    } else {
        parse_source_with(before, LoadLimits::default(), ParseOptions::TOOLING).program
    };
    let right = if interface {
        parse_interface_with(after, LoadLimits::default(), ParseOptions::TOOLING).program
    } else {
        parse_source_with(after, LoadLimits::default(), ParseOptions::TOOLING).program
    };
    assert!(
        structurally_equal(&left, &right),
        "AST changed\nbefore:\n{before}\nafter:\n{after}"
    );
}

#[test]
fn complex_fixture() {
    let source = r#"use z.mod
use a.mod

const CONST_A: int = 0xff
const CONST_B: str = "b"

struct S[T] { a: int, b: list[T] }
enum E { A, B(int, str), C }
impl[T: Show] S[T] {
    pub fn get(self) -> int { self.a }
    fn set(mut self, x: int) { self.a = x }
}

fn add(a: int, b: int) -> int { a + b }

test "adds" { assert(add(1, 2) == 3, "math") }

fn main() {
    let xs = [1, 2, 3]
    let m = {"a": 1, "b": 2}
    let s = S { a: 1, b: [1, 2] }
    let t = (1,)
    let f = fn(x) => x + 1
    let g = fn(x) { x + 1 }
    if true { print("x") } else { print("y") }
    match E.B(1, "x") { A => print("a"), B(n, s) if n > 0 => print("{s}"), C => { print("c") } }
    for value in xs { print("{value}") }
    while false { break }
}
"#;
    let first = format_ok(source, false);
    println!("{first}");
    let second = format_ok(&first, false);
    assert_eq!(first, second);
    assert_equivalent(source, &first, false);
}

#[test]
fn comment_fixture() {
    let source = r#"// top
use b.mod
// above a
use a.mod

/// doc f
fn f(
    // param a
    a: int,
    // param b
    b: int, // trailing b
) -> int { // after brace
    // before let
    let x = a + // after plus
        b
    x // trailing x
}
// end
"#;
    let first = format_ok(source, false);
    println!("{first}");
    let second = format_ok(&first, false);
    assert_eq!(first, second);
    assert_equivalent(source, &first, false);
}

#[test]
fn hugs_last_lambda() {
    let source = r#"fn main() {
    let xs = [1, 2, 3]
    let ys = xs.map(fn(x) {
        let y = x + 1
        y
    })
}
"#;
    let first = format_ok(source, false);
    println!("{first}");
    let second = format_ok(&first, false);
    assert_eq!(first, second);
    assert_equivalent(source, &first, false);
}

#[test]
fn narrow_layout_fixture() {
    let source = r#"struct LongStructName { first_field: int, second_field: str, third_field: list[int] }
enum LongEnum { FirstVariant, SecondVariant(int, str, bool), ThirdVariant }
fn long_function(first_parameter: int, second_parameter: str, third_parameter: list[int]) -> Result[(), str] { Ok(()) }
fn main() {
    let result = some_long_function_name(argument_one, argument_two, argument_three)
    let value = alpha + beta * gamma + delta
    let text = editor.text(doc).split(" ").filter(fn(w) => w != "").map(fn(w) => w.trim()).len()
    if first_condition and second_condition or third_condition { print("yes") } else { print("no") }
    match value { First => 1, Second(x) if x > 0 => 2, Third => 3 }
}
"#;
    let config = FmtConfig {
        max_width: 40,
        indent: 4,
        end_of_line: martite_config::EndOfLine::Lf,
    };
    let output = format_source(source, &config);
    assert!(output.diagnostics.is_empty(), "{:?}", output.diagnostics);
    let first = output.text.unwrap();
    println!("{first}");
    let second_output = format_source(&first, &config);
    if second_output.text.is_none() {
        for diagnostic in &second_output.diagnostics {
            if let Some(span) = diagnostic.span {
                let before = &first[..span.start as usize];
                let line = before.matches('\n').count() + 1;
                let column = before
                    .rsplit('\n')
                    .next()
                    .map_or(0, |line| line.chars().count())
                    + 1;
                println!("{}:{}: {}", line, column, diagnostic.message);
            }
        }
        panic!("second pass failed");
    }
    let second = second_output.text.unwrap();
    assert_eq!(first, second);
    assert_equivalent(source, &first, false);
}

#[test]
fn list_and_struct_comments() {
    let source = r#"struct S { a: int, b: int }
fn main() {
    let xs = [
        // first
        1, // one
        // second
        2, // two
    ]
    let m = {
        // key a
        "a": 1, // value a
        "b": 2,
    }
    let s = S { // fields
        a: 1, // a
        b: 2,
    }
    let p = Pair {
        // base
        ..s,
        a: 3,
    }
}
"#;
    let first = format_ok(source, false);
    println!("{first}");
    let second = format_ok(&first, false);
    assert_eq!(first, second);
    assert_equivalent(source, &first, false);
}

#[test]
fn formats_interface_files() {
    let source = "struct Doc\nfn text( doc:Doc )->str\non saved(doc:Doc)->Result[(),str]\n";
    let first = format_ok(source, true);
    assert_eq!(
        first,
        "struct Doc\n\nfn text(doc: Doc) -> str\n\non saved(doc: Doc) -> Result[(), str]\n"
    );
    assert_eq!(format_ok(&first, true), first);
    assert_equivalent(source, &first, true);
}

#[test]
fn normalizes_blank_lines() {
    let source = r#"


const CONST_A: int = 1
const CONST_B: int = 2

const CONST_C: int = 3
fn main() {

    let x = 1


    let y = 2
    // tail

}

"#;
    let first = format_ok(source, false);
    assert_eq!(
        first,
        "const CONST_A: int = 1\nconst CONST_B: int = 2\n\nconst CONST_C: int = 3\n\nfn main() {\n    let x = 1\n\n    let y = 2\n    // tail\n}\n"
    );
    assert_eq!(format_ok(&first, false), first);
}

#[test]
fn preserves_parentheses() {
    let source = r#"fn main() {
    let x = ((1 + 2)) * 3
    let y: (int) = (1)
    match 1 {
        (value) => print("{value}"),
    }
}
"#;
    let first = format_ok(source, false);
    println!("{first}");
    assert!(first.contains("((1 + 2)) * 3"), "{first}");
    assert!(first.contains("(int)"), "{first}");
    assert!(first.contains("(value)"), "{first}");
    assert_eq!(format_ok(&first, false), first);
    assert_equivalent(source, &first, false);
}

#[test]
fn generic_and_pattern_layout() {
    let source = r#"fn generic[VeryLongTypeParameter: Eq + Ord, OtherTypeParameter: Show](a: VeryLongTypeName[FirstLongArgument, SecondLongArgument], b: (int, str, bool)) -> map[str, list[VeryLongTypeName[int]]] {
    let tuple = (very_long_expression_one,)
    let map_value = {"a": [1, 2, 3], "b": [4, 5, 6]}
    let struct_value = LongStructName { first_field: very_long_expression, second_field: another_long_expression }
    match value {
        FirstVariant | SecondVariant => print("long long long long long long long long long")
    }
}
"#;
    let config = FmtConfig {
        max_width: 40,
        indent: 4,
        end_of_line: martite_config::EndOfLine::Lf,
    };
    let output = format_source(source, &config);
    assert!(output.diagnostics.is_empty(), "{:#?}", output.diagnostics);
    let first = output.text.unwrap();
    println!("{first}");
    assert_eq!(format_source(&first, &config).text.unwrap(), first);
    assert_equivalent(source, &first, false);
}

#[test]
fn sorts_import_runs_without_crossing_blank_lines() {
    let source = r#"use z.one
use a.two

// b comment
use z.three
use b.four
"#;
    let first = format_ok(source, false);
    assert_eq!(
        first,
        "use a.two\nuse z.one\n\nuse b.four\n// b comment\nuse z.three\n"
    );
    assert_eq!(format_ok(&first, false), first);
}

#[test]
fn golden_fixtures() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
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
        let expected = martite_base::normalize_source(
            &std::fs::read_to_string(&after).expect("read expected fixture"),
        );
        let actual = format_ok(&source, interface);
        assert_eq!(actual, expected, "fixture {}", before.display());
        assert_eq!(format_ok(&actual, interface), actual);
        assert_equivalent(&source, &actual, interface);
    }
}

#[test]
fn repository_examples_are_idempotent() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    for entry in walk(&root) {
        let extension = entry.extension().and_then(|ext| ext.to_str());
        let interface = extension == Some("mti");
        if extension != Some("mt") && !interface {
            continue;
        }
        let source = std::fs::read_to_string(&entry).expect("read example");
        let first = format_ok(&source, interface);
        let second = format_ok(&first, interface);
        assert_eq!(first, second, "not idempotent: {}", entry.display());
        assert_equivalent(&source, &first, interface);
    }
}

fn walk(root: &Path) -> Vec<std::path::PathBuf> {
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
