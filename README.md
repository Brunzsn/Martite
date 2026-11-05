# Martite

A Rust implementation of the Martite language described in `Martite - The Rules.md`.

## Workspace

The workspace is split along the compiler pipeline:

- `martite-base` — spans, source maps, providers, diagnostics, load limits
- `martite-syntax` — lexer, AST, source and `.mti` parser
- `martite-config` — `plugin.toml` and `martite.toml`
- `martite-resolve` — module graph, imports, grants, top-level names
- `martite-check` — bidirectional type checker, patterns, constants, handlers
- `martite-runtime` — tree-walking interpreter, standard library, limits, traps
- `martite-driver` — load orchestration
- `martite` — embedding and plugin host API
- `martite-cli` — `martite` binary and standalone `std.io`/`std.env` host
- `martite-lint` — rule registry, configuration, suppression, and fixes
- `martite-fmt` — canonical formatter, comments, and layout
- `martite-conformance` — cross-cutting conformance fixtures

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
```

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

The conformance crate covers representative failures for E001–E508, the normative `Show`
format, runtime panics, fuel/memory limits, plugin state, snapshots, and host grants.
The lint crate covers syntax and checked-tier rules, suppression and config precedence,
project-level fixes, and the `martite lint` CLI. The formatter crate covers section 14
golden fixtures, idempotence, AST equality, comment placement, and the `martite fmt` CLI.
