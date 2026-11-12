# AST interpreter snapshots

These files are the behavior oracle for the IR migration. Each line is a
deterministic record produced by the AST interpreter. The IR evaluator must
produce the same record for the same case.

The snapshots record:

- the result or trap (`kind`, message, source, line/column, span, source text,
  and frame trace);
- the runtime log;
- host calls and their arguments;
- exact fuel used;
- the minimum fuel, memory and depth budgets at which the recorded outcome is
  reproduced.

## Layout

```
v1/
  bench.jsonl
  closures.jsonl
  e2e.jsonl
  host.jsonl
  limits.jsonl
  patterns.jsonl
  plugins.jsonl
  traps.jsonl
```

Records are sorted by `id` and written with LF line endings. File paths in trap
records are fixture-relative and use `/`.

## Regenerating

Only regenerate when the AST interpreter behavior intentionally changes, or
when adding a case:

```sh
cargo test -p martite-conformance --test snapshots -- --ignored update_snapshots
```

The normal test compares the current interpreter against the checked-in files
and verifies each threshold at its boundary:

```sh
cargo test -p martite-conformance --test snapshots
```

Do not edit the JSONL files by hand.

## Thresholds

- A successful case records the smallest budget at which it still completes.
- A deterministic panic or host fault records the smallest budget at which the
  same trap is reproduced.
- Plugin scenarios use one runtime per hook, so their thresholds are the
  smallest per-hook budget that reproduces every recorded step; `fuel_used` is
  the sum across steps.
- Limit-driven outcomes (`out_of_fuel`, `out_of_memory`, `depth_exceeded`,
  `deadline`) record the input limits and `thresholds.note =
  "limit-driven outcome"`; a threshold search would be circular.
- Deadline cases are not part of the snapshot corpus because they depend on
  wall-clock timing.

## Coverage notes

- `bench` covers the executable benchmark corpus: main programs, the plugin
  handler fixture, the host-call fixture and the load/plugin fixture.
- `limits`, `traps`, `closures`, `patterns`, `e2e`, `plugins` and `host` cover
  the runtime-executable programs from those test files.
- `plugin_errors` cases assert load diagnostics (E501-E509). They do not
  produce a checked program, so they have no runtime snapshot; their executable
  cases live in `plugins.jsonl`.
