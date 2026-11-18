//! Front-end validation regressions for issue #178.

use martite_base::{Code, Diagnostic, LoadLimits, MemoryProvider, SourceMap};
use martite_check::check;
use martite_resolve::{ResolveOptions, resolve};

fn check_files(files: &[(&str, &str)]) -> Vec<Diagnostic> {
    let mut provider = MemoryProvider::new();
    for (path, text) in files {
        provider.insert(*path, *text);
    }
    let mut sources = SourceMap::new();
    let resolved = resolve(
        &provider,
        &ResolveOptions {
            entry: "main.mt".into(),
            interfaces: Vec::new(),
            grants: Vec::new(),
            limits: LoadLimits::default(),
        },
        &mut sources,
    );
    assert!(
        !resolved.has_errors(),
        "fixture must resolve: {:?}",
        resolved.diagnostics
    );
    check(resolved.program.expect("resolved program")).diagnostics
}

fn assert_clean(files: &[(&str, &str)]) {
    let diagnostics = check_files(files);
    assert!(
        diagnostics.is_empty(),
        "expected a clean check, got {diagnostics:?}"
    );
}

fn assert_error(files: &[(&str, &str)], code: Code, message: &str) {
    let diagnostics = check_files(files);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == code && d.message.contains(message)),
        "expected {code} containing `{message}`, got {diagnostics:?}"
    );
}

// ----------------------------------------------------------------------
// #178.1: duplicate bindings in one pattern
// ----------------------------------------------------------------------

#[test]
fn duplicate_binding_in_one_pattern_is_e303() {
    assert_error(
        &[(
            "main.mt",
            "fn main() {\n    let (x, x) = (1, 2)\n    print(\"{x}\")\n}\n",
        )],
        Code::E303,
        "bound more than once",
    );
}

#[test]
fn duplicate_binding_in_a_nested_pattern_is_e303() {
    assert_error(
        &[(
            "main.mt",
            "fn main() {\n    let ((x, x), y) = ((1, 2), 3)\n    print(\"{y}\")\n}\n",
        )],
        Code::E303,
        "bound more than once",
    );
}

#[test]
fn or_pattern_alternatives_may_bind_the_same_name() {
    assert_clean(&[(
        "main.mt",
        "enum E { A(int), B(int) }\nfn main() {\n    let e = E.A(1)\n    match e {\n        E.A(x) | E.B(x) => print(\"{x}\"),\n    }\n}\n",
    )]);
}

// ----------------------------------------------------------------------
// #178.2: range or string-slice place assignment
// ----------------------------------------------------------------------

#[test]
fn range_assignment_to_a_list_is_rejected() {
    assert_error(
        &[(
            "main.mt",
            "fn main() {\n    var xs = [1, 2]\n    xs[0..1] = []\n    print(\"{xs.len()}\")\n}\n",
        )],
        Code::E201,
        "list index place must be int",
    );
}

#[test]
fn range_assignment_to_a_string_is_rejected() {
    assert_error(
        &[(
            "main.mt",
            "fn main() {\n    var s = \"ab\"\n    s[0..1] = \"x\"\n}\n",
        )],
        Code::E401,
        "string slice is not a place",
    );
}

#[test]
fn valid_place_indexes_still_check() {
    assert_clean(&[(
        "main.mt",
        "fn main() {\n    var xs = [1, 2]\n    xs[0] = 3\n    var m = {\"a\": 1}\n    m[\"a\"] = 2\n    let part = xs[0..1]\n    let a = xs[0]\n    let b = m[\"a\"]\n    print(\"{a}{b}{part.len()}\")\n}\n",
    )]);
}

#[test]
fn slice_cannot_be_a_mutable_argument() {
    assert_error(
        &[(
            "main.mt",
            "fn bump(mut xs: list[int]) { xs.push(1) }\nfn main() {\n    var xs = [1, 2]\n    bump(mut xs[0..1])\n}\n",
        )],
        Code::E201,
        "list index place must be int",
    );
}

#[test]
fn slice_cannot_be_a_mutating_receiver() {
    assert_error(
        &[(
            "main.mt",
            "fn main() {\n    var xs = [1, 2]\n    xs[0..1].push(3)\n}\n",
        )],
        Code::E201,
        "list index place must be int",
    );
}

// ----------------------------------------------------------------------
// #178.3: private methods
// ----------------------------------------------------------------------

const PRIVATE_METHOD_MODULE: &str = "pub struct Box { n: int }\nimpl Box {\n    fn secret(self) -> int { self.n }\n    fn hidden() -> int { 7 }\n    pub fn make() -> Box { Box { n: 1 } }\n}\n";

#[test]
fn private_method_is_not_callable_from_another_module() {
    assert_error(
        &[
            ("a.mt", PRIVATE_METHOD_MODULE),
            (
                "main.mt",
                "use a.Box\nfn main() {\n    let b = Box.make()\n    let n = b.secret()\n    print(\"{n}\")\n}\n",
            ),
        ],
        Code::E106,
        "`secret` is private",
    );
}

#[test]
fn private_associated_function_is_not_callable_from_another_module() {
    assert_error(
        &[
            ("a.mt", PRIVATE_METHOD_MODULE),
            (
                "main.mt",
                "use a.Box\nfn main() {\n    let n = Box.hidden()\n    print(\"{n}\")\n}\n",
            ),
        ],
        Code::E106,
        "`hidden` is private",
    );
}

#[test]
fn public_method_remains_callable_from_another_module() {
    assert_clean(&[
        (
            "a.mt",
            "pub struct Box { n: int }\nimpl Box {\n    pub fn reveal(self) -> int { self.n }\n    pub fn make() -> Box { Box { n: 1 } }\n}\n",
        ),
        (
            "main.mt",
            "use a.Box\nfn main() {\n    let b = Box.make()\n    let n = b.reveal()\n    print(\"{n}\")\n}\n",
        ),
    ]);
}

#[test]
fn private_method_remains_callable_in_its_own_module() {
    assert_clean(&[(
        "main.mt",
        "struct Box { n: int }\nimpl Box {\n    fn secret(self) -> int { self.n }\n    pub fn make() -> Box { Box { n: 1 } }\n}\nfn main() {\n    let b = Box.make()\n    let n = b.secret()\n    print(\"{n}\")\n}\n",
    )]);
}
