# Martite

A Rust implementation of the Martite language described in `Martite - The Rules.md`.

## Workspace

The workspace is split along the compiler pipeline:

- `martite-base` — spans, source maps, providers, diagnostics, load limits
- `martite-builtin` — canonical builtin names, item/function signatures, arities, effects and safepoints
- `martite-fuel` — edition fuel table shared by the runtime and IR
- `martite-syntax` — lexer, AST, source and `.mti` parser
- `martite-config` — `plugin.toml` and `martite.toml`
- `martite-resolve` — module graph, imports, grants, top-level names
- `martite-check` — bidirectional type checker, patterns, constants, handlers
- `martite-ir` - resolved, self-contained tree IR, re-exported canonical builtin metadata, shared tree traversal, lowering (`lower` feature), verifier and text dumps
- `martite-opt` - ordered pass manager, `Preserve` rewriter and the first constant-folding pass
- `martite-runtime` - IR evaluator, values, standard library, limits and traps; it does not depend on the checker or syntax tree
- `martite-driver` — load orchestration
- `martite` — embedding and plugin host API
- `martite-cli` — `martite` binary and standalone `std.io`/`std.env` host
- `martite-lint` — rule registry, configuration, suppression, and fixes
- `martite-fmt` — canonical formatter, comments, and layout
- `martite-conformance` — cross-cutting conformance fixtures
- `martite-bench` — Criterion benchmarks for the compiler, runtime, tools, and CLI

## Architecture

The compiler pipeline is `syntax → resolve → check → lower → IR execution`;
tooling (`lint`, `fmt`, diagnostics) stays on the AST. The resolved tree IR in
`martite-ir` is the default evaluator and the semantics contract for the
optimizer. The design is in
[`Martite_IR_Design_Doc.md`](Martite_IR_Design_Doc.md); its phase-3 performance
gate was met before the default switch.

The AST interpreter has been retired. The IR evaluator is the only runtime;
the checked-in behavior snapshots are the regression oracle, and tooling stays
on the AST.

## Commands

```sh
cargo run -p martite-cli -- run examples/hello.mt
cargo run -p martite-cli -- check examples/bad.mt
cargo run -p martite-cli -- check --json examples/bad.mt
cargo run -p martite-cli -- lower examples/hello.mt
cargo run -p martite-cli -- run --dump-ir examples/hello.mt
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

`martite lower <file>` prints the resolved IR without executing it;
`martite lower --optimize <file>` prints the optimized IR instead.

`martite run`, `martite test`, and `martite lower` verify the lowered IR and
every changed optimizer pass by default, including release builds. Embedders can
change the policy with `Engine::set_verification`; benchmarks and fuzzing use
`VerifyMode::Never` because verification has no observable effect.

`martite run` and `martite test` run optimizer passes by default. The first pass
is constant folding under the `Preserve` policy, so fuel, traps and limits are
unchanged. `--no-optimize` executes the lowered program directly. Both commands
accept `--dump-ir[=before|after|both]` and write the dump to stderr, so program
output and exit codes are unchanged. `before` is the lowered program and `after`
is the program that executes.

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
re-parses the output and compares ASTs and comment anchors before writing, and
`--stdin` reads one source file from stdin. Files with syntax errors (E001–E003)
are reported and left untouched.

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

## Workspaces and interface packages

A workspace root is a `martite.toml` with a `[workspace]` section. Each member is
a plugin project, and interface packages are directories of `.mti` files shared
by the workspace:

```toml
[workspace]
members = ["plugins/*"]
interface-packages = ["interfaces/nvlaunch"]

[lint]
preset = "recommended"
```

```text
interfaces/nvlaunch/
  interface.toml        # [interface] name = "nvlaunch"
  launcher.mti          # module nvlaunch.launcher
  host.mti              # module nvlaunch.host
```

An interface package name comes from `interface.toml`, or from the directory
name when that file is absent. Module names are the package name plus the `.mti`
path inside the package. A plugin still declares only the modules it needs:

```toml
[permissions]
grant = ["nvlaunch.launcher", "nvlaunch.host"]
```

With no path at a workspace root, `martite check`, `martite test`, and
`martite lint` operate on every member, and `martite fmt` walks the whole
workspace. An explicit path checks one member but still inherits the workspace
interfaces. `--interface` remains available as an override.

Hosts can register the same package without repeating `include_str!` pairs:

```rust
let package = martite::InterfacePackage::load("interfaces/nvlaunch")?;
engine.interface_package(&package);
```

The host should load the package from a trusted embedded or installed location;
runtime plugin manifests never choose interface file paths.

## Host policy

`martite run` is the standalone host and is intentionally unrestricted: it has
ambient filesystem access plus `std.io` and `std.env`. Limit flags still apply.

Embeddings and plugin hosts use a sandboxed `FileSystemProvider` by default.
An embedding that deliberately wants ambient access must opt in with
`FileSystemProvider::unrestricted` (and should document that decision).
`std.io`/`std.env` are not registered for embeddings automatically.

## Embedding plugins

`Plugin` is a cheap handle to an immutable loaded program, and `Instance` owns a
clone of that handle. A host can keep plugins and instances in one registry
without self-referential types, leaks, or `Box::leak`:

```rust
use martite::{Instance, Plugin};

struct Slot {
    plugin: Plugin,
    instance: Instance,
}

let plugin = engine.load(manifest, &provider)?;
let instance = plugin.instantiate()?;
let slot = Slot { plugin, instance };
```

Host functions can attribute effects to the calling plugin without thread-local
state. The caller is the manifest identity of the plugin whose code is running:

```rust
engine.host_fn("app.host.notify", signature, |cx, args| {
    let source = cx.plugin_name().unwrap_or("<unknown>");
    let version = cx.plugin_version().unwrap_or("<unknown>");
    // log, store, or queue the effect with `source`/`version`
    Ok(martite::HostValue::Unit)
});
```

Limits default to `Plugin::limits()`. A host can tighten one query hook, or give
activation and reload a larger budget, without changing the instance:

```rust
let tight = martite::Limits {
    deadline: Some(std::time::Duration::from_millis(10)),
    ..slot.plugin.limits()
};
slot.instance.call_with_limits("query", vec![], tight)?;
```

Per-call limits are clamped by the engine maximum configured with
`Engine::set_limits`. The same overrides exist for `instantiate`,
`Reload::instantiate`, and `shutdown`.

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
- the full IR-vs-snapshot differential over every snapshot case, including
  exact fuel and the minimum fuel, memory and depth thresholds;
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
