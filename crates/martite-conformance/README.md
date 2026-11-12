# Martite conformance suite

This crate owns the cross-cutting section 13 suite. It is intentionally
table-driven: adding a new E-code or lint rule should mean adding one row to a
table, not writing another one-off test.

| File | Coverage |
| --- | --- |
| `tests/errors.rs` | E001–E407 pass/fail fixtures and load limits |
| `tests/plugin_errors.rs` | E501–E509 plugin, manifest, and host registration |
| `tests/lint_rules.rs` | L001–L032 fail/pass fixtures, tiers, syntax-only |
| `tests/lint_exemptions.rs` | documented section 12.5/12.6 exemptions |
| `tests/lint_suppressions.rs` | section 12.3 scopes and primary spans |
| `tests/lint_config.rs` | presets, groups, severity, thresholds, overrides, globs |
| `tests/show.rs` | normative section 9.2 `Show` rows |
| `tests/limits.rs` | section 8 fuel and memory tables |
| `tests/traps.rs` | panic sites and trap variants |
| `tests/plugins.rs` | plugin state, snapshots, handles, grants, test doubles |
| `tests/grammar.rs` | section 10 parser notes |
| `tests/fuzz_corpus.rs` | deterministic replay of `fuzz/corpus/checker` |
| `tests/formatter.rs` | section 14 golden fixtures and cross-tool checks |
| `tests/bench_fixtures.rs` | deterministic benchmark corpus: parse, check, run, plugin, formatter |
| `tests/snapshots.rs` | AST-interpreter behavior snapshots and threshold oracle |

The CLI-level fix and JSON checks live in `crates/martite-cli/tests/` because
the project-atomic fix engine is part of the CLI host.

When a fixture intentionally asserts a primary span, use the source slice from
`LintRun::span_text` or `assert_diagnostic`. Span assertions are what catch
rules that report the right code at the wrong place.
