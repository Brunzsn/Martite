use martite_base::{LoadLimits, MemoryProvider};
use martite_driver::{LoadOptions, load, lower};
use martite_ir::Printer;

fn lower_source(source: &str) -> martite_driver::LoweredProgram {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
    let loaded = load(
        &provider,
        &LoadOptions {
            entry: "main.mt".into(),
            edition: "1".into(),
            limits: LoadLimits::default(),
            ..LoadOptions::default()
        },
    );
    assert!(
        !loaded.has_errors(),
        "fixture must check: {:?}",
        loaded.diagnostics
    );
    let program = loaded.program.expect("checked program");
    lower(&program, &loaded.sources)
}

#[test]
fn lower_returns_a_load_shaped_result() {
    let lowered = lower_source("fn main() { print(1 + 2) }\n");
    assert!(
        !lowered.has_errors(),
        "lowering failed: {:?}",
        lowered.diagnostics
    );
    let program = lowered.program.expect("IR program");
    let dump = Printer::new(&program).program();
    assert!(dump.starts_with("ir-format 1\n"), "{dump}");
    assert!(dump.contains("\"main\""), "{dump}");
    assert!(dump.contains("span=src#"), "{dump}");
}

#[test]
fn lowering_the_same_checked_program_is_deterministic() {
    let mut provider = MemoryProvider::new();
    provider.insert(
        "main.mt",
        "fn add(a: int, b: int) -> int { a + b }\nfn main() { print(add(2, 3)) }\n",
    );
    let loaded = load(
        &provider,
        &LoadOptions {
            entry: "main.mt".into(),
            edition: "1".into(),
            limits: LoadLimits::default(),
            ..LoadOptions::default()
        },
    );
    assert!(
        !loaded.has_errors(),
        "fixture must check: {:?}",
        loaded.diagnostics
    );
    let program = loaded.program.expect("checked program");
    let first = lower(&program, &loaded.sources).program.expect("first IR");
    let second = lower(&program, &loaded.sources).program.expect("second IR");
    assert_eq!(
        Printer::new(&first).program(),
        Printer::new(&second).program()
    );
}
