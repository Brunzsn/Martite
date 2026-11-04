#![no_main]

use libfuzzer_sys::fuzz_target;
use martite_base::LoadLimits;

fuzz_target!(|data: &[u8]| {
    if let Ok(source) = std::str::from_utf8(data) {
        let limits = LoadLimits {
            file_size: 1024 * 1024,
            nesting_depth: 32,
            file_count: 256,
        };
        let _ = martite_syntax::lex(source, limits);
    }
});
