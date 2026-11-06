# Martite benchmarks

Criterion benchmarks for the lexer, parser, load/check pipeline, linter,
runtime, formatter, and CLI. The runtime also has a dedicated allocation target
using a counting global allocator.

The fixtures live in
[`crates/martite-conformance/fixtures/bench`](../martite-conformance/fixtures/bench)
and are validated by
[`tests/bench_fixtures.rs`](../martite-conformance/tests/bench_fixtures.rs).

## Running

```sh
# Build the CLI once so the startup group does not need its fallback build.
cargo build --release -p martite-cli

# Compile every benchmark without running it.
cargo bench -p martite-bench --no-run

# Run every group, including the allocation report.
cargo bench -p martite-bench

# Run one target or filter by benchmark name.
cargo bench -p martite-bench --bench runtime
cargo bench -p martite-bench -- fib
```

`martite-bench` keeps benchmark-only dependencies (`criterion` and
`stats_alloc`) out of every other crate. The CLI group looks for
`target/release/martite`, then `target/bench-cli/release/martite`, and finally
builds `martite-cli` into `target/bench-cli` on first use.

GitHub Actions runs this suite only via manual `workflow_dispatch`; it is not
triggered by pushes, pull requests, or a schedule. The workflow writes the
timing table and allocation report to the Actions job summary using
`.github/scripts/bench_summary.py`.

## Saving and comparing baselines

Criterion stores results under `target/criterion`. Save a baseline before a
performance change and compare against it afterwards:

```sh
cargo bench -p martite-bench -- --save-baseline before
# make the change
cargo bench -p martite-bench -- --baseline before
cargo bench -p martite-bench -- --baseline before --save-baseline after
```

Compare `change: [...]` in the Criterion output, not absolute times across
machines. Keep the machine idle and run the same target on the same OS and
toolchain when comparing. `--baseline` is forwarded to every Criterion target;
the allocation target ignores Criterion flags and always prints its table.

For a same-runner CI comparison, dispatch the **Benchmarks Comparison** workflow
with a baseline and candidate ref. It checks out both revisions in the same job,
runs the suite once per revision, and reports Criterion change percentages plus
allocation deltas in the job summary.

## Groups and fixtures

| Group | Benchmarks | Fixtures |
| --- | --- | --- |
| `lexer` | small, medium, large, strings with interpolation, comments | `syntax/*` |
| `parser` | source small/medium/large, `.mti` interface, deeply nested expression, malformed error recovery | `syntax/*`, `parser/*` |
| `load` | single module, multi-module, plugin with an interface | `load/*` |
| `lint` | full recommended and syntax-only, clean and noisy | `lint/*` |
| `runtime` | arithmetic loop, `fib`, function calls, list/map/string ops, method dispatch, pattern matching, closures, captures, `mut` params, plugin handler, snapshot, restore | `runtime/*` |
| `fmt` | medium, large, interface, whole golden corpus, idempotent reformat, check | `syntax/*`, `crates/martite-fmt/tests/fixtures` |
| `cli` | cold `check`, `lint`, `run`, and `fmt --check` | `syntax/small.mt`, `lint/clean.mt`, formatter fixture |
| allocation report | the runtime fixtures plus plugin handler/snapshot/restore, reported as allocations and bytes per iteration | `runtime/*` |

Every timed function uses `std::hint::black_box` for its input and output. File
I/O and program loading happen in setup unless the group is specifically
measuring loading or process startup.

## Allocation report

`benches/allocations.rs` installs `stats_alloc` as the target's global
allocator and writes a Markdown table to stdout and
`target/criterion/allocations.md`:

| benchmark | allocations | bytes | reallocations | deallocations |
| --- | ---: | ---: | ---: | ---: |

Values are per iteration after a short warmup. They are useful for seeing
AST/environment/parameter clones even when the timing signal is noisy. Treat
them as absolute counters to compare across commits, not as pass/fail limits.

## Results

No thresholds are enforced. These numbers are one local Windows run and are
only useful for comparing against a baseline on the same machine. Record a
fresh row when landing performance work.

- Machine: Intel Core i3-1005G1, 8 GB RAM, Windows 11 Home
- Rust: `rustc 1.97.1 (8bab26f4f 2026-07-14)`
- Date: 2026-11-06
- Commit: `2111ef3` (working tree with this suite)
- Command: `cargo bench -p martite-bench`

| Benchmark | Median |
| --- | ---: |
| `cli/check_small` | 64.619 ms |
| `cli/lint_small` | 56.186 ms |
| `cli/run_small` | 49.669 ms |
| `cli/fmt_check` | 53.087 ms |
| `fmt/medium` | 544.83 µs |
| `fmt/large` | 174.34 ms |
| `fmt/interface` | 45.363 µs |
| `fmt/corpus` | 1.8681 ms |
| `fmt/idempotent` | 457.32 µs |
| `fmt/check` | 579.08 µs |
| `lexer/small` | 6.4482 µs |
| `lexer/medium` | 37.579 µs |
| `lexer/large` | 3.0592 ms |
| `lexer/strings_interpolation` | 6.9832 µs |
| `lexer/comments` | 3.6999 µs |
| `lint/full_clean` | 5.9781 µs |
| `lint/syntax_only_clean` | 947.64 ns |
| `lint/full_noisy` | 13.868 µs |
| `lint/syntax_only_noisy` | 2.7218 µs |
| `load/single` | 69.050 µs |
| `load/multi_module` | 244.66 µs |
| `load/plugin_interface` | 190.50 µs |
| `parser/small` | 13.114 µs |
| `parser/medium` | 65.461 µs |
| `parser/large` | 4.5515 ms |
| `parser/interface` | 7.2030 µs |
| `parser/deep_expression` | 38.868 µs |
| `parser/error_recovery` | 4.0405 µs |
| `runtime/arithmetic_loop` | 828.73 µs |
| `runtime/fib` | 11.376 ms |
| `runtime/function_calls` | 6.6572 ms |
| `runtime/list_ops` | 4.1214 ms |
| `runtime/map_ops` | 546.53 µs |
| `runtime/string_ops` | 3.3526 ms |
| `runtime/method_dispatch` | 1.5521 ms |
| `runtime/pattern_match` | 7.4639 ms |
| `runtime/closures` | 7.4725 ms |
| `runtime/captures` | 4.6429 ms |
| `runtime/mut_params` | 11.707 ms |
| `runtime/plugin_handler` | 6.7795 µs |
| `runtime/plugin_snapshot` | 998.22 ns |
| `runtime/plugin_restore` | 1.0297 µs |

The allocation report from the same run:

| Benchmark | allocations | bytes | reallocations | deallocations |
| --- | ---: | ---: | ---: | ---: |
| `runtime/arithmetic_loop` | 19 | 3087 | 0 | 19 |
| `runtime/fib` | 67089 | 7026504 | 0 | 67089 |
| `runtime/function_calls` | 36019 | 2916984 | 0 | 36019 |
| `runtime/list_ops` | 5768 | 2449780 | 219 | 5768 |
| `runtime/map_ops` | 3878 | 220506 | 6 | 3878 |
| `runtime/string_ops` | 6340 | 1779695 | 34 | 6340 |
| `runtime/method_dispatch` | 13077 | 938433 | 0 | 13077 |
| `runtime/pattern_match` | 60164 | 5857851 | 1 | 60164 |
| `runtime/closures` | 40389 | 4569479 | 602 | 40389 |
| `runtime/captures` | 24098 | 3555621 | 402 | 24098 |
| `runtime/mut_params` | 51172 | 4510715 | 1 | 51172 |
| `runtime/plugin_handler` | 43 | 4170 | 0 | 43 |
| `runtime/plugin_snapshot` | 2 | 80 | 3 | 2 |
| `runtime/plugin_restore` | 5 | 297 | 0 | 5 |
