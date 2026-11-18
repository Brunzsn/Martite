use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(name: &str) -> Self {
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "martite-fix-{}-{}-{}",
            std::process::id(),
            unique,
            name
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create temp dir");
        Self { path }
    }

    fn file(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&path, text).expect("write fixture");
        path
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn martite(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_martite"))
        .args(args)
        .output()
        .expect("run martite")
}

fn s(value: &str) -> &std::ffi::OsStr {
    std::ffi::OsStr::new(value)
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

struct FixCase {
    name: &'static str,
    code: &'static str,
    suggested: bool,
    source: &'static str,
}

const CASES: &[FixCase] = &[
    FixCase {
        name: "L001 unused import",
        code: "L001",
        suggested: false,
        source: "use std.math\n\nfn main() {\n    print(\"ok\")\n}\n",
    },
    FixCase {
        name: "L002 var to let",
        code: "L002",
        suggested: false,
        source: "fn main() {\n    var value = 1\n    print(value)\n}\n",
    },
    FixCase {
        name: "L003 redundant arm",
        code: "L003",
        suggested: true,
        source: "fn choose(flag: bool) -> int {\n    match flag {\n        true => 1,\n        false => 0,\n        _ => 2,\n    }\n}\n\ntest \"choose\" {\n    assert(choose(true) == 1, \"one\")\n}\n",
    },
    FixCase {
        name: "L004 unreachable statement",
        code: "L004",
        suggested: true,
        source: "fn done() {\n    return\n    print(\"never\")\n}\n\ntest \"done\" {\n    done()\n    assert(true, \"ok\")\n}\n",
    },
    FixCase {
        name: "L005 constant condition",
        code: "L005",
        suggested: true,
        source: "fn always() {\n    if true {\n        print(\"yes\")\n    }\n}\n\ntest \"always\" {\n    always()\n    assert(true, \"ok\")\n}\n",
    },
    FixCase {
        name: "L011 boolean simplification",
        code: "L011",
        suggested: false,
        source: "fn check(flag: bool) -> bool {\n    flag == true\n}\n\ntest \"boolean\" {\n    assert(check(true), \"true\")\n}\n",
    },
    FixCase {
        name: "L012 compound assignment",
        code: "L012",
        suggested: false,
        source: "fn main() {\n    var total = 0\n    total = total + 1\n    print(total)\n}\n",
    },
    FixCase {
        name: "L013 final return",
        code: "L013",
        suggested: false,
        source: "fn done() {\n    return\n}\n\ntest \"done\" {\n    done()\n    assert(true, \"ok\")\n}\n",
    },
    FixCase {
        name: "L014 bool match",
        code: "L014",
        suggested: true,
        source: "fn choose(flag: bool) -> int {\n    match flag {\n        true => 1,\n        false => 0,\n    }\n}\n\ntest \"choose\" {\n    assert(choose(false) == 0, \"zero\")\n}\n",
    },
    FixCase {
        name: "L022 stale suppression",
        code: "L022",
        suggested: false,
        source: "// allow(L999)\nfn main() {\n    print(\"ok\")\n}\n",
    },
    FixCase {
        name: "L023 eager unwrap_or",
        code: "L023",
        suggested: true,
        source: "fn compute() -> int { 1 }\n\nfn main() {\n    let maybe: int? = None\n    let value = maybe.unwrap_or(compute())\n    print(value)\n}\n\ntest \"value\" {\n    assert(true, \"ok\")\n}\n",
    },
    FixCase {
        name: "L024 mixed and or",
        code: "L024",
        suggested: false,
        source: "fn check(a: bool, b: bool, c: bool) -> bool {\n    a and b or c\n}\n\ntest \"mixed\" {\n    assert(check(true, true, false), \"yes\")\n}\n",
    },
    FixCase {
        name: "L028 underscore rename",
        code: "L028",
        suggested: true,
        source: "fn main() {\n    let _value = 1\n    print(_value)\n}\n",
    },
    FixCase {
        name: "L029 forwarding lambda",
        code: "L029",
        suggested: true,
        source: "fn show(value: int) {\n    print(value)\n}\n\nfn main() {\n    let f = fn(value: int) => show(value)\n    f(1)\n}\n\ntest \"forward\" {\n    assert(true, \"ok\")\n}\n",
    },
    FixCase {
        name: "L032 stray doc comment",
        code: "L032",
        suggested: false,
        source: "fn main() {\n    /// stray\n    let value = 1\n    print(value)\n}\n",
    },
];

#[test]
fn every_fix_rule_removes_its_diagnostic_without_breaking_the_program() {
    for case in CASES {
        let dir = TempDir::new(case.code);
        let script = dir.file("main.mt", case.source);

        // Behavior guard: a deterministic test run must pass before and after.
        let before = martite(&[s("test"), script.as_os_str()]);
        assert!(
            before.status.success(),
            "{}: test failed before fix: {}",
            case.name,
            stderr(&before)
        );

        let mut args = vec![s("lint"), s("--fix")];
        if case.suggested {
            args.push(s("--suggested"));
        }
        args.push(s("--rule"));
        args.push(s(case.code));
        args.push(script.as_os_str());
        let fixed = martite(&args);
        assert!(
            fixed.status.success(),
            "{}: fix failed: {}",
            case.name,
            stderr(&fixed)
        );

        let after = martite(&[s("test"), script.as_os_str()]);
        assert!(
            after.status.success(),
            "{}: test failed after fix: {}",
            case.name,
            stderr(&after)
        );
        assert_eq!(
            stdout(&before),
            stdout(&after),
            "{}: behavior changed",
            case.name
        );

        let check = martite(&[s("check"), script.as_os_str()]);
        assert!(
            check.status.success(),
            "{}: fix introduced an E-code: {}",
            case.name,
            stderr(&check)
        );

        let lint = martite(&[s("lint"), s("--rule"), s(case.code), script.as_os_str()]);
        assert!(
            !stderr(&lint).contains(&format!("{}({})", "warning", case.code))
                && !stderr(&lint).contains(&format!("error[{}]", case.code)),
            "{}: diagnostic remains: {}",
            case.name,
            stderr(&lint)
        );
    }
}

#[test]
fn l015_fixes_declarations_and_call_sites_project_atomically() {
    let dir = TempDir::new("l015");
    let lib = dir.file(
        "lib.mt",
        "fn bump_lib(mut value: int) -> int {\n    value\n}\n\npub fn public_bump(mut value: int) -> int {\n    value\n}\n\npub fn call_bump() -> int {\n    var value = 1\n    bump_lib(mut value)\n}\n",
    );
    let main = dir.file(
        "main.mt",
        "use lib.call_bump\nuse lib.public_bump\n\nfn bump_main(mut value: int) -> int {\n    value\n}\n\nfn main() {\n    var value = 1\n    print(bump_main(mut value))\n    print(public_bump(mut value))\n    print(call_bump())\n}\n",
    );
    let fixed = martite(&[
        s("lint"),
        s("--fix"),
        s("--suggested"),
        s("--rule"),
        s("L015"),
        dir.path().as_os_str(),
    ]);
    assert!(fixed.status.success(), "{}", stderr(&fixed));

    let lib_text = std::fs::read_to_string(&lib).expect("lib");
    assert!(
        lib_text.contains("fn bump_lib(value: int)"),
        "private declaration was not fixed: {lib_text}"
    );
    assert!(
        lib_text.contains("bump_lib(value)"),
        "private call site was not fixed: {lib_text}"
    );
    assert!(
        lib_text.contains("pub fn public_bump(mut value: int)"),
        "public declaration must not be fixed: {lib_text}"
    );
    let main_text = std::fs::read_to_string(&main).expect("main");
    assert!(
        main_text.contains("fn bump_main(value: int)"),
        "second file was not fixed: {main_text}"
    );
    assert!(
        main_text.contains("bump_main(value)"),
        "second file call site was not fixed: {main_text}"
    );
    assert!(
        main_text.contains("public_bump(mut value)"),
        "public call site must not be fixed: {main_text}"
    );
}

#[test]
fn a_fix_that_would_introduce_an_e_code_is_discarded() {
    let dir = TempDir::new("discard");
    let script = dir.file(
        "main.mt",
        "fn main() {\n    let _value = 1\n    let value = 2\n    print(_value + value)\n}\n",
    );
    let original = std::fs::read_to_string(&script).unwrap();
    let output = martite(&[
        s("lint"),
        s("--fix"),
        s("--suggested"),
        s("--rule"),
        s("L028"),
        script.as_os_str(),
    ]);
    assert_eq!(output.status.code(), Some(1), "stderr: {}", stderr(&output));
    assert!(
        stderr(&output).contains("E103"),
        "expected the introduced E-code in stderr: {}",
        stderr(&output)
    );
    assert_eq!(
        std::fs::read_to_string(&script).unwrap(),
        original,
        "a partially applied project must not be written"
    );
}

#[test]
fn l001_binding_fix_prefixes_with_an_underscore() {
    let dir = TempDir::new("l001-binding");
    let script = dir.file("main.mt", "fn main() {\n    let unused = 1\n}\n");
    let fixed = martite(&[
        s("lint"),
        s("--fix"),
        s("--suggested"),
        s("--rule"),
        s("L001"),
        script.as_os_str(),
    ]);
    assert!(fixed.status.success(), "{}", stderr(&fixed));
    let text = std::fs::read_to_string(&script).unwrap();
    assert!(text.contains("let _unused = 1"), "{text}");
    let check = martite(&[s("check"), script.as_os_str()]);
    assert!(check.status.success(), "{}", stderr(&check));
}

#[test]
fn l020_removes_an_unused_manifest_grant() {
    let dir = TempDir::new("l020-fix");
    let manifest = dir.file(
        "plugin.toml",
        "[plugin]\nname = \"grant-fix\"\nversion = \"0.1.0\"\nlang = \"1\"\napi = \"1.0\"\nentry = \"main.mt\"\n\n[permissions]\ngrant = [\"app.api\", \"std.math\"]\n",
    );
    dir.file("main.mt", "use app.api\non check() {}\n");
    let interface = dir.file("app/api.mti", "on check()\n");
    let fixed = martite(&[
        s("lint"),
        s("--fix"),
        s("--suggested"),
        s("--rule"),
        s("L020"),
        s("--interface"),
        interface.as_os_str(),
        dir.path().as_os_str(),
    ]);
    assert!(fixed.status.success(), "{}", stderr(&fixed));
    let text = std::fs::read_to_string(&manifest).unwrap();
    assert!(!text.contains("std.math"), "grant was not removed: {text}");
    assert!(text.contains("app.api"), "{text}");
}

#[test]
fn l020_removes_only_the_intended_grant_when_names_share_a_prefix() {
    let dir = TempDir::new("l020-prefix");
    let manifest = dir.file(
        "plugin.toml",
        "[plugin]\nname = \"grant-prefix\"\nversion = \"0.1.0\"\nlang = \"1\"\napi = \"1.0\"\nentry = \"main.mt\"\n\n[permissions]\ngrant = [\"app.editor\", \"a\\u0070p\"]\n",
    );
    dir.file("main.mt", "use app.editor\non check() {}\n");
    let interface = dir.file("app/editor.mti", "on check()\n");
    let unused = dir.file("app.mti", "struct Placeholder\n");
    let fixed = martite(&[
        s("lint"),
        s("--fix"),
        s("--suggested"),
        s("--rule"),
        s("L020"),
        s("--interface"),
        interface.as_os_str(),
        s("--interface"),
        unused.as_os_str(),
        dir.path().as_os_str(),
    ]);
    assert!(fixed.status.success(), "{}", stderr(&fixed));
    let text = std::fs::read_to_string(&manifest).unwrap();
    assert!(text.contains("\"app.editor\""), "{text}");
    assert!(
        !text.contains("\\u0070"),
        "unused grant was not removed: {text}"
    );
}

#[test]
fn lint_fix_followed_by_fmt_is_stable() {
    let dir = TempDir::new("fix-fmt");
    let script = dir.file(
        "main.mt",
        "fn main(){var total=0\ntotal=total+1\nprint(total)}\n",
    );
    let fix = martite(&[s("lint"), s("--fix"), script.as_os_str()]);
    assert!(fix.status.success(), "{}", stderr(&fix));
    let fmt = martite(&[s("fmt"), script.as_os_str()]);
    assert!(fmt.status.success(), "{}", stderr(&fmt));
    let check = martite(&[s("fmt"), s("--check"), script.as_os_str()]);
    assert!(check.status.success(), "{}", stderr(&check));
}

/// Run `lint --fix --rule <rule>` twice and return the file text after the
/// first pass, checking that the second pass changes nothing.
fn fix_twice(script: &Path, rule: &str, suggested: bool) -> String {
    let mut args = vec![s("lint"), s("--fix")];
    if suggested {
        args.push(s("--suggested"));
    }
    args.extend([s("--rule"), s(rule), script.as_os_str()]);
    let first = martite(&args);
    assert!(
        first.status.success(),
        "{rule} fix failed: {}",
        stderr(&first)
    );
    let text = std::fs::read_to_string(script).expect("fixed file");
    let second = martite(&args);
    assert!(
        second.status.success(),
        "{rule} second fix failed: {}",
        stderr(&second)
    );
    assert_eq!(
        std::fs::read_to_string(script).expect("fixed file"),
        text,
        "{rule} fix is not idempotent"
    );
    text
}

#[test]
fn l011_not_comparison_fixes_are_idempotent_and_preserve_stdout() {
    let cases = [
        (
            "bare",
            "fn main() {\n    let a = true\n    let b = false\n    if not a == b {\n        print(\"branch\")\n    }\n}\n",
            "if a != b {",
            "branch\n",
        ),
        (
            "paren",
            "fn main() {\n    let a = true\n    let b = false\n    if not (a == b) {\n        print(\"paren\")\n    }\n}\n",
            "if (a != b) {",
            "paren\n",
        ),
        (
            "nested",
            "fn main() {\n    let a = false\n    let b = false\n    let c = true\n    if not ((a or b) == c) {\n        print(\"nested\")\n    }\n}\n",
            "if ((a or b) != c) {",
            "nested\n",
        ),
    ];
    for (name, source, expected, output) in cases {
        let dir = TempDir::new(&format!("l011-{name}"));
        let script = dir.file("main.mt", source);
        let before = martite(&[s("run"), script.as_os_str()]);
        assert!(before.status.success(), "{name}: {}", stderr(&before));
        assert_eq!(stdout(&before), output, "{name}");
        let text = fix_twice(&script, "L011", false);
        assert!(text.contains(expected), "{name}: {text}");
        assert!(!text.contains("not "), "{name}: {text}");
        let after = martite(&[s("run"), script.as_os_str()]);
        assert!(after.status.success(), "{name}: {}", stderr(&after));
        assert_eq!(stdout(&after), output, "{name}: behavior changed");
    }
}

#[test]
fn l002_keeps_a_mutated_destructuring_var_and_fixes_an_unmutated_one_once() {
    let dir = TempDir::new("l002-destructuring");
    let mutated = dir.file(
        "mutated.mt",
        "fn main() {\n    var (a, b) = (0, 0)\n    a += 1\n    print(\"{b}\")\n}\n",
    );
    let before = martite(&[s("run"), mutated.as_os_str()]);
    assert!(before.status.success(), "{}", stderr(&before));
    let original = std::fs::read_to_string(&mutated).unwrap();
    let fix = martite(&[
        s("lint"),
        s("--fix"),
        s("--rule"),
        s("L002"),
        mutated.as_os_str(),
    ]);
    assert!(fix.status.success(), "{}", stderr(&fix));
    assert_eq!(std::fs::read_to_string(&mutated).unwrap(), original);
    let after = martite(&[s("run"), mutated.as_os_str()]);
    assert_eq!(stdout(&before), stdout(&after));

    let immutable = dir.file(
        "immutable.mt",
        "fn main() {\n    var (a, b) = (0, 0)\n    print(\"{a}{b}\")\n}\n",
    );
    let text = fix_twice(&immutable, "L002", false);
    assert_eq!(text.matches("let").count(), 1, "{text}");
    assert!(!text.contains("var"), "{text}");
    let check = martite(&[s("check"), immutable.as_os_str()]);
    assert!(check.status.success(), "{}", stderr(&check));
}

#[test]
fn l004_removal_keeps_a_closing_brace_on_the_same_line() {
    let dir = TempDir::new("l004-brace");
    let script = dir.file(
        "main.mt",
        "fn f() {\n    panic(\"x\")\n    let x = 1 }\nfn main() { f() }\n",
    );
    let text = fix_twice(&script, "L004", true);
    assert!(text.contains("fn main() { f() }"), "{text}");
    assert!(
        text.lines().any(|line| line.trim() == "}"),
        "closing brace was eaten: {text}"
    );
    let check = martite(&[s("check"), script.as_os_str()]);
    assert!(check.status.success(), "{}", stderr(&check));
}

#[test]
fn l012_parenthesized_rhs_fix_is_idempotent_and_keeps_output() {
    let dir = TempDir::new("l012-paren");
    let script = dir.file(
        "main.mt",
        "fn main() {\n    var x = 0\n    x = x + (1)\n    print(\"{x}\")\n}\n",
    );
    let before = martite(&[s("run"), script.as_os_str()]);
    assert!(before.status.success(), "{}", stderr(&before));
    let text = fix_twice(&script, "L012", false);
    assert!(text.contains("x += (1)"), "{text}");
    let after = martite(&[s("run"), script.as_os_str()]);
    assert_eq!(stdout(&before), stdout(&after));
}

#[test]
fn l013_parenthesized_final_return_fix_is_idempotent_and_keeps_output() {
    let dir = TempDir::new("l013-paren");
    let script = dir.file(
        "main.mt",
        "fn f(a: ()) {\n    return (a)\n}\n\nfn main() {\n    f(())\n}\n",
    );
    let before = martite(&[s("run"), script.as_os_str()]);
    assert!(before.status.success(), "{}", stderr(&before));
    let text = fix_twice(&script, "L013", false);
    assert!(text.contains("    (a)\n"), "{text}");
    let after = martite(&[s("run"), script.as_os_str()]);
    assert_eq!(stdout(&before), stdout(&after));
}

#[test]
fn an_aliased_variant_import_is_not_removed() {
    let dir = TempDir::new("l001-alias");
    dir.file("m.mt", "pub enum Shape { Circle(float) }\n");
    let script = dir.file(
        "main.mt",
        "use m.Shape as S\n\nfn main() {\n    print(\"{S.Circle(1.0)}\")\n}\n",
    );
    let before = martite(&[s("run"), script.as_os_str()]);
    assert!(before.status.success(), "{}", stderr(&before));
    let original = std::fs::read_to_string(&script).unwrap();
    let fix = martite(&[
        s("lint"),
        s("--fix"),
        s("--rule"),
        s("L001"),
        script.as_os_str(),
    ]);
    assert!(fix.status.success(), "{}", stderr(&fix));
    assert_eq!(std::fs::read_to_string(&script).unwrap(), original);
    let after = martite(&[s("run"), script.as_os_str()]);
    assert_eq!(stdout(&before), stdout(&after));
}
