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
    for name in ["value.rs", "ops.rs", "json.rs"] {
        let path = root.join("crates/martite-runtime/src").join(name);
        let source = fs::read_to_string(&path).expect("read runtime source");
        assert!(
            !source.contains("martite_check"),
            "{name} must use the IR vocabulary, not martite-check"
        );
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
fn ir_evaluator_reads_only_the_ir_program() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let source = fs::read_to_string(root.join("crates/martite-runtime/src/ir_eval.rs"))
        .expect("read IR evaluator source");
    for forbidden in ["martite_check", "CheckedProgram", "BindingId", "HashMap"] {
        assert!(
            !source.contains(forbidden),
            "the IR evaluator must not use {forbidden}"
        );
    }
}
