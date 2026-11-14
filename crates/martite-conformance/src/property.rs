//! Property-test harness for lowering, verification and IR execution (#113).
//!
//! Programs are generated from a dense opcode stream so proptest can shrink a
//! failing case. The generator only emits a well-typed, host-free subset of
//! Martite: no `use`, no interfaces, no `std.io`/`std.env`, no `panic`/`assert`
//! and no unbounded loops. Every generated program carries `fn main()`.
//!
//! For each source, the harness checks the program, lowers it twice, verifies
//! the result, proves that a deliberately corrupted clone is rejected, and
//! executes the AST oracle and the IR at a limit sweep. The AST comparison is
//! temporary: #116 retires the AST interpreter, and the IR-only invariants
//! remain valid afterwards.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use martite_base::{Severity, SourceMap};
use martite_check::CheckedProgram;
use martite_ir::{FnId, IrProgram, LowerOptions, TypeTable, lower_with, verify};
use martite_runtime::{ExecLimits, IrRuntime, Runtime};

use crate::Case;
use crate::snapshot::{OutcomeRecord, outcome_from_result};

/// Number of proptest cases in the normal workspace suite.
pub const PROPERTY_CASES: u32 = 48;

/// Fuel, memory and depth caps used for a generated program. A generated
/// program is deliberately tiny; these caps are much larger than its actual
/// needs and keep the harness itself bounded.
pub const BASELINE_FUEL: u64 = 50_000;
pub const BASELINE_MEMORY: u64 = 1 << 20;
pub const BASELINE_DEPTH: u32 = 128;

/// Per-execution deadline. A generated program that hits this is skipped
/// rather than treated as a failure, so a slow runner cannot make the suite
/// flaky.
const EXECUTION_DEADLINE: Duration = Duration::from_millis(500);

/// Whole-program wall-clock cap. The limit sweep re-runs the program many
/// times; a program that cannot finish the sweep is skipped.
const WALL_CLOCK_CAP: Duration = Duration::from_secs(2);

/// Outcome of one checked source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyOutcome {
    Passed { fuel_used: u64, limit_checks: usize },
    Skipped { reason: String },
}

/// A property failure with enough context to replay and diagnose it without
/// the original proptest session.
#[derive(Debug, Clone)]
pub struct PropertyFailure {
    pub stage: &'static str,
    pub source: String,
    pub details: Vec<String>,
    pub ir_dump: Option<String>,
}

impl PropertyFailure {
    fn new(stage: &'static str, source: &str, detail: impl Into<String>) -> Self {
        Self {
            stage,
            source: source.into(),
            details: vec![detail.into()],
            ir_dump: None,
        }
    }

    fn with_dump(mut self, dump: String) -> Self {
        self.ir_dump = Some(dump);
        self
    }
}

impl fmt::Display for PropertyFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "property failure at {}", self.stage)?;
        writeln!(f, "\n--- source ---\n{}", self.source)?;
        writeln!(f, "--- details ---")?;
        for detail in &self.details {
            writeln!(f, "{detail}")?;
        }
        if let Some(dump) = &self.ir_dump {
            writeln!(f, "--- IR dump ---\n{dump}")?;
        }
        Ok(())
    }
}

impl std::error::Error for PropertyFailure {}

/// Check one generated or checked-in source.
///
/// The returned failure includes the source and the IR dump. `Skipped` means
/// the program exceeded the deadline or the harness wall-clock cap; it is not
/// a pass and is reported by the tests.
pub fn check_source(source: &str) -> Result<PropertyOutcome, PropertyFailure> {
    let start = Instant::now();
    let case = Case::single(source);
    let loaded = case.load();
    if loaded
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        let rendered = loaded
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.render(&loaded.sources))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(PropertyFailure::new(
            "check",
            source,
            format!("generated source must check cleanly:\n{rendered}"),
        ));
    }
    let Some(program) = loaded.program else {
        return Err(PropertyFailure::new(
            "check",
            source,
            "checker produced no program",
        ));
    };
    let sources = loaded.sources;

    if timed_out(start) {
        return Ok(skipped("wall-clock cap after check"));
    }

    let first = lower_once(&program, &sources, source)?;
    if let Err(error) = verify(&first) {
        return Err(PropertyFailure::new(
            "verify",
            source,
            format!("valid lowered IR was rejected: {error}"),
        )
        .with_dump(print_ir(&first)));
    }
    let dump = print_ir(&first);

    let second = lower_once(&program, &sources, source)?;
    if first != second || dump != print_ir(&second) {
        return Err(PropertyFailure::new(
            "lower determinism",
            source,
            "the same source lowered to different IR or a different dump",
        )
        .with_dump(dump));
    }
    let ir = first;

    verifier_rejects_corrupted_ir(&ir, source, dump.clone())?;
    if timed_out(start) {
        return Ok(skipped("wall-clock cap after lowering and verification"));
    }

    let ast_types = TypeTable::from_checker(&program);
    let baseline = baseline_limits();
    let first_ast = observe_ast(&program, &sources, &ast_types, baseline);
    let first_ir = observe_ir(&ir, &sources, baseline);
    if deadline(&first_ast) || deadline(&first_ir) {
        return Ok(skipped("deadline at baseline limits"));
    }
    if first_ast != first_ir {
        return Err(PropertyFailure::new(
            "baseline AST-vs-IR",
            source,
            format!("baseline observations differ\nAST: {first_ast:?}\nIR:  {first_ir:?}"),
        )
        .with_dump(dump));
    }
    let baseline_outcome = first_ir.outcome.clone();
    if !matches!(baseline_outcome, OutcomeRecord::Value { .. }) {
        return Err(PropertyFailure::new(
            "baseline execution",
            source,
            format!("generated program must complete successfully at baseline limits, got {baseline_outcome:?}"),
        )
        .with_dump(dump));
    }
    let fuel_used = first_ir.fuel_used;

    let sweep = limit_sweep(fuel_used);
    let mut checks = 0usize;
    for (name, limits) in sweep {
        if timed_out(start) {
            return Ok(skipped(format!(
                "wall-clock cap during limit sweep at `{name}`"
            )));
        }
        let ast = observe_ast(&program, &sources, &ast_types, limits);
        let ir_first = observe_ir(&ir, &sources, limits);
        let ir_second = observe_ir(&ir, &sources, limits);
        if deadline(&ast) || deadline(&ir_first) || deadline(&ir_second) {
            return Ok(skipped(format!("deadline during limit sweep at `{name}`")));
        }
        if ast != ir_first {
            return Err(PropertyFailure::new(
                "AST-vs-IR limit sweep",
                source,
                format!("`{name}` differs\nlimits: {limits:?}\nAST: {ast:?}\nIR:  {ir_first:?}"),
            )
            .with_dump(dump));
        }
        if ir_first != ir_second {
            return Err(PropertyFailure::new(
                "IR determinism",
                source,
                format!(
                    "`{name}` differs between two runs\nlimits: {limits:?}\nfirst:  {ir_first:?}\nsecond: {ir_second:?}"
                ),
            )
            .with_dump(dump));
        }
        if let Some(kind) = non_limit_trap(&ir_first) {
            return Err(PropertyFailure::new(
                "limit sweep trap",
                source,
                format!(
                    "`{name}` trapped with `{kind}` at {limits:?}; generated programs are panic-free"
                ),
            )
            .with_dump(dump));
        }
        if name == "fuel below minimum" {
            match &ir_first.outcome {
                OutcomeRecord::Trap { trap } if trap.kind == "out_of_fuel" => {}
                other => {
                    return Err(PropertyFailure::new(
                        "fuel boundary",
                        source,
                        format!(
                            "`{name}` must trap OutOfFuel at fuel={} (baseline used {fuel_used}), got {other:?}",
                            limits.fuel
                        ),
                    )
                    .with_dump(dump));
                }
            }
        }
        if name == "fuel at minimum" && !matches!(ir_first.outcome, OutcomeRecord::Value { .. }) {
            return Err(PropertyFailure::new(
                "fuel boundary",
                source,
                format!(
                    "`{name}` must complete at fuel={} (baseline used {fuel_used}), got {:?}",
                    limits.fuel, ir_first.outcome
                ),
            )
            .with_dump(dump));
        }
        checks += 1;
    }

    Ok(PropertyOutcome::Passed {
        fuel_used,
        limit_checks: checks,
    })
}

fn lower_once(
    program: &CheckedProgram,
    sources: &SourceMap,
    source: &str,
) -> Result<IrProgram, PropertyFailure> {
    match catch_unwind(AssertUnwindSafe(|| {
        lower_with(program, sources, LowerOptions { verify: false })
    })) {
        Ok(Ok(ir)) => Ok(ir),
        Ok(Err(error)) => Err(PropertyFailure::new(
            "lower",
            source,
            format!("lowering failed: {error}"),
        )),
        Err(payload) => Err(PropertyFailure::new(
            "lower panic",
            source,
            format!("lowering panicked: {}", panic_message(&payload)),
        )),
    }
}

fn verifier_rejects_corrupted_ir(
    ir: &IrProgram,
    source: &str,
    dump: String,
) -> Result<(), PropertyFailure> {
    let Some(main) = ir.entries.main else {
        return Ok(());
    };
    let mut broken = ir.clone();
    broken.entries.main = Some(FnId::new(broken.fns.len() as u32 + 1));
    match catch_unwind(AssertUnwindSafe(|| verify(&broken))) {
        Ok(Err(_)) => Ok(()),
        Ok(Ok(())) => Err(PropertyFailure::new(
            "verifier negative case",
            source,
            format!(
                "verifier accepted corrupted IR: main entry {main:?} was replaced out of range"
            ),
        )
        .with_dump(dump)),
        Err(payload) => Err(PropertyFailure::new(
            "verifier panic",
            source,
            format!(
                "verifier panicked on corrupted IR: {}",
                panic_message(&payload)
            ),
        )
        .with_dump(dump)),
    }
}

fn print_ir(ir: &IrProgram) -> String {
    match catch_unwind(AssertUnwindSafe(|| ir.print())) {
        Ok(dump) => dump,
        Err(payload) => format!("<IR dump panicked: {}>", panic_message(&payload)),
    }
}

fn panic_message(payload: &Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).into()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "<non-string panic payload>".into()
    }
}

fn baseline_limits() -> ExecLimits {
    ExecLimits {
        fuel: BASELINE_FUEL,
        memory: BASELINE_MEMORY,
        depth: BASELINE_DEPTH,
        deadline: Some(EXECUTION_DEADLINE),
    }
}

fn skipped(reason: impl Into<String>) -> PropertyOutcome {
    PropertyOutcome::Skipped {
        reason: reason.into(),
    }
}

fn timed_out(start: Instant) -> bool {
    start.elapsed() > WALL_CLOCK_CAP
}

fn deadline(observation: &RunObservation) -> bool {
    matches!(&observation.outcome, OutcomeRecord::Trap { trap } if trap.kind == "deadline")
}

fn non_limit_trap(observation: &RunObservation) -> Option<&str> {
    let OutcomeRecord::Trap { trap } = &observation.outcome else {
        return None;
    };
    match trap.kind.as_str() {
        "out_of_fuel" | "out_of_memory" | "depth_exceeded" | "deadline" => None,
        other => Some(other),
    }
}

/// A small sweep around the exact fuel threshold, plus low memory and depth.
fn limit_sweep(fuel_used: u64) -> Vec<(&'static str, ExecLimits)> {
    let baseline = baseline_limits();
    let mut sweep = vec![
        (
            "fuel at minimum",
            ExecLimits {
                fuel: fuel_used.saturating_add(1),
                ..baseline
            },
        ),
        (
            "fuel half",
            ExecLimits {
                fuel: (fuel_used / 2).max(1),
                ..baseline
            },
        ),
        (
            "fuel one",
            ExecLimits {
                fuel: 1,
                ..baseline
            },
        ),
        (
            "memory low",
            ExecLimits {
                memory: baseline.memory / 8,
                ..baseline
            },
        ),
        (
            "memory zero",
            ExecLimits {
                memory: 0,
                ..baseline
            },
        ),
        (
            "depth one",
            ExecLimits {
                depth: 1,
                ..baseline
            },
        ),
        (
            "depth zero",
            ExecLimits {
                depth: 0,
                ..baseline
            },
        ),
    ];
    if fuel_used > 0 {
        sweep.insert(
            1,
            (
                "fuel below minimum",
                ExecLimits {
                    fuel: fuel_used,
                    ..baseline
                },
            ),
        );
    }
    sweep
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RunObservation {
    outcome: OutcomeRecord,
    log: Vec<String>,
    fuel_used: u64,
}

fn observe_ast(
    program: &CheckedProgram,
    sources: &SourceMap,
    types: &TypeTable,
    limits: ExecLimits,
) -> RunObservation {
    let mut runtime = Runtime::new(program, limits);
    let result = runtime.run_main();
    let log = match &result {
        Ok(main) => main.log.clone(),
        Err(_) => runtime.take_log(),
    };
    let fuel_used = runtime.fuel_used();
    RunObservation {
        outcome: outcome_from_result(result, sources, types),
        log,
        fuel_used,
    }
}

fn observe_ir(ir: &IrProgram, sources: &SourceMap, limits: ExecLimits) -> RunObservation {
    let mut runtime = IrRuntime::new(ir, limits);
    let result = runtime.run_main();
    let log = match &result {
        Ok(main) => main.log.clone(),
        Err(_) => runtime.take_log(),
    };
    let fuel_used = runtime.fuel_used();
    RunObservation {
        outcome: outcome_from_result(result, sources, &ir.types),
        log,
        fuel_used,
    }
}

// ---------------------------------------------------------------------------
// Source generator
// ---------------------------------------------------------------------------

const PRELUDE: &str = r#"const BASE: int = 7

struct Pair { left: int, right: int }

impl Pair {
    fn total(self) -> int { self.left + self.right }
}

enum Token { Number(int), Word(str), Empty }

fn add(a: int, b: int) -> int { a + b }

fn sum_to(n: int) -> int {
    var total = 0
    var i = 0
    while i < n {
        i += 1
        total += i
    }
    total
}

fn bump(mut value: int, amount: int) -> int {
    value += amount
    value
}

fn maybe(x: int) -> int? {
    let value: int? = if x % 2 == 0 { Some(x) } else { None }
    value
}

fn double_maybe(x: int) -> int? {
    let value = maybe(x)?
    Some(value * 2)
}

fn need_positive(x: int) -> Result[int, str] {
    let value: Result[int, str] = if x >= 0 { Ok(x) } else { Err("negative") }
    value
}

fn try_positive(x: int) -> Result[int, str] {
    let value = need_positive(x)?
    Ok(value + 1)
}

fn rec(n: int) -> int {
    if n <= 0 { 0 } else { n + rec(n - 1) }
}
"#;

const STRINGS: &[&str] = &["", "a", "ab", "hello", "x y", "42", "Martite"];

/// Generate one well-typed, host-free program from an opcode stream.
///
/// The stream is consumed deterministically. Running out of opcodes simply
/// stops adding statements, so an empty stream still produces a valid program.
pub fn generate_program(ops: &[u64]) -> String {
    let mut generator = Generator {
        rng: Rng { ops, at: 0 },
        next_name: 0,
        out: String::new(),
    };
    generator.out.push_str(PRELUDE);
    generator.out.push_str("\nfn main() {\n");
    let mut env = Env::default();
    let count = 1 + generator.rng.index(8);
    for _ in 0..count {
        generator.gen_stmt(&mut env, 1, 0);
    }
    generator.out.push_str("}\n");
    generator.out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ty {
    Int,
    Bool,
    Str,
    Float,
    ListInt,
    MapStrInt,
    TupleIntStr,
    Pair,
    Token,
    OptInt,
    ResIntStr,
}

impl Ty {
    const ALL: [Ty; 11] = [
        Ty::Int,
        Ty::Bool,
        Ty::Str,
        Ty::Float,
        Ty::ListInt,
        Ty::MapStrInt,
        Ty::TupleIntStr,
        Ty::Pair,
        Ty::Token,
        Ty::OptInt,
        Ty::ResIntStr,
    ];

    const MATCHABLE: [Ty; 7] = [
        Ty::Int,
        Ty::Str,
        Ty::Pair,
        Ty::TupleIntStr,
        Ty::Token,
        Ty::OptInt,
        Ty::ResIntStr,
    ];

    fn name(self) -> &'static str {
        match self {
            Ty::Int => "int",
            Ty::Bool => "bool",
            Ty::Str => "str",
            Ty::Float => "float",
            Ty::ListInt => "list[int]",
            Ty::MapStrInt => "map[str, int]",
            Ty::TupleIntStr => "(int, str)",
            Ty::Pair => "Pair",
            Ty::Token => "Token",
            Ty::OptInt => "int?",
            Ty::ResIntStr => "Result[int, str]",
        }
    }
}

#[derive(Debug, Clone)]
struct Var {
    name: String,
    ty: Ty,
    mutable: bool,
}

#[derive(Debug, Clone, Default)]
struct Env {
    vars: Vec<Var>,
}

impl Env {
    fn add(&mut self, name: String, ty: Ty, mutable: bool) {
        self.vars.push(Var { name, ty, mutable });
    }

    fn matching(&self, ty: Ty, mutable: Option<bool>) -> Vec<Var> {
        self.vars
            .iter()
            .filter(|var| var.ty == ty && mutable.is_none_or(|wanted| var.mutable == wanted))
            .cloned()
            .collect()
    }
}

struct Rng<'a> {
    ops: &'a [u64],
    at: usize,
}

impl Rng<'_> {
    fn next(&mut self) -> u64 {
        let value = self.ops.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        value
    }

    fn index(&mut self, len: usize) -> usize {
        if len == 0 {
            0
        } else {
            (self.next() as usize) % len
        }
    }

    fn bool(&mut self) -> bool {
        self.next() & 1 == 1
    }

    fn int(&mut self, low: i64, high: i64) -> i64 {
        debug_assert!(low <= high);
        let width = (high - low + 1) as u64;
        low + (self.next() % width) as i64
    }
}

struct Generator<'a> {
    rng: Rng<'a>,
    next_name: u32,
    out: String,
}

impl Generator<'_> {
    fn fresh(&mut self, prefix: &str) -> String {
        let index = self.next_name;
        self.next_name += 1;
        format!("{prefix}{index}")
    }

    fn pick_var(&mut self, env: &Env, ty: Ty, mutable: Option<bool>) -> Option<String> {
        let vars = env.matching(ty, mutable);
        if vars.is_empty() {
            None
        } else {
            Some(vars[self.rng.index(vars.len())].name.clone())
        }
    }

    fn line(&mut self, indent: usize, text: &str) {
        for _ in 0..indent {
            self.out.push_str("    ");
        }
        self.out.push_str(text);
        self.out.push('\n');
    }

    fn gen_stmt(&mut self, env: &mut Env, indent: usize, depth: u32) {
        let choice = self.rng.index(100);
        if depth >= 2 {
            return self.gen_simple_stmt(env, indent);
        }
        match choice {
            0..=24 => self.gen_bind(env, indent, depth),
            25..=38 => self.gen_print(env, indent, depth),
            39..=51 => self.gen_if(env, indent, depth),
            52..=62 => self.gen_while(env, indent, depth),
            63..=71 => self.gen_for(env, indent, depth),
            72..=78 => self.gen_for_enumerate(env, indent, depth),
            79..=87 => self.gen_match_stmt(env, indent, depth),
            _ => self.gen_simple_stmt(env, indent),
        }
    }

    fn gen_simple_stmt(&mut self, env: &mut Env, indent: usize) {
        match self.rng.index(6) {
            0 => self.gen_assign(env, indent),
            1 => self.gen_mut_call(env, indent),
            2 => self.gen_list_mutation(env, indent),
            3 => self.gen_struct_mutation(env, indent),
            4 => self.gen_discard(env, indent, 2),
            _ => {
                let ty = Ty::ALL[self.rng.index(Ty::ALL.len())];
                let mut mutable = self.rng.bool();
                if ty == Ty::Float && mutable {
                    mutable = false;
                }
                let name = self.fresh("v");
                let expr = self.gen_expr(ty, env, 3);
                self.line(
                    indent,
                    &format!(
                        "{} {name}: {} = {expr}",
                        if mutable { "var" } else { "let" },
                        ty.name()
                    ),
                );
                env.add(name, ty, mutable);
            }
        }
    }

    fn gen_bind(&mut self, env: &mut Env, indent: usize, depth: u32) {
        let ty = Ty::ALL[self.rng.index(Ty::ALL.len())];
        let mutable = self.rng.bool();
        let name = self.fresh("v");
        let expr = self.gen_expr(ty, env, depth + 1);
        self.line(
            indent,
            &format!(
                "{} {name}: {} = {expr}",
                if mutable { "var" } else { "let" },
                ty.name()
            ),
        );
        env.add(name, ty, mutable);
    }

    fn gen_print(&mut self, env: &mut Env, indent: usize, depth: u32) {
        let ty = Ty::ALL[self.rng.index(Ty::ALL.len())];
        let expr = self.gen_expr(ty, env, depth + 1);
        let name = self.fresh("v");
        self.line(indent, &format!("let {name}: {} = {expr}", ty.name()));
        match ty {
            Ty::Int | Ty::Bool | Ty::Float | Ty::OptInt | Ty::ResIntStr | Ty::Token => {
                self.line(indent, &format!("print(\"{{{name}}}\")"));
            }
            _ => self.line(indent, &format!("print({name})")),
        }
    }

    fn gen_discard(&mut self, env: &Env, indent: usize, depth: u32) {
        let ty = Ty::ALL[self.rng.index(Ty::ALL.len())];
        let expr = self.gen_expr(ty, env, depth);
        self.line(indent, &format!("let _: {} = {expr}", ty.name()));
    }

    fn gen_if(&mut self, env: &mut Env, indent: usize, depth: u32) {
        let cond = self.gen_expr(Ty::Bool, env, depth + 1);
        self.line(indent, &format!("if {cond} {{"));
        self.gen_block(env, indent + 1, depth + 1);
        self.line(indent, "} else {");
        self.gen_block(env, indent + 1, depth + 1);
        self.line(indent, "}");
    }

    fn gen_while(&mut self, env: &mut Env, indent: usize, depth: u32) {
        let index = self.fresh("i");
        let bound = self.rng.int(1, 4);
        self.line(indent, &format!("var {index}: int = 0"));
        self.line(indent, &format!("while {index} < {bound} {{"));
        self.line(indent + 1, &format!("{index} += 1"));
        // The body cannot see the counter: a generated assignment or mutable
        // call could otherwise make the loop diverge.
        self.gen_block(env, indent + 1, depth + 1);
        self.line(indent, "}");
        env.add(index, Ty::Int, true);
    }

    fn gen_for(&mut self, env: &mut Env, indent: usize, depth: u32) {
        let list = self.gen_expr(Ty::ListInt, env, depth + 1);
        let item = self.fresh("v");
        self.line(indent, &format!("for {item} in {list} {{"));
        let mut child = env.clone();
        child.add(item, Ty::Int, false);
        self.gen_block(&child, indent + 1, depth + 1);
        self.line(indent, "}");
    }

    fn gen_for_enumerate(&mut self, env: &mut Env, indent: usize, depth: u32) {
        let list = self.gen_expr(Ty::ListInt, env, depth + 1);
        let index = self.fresh("i");
        let item = self.fresh("v");
        self.line(
            indent,
            &format!("for ({index}, {item}) in {list}.enumerate() {{"),
        );
        let mut child = env.clone();
        child.add(index, Ty::Int, false);
        child.add(item, Ty::Int, false);
        self.gen_block(&child, indent + 1, depth + 1);
        self.line(indent, "}");
    }

    fn gen_block(&mut self, env: &Env, indent: usize, depth: u32) {
        let mut child = env.clone();
        let count = 1 + self.rng.index(2);
        for _ in 0..count {
            self.gen_stmt(&mut child, indent, depth);
        }
    }

    fn gen_match_stmt(&mut self, env: &mut Env, indent: usize, depth: u32) {
        let ty = Ty::MATCHABLE[self.rng.index(Ty::MATCHABLE.len())];
        let scrutinee = self.gen_expr(ty, env, depth + 1);
        let name = self.fresh("v");
        self.line(indent, &format!("let {name}: {} = {scrutinee}", ty.name()));
        self.line(indent, &format!("match ({name}) {{"));
        for (pattern, bindings) in self.match_arms(ty) {
            self.line(indent + 1, &format!("{pattern} => {{"));
            let mut child = env.clone();
            for (name, ty) in bindings {
                child.add(name, ty, false);
            }
            self.gen_block(&child, indent + 2, depth + 1);
            self.line(indent + 1, "}");
        }
        self.line(indent, "}");
    }

    fn match_arms(&mut self, ty: Ty) -> Vec<(String, Vec<(String, Ty)>)> {
        match ty {
            Ty::Int => vec![("0".into(), Vec::new()), ("_".into(), Vec::new())],
            Ty::Str => vec![("\"a\"".into(), Vec::new()), ("_".into(), Vec::new())],
            Ty::Pair => {
                let left = self.fresh("m");
                let right = self.fresh("m");
                vec![(
                    format!("Pair {{ left: {left}, right: {right} }}"),
                    vec![(left, Ty::Int), (right, Ty::Int)],
                )]
            }
            Ty::TupleIntStr => {
                let first = self.fresh("m");
                let second = self.fresh("m");
                vec![(
                    format!("({first}, {second})"),
                    vec![(first, Ty::Int), (second, Ty::Str)],
                )]
            }
            Ty::Token => {
                let guard_name = self.fresh("m");
                let number = self.fresh("m");
                let word = self.fresh("m");
                vec![
                    (
                        format!("Number({guard_name}) if {guard_name} > 0"),
                        vec![(guard_name, Ty::Int)],
                    ),
                    (format!("Number({number})"), vec![(number, Ty::Int)]),
                    (format!("Word({word})"), vec![(word, Ty::Str)]),
                    ("Empty".into(), Vec::new()),
                ]
            }
            Ty::OptInt => {
                let value = self.fresh("m");
                vec![
                    (format!("Some({value})"), vec![(value, Ty::Int)]),
                    ("None".into(), Vec::new()),
                ]
            }
            Ty::ResIntStr => {
                let value = self.fresh("m");
                let error = self.fresh("m");
                vec![
                    (format!("Ok({value})"), vec![(value, Ty::Int)]),
                    (format!("Err({error})"), vec![(error, Ty::Str)]),
                ]
            }
            _ => vec![("_".into(), Vec::new())],
        }
    }

    fn gen_assign(&mut self, env: &mut Env, indent: usize) {
        if let Some(name) = self.pick_var(env, Ty::Int, Some(true)) {
            if self.rng.bool() {
                let expr = self.gen_expr(Ty::Int, env, 2);
                self.line(indent, &format!("{name} += {expr}"));
            } else {
                let expr = self.gen_expr(Ty::Int, env, 2);
                self.line(indent, &format!("{name} = {expr}"));
            }
        } else if let Some(name) = self.pick_var(env, Ty::Str, Some(true)) {
            let expr = self.gen_expr(Ty::Str, env, 2);
            self.line(indent, &format!("{name} = {expr}"));
        } else if let Some(name) = self.pick_var(env, Ty::ListInt, Some(true)) {
            let expr = self.gen_expr(Ty::ListInt, env, 2);
            self.line(indent, &format!("{name} = {expr}"));
        } else {
            self.gen_discard(env, indent, 2);
        }
    }

    fn gen_mut_call(&mut self, env: &mut Env, indent: usize) {
        let Some(name) = self.pick_var(env, Ty::Int, Some(true)) else {
            self.gen_discard(env, indent, 2);
            return;
        };
        let amount = self.rng.int(-4, 4);
        self.line(indent, &format!("{name} = bump(mut {name}, {amount})"));
    }

    fn gen_list_mutation(&mut self, env: &mut Env, indent: usize) {
        let Some(name) = self.pick_var(env, Ty::ListInt, Some(true)) else {
            self.gen_discard(env, indent, 2);
            return;
        };
        match self.rng.index(5) {
            0 => {
                let value = self.rng.int(-5, 5);
                self.line(indent, &format!("{name}.push({value})"));
            }
            1 => self.line(indent, &format!("let _ = {name}.pop()")),
            2 => self.line(indent, &format!("{name}.sort()")),
            3 => self.line(indent, &format!("{name}.clear()")),
            _ => {
                let list = self.gen_expr(Ty::ListInt, env, 2);
                self.line(indent, &format!("{name}.extend({list})"));
            }
        }
    }

    fn gen_struct_mutation(&mut self, env: &mut Env, indent: usize) {
        let Some(name) = self.pick_var(env, Ty::Pair, Some(true)) else {
            self.gen_discard(env, indent, 2);
            return;
        };
        let value = self.rng.int(-5, 5);
        if self.rng.bool() {
            self.line(indent, &format!("{name}.left += {value}"));
        } else {
            self.line(indent, &format!("{name}.left = {value}"));
        }
    }

    fn gen_expr(&mut self, ty: Ty, env: &Env, depth: u32) -> String {
        if depth == 0 {
            return self.literal(ty);
        }
        match ty {
            Ty::Int => self.gen_int(env, depth),
            Ty::Bool => self.gen_bool(env, depth),
            Ty::Str => self.gen_str(env, depth),
            Ty::Float => self.gen_float(env, depth),
            Ty::ListInt => self.gen_list(env, depth),
            Ty::MapStrInt => self.gen_map(env, depth),
            Ty::TupleIntStr => self.gen_tuple(env, depth),
            Ty::Pair => self.gen_pair(env, depth),
            Ty::Token => self.gen_token(env, depth),
            Ty::OptInt => self.gen_option(env, depth),
            Ty::ResIntStr => self.gen_result(env, depth),
        }
    }

    fn literal(&mut self, ty: Ty) -> String {
        match ty {
            Ty::Int => self.rng.int(-10, 10).to_string(),
            Ty::Bool => if self.rng.bool() { "true" } else { "false" }.into(),
            Ty::Str => {
                let value = STRINGS[self.rng.index(STRINGS.len())];
                format!("\"{value}\"")
            }
            Ty::Float => ["0.0", "1.5", "-2.25", "3.0", "0.5"][self.rng.index(5)].into(),
            Ty::ListInt => {
                let count = 1 + self.rng.index(4);
                let values = (0..count)
                    .map(|_| self.rng.int(-5, 5).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("[{values}]")
            }
            Ty::MapStrInt => "{\"a\": 1, \"b\": 2}".into(),
            Ty::TupleIntStr => {
                let value = self.rng.int(-5, 5);
                format!("({value}, \"x\")")
            }
            Ty::Pair => {
                let left = self.rng.int(-5, 5);
                let right = self.rng.int(-5, 5);
                format!("Pair {{ left: {left}, right: {right} }}")
            }
            Ty::Token => match self.rng.index(3) {
                0 => format!("Token.Number({})", self.rng.int(-5, 5)),
                1 => {
                    let text = STRINGS[self.rng.index(STRINGS.len())];
                    format!("Token.Word(\"{text}\")")
                }
                _ => "Token.Empty".into(),
            },
            Ty::OptInt => {
                if self.rng.bool() {
                    format!("Some({})", self.rng.int(-5, 5))
                } else {
                    "None".into()
                }
            }
            Ty::ResIntStr => {
                if self.rng.bool() {
                    format!("Ok({})", self.rng.int(-5, 5))
                } else {
                    let text = STRINGS[self.rng.index(STRINGS.len())];
                    format!("Err(\"{text}\")")
                }
            }
        }
    }

    fn gen_int(&mut self, env: &Env, depth: u32) -> String {
        let choice = self.rng.index(20);
        match choice {
            0 if self.pick_var(env, Ty::Int, None).is_some() => {
                self.pick_var(env, Ty::Int, None).expect("int variable")
            }
            1 => "BASE".into(),
            2 => {
                let left = self.gen_expr(Ty::Int, env, depth - 1);
                let right = self.gen_expr(Ty::Int, env, depth - 1);
                format!("add({left}, {right})")
            }
            3 => {
                let n = self.rng.int(0, 6);
                format!("sum_to({n})")
            }
            4 => {
                let n = self.rng.int(0, 8);
                format!("rec({n})")
            }
            5 => {
                let left = self.gen_expr(Ty::Int, env, depth - 1);
                let right = self.gen_expr(Ty::Int, env, depth - 1);
                format!("({left} + {right})")
            }
            6 => {
                let left = self.gen_expr(Ty::Int, env, depth - 1);
                let right = self.gen_expr(Ty::Int, env, depth - 1);
                format!("({left} - {right})")
            }
            7 => {
                let left = self.gen_expr(Ty::Int, env, depth - 1);
                let right = self.gen_expr(Ty::Int, env, depth - 1);
                format!("({left} * {right})")
            }
            8 => {
                let left = self.gen_expr(Ty::Int, env, depth - 1);
                let divisor = self.rng.int(1, 9);
                format!("({left} / {divisor})")
            }
            9 => {
                let left = self.gen_expr(Ty::Int, env, depth - 1);
                let divisor = self.rng.int(1, 9);
                format!("({left} % {divisor})")
            }
            10 => {
                let cond = self.gen_expr(Ty::Bool, env, depth - 1);
                let left = self.gen_expr(Ty::Int, env, depth - 1);
                let right = self.gen_expr(Ty::Int, env, depth - 1);
                format!("if {cond} {{ {left} }} else {{ {right} }}")
            }
            11 => {
                let list = self.gen_expr(Ty::ListInt, env, depth - 1);
                format!("{list}.len()")
            }
            12 => {
                if let Some(text) = self.pick_var(env, Ty::Str, None) {
                    format!("{text}.len()")
                } else {
                    self.literal(Ty::Int)
                }
            }
            13 => {
                let list = self.gen_expr(Ty::ListInt, env, depth - 1);
                format!("{list}.fold(0, fn(acc, item) => acc + item)")
            }
            14 => {
                if let Some(option) = self.pick_var(env, Ty::OptInt, None) {
                    let fallback = self.gen_expr(Ty::Int, env, depth - 1);
                    format!("({option} ?? {fallback})")
                } else {
                    self.literal(Ty::Int)
                }
            }
            15 => {
                if let Some(pair) = self.pick_var(env, Ty::Pair, None) {
                    format!("{pair}.total()")
                } else {
                    self.literal(Ty::Int)
                }
            }
            16 => {
                let value = ["0.0", "1.5", "-2.25", "3.0", "0.5"][self.rng.index(5)];
                format!("int({value})")
            }
            17 => {
                if let Some(tuple) = self.pick_var(env, Ty::TupleIntStr, None) {
                    format!("{tuple}.0")
                } else {
                    self.literal(Ty::Int)
                }
            }
            18 => {
                let token = self.gen_expr(Ty::Token, env, depth - 1);
                let number = self.fresh("m");
                let word = self.fresh("m");
                format!(
                    "match ({token}) {{ Number({number}) => {number}, Word({word}) => {word}.len(), Empty => 0 }}"
                )
            }
            _ => {
                let value = self.gen_expr(Ty::Int, env, depth - 1);
                let binding = self.fresh("m");
                format!("match (maybe({value})) {{ Some({binding}) => {binding}, None => 0 }}")
            }
        }
    }

    fn gen_bool(&mut self, env: &Env, depth: u32) -> String {
        match self.rng.index(18) {
            0 if self.pick_var(env, Ty::Bool, None).is_some() => {
                self.pick_var(env, Ty::Bool, None).expect("bool variable")
            }
            1 => {
                let left = self.gen_expr(Ty::Int, env, depth - 1);
                let right = self.gen_expr(Ty::Int, env, depth - 1);
                format!("({left} < {right})")
            }
            2 => {
                let left = self.gen_expr(Ty::Int, env, depth - 1);
                let right = self.gen_expr(Ty::Int, env, depth - 1);
                format!("({left} == {right})")
            }
            3 => {
                let left = self.gen_expr(Ty::Str, env, depth - 1);
                let right = self.gen_expr(Ty::Str, env, depth - 1);
                format!("({left} == {right})")
            }
            4 => {
                let value = self.gen_expr(Ty::Bool, env, depth - 1);
                format!("not ({value})")
            }
            5 => {
                let left = self.gen_expr(Ty::Bool, env, depth - 1);
                let right = self.gen_expr(Ty::Bool, env, depth - 1);
                format!("({left} and {right})")
            }
            6 => {
                let left = self.gen_expr(Ty::Bool, env, depth - 1);
                let right = self.gen_expr(Ty::Bool, env, depth - 1);
                format!("({left} or {right})")
            }
            7 => {
                let list = self.gen_expr(Ty::ListInt, env, depth - 1);
                let value = self.gen_expr(Ty::Int, env, depth - 1);
                format!("{list}.contains({value})")
            }
            8 => {
                if let Some(option) = self.pick_var(env, Ty::OptInt, None) {
                    if self.rng.bool() {
                        format!("{option}.is_some()")
                    } else {
                        format!("{option}.is_none()")
                    }
                } else {
                    self.literal(Ty::Bool)
                }
            }
            9 => {
                if let Some(result) = self.pick_var(env, Ty::ResIntStr, None) {
                    if self.rng.bool() {
                        format!("{result}.is_ok()")
                    } else {
                        format!("{result}.is_err()")
                    }
                } else {
                    self.literal(Ty::Bool)
                }
            }
            10 => {
                let text = self.gen_expr(Ty::Str, env, depth - 1);
                let needle = STRINGS[self.rng.index(STRINGS.len())];
                format!("{text}.contains(\"{needle}\")")
            }
            11 => {
                let text = self.gen_expr(Ty::Str, env, depth - 1);
                let prefix = STRINGS[self.rng.index(STRINGS.len())];
                format!("{text}.starts_with(\"{prefix}\")")
            }
            12 => {
                let text = self.gen_expr(Ty::Str, env, depth - 1);
                let suffix = STRINGS[self.rng.index(STRINGS.len())];
                format!("{text}.ends_with(\"{suffix}\")")
            }
            13 => {
                let cond = self.gen_expr(Ty::Bool, env, depth - 1);
                let left = self.gen_expr(Ty::Bool, env, depth - 1);
                let right = self.gen_expr(Ty::Bool, env, depth - 1);
                format!("if {cond} {{ {left} }} else {{ {right} }}")
            }
            14 => {
                let left = self.gen_expr(Ty::Int, env, depth - 1);
                let right = self.gen_expr(Ty::Int, env, depth - 1);
                format!("({left} > {right})")
            }
            15 => {
                let left = self.gen_expr(Ty::Int, env, depth - 1);
                let right = self.gen_expr(Ty::Int, env, depth - 1);
                format!("({left} != {right})")
            }
            16 => {
                let text = self.gen_expr(Ty::Str, env, depth - 1);
                let other = self.gen_expr(Ty::Str, env, depth - 1);
                format!("({text} != {other})")
            }
            _ => self.literal(Ty::Bool),
        }
    }

    fn gen_str(&mut self, env: &Env, depth: u32) -> String {
        match self.rng.index(12) {
            0 if self.pick_var(env, Ty::Str, None).is_some() => {
                self.pick_var(env, Ty::Str, None).expect("str variable")
            }
            1 => {
                let left = self.gen_expr(Ty::Str, env, depth - 1);
                let right = self.gen_expr(Ty::Str, env, depth - 1);
                format!("({left} + {right})")
            }
            2 => {
                let value = self.gen_expr(Ty::Int, env, depth - 1);
                format!("str({value})")
            }
            3 => {
                let value = self.gen_expr(Ty::Bool, env, depth - 1);
                format!("str({value})")
            }
            4 => {
                let value = self.gen_expr(Ty::Float, env, depth - 1);
                format!("str({value})")
            }
            5 => {
                let text = self.gen_expr(Ty::Str, env, depth - 1);
                format!("{text}.to_upper()")
            }
            6 => {
                let text = self.gen_expr(Ty::Str, env, depth - 1);
                format!("{text}.trim()")
            }
            7 => {
                let text = self.gen_expr(Ty::Str, env, depth - 1);
                let from = ["a", "b", "x", "hello"][self.rng.index(4)];
                let to = STRINGS[self.rng.index(STRINGS.len())];
                format!("{text}.replace(\"{from}\", \"{to}\")")
            }
            8 => {
                let cond = self.gen_expr(Ty::Bool, env, depth - 1);
                let left = self.gen_expr(Ty::Str, env, depth - 1);
                let right = self.gen_expr(Ty::Str, env, depth - 1);
                format!("if {cond} {{ {left} }} else {{ {right} }}")
            }
            9 => {
                let token = self.gen_expr(Ty::Token, env, depth - 1);
                let number = self.fresh("m");
                let word = self.fresh("m");
                format!(
                    "match ({token}) {{ Number({number}) => str({number}), Word({word}) => {word}, Empty => \"empty\" }}"
                )
            }
            10 => {
                if let Some(tuple) = self.pick_var(env, Ty::TupleIntStr, None) {
                    format!("{tuple}.1")
                } else {
                    self.literal(Ty::Str)
                }
            }
            _ => self.literal(Ty::Str),
        }
    }

    fn gen_float(&mut self, env: &Env, depth: u32) -> String {
        match self.rng.index(7) {
            0 if self.pick_var(env, Ty::Float, None).is_some() => {
                self.pick_var(env, Ty::Float, None).expect("float variable")
            }
            1 => {
                let value = self.gen_expr(Ty::Int, env, depth - 1);
                format!("float({value})")
            }
            2 => {
                let left = self.gen_expr(Ty::Float, env, depth - 1);
                let right = self.gen_expr(Ty::Float, env, depth - 1);
                format!("({left} + {right})")
            }
            3 => {
                let left = self.gen_expr(Ty::Float, env, depth - 1);
                let right = self.gen_expr(Ty::Float, env, depth - 1);
                format!("({left} - {right})")
            }
            4 => {
                let left = self.gen_expr(Ty::Float, env, depth - 1);
                let right = self.gen_expr(Ty::Float, env, depth - 1);
                format!("({left} * {right})")
            }
            5 => {
                let left = self.gen_expr(Ty::Float, env, depth - 1);
                let divisor = ["1.5", "2.0", "3.25"][self.rng.index(3)];
                format!("({left} / {divisor})")
            }
            _ => self.literal(Ty::Float),
        }
    }

    fn gen_list(&mut self, env: &Env, depth: u32) -> String {
        match self.rng.index(9) {
            0 => {
                let base = self.gen_expr(Ty::ListInt, env, depth - 1);
                let item = self.fresh("x");
                let captured = self.gen_expr(Ty::Int, env, depth - 1);
                format!("({base}).map(fn({item}) => {item} + {captured})")
            }
            1 => {
                let base = self.gen_expr(Ty::ListInt, env, depth - 1);
                let item = self.fresh("x");
                format!("({base}).filter(fn({item}) => {item} % 2 == 0)")
            }
            2 => {
                let base = self.gen_expr(Ty::ListInt, env, depth - 1);
                format!("({base}).reversed()")
            }
            3 => {
                let base = self.gen_expr(Ty::ListInt, env, depth - 1);
                format!("({base}).sorted()")
            }
            4 => {
                let end = self.rng.int(0, 5);
                format!("(0..{end}).to_list()")
            }
            5 => {
                let cond = self.gen_expr(Ty::Bool, env, depth - 1);
                let left = self.gen_expr(Ty::ListInt, env, depth - 1);
                let right = self.gen_expr(Ty::ListInt, env, depth - 1);
                format!("if {cond} {{ {left} }} else {{ {right} }}")
            }
            6 if self.pick_var(env, Ty::ListInt, None).is_some() => self
                .pick_var(env, Ty::ListInt, None)
                .expect("list variable"),
            _ => self.literal(Ty::ListInt),
        }
    }

    fn gen_map(&mut self, env: &Env, depth: u32) -> String {
        match self.rng.index(3) {
            0 if self.pick_var(env, Ty::MapStrInt, None).is_some() => self
                .pick_var(env, Ty::MapStrInt, None)
                .expect("map variable"),
            1 => {
                let cond = self.gen_expr(Ty::Bool, env, depth - 1);
                let left = self.gen_expr(Ty::MapStrInt, env, depth - 1);
                let right = self.gen_expr(Ty::MapStrInt, env, depth - 1);
                format!("if {cond} {{ {left} }} else {{ {right} }}")
            }
            _ => self.literal(Ty::MapStrInt),
        }
    }

    fn gen_tuple(&mut self, env: &Env, depth: u32) -> String {
        match self.rng.index(3) {
            0 if self.pick_var(env, Ty::TupleIntStr, None).is_some() => self
                .pick_var(env, Ty::TupleIntStr, None)
                .expect("tuple variable"),
            1 => {
                let first = self.gen_expr(Ty::Int, env, depth - 1);
                let second = self.gen_expr(Ty::Str, env, depth - 1);
                format!("({first}, {second})")
            }
            _ => self.literal(Ty::TupleIntStr),
        }
    }

    fn gen_pair(&mut self, env: &Env, depth: u32) -> String {
        match self.rng.index(4) {
            0 => {
                let left = self.gen_expr(Ty::Int, env, depth - 1);
                let right = self.gen_expr(Ty::Int, env, depth - 1);
                format!("Pair {{ left: {left}, right: {right} }}")
            }
            1 if self.pick_var(env, Ty::Pair, None).is_some() => {
                let base = self.pick_var(env, Ty::Pair, None).expect("pair variable");
                let left = self.gen_expr(Ty::Int, env, depth - 1);
                format!("Pair {{ ..{base}, left: {left} }}")
            }
            2 if self.pick_var(env, Ty::Pair, None).is_some() => {
                self.pick_var(env, Ty::Pair, None).expect("pair variable")
            }
            _ => self.literal(Ty::Pair),
        }
    }

    fn gen_token(&mut self, env: &Env, depth: u32) -> String {
        match self.rng.index(5) {
            0 => {
                let value = self.gen_expr(Ty::Int, env, depth - 1);
                format!("Token.Number({value})")
            }
            1 => {
                let text = self.gen_expr(Ty::Str, env, depth - 1);
                format!("Token.Word({text})")
            }
            2 => "Token.Empty".into(),
            3 if self.pick_var(env, Ty::Token, None).is_some() => {
                self.pick_var(env, Ty::Token, None).expect("token variable")
            }
            _ => self.literal(Ty::Token),
        }
    }

    fn gen_option(&mut self, env: &Env, depth: u32) -> String {
        match self.rng.index(6) {
            0 => {
                let value = self.gen_expr(Ty::Int, env, depth - 1);
                format!("Some({value})")
            }
            1 => "None".into(),
            2 => {
                let value = self.gen_expr(Ty::Int, env, depth - 1);
                format!("maybe({value})")
            }
            3 => {
                let list = self.gen_expr(Ty::ListInt, env, depth - 1);
                let index = self.rng.int(0, 6);
                format!("{list}.get({index})")
            }
            4 => {
                let text = self.gen_expr(Ty::Str, env, depth - 1);
                format!("{text}.parse_int()")
            }
            _ => {
                if let Some(option) = self.pick_var(env, Ty::OptInt, None) {
                    option
                } else {
                    self.literal(Ty::OptInt)
                }
            }
        }
    }

    fn gen_result(&mut self, env: &Env, depth: u32) -> String {
        match self.rng.index(5) {
            0 => {
                let value = self.gen_expr(Ty::Int, env, depth - 1);
                format!("Ok({value})")
            }
            1 => {
                let text = self.gen_expr(Ty::Str, env, depth - 1);
                format!("Err({text})")
            }
            2 => {
                let value = self.gen_expr(Ty::Int, env, depth - 1);
                format!("need_positive({value})")
            }
            3 => {
                let value = self.gen_expr(Ty::Int, env, depth - 1);
                format!("try_positive({value})")
            }
            _ => {
                if let Some(result) = self.pick_var(env, Ty::ResIntStr, None) {
                    result
                } else {
                    self.literal(Ty::ResIntStr)
                }
            }
        }
    }
}
