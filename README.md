# Martite

A Rust implementation of the Martite language described in `Martite - The Rules.md`.

## Workspace

The workspace is split along the compiler pipeline:

- `martite-base` — spans, source maps, providers, diagnostics, load limits
- `martite-fuel` — edition fuel table shared by the runtime and IR
- `martite-syntax` — lexer, AST, source and `.mti` parser
- `martite-config` — `plugin.toml` and `martite.toml`
- `martite-resolve` — module graph, imports, grants, top-level names
- `martite-check` — bidirectional type checker, patterns, constants, handlers
- `martite-ir` — resolved, self-contained tree IR data model
- `martite-runtime` — tree-walking interpreter, IR-owned values, standard library, limits, traps
- `martite-driver` — load orchestration
- `martite` — embedding and plugin host API
- `martite-cli` — `martite` binary and standalone `std.io`/`std.env` host
- `martite-lint` — rule registry, configuration, suppression, and fixes
- `martite-fmt` — canonical formatter, comments, and layout
- `martite-conformance` — cross-cutting conformance fixtures
- `martite-bench` — Criterion benchmarks for the compiler, runtime, tools, and CLI

## Architecture

The compiler pipeline is `syntax → resolve → check → runtime`; tooling (`lint`,
`fmt`, diagnostics) stays on the AST. Execution is moving from the checked-AST
tree-walker to a resolved tree IR (`martite-ir`). The design and its semantics
contract are in [`Martite_IR_Design_Doc.md`](Martite_IR_Design_Doc.md); the
document stays a draft until the phase-3 performance gate is met.

## Commands

```sh
cargo run -p martite-cli -- run examples/hello.mt
cargo run -p martite-cli -- check examples/bad.mt
cargo run -p martite-cli -- check --json examples/bad.mt
cargo run -p martite-cli -- test examples/tests.mt
cargo run -p martite-cli -- check --interface examples/plugin/app/editor.mti examples/plugin
cargo run -p martite-cli -- lint examples/hello.mt
cargo run -p martite-cli -- lint --syntax-only examples/hello.mt
cargo run -p martite-cli -- lint --rule L005,L011 --deny-warnings examples/hello.mt
cargo run -p martite-cli -- lint --fix --suggested examples/hello.mt
cargo run -p martite-cli -- lint --list-rules
cargo run -p martite-cli -- lint --explain L019
cargo run -p martite-cli -- fmt examples
cargo run -p martite-cli -- fmt --check examples
cargo run -p martite-cli -- fmt --verify examples/hello.mt
cargo run -p martite-cli -- fmt --stdin < examples/hello.mt
cargo bench -p martite-bench
```

Benchmarks use the deterministic corpus in
`crates/martite-conformance/fixtures/bench`. See
[`crates/martite-bench/README.md`](crates/martite-bench/README.md) for the
groups, baselines, and allocation reporting.

Formatter settings live in `martite.toml`:

```toml
[fmt]
max_width = 100   # 40 to 200
indent = 4        # 2 or 4
end_of_line = "lf" # "lf" or "crlf"
```

`[format]` is accepted as a deprecated alias for `[fmt]`. `martite fmt` formats
`.mt` and `.mti` files in place; `--check` prints a unified diff, `--verify`
re-parses the output and compares ASTs before writing, and `--stdin` reads one
source file from stdin. Files with syntax errors (E001–E003) are reported and
left untouched.

Lint settings live in `martite.toml`:

```toml
[lint]
preset = "recommended"

[lint.severity]
L018 = "warn"
L026 = "off"

[lint.thresholds]
max_fn_lines = 60
max_params = 5
max_nesting = 4
max_branches = 12

[[lint.override]]
paths = ["tests/**"]
off = ["L008", "L018"]
```

`--fix` applies machine fixes; `--fix --suggested` also applies fixes that need a
human check. Fixes are applied through an in-memory overlay and are only written
after the whole project still type-checks.

Exit codes follow the specification: `0` success, `1` diagnostics or `main` returned `Err`,
`2` usage/load errors, and `101` for script panics.

## Host policy

`martite run` is the standalone host and is intentionally unrestricted: it has
ambient filesystem access plus `std.io` and `std.env`. Limit flags still apply.

Embeddings and plugin hosts use a sandboxed `FileSystemProvider` by default.
An embedding that deliberately wants ambient access must opt in with
`FileSystemProvider::unrestricted` (and should document that decision).
`std.io`/`std.env` are not registered for embeddings automatically.

## Tests

```sh
cargo test --workspace
```

`martite-conformance` is the cross-cutting section 13 suite:

- one failing and one passing fixture for every E-code, including E508/E509;
- one failing and one passing fixture for every lint rule, tier metadata,
  syntax-only behavior, documented exemptions, primary spans, and suppression;
- golden preset/group/severity/threshold/option/override and glob tests;
- golden `Show`, fuel, memory, depth, deadline, panic, and trap tests;
- grammar fixtures for the section 10 parser notes;
- plugin state, snapshots, handles, grants, settings, and test doubles;
- formatter idempotence, AST equality, comment survival, and suppression
  invariance;
- parse, check, run, plugin, and formatter coverage for the shared benchmark
  corpus.

## Benchmarks

The benchmark suite is a separate, unpublished workspace member. Benchmark-only
dependencies live there, never in production crates.

```sh
cargo build --release -p martite-cli   # first time only, for the CLI group
cargo bench -p martite-bench
cargo bench -p martite-bench --no-run
cargo bench -p martite-bench -- --save-baseline main
cargo bench -p martite-bench -- --baseline main
```

`cargo bench` prints a Criterion report for each compiler/runtime/tool group and
a separate allocation table. Results are informational: the suite asserts
correctness of its fixtures, not performance thresholds. A manual GitHub Actions
run writes the same overview to the job summary. See
[`crates/martite-bench/README.md`](crates/martite-bench/README.md).

The checker fuzz target lives in the separate `fuzz/` workspace:

```sh
cd fuzz
cargo fuzz run checker -- -runs=10000
```

The deterministic corpus is replayed by the normal workspace test suite, so CI
does not need `cargo-fuzz` to catch the checked-in regressions. See
`fuzz/README.md` for the other targets.
