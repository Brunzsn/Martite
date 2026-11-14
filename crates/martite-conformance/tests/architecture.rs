use std::fs;
use std::path::Path;

const CRATES: &[&str] = &[
    "martite-base",
    "martite-fuel",
    "martite-syntax",
    "martite-config",
    "martite-resolve",
    "martite-check",
    "martite-ir",
    "martite-runtime",
    "martite-driver",
    "martite-lint",
    "martite-fmt",
    "martite",
    "martite-cli",
    "martite-conformance",
    "martite-bench",
];

#[test]
fn workspace_is_split_into_pipeline_crates() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in CRATES {
        let manifest = root.join("crates").join(name).join("Cargo.toml");
        assert!(manifest.is_file(), "missing crate manifest: {manifest:?}");
    }
}

#[test]
fn benchmarks_are_isolated_from_production_crates() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bench = fs::read_to_string(root.join("crates/martite-bench/Cargo.toml"))
        .expect("read benchmark manifest");
    assert!(
        bench.contains("publish = false"),
        "bench crate must not publish"
    );

    for name in CRATES.iter().filter(|name| **name != "martite-bench") {
        let manifest = fs::read_to_string(root.join("crates").join(name).join("Cargo.toml"))
            .expect("read crate manifest");
        assert!(
            !manifest.contains("martite-bench"),
            "{name} must not depend on the benchmark crate"
        );
        assert!(
            !manifest.contains("criterion") && !manifest.contains("stats_alloc"),
            "{name} must not depend on benchmark-only tooling"
        );
    }
}

#[test]
fn runtime_values_use_the_ir_vocabulary() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in [
        "lib.rs",
        "value.rs",
        "ops.rs",
        "json.rs",
        "exec.rs",
        "ir_eval.rs",
        "ir_eval/builtins.rs",
    ] {
        let path = root.join("crates/martite-runtime/src").join(name);
        let source = fs::read_to_string(&path).expect("read runtime source");
        for forbidden in [
            "martite_check",
            "CheckedProgram",
            "BindingId",
            "DefId",
            "NodeId",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} must use the IR vocabulary, not {forbidden}"
            );
        }
    }
    let value = fs::read_to_string(root.join("crates/martite-runtime/src/value.rs"))
        .expect("read runtime value source");
    assert!(
        !value.contains("NodeId"),
        "runtime values must not carry syntax node ids"
    );
}

#[test]
fn ir_stays_above_the_runtime() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest =
        fs::read_to_string(root.join("crates/martite-ir/Cargo.toml")).expect("read IR manifest");
    assert!(
        !manifest.contains("martite-runtime"),
        "martite-ir must not depend on the runtime"
    );
    assert!(
        !manifest.contains("martite-bench"),
        "martite-ir must not depend on the benchmark crate"
    );
}

#[test]
fn runtime_is_ir_only() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest = fs::read_to_string(root.join("crates/martite-runtime/Cargo.toml"))
        .expect("read runtime manifest");
    for forbidden in ["ast-eval", "martite-check", "martite-syntax"] {
        assert!(
            !manifest.contains(forbidden),
            "martite-runtime must not reference {forbidden}"
        );
    }

    let lib = fs::read_to_string(root.join("crates/martite-runtime/src/lib.rs"))
        .expect("read runtime lib");
    assert!(
        lib.contains("pub mod ir_eval;"),
        "IR evaluator must be built"
    );
    assert!(
        !lib.contains("interp"),
        "the AST evaluator must be removed from the runtime"
    );
    assert!(
        !root.join("crates/martite-runtime/src/interp.rs").exists(),
        "interp.rs must be deleted"
    );
    assert!(
        !root.join("crates/martite-runtime/src/builtins.rs").exists(),
        "the AST builtin include must be deleted"
    );

    let cli =
        fs::read_to_string(root.join("crates/martite-cli/Cargo.toml")).expect("read CLI manifest");
    assert!(
        !cli.contains("ast-eval"),
        "the temporary CLI evaluator feature must be removed"
    );
}

#[test]
fn ir_lowering_is_an_opt_in_dependency() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let manifest =
        fs::read_to_string(root.join("crates/martite-ir/Cargo.toml")).expect("read IR manifest");
    assert!(
        manifest.contains("lower = [\"dep:martite-check\", \"dep:martite-syntax\"]"),
        "lowering and the checker must be opt-in"
    );
    let workspace = fs::read_to_string(root.join("Cargo.toml")).expect("read workspace manifest");
    assert!(
        workspace.contains("martite-ir = { path = \"crates/martite-ir\", version = \"0.1.0\", default-features = false }"),
        "the workspace IR dependency must not enable lowering by default"
    );
}

#[test]
fn ir_evaluator_reads_only_the_ir_program() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in ["ir_eval.rs", "ir_eval/builtins.rs"] {
        let path = root.join("crates/martite-runtime/src").join(name);
        let source = fs::read_to_string(&path).expect("read IR evaluator source");
        for forbidden in [
            "martite_check",
            "CheckedProgram",
            "BindingId",
            "DefId",
            "NodeId",
            "HashMap",
            "BuiltinFn::",
            "BuiltinItem::",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name}: the IR evaluator must not use {forbidden}"
            );
        }
    }
}
