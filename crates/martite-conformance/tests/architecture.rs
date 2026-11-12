use std::fs;
use std::path::Path;

const CRATES: &[&str] = &[
    "martite-base",
    "martite-fuel",
    "martite-syntax",
    "martite-config",
    "martite-resolve",
    "martite-check",
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
