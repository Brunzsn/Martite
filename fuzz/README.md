# Martite fuzz targets

This is a separate Cargo workspace so `cargo test --workspace` does not build
libFuzzer. The `checker` target runs the lexer and the full parse → resolve →
check pipeline with bounded load limits.

```sh
cd fuzz
cargo fuzz run lexer
cargo fuzz run parser
cargo fuzz run json
cargo fuzz run checker -- -runs=100000
```

A short local run is also a useful CI smoke test:

```sh
cd fuzz
cargo fuzz run checker -- -runs=10000
```

The deterministic corpus in `corpus/checker/` is replayed by
`crates/martite-conformance/tests/fuzz_corpus.rs`, so `cargo test --workspace`
still covers every checked-in regression input without requiring `cargo-fuzz`.
