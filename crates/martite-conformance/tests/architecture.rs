use std::path::Path;

#[test]
fn workspace_is_split_into_pipeline_crates() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in [
        "martite-base",
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
    ] {
        let manifest = root.join("crates").join(name).join("Cargo.toml");
        assert!(manifest.is_file(), "missing crate manifest: {manifest:?}");
    }
}
