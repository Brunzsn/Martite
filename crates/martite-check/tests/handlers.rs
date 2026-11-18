//! Regression tests for handler call enforcement (#173).
//!
//! Handlers are not first-class functions: E505 rejects calls to them outside
//! `test` blocks even when they reach the call through a local alias.

use martite_base::{Code, LoadLimits, MemoryProvider, SourceMap};
use martite_check::check;
use martite_resolve::{InterfaceSource, ResolveOptions, resolve};

const HOOKS: &str = "on a()\non b()\n";

fn codes_for(source: &str) -> Vec<Code> {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
    let mut sources = SourceMap::new();
    let resolved = resolve(
        &provider,
        &ResolveOptions {
            entry: "main.mt".into(),
            interfaces: vec![InterfaceSource {
                path: "app.hooks".into(),
                text: HOOKS.into(),
            }],
            grants: vec!["app.hooks".into()],
            limits: LoadLimits::default(),
        },
        &mut sources,
    );
    let program = resolved
        .program
        .unwrap_or_else(|| panic!("resolve failed: {:?}", resolved.diagnostics));
    check(program).diagnostics.iter().map(|d| d.code).collect()
}

fn assert_code(source: &str, expected: Code) {
    let codes = codes_for(source);
    assert!(
        codes.contains(&expected),
        "expected {expected}, got {codes:?}"
    );
}

fn assert_clean(source: &str) {
    let codes = codes_for(source);
    assert!(codes.is_empty(), "expected a clean check, got {codes:?}");
}

#[test]
fn handler_alias_call_outside_test_is_rejected() {
    assert_code(
        "on a() {\n    let f = b\n    f()\n}\n\non b() { }\n",
        Code::E505,
    );
}

#[test]
fn chained_handler_alias_call_outside_test_is_rejected() {
    assert_code(
        "on a() {\n    let f = b\n    let g = f\n    g()\n}\n\non b() { }\n",
        Code::E505,
    );
}

#[test]
fn direct_handler_call_outside_test_is_rejected() {
    assert_code("on a() { b() }\non b() { }\n", Code::E505);
}

#[test]
fn handlers_are_callable_inside_tests() {
    assert_clean("on a() {}\non b() {}\ntest \"calls\" { a() }\n");
    assert_clean(
        "on a() {}\non b() {}\ntest \"calls through alias\" {\n    let f = b\n    f()\n}\n",
    );
}

#[test]
fn ordinary_function_aliases_remain_callable() {
    assert_clean("fn inc(x: int) -> int { x + 1 }\nfn main() { let f = inc\n let _ = f(1) }\n");
}
