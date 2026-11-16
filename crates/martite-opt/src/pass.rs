//! Pass traits, the shared pass context and the deterministic pass manager.

use std::collections::BTreeSet;

use martite_ir::walk::children_of;
use martite_ir::{
    ConstId, ConstValue, IrFn, IrId, IrProgram, StrId, Ty, TyId, VerifyMode, recompute_effects,
    verify_with,
};

use crate::error::{OptimizeError, RewriteError};

/// How a pass treats fuel, memory and sharing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CostPolicy {
    /// Keep fuel, memory thresholds and sharing exactly unchanged.
    #[default]
    Preserve,
    /// Opt-in, edition-gated policy that may reduce fuel or memory.
    Reduce,
}

/// Whether a pass changed the program.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changed(bool);

impl Changed {
    pub const NO: Self = Self(false);
    pub const YES: Self = Self(true);

    pub const fn new(changed: bool) -> Self {
        Self(changed)
    }

    pub const fn is_changed(self) -> bool {
        self.0
    }

    pub const fn or(self, other: Self) -> Self {
        Self(self.0 || other.0)
    }
}

impl From<bool> for Changed {
    fn from(value: bool) -> Self {
        Self(value)
    }
}

/// Counters for one pass or for a whole optimization run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RewriteStats {
    pub rewrites: usize,
    pub nodes_added: usize,
    pub nodes_removed: usize,
}

impl RewriteStats {
    pub fn add(&mut self, other: Self) {
        self.rewrites += other.rewrites;
        self.nodes_added += other.nodes_added;
        self.nodes_removed += other.nodes_removed;
    }
}

/// A function-level pass.
pub trait FnPass {
    fn name(&self) -> &'static str;
    fn cost_policy(&self) -> CostPolicy {
        CostPolicy::Preserve
    }
    fn run(&mut self, function: &mut IrFn, cx: &mut PassCx<'_>) -> Changed;
}

/// A whole-program pass.
pub trait ProgramPass {
    fn name(&self) -> &'static str;
    fn cost_policy(&self) -> CostPolicy {
        CostPolicy::Preserve
    }
    fn run(&mut self, program: &mut IrProgram, cx: &mut PassCx<'_>) -> Changed;
}

/// The context shared by all passes.
///
/// Function passes hold a read-only view of the program that produced the
/// function. Program passes run without a program view because they already
/// receive `&mut IrProgram`; they must not use the interning helpers.
pub struct PassCx<'p> {
    program: Option<&'p IrProgram>,
    policy: CostPolicy,
    edition: String,
    pending_consts: Vec<ConstValue>,
    pending_strs: Vec<Box<str>>,
    error: Option<RewriteError>,
    stats: RewriteStats,
}

impl<'p> PassCx<'p> {
    fn for_function(program: &'p IrProgram, options: &OptimizeOptions) -> Self {
        Self {
            program: Some(program),
            policy: options.policy,
            edition: options.edition.clone(),
            pending_consts: Vec::new(),
            pending_strs: Vec::new(),
            error: None,
            stats: RewriteStats::default(),
        }
    }

    fn for_program(options: &OptimizeOptions) -> Self {
        Self {
            program: None,
            policy: options.policy,
            edition: options.edition.clone(),
            pending_consts: Vec::new(),
            pending_strs: Vec::new(),
            error: None,
            stats: RewriteStats::default(),
        }
    }

    /// The program that produced the function currently being optimized.
    ///
    /// The returned reference has the context's own lifetime, so a pass may
    /// keep it while still using the mutable interning helpers.
    pub fn program(&self) -> Option<&'p IrProgram> {
        self.program
    }

    pub fn policy(&self) -> CostPolicy {
        self.policy
    }

    pub fn edition(&self) -> &str {
        &self.edition
    }

    pub fn stats(&self) -> RewriteStats {
        self.stats
    }

    pub fn record(&mut self, stats: RewriteStats) {
        self.stats.add(stats);
    }

    pub(crate) fn record_error(&mut self, error: RewriteError) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    fn take_error(&mut self) -> Option<RewriteError> {
        self.error.take()
    }

    fn take_pending(&mut self) -> (Vec<ConstValue>, Vec<Box<str>>) {
        (
            std::mem::take(&mut self.pending_consts),
            std::mem::take(&mut self.pending_strs),
        )
    }

    /// Read the constant pool entry, including the current function pass's
    /// pending additions.
    pub fn constant(&self, id: ConstId) -> Option<ConstValue> {
        if let Some(program) = self.program
            && let Some(value) = program.constant(id)
        {
            return Some(value.clone());
        }
        let index = id.index().checked_sub(self.base_consts());
        index.and_then(|index| self.pending_consts.get(index).cloned())
    }

    /// Read an owned type by id.
    pub fn ty(&self, id: TyId) -> Option<Ty> {
        self.program?.types.get_ty(id).cloned()
    }

    /// Semantic children of one node.
    pub fn children_of(&self, function: &IrFn, id: IrId) -> Vec<IrId> {
        let Some(program) = self.program else {
            return Vec::new();
        };
        function
            .node(id)
            .copied()
            .map(|node| children_of(program, function, node))
            .unwrap_or_default()
    }

    /// Append an integer constant, deduplicating against the existing and
    /// pending pools.
    pub fn intern_int(&mut self, value: i64) -> ConstId {
        self.intern_const(ConstValue::Int(value))
    }

    /// Append a float constant, comparing bit patterns so `-0.0` and `0.0`
    /// stay distinct.
    pub fn intern_float(&mut self, value: f64) -> ConstId {
        self.intern_const(ConstValue::Float(value))
    }

    /// Append a constant, deduplicating against the existing and pending pools.
    pub fn intern_const(&mut self, value: ConstValue) -> ConstId {
        let base = self.base_consts();
        if let Some(program) = self.program {
            for (index, existing) in program.consts.iter().enumerate() {
                if const_eq(existing, &value) {
                    return ConstId::new(index as u32);
                }
            }
        }
        for (index, existing) in self.pending_consts.iter().enumerate() {
            if const_eq(existing, &value) {
                return ConstId::new((base + index) as u32);
            }
        }
        let id = ConstId::new((base + self.pending_consts.len()) as u32);
        self.pending_consts.push(value);
        id
    }

    /// Append a static string without deduplicating (literal sites are
    /// distinct).
    pub fn intern_str(&mut self, value: impl Into<Box<str>>) -> StrId {
        let base = self
            .program
            .map(|program| program.strs.len())
            .unwrap_or_default();
        let id = StrId::new((base + self.pending_strs.len()) as u32);
        self.pending_strs.push(value.into());
        id
    }

    fn base_consts(&self) -> usize {
        self.program
            .map(|program| program.consts.len())
            .unwrap_or_default()
    }
}

fn const_eq(left: &ConstValue, right: &ConstValue) -> bool {
    match (left, right) {
        (ConstValue::Float(left), ConstValue::Float(right)) => left.to_bits() == right.to_bits(),
        _ => left == right,
    }
}

/// Options for one optimization run.
#[derive(Debug, Clone)]
pub struct OptimizeOptions {
    pub policy: CostPolicy,
    pub fixpoint: bool,
    pub max_iterations: u32,
    pub edition: String,
    pub allow_reduce: bool,
    /// Verification policy applied after every pass that changes the program.
    pub verify: VerifyMode,
    pub disabled: BTreeSet<String>,
}

impl Default for OptimizeOptions {
    fn default() -> Self {
        Self {
            policy: CostPolicy::Preserve,
            fixpoint: false,
            max_iterations: 8,
            edition: "1".into(),
            allow_reduce: false,
            verify: VerifyMode::Always,
            disabled: BTreeSet::new(),
        }
    }
}

impl OptimizeOptions {
    pub fn with_fixpoint(mut self, max_iterations: u32) -> Self {
        self.fixpoint = true;
        self.max_iterations = max_iterations.max(1);
        self
    }

    pub fn disable(mut self, name: impl Into<String>) -> Self {
        self.disabled.insert(name.into());
        self
    }
}

/// The result of a successful optimization run.
#[derive(Debug, Clone, Default)]
pub struct OptimizeReport {
    pub changed: bool,
    pub iterations: u32,
    pub passes: Vec<PassReport>,
    pub stats: RewriteStats,
}

#[derive(Debug, Clone)]
pub struct PassReport {
    pub name: &'static str,
    pub changed: bool,
    pub stats: RewriteStats,
}

enum PassEntry {
    Function(Box<dyn FnPass>),
    Program(Box<dyn ProgramPass>),
}

impl PassEntry {
    fn name(&self) -> &'static str {
        match self {
            Self::Function(pass) => pass.name(),
            Self::Program(pass) => pass.name(),
        }
    }

    fn cost_policy(&self) -> CostPolicy {
        match self {
            Self::Function(pass) => pass.cost_policy(),
            Self::Program(pass) => pass.cost_policy(),
        }
    }
}

/// Deterministic, ordered pass manager.
pub struct PassManager {
    passes: Vec<PassEntry>,
    options: OptimizeOptions,
}

impl PassManager {
    pub fn new(options: OptimizeOptions) -> Self {
        Self {
            passes: Vec::new(),
            options,
        }
    }

    pub fn options(&self) -> &OptimizeOptions {
        &self.options
    }

    pub fn options_mut(&mut self) -> &mut OptimizeOptions {
        &mut self.options
    }

    pub fn add_fn_pass(&mut self, pass: impl FnPass + 'static) -> &mut Self {
        self.passes.push(PassEntry::Function(Box::new(pass)));
        self
    }

    pub fn add_program_pass(&mut self, pass: impl ProgramPass + 'static) -> &mut Self {
        self.passes.push(PassEntry::Program(Box::new(pass)));
        self
    }

    pub fn disable(&mut self, name: &str) -> bool {
        if self.passes.iter().any(|pass| pass.name() == name) {
            self.options.disabled.insert(name.into());
            true
        } else {
            false
        }
    }

    pub fn enable(&mut self, name: &str) -> bool {
        self.options.disabled.remove(name)
    }

    pub fn is_enabled(&self, name: &str) -> bool {
        self.passes.iter().any(|pass| pass.name() == name) && !self.options.disabled.contains(name)
    }

    pub fn pass_names(&self) -> Vec<&'static str> {
        self.passes.iter().map(PassEntry::name).collect()
    }

    pub fn run(&mut self, program: &mut IrProgram) -> Result<OptimizeReport, OptimizeError> {
        let mut report = OptimizeReport::default();
        let mut iteration = 0u32;
        loop {
            iteration += 1;
            let mut iteration_changed = false;
            for entry in &mut self.passes {
                let name = entry.name();
                if self.options.disabled.contains(name) {
                    continue;
                }
                let policy = entry.cost_policy();
                if policy == CostPolicy::Reduce && !self.options.allow_reduce {
                    return Err(OptimizeError::Policy { pass: name, policy });
                }
                let mut pass_stats = RewriteStats::default();
                let mut pass_changed = false;
                match entry {
                    PassEntry::Function(pass) => {
                        for index in 0..program.fns.len() {
                            if program.fns[index].body.is_none() {
                                continue;
                            }
                            let mut function = program.fns[index].clone();
                            let (changed, error, pending, stats) = {
                                let mut cx = PassCx::for_function(program, &self.options);
                                let changed = pass.run(&mut function, &mut cx);
                                (changed, cx.take_error(), cx.take_pending(), cx.stats())
                            };
                            if let Some(error) = error {
                                return Err(OptimizeError::Rewrite { pass: name, error });
                            }
                            if !changed.is_changed() {
                                continue;
                            }
                            let (consts, strs) = pending;
                            program.consts.extend(consts);
                            program.strs.extend(strs);
                            program.fns[index] = function;
                            recompute_effects(program);
                            verify_after(program, name, &self.options)?;
                            pass_changed = true;
                            pass_stats.add(stats);
                        }
                    }
                    PassEntry::Program(pass) => {
                        let (changed, error, stats) = {
                            let mut cx = PassCx::for_program(&self.options);
                            let changed = pass.run(program, &mut cx);
                            (changed, cx.take_error(), cx.stats())
                        };
                        if let Some(error) = error {
                            return Err(OptimizeError::Rewrite { pass: name, error });
                        }
                        if changed.is_changed() {
                            recompute_effects(program);
                            verify_after(program, name, &self.options)?;
                            pass_changed = true;
                            pass_stats.add(stats);
                        }
                    }
                }
                if pass_changed {
                    iteration_changed = true;
                    report.changed = true;
                    report.stats.add(pass_stats);
                    report.passes.push(PassReport {
                        name,
                        changed: true,
                        stats: pass_stats,
                    });
                }
            }
            if !iteration_changed || !self.options.fixpoint {
                report.iterations = iteration;
                return Ok(report);
            }
            if iteration >= self.options.max_iterations {
                return Err(OptimizeError::FixpointLimit {
                    limit: self.options.max_iterations,
                });
            }
        }
    }
}

fn verify_after(
    program: &IrProgram,
    pass: &'static str,
    options: &OptimizeOptions,
) -> Result<(), OptimizeError> {
    verify_with(program, options.verify).map_err(|error| OptimizeError::Verify {
        pass,
        error: Box::new(error),
    })?;
    Ok(())
}
