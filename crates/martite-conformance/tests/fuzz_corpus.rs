use std::path::Path;

use martite_base::{LoadLimits, MemoryProvider};
use martite_driver::{LoadOptions, load};

#[test]
fn checker_corpus_replays_without_panicking() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/checker");
    let limits = LoadLimits {
        file_size: 64 * 1024,
        nesting_depth: 64,
        file_count: 8,
    };
    let mut files = 0;
    for entry in std::fs::read_dir(&root).expect("read corpus") {
        let path = entry.expect("corpus entry").path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("mt") {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read corpus file");
        let _ = martite_syntax::lex(&source, limits);
        let mut provider = MemoryProvider::new();
        provider.insert("main.mt", source.as_str());
        let _ = load(
            &provider,
            &LoadOptions {
                entry: "main.mt".into(),
                interfaces: Vec::new(),
                grants: Vec::new(),
                limits,
                edition: "1".into(),
            },
        );
        files += 1;
    }
    assert!(files >= 4, "expected a small checker corpus, found {files}");
}
