#![no_main]

use libfuzzer_sys::fuzz_target;
use martite_base::{LoadLimits, MemoryProvider};
use martite_driver::{LoadOptions, load};

fuzz_target!(|data: &[u8]| {
    let Ok(source) = std::str::from_utf8(data) else {
        return;
    };
    // Exercise the lexer explicitly, then the whole parse → resolve → check
    // pipeline. Limits are deliberately small so the fuzzer explores the
    // load-limit paths instead of spending all its time on huge inputs.
    let limits = LoadLimits {
        file_size: 64 * 1024,
        nesting_depth: 64,
        file_count: 8,
    };
    let _ = martite_syntax::lex(source, limits);
    let mut provider = MemoryProvider::new();
    provider.insert("main.mt", source);
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
});
