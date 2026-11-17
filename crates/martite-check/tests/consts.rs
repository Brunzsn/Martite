use std::rc::Rc;

use martite_base::{Code, Diagnostic, LoadLimits, MemoryProvider, SourceMap};
use martite_check::{CheckedProgram, ConstValue, check};
use martite_resolve::{ResolveOptions, ResolveOutput, resolve};

fn resolve_source(source: &str) -> (SourceMap, ResolveOutput) {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
    let mut sources = SourceMap::new();
    let output = resolve(
        &provider,
        &ResolveOptions {
            entry: "main.mt".into(),
            interfaces: Vec::new(),
            grants: Vec::new(),
            limits: LoadLimits::default(),
        },
        &mut sources,
    );
    (sources, output)
}

fn check_source(source: &str) -> (Option<CheckedProgram>, Vec<Diagnostic>) {
    let (_, resolved) = resolve_source(source);
    assert!(
        !resolved.has_errors(),
        "fixture must resolve: {:?}",
        resolved.diagnostics
    );
    let checked = check(resolved.program.expect("resolved program"));
    (checked.program, checked.diagnostics)
}

fn nested_list_type(levels: usize) -> String {
    format!("{}int{}", "list[".repeat(levels), "]".repeat(levels))
}

/// One constant per level, each referencing the previous one twice:
/// `const Cn: list[...] = [C(n-1), C(n-1)]`.
fn deep_shared_const_source(levels: usize) -> String {
    let mut source = String::new();
    for level in 1..=levels {
        let ty = nested_list_type(level);
        if level == 1 {
            source.push_str(&format!("const C1: {ty} = [0]\n"));
        } else {
            source.push_str(&format!(
                "const C{level}: {ty} = [C{}, C{}]\n",
                level - 1,
                level - 1
            ));
        }
    }
    source.push_str("fn main() { }\n");
    source
}

#[test]
fn deep_shared_constants_check_with_shared_payloads() {
    let source = deep_shared_const_source(22);
    let (program, diagnostics) = check_source(&source);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let program = program.expect("checked program");

    let info = program
        .consts
        .values()
        .max_by_key(|info| info.def.0)
        .expect("constants");
    let ConstValue::List(items) = &info.value else {
        panic!("top constant is not a list: {:?}", info.value);
    };
    assert_eq!(items.len(), 2);
    let (ConstValue::List(left), ConstValue::List(right)) = (&items[0], &items[1]) else {
        panic!("nested constants are not lists");
    };
    assert!(
        Rc::ptr_eq(left, right),
        "repeated constant references must share one payload"
    );
}

#[test]
fn constant_over_the_byte_budget_is_rejected() {
    // Doubling string concatenation grows the produced payloads quickly from
    // a tiny source. Each level stays within the file's own load limit.
    let mut source = format!("const S1: str = \"{}\"\n", "x".repeat(1024));
    for level in 2..=14 {
        source.push_str(&format!(
            "const S{level}: str = S{} + S{}\n",
            level - 1,
            level - 1
        ));
    }
    source.push_str("fn main() { }\n");

    let (_, diagnostics) = check_source(&source);
    assert!(
        diagnostics
            .iter()
            .any(|d| d.code == Code::E406 && d.message.contains("constant budget")),
        "expected an E406 budget diagnostic, got {diagnostics:?}"
    );
}

#[test]
fn constant_cycle_still_reports_e105() {
    let (_, resolved) = resolve_source(
        "const CYCLE_A: int = CYCLE_B\nconst CYCLE_B: int = CYCLE_A\nfn main() { }\n",
    );
    assert!(
        resolved.diagnostics.iter().any(|d| d.code == Code::E105),
        "expected E105, got {:?}",
        resolved.diagnostics
    );
}

#[test]
fn constant_type_mismatch_still_reports_e201() {
    let (_, diagnostics) = check_source("const TEXT: int = \"text\"\nfn main() { }\n");
    assert!(
        diagnostics.iter().any(|d| d.code == Code::E201),
        "expected E201, got {diagnostics:?}"
    );
}
