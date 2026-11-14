# IR property seed corpus

These programs are replayed by `crates/martite-conformance/tests/ir_property.rs`
without `cargo-fuzz`. Each file is a complete `fn main` script and is checked,
lowered, verified and executed with the AST-vs-IR limit sweep.

When the proptest finds a failure:

1. Let proptest shrink the opcode vector (`max_shrink_iters` is capped at 512).
2. Copy the minimized source from the failure report into this directory.
3. Trim unused bindings or helper definitions by hand if that makes the seed
   easier to read, then re-run the `ir_property` test.
4. Keep the seed in the normal workspace suite.

The generator only produces host-free programs: no `use`, no interface files,
no `std.io`/`std.env`, and no `panic`/`assert`.
