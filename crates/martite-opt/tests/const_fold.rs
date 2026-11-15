use martite_base::{LoadLimits, MemoryProvider, Severity, SourceMap};
use martite_driver::{LoadOptions, load};
use martite_ir::{IrProgram, NodeKind, lower, verify};
use martite_opt::{CostPolicy, OptimizeOptions, RewriteErrorKind, Rewriter, optimize};
use martite_runtime::{ExecLimits, IrRuntime};

fn checked(source: &str) -> (IrProgram, SourceMap) {
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
    let loaded = load(
        &provider,
        &LoadOptions {
            entry: "main.mt".into(),
            interfaces: Vec::new(),
            grants: Vec::new(),
            limits: LoadLimits::default(),
            edition: "1".into(),
        },
    );
    assert!(
        !loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error),
        "{}",
        loaded
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.render(&loaded.sources))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let program = loaded.program.expect("checked program");
    let ir = lower(&program, &loaded.sources).expect("lower");
    (ir, loaded.sources)
}

fn run(ir: &IrProgram) -> (Option<String>, u64, Vec<String>) {
    let mut runtime = IrRuntime::new(
        ir,
        ExecLimits {
            fuel: 1_000_000,
            memory: 8 << 20,
            depth: 200,
            deadline: None,
        },
    );
    let result = runtime.run_main();
    let log = match &result {
        Ok(main) => main.log.clone(),
        Err(_) => runtime.take_log(),
    };
    let outcome = match result {
        Ok(main) => Some(format!("{:?}", main.value)),
        Err(trap) => Some(format!("{trap:?}")),
    };
    (outcome, runtime.fuel_used(), log)
}

#[test]
fn folds_nested_scalar_operators_with_exact_fuel() {
    let (baseline, _sources) = checked(
        r#"
fn main() {
    let value = (1 + 2) * 3 - 4 / 2 + -5
    print("{value}")
}
"#,
    );
    verify(&baseline).expect("baseline verifies");
    let baseline_run = run(&baseline);

    let mut optimized = baseline.clone();
    let report = optimize(&mut optimized, OptimizeOptions::default()).expect("optimize");
    assert!(report.changed);
    assert!(!report.passes.is_empty());
    verify(&optimized).expect("optimized verifies");
    assert!(
        optimized
            .fns
            .iter()
            .flat_map(|function| &function.nodes)
            .any(|node| matches!(node.kind, NodeKind::Charge(_))),
        "fold inserted a carried charge"
    );
    assert_eq!(
        run(&optimized),
        baseline_run,
        "fuel, log and value must match"
    );

    let mut again = baseline.clone();
    optimize(&mut again, OptimizeOptions::default()).expect("optimize again");
    assert_eq!(again, optimized, "optimization must be deterministic");
}

#[test]
fn preserves_trapping_arithmetic() {
    for source in [
        "fn main() { let _ = 1 / 0 }",
        "fn main() { let _ = 9223372036854775807 + 1 }",
        "fn main() { let _ = -9223372036854775807 - 2 }",
    ] {
        let (baseline, _sources) = checked(source);
        let mut optimized = baseline.clone();
        optimize(&mut optimized, OptimizeOptions::default()).expect("optimize");
        verify(&optimized).expect("optimized verifies");
        assert_eq!(run(&optimized), run(&baseline), "{source}");
    }
}

#[test]
fn folds_inside_match_strings_and_structs() {
    let (baseline, _sources) = checked(
        r#"
struct Pair { left: int, right: int }

fn add(left: int, right: int) -> int { left + right }

fn pick(value: int) -> int {
    match value {
        0 => 10 + 20,
        other => other * (2 + 3),
    }
}

fn main() {
    let pair = Pair { left: pick(0), right: pick(2) }
    let label = "sum {1 + 2} {pair.left + pair.right} {add(1 + 2, 3 + 4)}"
    let with_arg = fn(value: int) => (1 + 2) + value
    let no_arg = fn() => 3 * 4
    print(label)
    print(with_arg(1) + no_arg())
}
"#,
    );
    let mut optimized = baseline.clone();
    optimize(&mut optimized, OptimizeOptions::default()).expect("optimize");
    verify(&optimized).expect("optimized verifies");
    assert_eq!(run(&optimized), run(&baseline));
}

#[test]
fn rewriter_refuses_to_remove_a_safepoint() {
    let (ir, _sources) = checked("fn main() { let _ = 1 + 2 }");
    let function = &ir.fns[0];
    let statement = function
        .nodes
        .iter()
        .position(|node| matches!(node.kind, NodeKind::Let { .. }))
        .map(|index| martite_ir::IrId::new(index as u32))
        .expect("let statement");
    let mut rewriter = Rewriter::new(&ir, CostPolicy::Preserve);
    let error = rewriter
        .remove_statement(function, statement)
        .expect_err("safepoint removal must fail");
    assert!(matches!(error.kind, RewriteErrorKind::Safepoint { .. }));
}

#[test]
fn rewriter_refuses_shared_heap_reads() {
    let (ir, _sources) = checked(
        r#"
fn main() {
    let values = [1, 2]
    let copy = values
    let _ = copy
}
"#,
    );
    let function = &ir.fns[0];
    let statement = function
        .nodes
        .iter()
        .position(|node| matches!(node.kind, NodeKind::Let { .. }))
        .map(|index| martite_ir::IrId::new(index as u32))
        .expect("let statement");
    let mut rewriter = Rewriter::new(&ir, CostPolicy::Preserve);
    let error = rewriter
        .remove_statement(function, statement)
        .expect_err("share removal must fail");
    assert!(matches!(
        error.kind,
        RewriteErrorKind::Shares { .. } | RewriteErrorKind::Safepoint { .. }
    ));
}

#[test]
fn rewriter_refuses_non_statement_removal() {
    let (ir, _sources) = checked("fn main() { let value = 1 }");
    let function = &ir.fns[0];
    let literal = function
        .nodes
        .iter()
        .position(|node| matches!(node.kind, NodeKind::Int(_)))
        .map(|index| martite_ir::IrId::new(index as u32))
        .expect("integer literal");
    let mut rewriter = Rewriter::new(&ir, CostPolicy::Preserve);
    let error = rewriter
        .remove_statement(function, literal)
        .expect_err("value removal must fail");
    assert!(matches!(error.kind, RewriteErrorKind::NotAStatement { .. }));
}
