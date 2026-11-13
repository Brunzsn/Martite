# Martite IR: Design Doc

**Status:** Draft (until the phase-3 performance gate in section 15 is met)
**Scope:** a resolved, self-contained tree IR between the type checker and the runtime. It must (1) execute faster than the AST interpreter with identical observable behavior, and (2) give a future optimizer a semantics contract it can actually be held to.

## 1. Summary

Today the interpreter walks the checked AST and consults `NodeId`-keyed side tables (`expr_types`, `resolutions`, `pattern_resolutions`, `LambdaInfo`). This document proposes a new crate, `martite-ir`, holding a small **tree IR**. `CheckedProgram` is lowered into it and the runtime executes it. The IR is plain data, still evaluated by a recursive evaluator, and not bytecode.

Fuel, memory and trap behavior depend on details of the current interpreter that the IR must preserve: per-site charge ordering, copy-on-write charging keyed to reference counts, memory-check placement, and dead-local retention. The IR makes these first-class: a **charge schedule** per node, an explicit **sharing rule**, **safepoints**, and **slot-liveness rules**. The work is ordered so the IR pays for itself as a faster evaluator *before* any optimizer exists.

## 2. Motivation

- **Execution cost that an IR removes** (from reading the code; confirm with a baseline, section 15):
  - `Locals` is a linear scan by `BindingId`; cost grows with live bindings.
  - Captures live in `Rc<HashMap<BindingId, Value>>`; each lambda call does an `FxHashMap` lookup for its body.
  - String literals and constants go through per-use `HashMap` lookups.
  - Each call walks `def -> module -> ast.items[i]` to find a body.
  - `NodeTable` lookups are *not* a significant cost (they are `Vec` index plus bounds check).
- **The AST is a poor rewrite target.** Semantics live in side tables keyed by `NodeId`; a rewrite must mint ids and patch every table.
- **Optimizers need facts on nodes**: charge schedule, effects, types, sharing, safepoints.
- **`lint`, `fmt`, diagnostics keep the AST.** They need parentheses, trivia and spans and are unaffected.

## 3. Goals and non-goals

**Goals**

1. Self-contained: the runtime needs no `CheckedProgram` side tables.
2. Faithful: with no passes, IR execution is observably identical to AST execution, including fuel, memory and depth *thresholds* (section 7).
3. Faster: slot-indexed frames and pre-resolved dispatch (section 15 sets the gate).
4. Facts on nodes: type, charge schedule, effects, safepoints, provenance.
5. Optimizer-ready *contract* (sections 7-9), with optimizer machinery deferred until a first pass needs it.
6. Backend-neutral: nothing in the IR depends on how it is executed.

**Non-goals**

- Choosing optimizations.
- Designing a bytecode VM (we only avoid blocking one).
- Changing language semantics, the edition fuel table, or the trap set *in the migration phases*. Deliberate semantic changes are listed in section 14 and ship separately, edition-gated.
- Replacing the AST for tooling.

## 4. Pipeline and crate layout

```
sources -> parse -> resolve -> check -> lower -> [optimize] -> runtime
                               |                  |
                         CheckedProgram        IrProgram
```

| Crate | Role | Depends on |
| --- | --- | --- |
| `martite-fuel` | **Single fuel table**: static charge constants and dynamic cost functions (`sort_cost`, string bytes `div_ceil(8)`, comparison cost, ...). Every charge site in `interp.rs` and `builtins.rs` calls it. Helpers take counts, not `Value`, so the IR can use the table without the runtime | none |
| `martite-ir` | IR types, printer, verifier, lowering from `CheckedProgram` | base, fuel, syntax, check |
| `martite-opt` | passes, pass manager (created with the first pass, section 11) | ir |
| `martite-runtime` | executes `IrProgram` | base, fuel, ir |
| `martite-driver` | `load` (unchanged) plus `lower` | base, syntax, resolve, check, ir (not runtime) |
| `martite` (embedding facade) | wires driver and runtime | as today plus ir |

Notes:

- `TypeTable`/`Ty`/`TypeId` are needed by `Value`, `show`, and the embedder. Either (a) move them into a crate below `martite-check` that both depend on, or (b) lower them into an IR-owned `TypeTable` with variant and field names. The preferred option is (b): the IR stays independent of checker internals and the runtime no longer depends on `martite-check`.
- `martite-conformance/tests/architecture.rs` gains `martite-ir` (and `martite-opt` when it exists) in `CRATES`. The bench-isolation rule applies to both.
- The AST interpreter stays until the IR evaluator passes all differential and threshold tests (section 13), then is retired in favor of recorded snapshots.

## 5. Design principles

1. **Plain data.** Enums, newtype indices, `Vec` arenas. No `Box<dyn Fn>`, no interpreter state, no references into the checker.
2. **Everything explicit.** Locals are frame slots; calls name a function index; fields are offsets; charges, safepoints and aliasing are explicit.
3. **Facts live on nodes**, so a pass reads them locally.
4. **Provenance everywhere**, including synthetic nodes.
5. **One semantics contract** (section 7) that every backend and pass must preserve, written down from what the interpreter *does*, not what the fuel table suggests.
6. **Do not desugar across a charge boundary.** Lowering and passes may only rewrite `A` into `B` if `B`'s charge schedule, safepoints and sharing behavior equal `A`'s (or the pass declares and is allowed a `Reduce` policy). Example: `x += 1` is not `x = x + 1`; they charge differently.
7. **Verifiable**, cheaply enough to run after every pass in tests.

## 6. Data model

Sketch only; names are illustrative.

```rust
pub struct IrProgram {
    pub fns: Vec<IrFn>,            // FnId; includes lambdas, handlers, tests, hooks
    pub consts: Vec<ConstValue>,   // const pool (see 6.7)
    pub strs: Vec<Box<str>>,       // static string literals, interned
    pub types: TypeTable,          // IR-owned, erased generics
    pub hosts: Vec<HostFnInfo>,    // stable name + signature + opaque-handle return type
    pub grants: Vec<String>,       // grants the program was loaded with
    pub sources: Vec<Box<str>>,    // source names, indexed by SourceId
    pub entries: Entries,          // see 6.6
}

pub struct IrFn {
    pub id: FnId,
    pub name: Box<str>,            // appears in panic traces; lambdas are "<lambda>"
    pub source: SourceId,          // per function: fixes the lambda source-file bug
    pub kind: FnKind,              // Free | Method(TypeId) | Handler | Test | Hook | Lambda | Host
    pub params: Vec<ParamInfo>,    // slot, mutability, type; self first if present
    pub locals: u32,               // frame size in slots
    pub slot_info: Vec<SlotInfo>,  // type, mutability, debug name
    pub captures: Vec<CaptureInfo>,// lambdas only: (outer slot -> capture index)
    pub nodes: Vec<Node>,          // per-function arena
    pub spans: Vec<Span>,          // side vector indexed by IrId (not inline)
    pub body: IrId,
}

pub struct Node {              // target: <= 32 bytes; children beyond two go in a shared Vec<IrId>
    pub kind: NodeKind,
    pub ty: TyId,
    pub charge: Charge,        // pre / post / per_iter, see section 8
    pub effects: Effects,      // subtree summary, see section 9
}
```

Provenance: `spans[id]` holds the source span; a `Synthetic { from: IrId }` marker lives in a parallel bit/vector only for nodes created by passes.

### 6.1 Node kinds

Complete for the current language (each kind has fixed arity and child roles):

- **Values:** int/float/bool/unit literals, `StrLit(StrId)`, `ConstRef(ConstId)`, `FnRef(FnId)`, `BuiltinRef`, `BuiltinModuleRef` (function *values*).
- **Locals:** `SlotRead { slot, mode }` with `mode = Share | Take` (section 7, rule S1), `CaptureRead(idx)`, `SlotWrite`, `SlotInit`.
- **Places:** root slot plus accessors (`Field(i)`, `Tuple(i)`, `Index(expr)`); used by assignment, compound assignment and mutable arguments. Place chains are not evaluated as values, which is why they charge only their index expressions.
- **Operators:** `Unary`, `Binary { op, class }` where `class` is `Int | Float | Str | Dyn` (`Dyn` covers generic `Eq`/`Ord` and any operand whose type is a type parameter); `And`, `Or`, `Coalesce` as short-circuit nodes.
- **Calls:** `CallDirect { fn, args }`, `CallMethod { fn, receiver, args }`, `CallValue { callee, args }` (dynamic dispatch over user fn / closure / builtin / builtin-module fn), `CallBuiltinFn`, `CallBuiltinMethod { receiver_kind, name }`, `CallBuiltinModule`, `CallHost { host, args }`. Each argument is `ArgMode::Value(expr)` or `ArgMode::MutPlace(place)` (6.4).
- **Aggregates:** `Struct { type, fields-in-decl-order, base? }`, `EnumVariant`, `PreludeVariant(None|Some|Ok|Err)`, `Tuple`, `List`, `Map`, `Range`.
- **Control flow:** `Block { stmts, tail }`, `If`, `While`, `For { iter_kind: List|Map|Str|Range }`, `Break`, `Continue`, `Return`, `Match`, `Try` (an early return from the enclosing function or lambda).
- **Statements:** `Let`, `Assign`, `CompoundAssign(op)`, `ExprStmt` (safepoint-bearing, section 8.3).
- **Conversions:** `ConvertInt`, `ConvertFloat`, `ConvertStr`.
- **Closures:** `MakeClosure { fn, captures }`.
- **Other:** `StrInterp`, `Assert`, `Panic`, `Charge(n)` (inserted only by passes under `Preserve`).

`Block` carries a flag for "lowered from an expression `{ ... }`" (entry charge 1) versus "a body" (entry charge 0), because the interpreter charges them differently.

### 6.2 Identity and ownership

- A node is owned by one function's arena and referenced by `IrId`.
- It is a **tree**: each node has one parent; no shared subtrees; a pass that wants to reuse a subtree clones it.
- Lambdas are separate `IrFn`s. A closure value is `FnId + Rc<[Value]>` captures, copied by value at creation, matching today's semantics.

### 6.3 Patterns and `match`

Initial lowering keeps `match` as ordered arms: pattern (variant index, struct field offsets, binding slots), optional guard, body. Patterns charge nothing; only guard and body expressions do.

Matching is **two-phase**: a pure *test* walks the value with no writes; on success the arm's binding slots are written, then the guard runs; if the guard fails, those slots are cleared (restored to their pre-arm state) before the next arm is tried. This reproduces today's undo log and keeps failed arms out of the memory census (rule M2). Or-patterns bind the matched alternative (confirm against `match_pattern`; `bind_pattern` binds only the first alternative today).

Decision-tree compilation is a later optimization and changes no fuel behavior, since patterns are free.

### 6.4 Mutable parameters and writeback (resolved from the code)

Evaluation of a call with `mut` arguments is, in order:

1. Evaluate receiver/argument places left to right (receiver first). Index expressions inside a place are evaluated and charged here; reading the place itself is free.
2. Read each `mut` place into the argument value with mode `Share` (a clone, so the caller's slot still references the container).
3. Run the callee. On success, each `mut` parameter is moved out of the callee frame, in parameter order.
4. After the callee frame is popped and the returned value passes its memory and value-depth checks, write back: receiver first, then arguments in order, each as a place write (root taken, path set with copy-on-write charging and a value-depth check).
5. On a trap nothing is written back.

The IR encodes this directly as `ArgMode::MutPlace(place)` plus the order above; there is no separate writeback list. Today step 2 makes the callee's first mutation copy the container (charged), see section 14, item 2. Handlers follow the same convention: state is the first (`self`) argument, and the updated state is written back to the embedder on success only.

*Open:* whether `f(mut xs, mut xs)` is rejected by the checker. The IR verifier should assert distinct roots for `mut` arguments.

### 6.5 Generics

Erased. The runtime is dynamic over `Value`; `TyId` is carried for verification and facts, not for monomorphization. Operators over a type parameter lower to `class = Dyn`.

### 6.6 Entries

```rust
pub struct Entries {
    pub main: Option<FnId>,
    pub tests: Vec<FnId>,
    pub handlers: Vec<(Box<str>, FnId)>,  // by name; state-as-self ABI per 6.4
    pub hooks: Vec<(Box<str>, HookSig)>,
    pub init: Option<FnId>,
    pub shutdown: Option<FnId>,
    pub state_type: Option<TyId>,
    pub json_type: Option<TypeId>,
    pub entry_kind: EntryKind,            // Library | Script | Plugin
}
```

### 6.7 Constants and literals

Constants and static string literals live in pools. First use in a run allocates and is accounted once; later uses share the payload and still charge their bytes (`div_ceil(len, 8)`) on every use, as today. Today the "first use" cache survives `reset()`, so accounting is history-dependent across invocations on one `Runtime`. The IR defines it as **once per `Runtime`** (preserving behavior) and flags per-invocation accounting as a candidate change (section 14).

### 6.8 Node layout (scaffold decision)

`martite-ir` pins `size_of::<Node>() == 32`: a 12-byte `NodeKind`, the 4-byte `TyId`, the 12-byte `Charge` and the 4-byte `Effects`. `NodeKind`'s variant is the operator or kind discriminator. Fixed-arity children are inline (at most two `IrId`s or one `ChildRange`); variable-arity children live in a per-function `Vec<IrId>`. The binary operand class is derived from the node and child types instead of being stored, and `Int`/`Float` literal payloads live in the const pool; both decisions keep `NodeKind` 4-byte aligned and within 12 bytes.

Spans and synthetic origins are parallel side vectors on `IrFn`. Pattern, match-arm, string-interpolation and struct-update payloads use per-function side arenas so they do not enlarge a node. The size budget is pinned by `crates/martite-ir/tests/layout.rs`.

## 7. Semantics contract

Applies to every backend and every optimizer configuration. "Observable" is stated precisely, and includes more than the final value and trap kind.

**Observable behavior**

1. Final result value, or the trap: kind, **message**, **source id**, **span**, and **trace** (the frame-name stack).
2. Order and content of `print` output and the runtime log.
3. The sequence of host calls and their arguments.
4. Fuel consumed (exactly, including the edge: fuel traps when remaining reaches exactly 0, so a limit of L allows at most L-1 units).
5. **Thresholds**: for each of fuel, memory and call depth, the smallest limit at which the program completes. Equal outcome at one limit is not enough.
6. Exception: `Deadline` is wall-clock based and excluded from determinism claims.

**Rules**

- **Evaluation order** is child order, left to right, with these documented exceptions (all from the interpreter):
  - Binary and index: both operands evaluate before the operation can trap.
  - Compound assignment: place index expressions, then the current place value, then the right-hand side.
  - Mutable arguments: places before reads; writeback after return (6.4).
  - Struct literal: `base` first, then fields in *source* order into *declaration* slots.
  - Map literal: each key then value in order; key-type errors are raised after both are evaluated.
  - Interpolated strings: parts in order; the slot-count charge is taken after all parts evaluate.
  - `for`: the iterable is evaluated once and iterated as a snapshot of its value at loop start.
- **Traps** (overflow, divide by zero, bounds, assert/panic, out of fuel, out of memory, depth, host fault) occur with the same kind at an equivalent point: every side effect before the trap in source order has happened and none after. Host calls charge fuel *after* the host returns, so a host's side effects precede an `OutOfFuel` raised by that call.
- **Short-circuit** forms (`and`, `or`, `??`, `if`, `match`) evaluate only what the language says.
- **Depth** has two parts: (a) *call depth*: one counted frame per user-function or closure call, with the check `frames.len() >= limit` applied also at host calls and builtin-invoked closures without pushing a frame; (b) *value depth*: nesting above `MAX_VALUE_DEPTH` (128) in constructed values, writes and returned values also reports `DepthExceeded`. Syntactic nesting is bounded by the parser, which bounds the evaluator's Rust recursion; the IR keeps that dependency explicit.
- **Rule S1 (sharing is semantic).** Copy-on-write is charged: mutating a list, map, struct or tuple whose reference is shared charges its slot count and accounts an allocation. Therefore the number of live references to a container at each mutation is observable through fuel and memory. A slot read of a heap-typed value is `Share` (adds a reference). A pass under `Preserve` may not add, remove, move or duplicate a heap-typed slot read, or change a slot's liveness across a mutation of the same container. `Take` (move out) is a `Reduce`-class change.
- **Rule M1 (safepoints).** `OutOfMemory` and `Deadline` may only be raised at safepoints (8.3). Safepoints are explicit and `Preserve` passes may not remove or merge them.
- **Rule M2 (liveness).** The memory census scans all *slots of all live frames* plus captures, deduplicating shared payloads. Slots are never cleared before frame exit, except an arm's binding slots on guard failure (6.3). So lowering uses one slot per binding with no reuse, and no pass may reuse, clear or elide a heap-valued slot write under `Preserve`.
- **Determinism:** same program and inputs produce the same result and the same observable sequence. No iteration over unordered containers may leak into output; `OrderedMap` is insertion-ordered.

## 8. Fuel model

### 8.1 Charge schedule

Every node carries a schedule rather than a single cost:

```rust
pub struct Charge {
    pub pre: u32,       // charged on entry, before children (1 for every evaluated expression)
    pub post: u32,      // static charge after children (e.g. list/tuple/struct/variant: element count; map: 2 * entries)
    pub per_iter: u32,  // loops: while charges before each condition check; for charges per element
}
```

plus the effect flag `DYN_COST` for charges computed at run time from values (string bytes, `show` slots, comparison cost, sort cost, copy-on-write slots, most builtins). Dynamic formulas live in `martite-fuel`, never inline in the evaluator.

Rules that lowering must follow, all derived from the interpreter:

1. Charge 1 on entry of every expression that is *evaluated*. A direct call's callee, a method call's callee `Field`, and the `Var`/`Field` chain of a place are never evaluated and charge 0. The same callee in an indirect call is evaluated and charges 1.
2. Statements `let`, assignment, `return`, `break`, `continue` and expression statements charge nothing themselves; their child expressions charge.
3. Compound assignment charges neither a Binary entry nor a read; only the operator's dynamic cost and the child expressions.
4. `while` charges `per_iter = 1` before every condition check, including the final failing one. `for` charges 1 per element, before binding.
5. Post-charges fire after children evaluate; lowering must not hoist them.

The authoritative test is differential: every fixture's exact fuel used must match the AST interpreter, and the golden `FuelRow` table in `limits.rs` must pass against the IR evaluator.

### 8.2 Charging strategies

- **Exact:** charge per entry as evaluated, calling `charge` with the same granularity as today. Required in the migration phases, because the tick counter that drives safepoint cadence advances per `charge` call (8.4).
- **Coalesced:** sum a straight-line region and charge once. Faster, but `OutOfFuel` can fire before some side effects of the region. Blocked until the cadence change in 8.4 lands; it also needs a specified, tested deviation. **Decision needed after phase 3.**

### 8.3 Safepoints

Today memory is checked: after each `let`, expression statement and assignment; on function and lambda entry; on return; and on every 256th `charge` call; the deadline on every 1024th. The IR marks these as `SAFEPOINT` on the corresponding nodes (`Let`, `Assign`, `CompoundAssign`, `ExprStmt`, function entry/exit), and the evaluator keeps the same tick counter and `& 255`/`& 1023` cadence. Memory accounting keeps its two-level scheme: a conservative `pending_bytes` estimate triggers an exact scan only when it could cross the limit.

### 8.4 Cadence decision (after migration)

Replace "every 256 charge calls" with "every 256 units of fuel consumed" (check when the running total crosses a multiple of 256). This is a deliberate semantic change: it updates the `min_memory` tests and golden thresholds once, and in exchange it makes safepoint timing independent of how charges are grouped, which unblocks Coalesced charging and node-removing passes.

### 8.5 Cost policy for passes

- `Preserve` (default): fuel, memory thresholds and sharing are all unchanged. A pass that removes a node moves its `Charge` to a surviving node or an explicit `Charge(n)`; it may not touch safepoints or heap-typed slot reads (S1, M1, M2). Fuel and the thresholds in section 7 are then identical with and without optimization.
- `Reduce` (opt-in, edition-gated): may lower fuel, memory or both. Programs that previously trapped may complete; programs that completed may not start trapping. Candidate `Reduce` changes are listed in section 14.

## 9. Effects and node facts

Each node stores an `Effects` summary of its whole subtree. It is a fact about the program, not an optimization.

| Flag | Meaning |
| --- | --- |
| `MAY_TRAP` | can raise a trap other than fuel/memory/depth/deadline |
| `READS_LOCAL`, `WRITES_LOCAL` | touches frame slots |
| `READS_HEAP`, `WRITES_HEAP` | reads or mutates aggregate data |
| `SHARES` | takes an extra reference to a heap value (heap-typed `SlotRead` with `Share`) |
| `ALLOCATES` | allocates (memory accounting) |
| `DYN_COST` | charges fuel computed from runtime values |
| `SAFEPOINT` | memory/deadline may be checked here |
| `CALLS` | calls a user function or closure whose effects are not locally known |
| `HOST` | host call (also implies a possible `HostFault`) |
| `LOGS` | prints or logs |
| `PANICS` | can panic or assert |
| `NONLOCAL_EXIT` | contains `break`, `continue`, `return` or `Try` that leaves the subtree |

Rules:

- Leaf effects come from the node kind; parent effects are the union of children plus its own.
- Stored effects must be a **superset** of true effects. The verifier recomputes and compares.
- Whole-program facts (call graph, use-def) are analyses computed on demand, not stored.

## 10. Lowering from `CheckedProgram`

Inputs: `CheckedProgram` (resolved program, `expr_types`, `resolutions`, `pattern_resolutions`, `struct_lit_fields`, `struct_pattern_fields`, `pattern_bindings`, `LambdaInfo`, `FnSig`, `ConstInfo`, entries).

1. Allocate an `FnId` for every function, method, handler, test, hook, host function and lambda; record `source` per function.
2. Assign one slot to every `BindingId` a function declares; parameters first. No reuse.
3. Walk each body once. For every expression read its type and resolution and emit a node with kind, type, charge schedule (8.1), effects and span.
4. Resolve method calls to `FnId`s and builtin methods to a builtin id plus receiver kind; resolve host functions to `HostFnInfo` (including the opaque-handle return type).
5. Lower patterns (6.3), lambdas (separate fns with capture lists), constants and static strings (pools).
6. Run the verifier.

Rules:

- **Lowering never optimizes** and never desugars across a charge boundary (principle 6).
- After lowering nothing reads the AST `NodeTable`s. Spans and source ids come from the IR.
- Lowering must be total on programs that passed the checker; a failure is an internal compiler error.
- Irrefutable destructuring (`let`, `for`, lambda parameters) is validated here or by the verifier. The interpreter's silent skipping of shape mismatches does not carry over.

## 11. Optimizer interface (built with the first pass)

The pass manager, analysis cache and rewriter are **not built up front**. They are built together with the first real pass so their shape comes from a real need. What is fixed now is the contract (sections 7-9) and these interface requirements:

```rust
pub trait FnPass {
    fn name(&self) -> &'static str;
    fn cost_policy(&self) -> CostPolicy;   // Preserve | Reduce
    fn run(&mut self, f: &mut IrFn, cx: &mut PassCx) -> Changed;
}
pub trait ProgramPass { /* same shape over IrProgram */ }
```

- `PassCx`: on-demand analyses invalidated on `Changed`; per-pass counters; active edition and policy.
- **Rewriter** (the only way passes mutate IR): sets provenance, recomputes effects bottom-up, enforces the cost policy (`carry_charge`), keeps the tree property, and **refuses** under `Preserve` to remove, duplicate or reorder `SAFEPOINT` nodes or `SHARES` reads.
- **Pass manager:** ordered list, fixpoint mode with an iteration cap, verifier after every pass in tests and debug builds, enable/disable by name, bisect limit, deterministic.
- **Dumps:** a text printer showing charge, effects and origin, and a CLI flag to dump the IR before and after optimization; golden dumps in tests.

Starter passes that are safe under `Preserve` (they do not touch sharing or safepoints): constant folding of scalar operators, branch folding on literal conditions, trivial dead-code removal of literal-only expression statements with their charge carried. Anything involving heap values needs the S1/M2 argument first.

## 12. Verifier

Runs after lowering and, in tests and debug builds, after each pass. Checks:

- All ids (`IrId`, `FnId`, slot, const, str, host) are in range; the tree property holds.
- Each node kind has the right number and roles of children; types match the kind (including `Binary` operand class).
- Call arity and argument types match the callee; `MutPlace` roots are distinct; no `mut` arguments on `CallValue`.
- `break`/`continue` only inside a loop in the same function; `return`/`Try` types match the function.
- **Definite assignment:** every slot read is dominated by a write (this replaces the interpreter's runtime "unbound local" check).
- Stored `effects` are a superset of recomputed effects; stored `charge` equals what the policy allows; safepoints present where lowering put them.
- Host calls reference only hosts whose module appears in `IrProgram.grants` (defense in depth; the resolver already enforces grants).
- Every node has a span; every function has a source id.

## 13. Testing strategy

- **Behavior snapshots first.** Before any IR code runs, record from the current interpreter, for every conformance, e2e, host, plugin, limits, traps and bench fixture: result or trap (kind, message, source, span, trace), log, host call trace, **fuel used**, and **minimum fuel, memory and depth** at which it completes (binary search, as `limits.rs` already does for memory). Check these in. They outlive the AST interpreter.
- **Differential, migration:** AST interpreter versus IR evaluator on all of the above while both exist; afterwards IR versus snapshots.
- **Golden fuel table:** the existing `FuelRow` cases run against the IR evaluator unchanged, including the copy-on-write rows.
- **Add missing cases** found in the code review: `mut`-parameter and handler-state fuel; lambda traps across modules (to pin the source-file behavior); `for` over a list the body mutates; `Try` in lambdas; compound assignment versus the written-out form; fuel exactly at the limit.
- **Verifier tests:** hand-built invalid IR per invariant.
- **Optimizer differential (when passes exist):** off versus on for every fixture and the fuzz corpus; under `Preserve`, fuel, trap and all three thresholds must match exactly.
- **Property tests:** random well-typed programs (extend `fuzz_corpus`) through lower, verify, optimize, verify, execute, with limit sweeps.
- **Benchmarks:** lowering time and IR execution reported separately from parse and check; each pass lands with a benchmark delta.

## 14. Deliberate semantic changes (not part of migration)

Each is separate, edition-gated, and lands with updated goldens. **These items are not part of the IR migration**; none is required for the IR to ship.

1. **Fuel-based safepoint cadence** (8.4). Unblocks Coalesced charging.
2. **`Take` for mutable arguments** (and handler state with a trap-safe copy). Removes the per-call container copy described in 6.4. Today's behavior is not pinned by a golden test and may be unintentional; confirm intent first.
3. **`Kill` at scope exit** for dead locals. Today a value bound in a loop body stays live until the function returns, which can raise `OutOfMemory` for programs that logically free it.
4. **Lambda source file** taken from the defining function, not the caller (existing bug, `interp.rs:616`).
5. **Per-invocation literal/constant accounting** instead of per-`Runtime` (6.7).

## 15. Migration plan

0. **Baseline.** Run `martite-bench`; record behavior snapshots (section 13). **Gate:** after phase 3, the IR evaluator must not regress any runtime bench and must show a measurable gain on `fib`, `function_calls`, `method_dispatch`, `closures` and `arithmetic_loop`. If it does not, stop before phase 5.
1. **Fuel table extraction.** Move all charge formulas into one module; golden rows stay green. No behavior change.
2. **Foundations:** IR types, printer, verifier in `martite-ir`.
3. **Lowering and IR evaluator** behind a flag, **Exact** charging and the same tick cadence. Differential and threshold tests green on all fixtures.
4. **Switch default** to the IR evaluator; runtime stops reading `NodeTable`s and drops `martite-check`; retire the AST interpreter once snapshots pass.
5. **Section 14 decisions**, one at a time, each with golden updates.
6. **First pass**, with the pass manager built alongside it; then further passes one at a time, each with a benchmark delta and an explicit cost policy.
7. **Optional:** bytecode lowering from the IR as a second backend, validated against the tree backend.

## 16. Alternatives considered

- **Fix the interpreter in place, no IR.** A dense slot map computed at check time (replacing the linear `Locals` scan), `Vec`-indexed lambda and literal caches, and a cached function-body pointer probably capture much of the speedup for a fraction of the work. The IR is still justified for the optimizer, but if the phase-3 gate shows the speedup is the only goal, this is the cheaper route. Run the baseline before committing.
- **Bytecode now.** Rejected for now: values are `Rc`-based and dynamically typed, charges are per-site and order-sensitive, and copy-on-write ties fuel to reference counts. A register bytecode would reproduce all of that without the evaluator wins that justify it.
- **A single static `cost: u32` per node.** Rejected: it cannot express pre/post/per-iteration/dynamic/reference-count charges.

## 17. Risks and open questions

- **Cadence and Coalesced charging:** decided in section 14, item 1; until then, Exact only.
- **Intent of the mutable-argument copy:** confirm with a measurement before treating it as a contract (6.4).
- **Aliasing in `mut` arguments:** confirm the checker rejects repeated roots.
- **Closure capture layout:** by-value snapshot; must match the `closures` and `captures` fixtures.
- **Stack depth:** recursive evaluation depends on the parser's `nesting_depth` bound; keep that explicit if the IR gets deeper trees than the AST (for example after inlining).
- **Edition gating:** every section 14 item and any `Reduce` pass is edition-gated; the IR shape itself is not.
- **Two representations to maintain** (AST and IR): mitigated by keeping lint and fmt on the AST and the IR strictly downstream.

## Appendix: example text dump

`IrProgram::print` and the `Printer` type emit a flat arena dump. Program facts come
first in a fixed order (source names, grants, hosts, entries, const and string
pools). Each function then prints its signature, side arenas and every node in
arena order. Children are explicit ids and unreachable nodes are still listed,
so a lowering bug that leaves an orphan in the arena is visible.

The dumps live in `crates/martite-ir/tests/printer/golden` and are regenerated
by the ignored `update_printer_goldens` test. A complete dump for the
`literals` fixture:

```text
ir-format 1
sources:
  #0 "literals.mt"
grants:
  (none)
hosts:
  (none)
entries:
  kind=script
  main=fn#0
  tests=[]
  handlers=[]
  hooks=[]
  init=-
  shutdown=-
  state=-
  json=-
consts:
  #0 int 7
  #1 float 2.5
  #2 bool true
strings:
  #0 "hello"

fn #0 "main" kind=free source=src#0("literals.mt") slots=1 body=#5
  slots:
    #0 ty=ty#0 mut=no name=-
  nodes:
    #0 int const=#0 value=7 ty=ty#0 charge=1/0/0 effects=NONE span=src#0:0..1 origin=-
    #1 float const=#1 value=2.5 ty=ty#0 charge=1/0/0 effects=NONE span=src#0:1..2 origin=-
    #2 bool value=true ty=ty#0 charge=1/0/0 effects=NONE span=src#0:2..3 origin=-
    #3 str#0("hello") ty=ty#0 charge=1/0/0 effects=NONE span=src#0:3..4 origin=-
    #4 tuple items=[#0, #1, #2, #3] ty=ty#0 charge=1/4/0 effects=ALLOCATES span=src#0:4..5 origin=-
    #5 return value=#4 ty=ty#1 charge=0/0/0 effects=NONE span=src#0:5..6 origin=-
```

Every node line ends with the same fields: the node type, the
`pre/post/per_iter` charge schedule, the effect flags in their defined bit
order (`DYN_COST` included), the source id plus byte range, and the synthetic
origin (`-` for nodes produced by lowering). Pass-created nodes and carried
charges look like this:

```text
    #2 add lhs=#0 rhs=#1 ty=ty#0 charge=1/0/0 effects=MAY_TRAP span=src#0:2..3 origin=#0
    #3 charge units=2 ty=ty#0 charge=0/0/0 effects=NONE span=src#0:3..4 origin=#2
```

Note that `call_direct`'s callee is not a node: `target=fn#N` names the callee
and charges nothing, while the call node and its arguments do.
