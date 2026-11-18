use martite_base::{LoadLimits, MemoryProvider};
use martite_driver::{LoadOptions, load, validate_project_entry};

fn script_provider() -> MemoryProvider {
    let mut provider = MemoryProvider::new();
    provider.insert("main.v1.mt", "fn main() { print(1) }\n");
    provider.insert("main/v1.mt", "fn main( {\n");
    provider
}

#[test]
fn entry_with_dots_loads_the_literal_file() {
    let provider = script_provider();
    let loaded = load(
        &provider,
        &LoadOptions {
            entry: "main.v1.mt".into(),
            edition: "1".into(),
            limits: LoadLimits::default(),
            ..LoadOptions::default()
        },
    );
    assert!(
        !loaded.has_errors(),
        "unexpected diagnostics: {:?}",
        loaded.diagnostics
    );
    let program = loaded.program.expect("checked program");
    let source = loaded
        .sources
        .get(program.resolved.entry_module().source)
        .expect("entry source");
    assert_eq!(source.name(), "main.v1.mt");
}

#[test]
fn entry_components_are_rejected_before_loading() {
    for entry in ["../main.mt", ".hidden/main.mt", "main.mt ", "dir//main.mt"] {
        let provider = script_provider();
        let loaded = load(
            &provider,
            &LoadOptions {
                entry: entry.into(),
                edition: "1".into(),
                limits: LoadLimits::default(),
                ..LoadOptions::default()
            },
        );
        assert!(loaded.has_errors(), "{entry:?}");
        assert!(
            loaded
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("relative path")),
            "{entry:?}: {:?}",
            loaded.diagnostics
        );
    }
}

#[test]
fn project_entry_stays_under_the_canonical_root() {
    let root = std::env::temp_dir().join(format!(
        "martite-driver-entry-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/main.mt"), "fn main() {}\n").unwrap();

    assert!(validate_project_entry(&root, "src/main.mt").is_ok());
    assert!(validate_project_entry(&root, "../main.mt").is_err());
    assert!(validate_project_entry(&root, ".hidden/main.mt").is_err());
    assert!(validate_project_entry(&root, "src\\main.mt").is_ok());

    let _ = std::fs::remove_dir_all(&root);
}
