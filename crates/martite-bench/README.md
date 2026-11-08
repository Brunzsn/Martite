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
cargo bench -p martite-bench --benches -- --save-baseline before
# make the change
cargo bench -p martite-bench --benches -- --baseline before
cargo bench -p martite-bench --benches -- --baseline before --save-baseline after
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
| `runtime` | arithmetic loop, `fib`, function calls, list/map/string ops, method dispatch, pattern matching, closures, captures, `mut` params, field/index assignment, `Option` lookups, map iteration, host calls, plugin handler, snapshot, restore | `runtime/*` |
| `fmt` | medium, large, interface, whole golden corpus, idempotent reformat, check | `syntax/*`, `crates/martite-fmt/tests/fixtures` |
| `cli` | cold `check`, `lint`, `run`, and `fmt --check` | `syntax/small.mt`, `lint/clean.mt`, formatter fixture |
| allocation report | the runtime fixtures plus host calls and plugin handler/snapshot/restore, reported as allocations and bytes per iteration | `runtime/*` |

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

No thresholds are enforced. These numbers are one GitHub Actions
`windows-latest` run and are only useful for comparing against a baseline on
the same runner class. Record a fresh row when landing performance work.

- Machine: GitHub Actions `windows-latest` (Windows Server 2025)
- Rust: `rustc 1.99.1 (58cf8f9c3 2026-10-13)`
- Date: 2026-11-08
- Commit: `cf1c1e9`
- Command: `cargo bench -p martite-bench` (Benchmarks workflow)

| Benchmark | Median |
| --- | ---: |
| `cli/check_small` | 6.549 ms |
| `cli/lint_small` | 6.806 ms |
| `cli/run_small` | 6.633 ms |
| `cli/fmt_check` | 6.756 ms |
| `fmt/medium` | 129.781 µs |
| `fmt/large` | 17.205 ms |
| `fmt/interface` | 16.918 µs |
| `fmt/corpus` | 741.087 µs |
| `fmt/idempotent` | 122.679 µs |
| `fmt/check` | 125.030 µs |
| `lexer/small` | 1.308 µs |
| `lexer/medium` | 7.740 µs |
| `lexer/large` | 787.607 µs |
| `lexer/strings_interpolation` | 2.784 µs |
| `lexer/comments` | 1.054 µs |
| `lint/full_clean` | 4.820 µs |
| `lint/syntax_only_clean` | 723.84 ns |
| `lint/full_noisy` | 8.657 µs |
| `lint/syntax_only_noisy` | 1.426 µs |
| `load/single` | 16.129 µs |
| `load/multi_module` | 51.620 µs |
| `load/plugin_interface` | 37.109 µs |
| `parser/small` | 6.321 µs |
| `parser/medium` | 28.961 µs |
| `parser/large` | 2.349 ms |
| `parser/interface` | 3.333 µs |
| `parser/deep_expression` | 25.870 µs |
| `parser/error_recovery` | 1.616 µs |
| `runtime/arithmetic_loop` | 440.471 µs |
| `runtime/fib` | 635.592 µs |
| `runtime/function_calls` | 1.146 ms |
| `runtime/list_ops` | 180.442 µs |
| `runtime/map_ops` | 143.571 µs |
| `runtime/string_ops` | 103.634 µs |
| `runtime/method_dispatch` | 447.691 µs |
| `runtime/pattern_match` | 803.603 µs |
| `runtime/closures` | 809.807 µs |
| `runtime/captures` | 587.687 µs |
| `runtime/mut_params` | 1.851 ms |
| `runtime/field_index_ops` | 11.134 ms |
| `runtime/option_ops` | 2.304 ms |
| `runtime/map_iteration` | 11.684 ms |
| `runtime/host_calls` | 487.549 µs |
| `runtime/plugin_handler` | 1.039 µs |
| `runtime/plugin_snapshot` | 600.04 ns |
| `runtime/plugin_restore` | 605.99 ns |

The allocation report from the same run:

| Benchmark | allocations | bytes | reallocations | deallocations |
| --- | ---: | ---: | ---: | ---: |
| `runtime/arithmetic_loop` | 0 | 0 | 0 | 0 |
| `runtime/fib` | 0 | 0 | 0 | 0 |
| `runtime/function_calls` | 0 | 0 | 0 | 0 |
| `runtime/list_ops` | 12 | 31920 | 12 | 12 |
| `runtime/map_ops` | 642 | 12282 | 1 | 642 |
| `runtime/string_ops` | 419 | 65774 | 16 | 419 |
| `runtime/method_dispatch` | 1002 | 52104 | 0 | 1002 |
| `runtime/pattern_match` | 1008 | 128680 | 0 | 1008 |
| `runtime/closures` | 1803 | 176464 | 0 | 1803 |
| `runtime/captures` | 1403 | 132320 | 0 | 1403 |
| `runtime/mut_params` | 1003 | 88272 | 0 | 1003 |
| `runtime/field_index_ops` | 45 | 4320 | 1 | 45 |
| `runtime/option_ops` | 3204 | 77776 | 0 | 3204 |
| `runtime/map_iteration` | 98313 | 5788108 | 6 | 98313 |
| `runtime/host_calls` | 4 | 1427 | 0 | 4 |
| `runtime/plugin_handler` | 5 | 1488 | 0 | 5 |
| `runtime/plugin_snapshot` | 2 | 80 | 3 | 2 |
| `runtime/plugin_restore` | 5 | 233 | 0 | 5 |
