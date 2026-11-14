//! Deterministic behavior snapshots for the AST interpreter.
//!
//! Snapshots are the regression oracle for the IR migration. They record the
//! observable result, trap, log, host calls, fuel and the minimum fuel,
//! memory and depth budgets at which the AST interpreter produces that result.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use martite::{Engine, HostSignature, Plugin};
use martite_base::{MemoryProvider, SourceId, SourceMap, Span};
use martite_check::{CheckedProgram, DefId};
use martite_config::parse_manifest;
use martite_ir::{HostId, IrProgram, TypeTable, lower, verify};
use martite_runtime::{
    ExecLimits, HostContext, HostDispatcher, HostFault, IrHostDispatcher, IrRuntime, Runtime, Trap,
    Value,
};
use martite_syntax::Item;
use serde::{Deserialize, Serialize};

use crate::{Case, ProbeCaps};

pub const SNAPSHOT_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotRecord {
    pub schema: u32,
    pub id: String,
    pub kind: String,
    pub limits: LimitsRecord,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<OutcomeRecord>,
    #[serde(default)]
    pub log: Vec<String>,
    #[serde(default)]
    pub host_calls: Vec<HostCallRecord>,
    pub fuel_used: u64,
    pub thresholds: ThresholdsRecord,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<StepRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitsRecord {
    pub fuel: u64,
    pub memory: u64,
    pub depth: u32,
}

impl From<ExecLimits> for LimitsRecord {
    fn from(limits: ExecLimits) -> Self {
        Self {
            fuel: limits.fuel,
            memory: limits.memory,
            depth: limits.depth,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutcomeRecord {
    Value { show: String },
    Trap { trap: TrapRecord },
    NoHandler,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrapRecord {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<SpanRecord>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanRecord {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostCallRecord {
    pub name: String,
    pub args: Vec<String>,
    pub charged: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThresholdsRecord {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuel: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One IR threshold that differs from the AST oracle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThresholdMismatch {
    pub field: &'static str,
    pub expected: String,
    pub actual: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecord {
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<OutcomeRecord>,
    #[serde(default)]
    pub log: Vec<String>,
    #[serde(default)]
    pub host_calls: Vec<HostCallRecord>,
    pub fuel_used: u64,
}

#[derive(Clone)]
pub struct HostSet {
    fns: Vec<HostFn>,
    trace: Rc<RefCell<Vec<RawHostCall>>>,
}

#[derive(Clone)]
struct HostFn {
    name: String,
    signature: HostSignature,
    function: Rc<dyn Fn(&mut HostContext, &[Value]) -> Result<Value, HostFault>>,
}

struct RawHostCall {
    name: String,
    args: Vec<Value>,
    charged: u64,
}

impl Default for HostSet {
    fn default() -> Self {
        Self::new()
    }
}

impl HostSet {
    pub fn new() -> Self {
        Self {
            fns: Vec::new(),
            trace: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn add(
        &mut self,
        name: impl Into<String>,
        signature: HostSignature,
        function: impl Fn(&mut HostContext, &[Value]) -> Result<Value, HostFault> + 'static,
    ) {
        self.fns.push(HostFn {
            name: name.into(),
            signature,
            function: Rc::new(function),
        });
    }

    pub fn register(&self, engine: &mut Engine) {
        for host in &self.fns {
            let function = host.function.clone();
            let trace = self.trace.clone();
            let name = host.name.clone();
            engine.host_fn(
                host.name.clone(),
                host.signature.clone(),
                move |cx, args| {
                    let index = {
                        let mut trace = trace.borrow_mut();
                        trace.push(RawHostCall {
                            name: name.clone(),
                            args: args.to_vec(),
                            charged: 0,
                        });
                        trace.len() - 1
                    };
                    let result = function(cx, args);
                    if let Some(call) = trace.borrow_mut().get_mut(index) {
                        call.charged = cx.charged;
                    }
                    result
                },
            );
        }
    }

    pub fn clear_trace(&self) {
        self.trace.borrow_mut().clear();
    }

    pub fn take_calls(&self, types: &TypeTable) -> Vec<HostCallRecord> {
        let calls = std::mem::take(&mut *self.trace.borrow_mut());
        calls
            .into_iter()
            .map(|call| HostCallRecord {
                name: call.name,
                args: call
                    .args
                    .iter()
                    .map(|value| value_repr(value, types, false))
                    .collect(),
                charged: call.charged,
            })
            .collect()
    }

    /// IR-side dispatcher for the lowered program.
    ///
    /// Registered host names are qualified (`module.function`); the IR host
    /// table carries the module and function name separately.
    fn ir_dispatcher(&self, program: &IrProgram) -> IrRecordingDispatcher<'_> {
        let mut by_name = HashMap::new();
        for host in &self.fns {
            by_name.insert(host.name.as_str(), host);
        }
        let fns = program
            .hosts
            .iter()
            .map(|info| {
                let name = format!("{}.{}", info.module, info.name);
                by_name.get(name.as_str()).copied()
            })
            .collect();
        IrRecordingDispatcher {
            fns,
            trace: self.trace.clone(),
        }
    }

    fn dispatcher(&self, program: &CheckedProgram) -> RecordingDispatcher {
        let mut by_name = HashMap::new();
        for module in &program.resolved.modules {
            if !module.is_interface {
                continue;
            }
            for item in &module.ast.items {
                if let Item::Fn(function) = item {
                    if let Some(def) = program.resolved.node_to_def.get(&function.id) {
                        by_name.insert(format!("{}.{}", module.path, function.name), *def);
                    }
                }
            }
        }
        let mut fns = HashMap::new();
        for host in &self.fns {
            let Some(def) = by_name.get(&host.name).copied() else {
                panic!(
                    "host function `{}` is not present in the program",
                    host.name
                );
            };
            fns.insert(def, (host.name.clone(), host.function.clone()));
        }
        RecordingDispatcher {
            fns,
            trace: self.trace.clone(),
        }
    }
}

struct RecordingDispatcher {
    fns: HashMap<
        DefId,
        (
            String,
            Rc<dyn Fn(&mut HostContext, &[Value]) -> Result<Value, HostFault>>,
        ),
    >,
    trace: Rc<RefCell<Vec<RawHostCall>>>,
}

impl HostDispatcher for RecordingDispatcher {
    fn call(
        &mut self,
        def: DefId,
        args: &[Value],
        cx: &mut HostContext,
    ) -> Result<Value, HostFault> {
        let Some((name, function)) = self.fns.get(&def) else {
            return Err(HostFault::new("host function is not registered"));
        };
        let index = {
            let mut trace = self.trace.borrow_mut();
            trace.push(RawHostCall {
                name: name.clone(),
                args: args.to_vec(),
                charged: 0,
            });
            trace.len() - 1
        };
        let result = function(cx, args);
        if let Some(call) = self.trace.borrow_mut().get_mut(index) {
            call.charged = cx.charged;
        }
        result
    }
}

/// IR-side adapter over a [`HostSet`]. The IR dispatches by [`HostId`]; the
/// mapping from the program's host table to the registered functions is built
/// once when the dispatcher is created.
struct IrRecordingDispatcher<'a> {
    fns: Vec<Option<&'a HostFn>>,
    trace: Rc<RefCell<Vec<RawHostCall>>>,
}

impl IrHostDispatcher for IrRecordingDispatcher<'_> {
    fn call(
        &mut self,
        host: HostId,
        args: &[Value],
        cx: &mut HostContext,
    ) -> Result<Value, HostFault> {
        let Some(host_fn) = self.fns.get(host.index()).and_then(|entry| *entry) else {
            return Err(HostFault::new("host function is not registered"));
        };
        let index = {
            let mut trace = self.trace.borrow_mut();
            trace.push(RawHostCall {
                name: host_fn.name.clone(),
                args: args.to_vec(),
                charged: 0,
            });
            trace.len() - 1
        };
        let result = (host_fn.function)(cx, args);
        if let Some(call) = self.trace.borrow_mut().get_mut(index) {
            call.charged = cx.charged;
        }
        result
    }
}

#[derive(Debug, Clone)]
pub enum ArgSpec {
    Value(Value),
    Handle {
        type_name: &'static str,
        token: &'static str,
    },
}

impl ArgSpec {
    pub fn int(value: i64) -> Self {
        Self::Value(Value::Int(value))
    }

    pub fn str(value: impl Into<String>) -> Self {
        Self::Value(Value::str(value))
    }

    pub fn handle(type_name: &'static str, token: &'static str) -> Self {
        Self::Handle { type_name, token }
    }
}

pub struct PluginScenario {
    pub id: String,
    pub manifest: String,
    pub files: Vec<(String, String)>,
    pub hosts: Option<HostSet>,
    pub steps: Vec<PluginStep>,
}

pub enum PluginStep {
    Init,
    Call {
        hook: &'static str,
        args: Vec<ArgSpec>,
    },
    Shutdown,
}

pub enum SnapshotCase {
    Main {
        id: String,
        case: Case,
        hosts: Option<HostSet>,
        caps: ProbeCaps,
    },
    Plugin {
        id: String,
        scenario: PluginScenario,
        caps: ProbeCaps,
    },
}

impl SnapshotCase {
    pub fn id(&self) -> &str {
        match self {
            Self::Main { id, .. } | Self::Plugin { id, .. } => id,
        }
    }

    pub fn group(&self) -> &str {
        self.id().split('/').next().unwrap_or("misc")
    }

    /// The checked program and its source names, when the case produces one.
    ///
    /// Used by the IR lowering gate. Plugin error cases that never reach the
    /// checker return `None`.
    pub fn checked_program(&self) -> Option<(CheckedProgram, SourceMap)> {
        match self {
            Self::Main { case, .. } => {
                let loaded = case.load();
                loaded.program.map(|program| (program, loaded.sources))
            }
            Self::Plugin { scenario, .. } => {
                let plugin = load_plugin(scenario);
                Some((plugin.program().clone(), plugin.sources().clone()))
            }
        }
    }

    pub fn observe(&self) -> SnapshotRecord {
        match self {
            Self::Main {
                id,
                case,
                hosts,
                caps,
            } => {
                let limits = caps_to_limits(*caps);
                let observation = run_main(case, hosts.as_ref(), limits);
                SnapshotRecord {
                    schema: SNAPSHOT_SCHEMA,
                    id: id.clone(),
                    kind: "main".into(),
                    limits: limits.into(),
                    outcome: Some(observation.outcome),
                    log: observation.log,
                    host_calls: observation.host_calls,
                    fuel_used: observation.fuel_used,
                    thresholds: ThresholdsRecord::default(),
                    steps: Vec::new(),
                }
            }
            Self::Plugin { id, scenario, caps } => {
                let limits = caps_to_limits(*caps);
                let plugin = load_plugin(scenario);
                let observation = run_plugin(&plugin, scenario, limits);
                SnapshotRecord {
                    schema: SNAPSHOT_SCHEMA,
                    id: id.clone(),
                    kind: "plugin".into(),
                    limits: limits.into(),
                    outcome: None,
                    log: Vec::new(),
                    host_calls: Vec::new(),
                    fuel_used: observation.fuel_used,
                    thresholds: ThresholdsRecord::default(),
                    steps: observation.steps,
                }
            }
        }
    }

    pub fn record(&self) -> SnapshotRecord {
        let mut record = self.observe();
        record.thresholds = self.thresholds(&record);
        record
    }

    /// Observe the same scenario through the IR evaluator.
    ///
    /// This is the seed for the full AST-vs-IR differential harness (#112):
    /// it records the same outcome, log, host calls, fuel and plugin steps as
    /// [`Self::observe`], so a test can compare the two records directly.
    pub fn observe_ir(&self) -> SnapshotRecord {
        match self {
            Self::Main {
                id,
                case,
                hosts,
                caps,
            } => {
                let limits = caps_to_limits(*caps);
                let observation = run_main_ir(case, hosts.as_ref(), limits);
                SnapshotRecord {
                    schema: SNAPSHOT_SCHEMA,
                    id: id.clone(),
                    kind: "main".into(),
                    limits: limits.into(),
                    outcome: Some(observation.outcome),
                    log: observation.log,
                    host_calls: observation.host_calls,
                    fuel_used: observation.fuel_used,
                    thresholds: ThresholdsRecord::default(),
                    steps: Vec::new(),
                }
            }
            Self::Plugin { id, scenario, caps } => {
                let limits = caps_to_limits(*caps);
                let plugin = load_plugin(scenario);
                let observation = run_plugin_ir(&plugin, scenario, limits);
                SnapshotRecord {
                    schema: SNAPSHOT_SCHEMA,
                    id: id.clone(),
                    kind: "plugin".into(),
                    limits: limits.into(),
                    outcome: None,
                    log: Vec::new(),
                    host_calls: Vec::new(),
                    fuel_used: observation.fuel_used,
                    thresholds: ThresholdsRecord::default(),
                    steps: observation.steps,
                }
            }
        }
    }

    fn thresholds(&self, baseline: &SnapshotRecord) -> ThresholdsRecord {
        if let Some(note) = limit_note(baseline) {
            return ThresholdsRecord {
                note: Some(note),
                ..ThresholdsRecord::default()
            };
        }
        let caps = self.caps();
        let expected = expected_outcomes(baseline);
        search_thresholds(caps, &expected, |limits| self.outcomes_at(limits))
    }

    pub fn verify(&self, expected: &SnapshotRecord) {
        let actual = self.observe();
        assert_eq!(actual.id, expected.id, "snapshot id");
        assert_eq!(actual.kind, expected.kind, "{}: kind", expected.id);
        assert_eq!(actual.limits, expected.limits, "{}: limits", expected.id);
        assert_eq!(actual.outcome, expected.outcome, "{}: outcome", expected.id);
        assert_eq!(actual.log, expected.log, "{}: log", expected.id);
        assert_eq!(
            actual.host_calls, expected.host_calls,
            "{}: host calls",
            expected.id
        );
        assert_eq!(
            actual.fuel_used, expected.fuel_used,
            "{}: fuel used",
            expected.id
        );
        assert_eq!(actual.steps, expected.steps, "{}: steps", expected.id);
        self.verify_thresholds(expected);
    }

    fn verify_thresholds(&self, expected: &SnapshotRecord) {
        if expected.thresholds.note.is_some() {
            return;
        }
        let caps = self.caps();
        let outcomes = expected_outcomes(expected);
        let fuel = expected
            .thresholds
            .fuel
            .unwrap_or_else(|| panic!("{}: missing fuel threshold", expected.id));
        let memory = expected
            .thresholds
            .memory
            .unwrap_or_else(|| panic!("{}: missing memory threshold", expected.id));
        let depth = expected
            .thresholds
            .depth
            .unwrap_or_else(|| panic!("{}: missing depth threshold", expected.id));
        let at_fuel = self.outcomes_at(ExecLimits {
            fuel,
            memory: caps.memory,
            depth: caps.depth,
            deadline: None,
        });
        assert_eq!(at_fuel, outcomes, "{}: fuel threshold", expected.id);
        if fuel > 1 {
            assert_ne!(
                self.outcomes_at(ExecLimits {
                    fuel: fuel - 1,
                    memory: caps.memory,
                    depth: caps.depth,
                    deadline: None,
                }),
                outcomes,
                "{}: fuel threshold is not minimal",
                expected.id
            );
        }
        let at_memory = self.outcomes_at(ExecLimits {
            fuel: caps.fuel,
            memory,
            depth: caps.depth,
            deadline: None,
        });
        assert_eq!(at_memory, outcomes, "{}: memory threshold", expected.id);
        if memory > 0 {
            assert_ne!(
                self.outcomes_at(ExecLimits {
                    fuel: caps.fuel,
                    memory: memory - 1,
                    depth: caps.depth,
                    deadline: None,
                }),
                outcomes,
                "{}: memory threshold is not minimal",
                expected.id
            );
        }
        let at_depth = self.outcomes_at(ExecLimits {
            fuel: caps.fuel,
            memory: caps.memory,
            depth,
            deadline: None,
        });
        assert_eq!(at_depth, outcomes, "{}: depth threshold", expected.id);
        if depth > 0 {
            assert_ne!(
                self.outcomes_at(ExecLimits {
                    fuel: caps.fuel,
                    memory: caps.memory,
                    depth: depth - 1,
                    deadline: None,
                }),
                outcomes,
                "{}: depth threshold is not minimal",
                expected.id
            );
        }
    }

    /// Check the IR evaluator at the AST oracle's minimum thresholds.
    ///
    /// The AST record already proves that each threshold is minimal. The IR
    /// must complete at the threshold and differ one unit below it, which
    /// makes its minimum equal to the oracle's without a second binary search.
    pub fn verify_ir_thresholds(
        &self,
        expected: &SnapshotRecord,
    ) -> Result<(), Vec<ThresholdMismatch>> {
        if expected.thresholds.note.is_some() {
            return Ok(());
        }
        let caps = self.caps();
        let outcomes = expected_outcomes(expected);
        let Some(fuel) = expected.thresholds.fuel else {
            return Err(vec![missing_threshold(expected, "thresholds.fuel")]);
        };
        let Some(memory) = expected.thresholds.memory else {
            return Err(vec![missing_threshold(expected, "thresholds.memory")]);
        };
        let Some(depth) = expected.thresholds.depth else {
            return Err(vec![missing_threshold(expected, "thresholds.depth")]);
        };

        let mut mismatches = Vec::new();
        self.check_ir_boundary(
            "thresholds.fuel",
            &outcomes,
            ExecLimits {
                fuel,
                memory: caps.memory,
                depth: caps.depth,
                deadline: None,
            },
            (fuel > 1).then(|| ExecLimits {
                fuel: fuel - 1,
                memory: caps.memory,
                depth: caps.depth,
                deadline: None,
            }),
            &mut mismatches,
        );
        self.check_ir_boundary(
            "thresholds.memory",
            &outcomes,
            ExecLimits {
                fuel: caps.fuel,
                memory,
                depth: caps.depth,
                deadline: None,
            },
            (memory > 0).then(|| ExecLimits {
                fuel: caps.fuel,
                memory: memory - 1,
                depth: caps.depth,
                deadline: None,
            }),
            &mut mismatches,
        );
        self.check_ir_boundary(
            "thresholds.depth",
            &outcomes,
            ExecLimits {
                fuel: caps.fuel,
                memory: caps.memory,
                depth,
                deadline: None,
            },
            (depth > 0).then(|| ExecLimits {
                fuel: caps.fuel,
                memory: caps.memory,
                depth: depth - 1,
                deadline: None,
            }),
            &mut mismatches,
        );

        if mismatches.is_empty() {
            Ok(())
        } else {
            Err(mismatches)
        }
    }

    fn check_ir_boundary(
        &self,
        field: &'static str,
        outcomes: &[OutcomeRecord],
        at: ExecLimits,
        below: Option<ExecLimits>,
        mismatches: &mut Vec<ThresholdMismatch>,
    ) {
        let actual = self.outcomes_at_ir(at);
        if actual != outcomes {
            mismatches.push(ThresholdMismatch {
                field,
                expected: format!("{outcomes:?}"),
                actual: format!("{actual:?}"),
            });
        }
        if let Some(below) = below {
            let actual_below = self.outcomes_at_ir(below);
            if actual_below == outcomes {
                mismatches.push(ThresholdMismatch {
                    field,
                    expected: "a different outcome one unit below the threshold".into(),
                    actual: format!("same outcome: {actual_below:?}"),
                });
            }
        }
    }

    fn caps(&self) -> ProbeCaps {
        match self {
            Self::Main { caps, .. } | Self::Plugin { caps, .. } => *caps,
        }
    }

    fn outcomes_at(&self, limits: ExecLimits) -> Vec<OutcomeRecord> {
        match self {
            Self::Main { case, hosts, .. } => {
                vec![run_main(case, hosts.as_ref(), limits).outcome]
            }
            Self::Plugin { scenario, .. } => {
                let plugin = load_plugin(scenario);
                run_plugin(&plugin, scenario, limits)
                    .steps
                    .into_iter()
                    .filter_map(|step| step.outcome)
                    .collect()
            }
        }
    }

    fn outcomes_at_ir(&self, limits: ExecLimits) -> Vec<OutcomeRecord> {
        match self {
            Self::Main { case, hosts, .. } => {
                vec![run_main_ir(case, hosts.as_ref(), limits).outcome]
            }
            Self::Plugin { scenario, .. } => {
                let plugin = load_plugin(scenario);
                run_plugin_ir(&plugin, scenario, limits)
                    .steps
                    .into_iter()
                    .filter_map(|step| step.outcome)
                    .collect()
            }
        }
    }
}

fn search_thresholds(
    caps: ProbeCaps,
    expected: &[OutcomeRecord],
    mut outcomes_at: impl FnMut(ExecLimits) -> Vec<OutcomeRecord>,
) -> ThresholdsRecord {
    let fuel = binary_min_from(1, caps.fuel.max(1), |value| {
        outcomes_at(ExecLimits {
            fuel: value,
            memory: caps.memory,
            depth: caps.depth,
            deadline: None,
        }) == expected
    });
    let memory = binary_min(caps.memory, |value| {
        outcomes_at(ExecLimits {
            fuel: caps.fuel,
            memory: value,
            depth: caps.depth,
            deadline: None,
        }) == expected
    });
    let depth = binary_min(caps.depth as u64, |value| {
        outcomes_at(ExecLimits {
            fuel: caps.fuel,
            memory: caps.memory,
            depth: value as u32,
            deadline: None,
        }) == expected
    }) as u32;
    ThresholdsRecord {
        fuel: Some(fuel),
        memory: Some(memory),
        depth: Some(depth),
        note: None,
    }
}

fn caps_to_limits(caps: ProbeCaps) -> ExecLimits {
    ExecLimits {
        fuel: caps.fuel,
        memory: caps.memory,
        depth: caps.depth,
        deadline: None,
    }
}

fn expected_outcomes(record: &SnapshotRecord) -> Vec<OutcomeRecord> {
    if record.kind == "plugin" {
        record
            .steps
            .iter()
            .filter_map(|step| step.outcome.clone())
            .collect()
    } else {
        record.outcome.iter().cloned().collect()
    }
}

fn missing_threshold(record: &SnapshotRecord, field: &'static str) -> ThresholdMismatch {
    ThresholdMismatch {
        field,
        expected: "a recorded threshold".into(),
        actual: format!("missing from {}", record.id),
    }
}

fn limit_note(record: &SnapshotRecord) -> Option<String> {
    let outcome = record
        .outcome
        .as_ref()
        .or_else(|| record.steps.iter().find_map(|step| step.outcome.as_ref()))?;
    let OutcomeRecord::Trap { trap } = outcome else {
        return None;
    };
    match trap.kind.as_str() {
        "out_of_fuel" | "out_of_memory" | "depth_exceeded" | "deadline" => {
            Some("limit-driven outcome".into())
        }
        _ => None,
    }
}

fn binary_min(high: u64, ok: impl FnMut(u64) -> bool) -> u64 {
    binary_min_from(0, high, ok)
}

fn binary_min_from(mut low: u64, mut high: u64, mut ok: impl FnMut(u64) -> bool) -> u64 {
    assert!(
        ok(high),
        "fixture does not reproduce its outcome at the probe cap"
    );
    while low < high {
        let mid = low + (high - low) / 2;
        if ok(mid) {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    low
}

struct MainObservation {
    outcome: OutcomeRecord,
    log: Vec<String>,
    host_calls: Vec<HostCallRecord>,
    fuel_used: u64,
}

fn run_main(case: &Case, hosts: Option<&HostSet>, limits: ExecLimits) -> MainObservation {
    if let Some(hosts) = hosts {
        hosts.clear_trace();
    }
    let loaded = case.load();
    assert!(
        !loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == martite_base::Severity::Error),
        "snapshot fixture must load cleanly:\n{}",
        loaded
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.render(&loaded.sources))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let sources = loaded.sources;
    let program = loaded.program.expect("snapshot program");
    let types = TypeTable::from_checker(&program);
    let mut dispatcher = hosts.map(|hosts| hosts.dispatcher(&program));
    let mut runtime = match dispatcher.as_mut() {
        Some(dispatcher) => {
            Runtime::with_host(&program, limits, dispatcher as &mut dyn HostDispatcher)
        }
        None => Runtime::new(&program, limits),
    };
    let result = runtime.run_main();
    let log = match &result {
        Ok(main) => main.log.clone(),
        Err(_) => runtime.take_log(),
    };
    let fuel_used = runtime.fuel_used();
    let host_calls = hosts
        .map(|hosts| hosts.take_calls(&types))
        .unwrap_or_default();
    MainObservation {
        outcome: outcome_from_result(result, &sources, &types),
        log,
        host_calls,
        fuel_used,
    }
}

struct PluginObservation {
    steps: Vec<StepRecord>,
    fuel_used: u64,
}

fn run_main_ir(case: &Case, hosts: Option<&HostSet>, limits: ExecLimits) -> MainObservation {
    if let Some(hosts) = hosts {
        hosts.clear_trace();
    }
    let loaded = case.load();
    assert!(
        !loaded
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == martite_base::Severity::Error),
        "IR snapshot fixture must load cleanly:\n{}",
        loaded
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.render(&loaded.sources))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let sources = loaded.sources;
    let program = loaded.program.expect("IR snapshot program");
    let ir = lower(&program, &sources).expect("IR snapshot fixture lowers");
    verify(&ir).expect("IR snapshot fixture verifies");
    let mut dispatcher = hosts.map(|hosts| hosts.ir_dispatcher(&ir));
    let mut runtime = match dispatcher.as_mut() {
        Some(dispatcher) => {
            IrRuntime::with_host(&ir, limits, dispatcher as &mut dyn IrHostDispatcher)
        }
        None => IrRuntime::new(&ir, limits),
    };
    let result = runtime.run_main();
    let log = match &result {
        Ok(main) => main.log.clone(),
        Err(_) => runtime.take_log(),
    };
    let fuel_used = runtime.fuel_used();
    let host_calls = hosts
        .map(|hosts| hosts.take_calls(&ir.types))
        .unwrap_or_default();
    MainObservation {
        outcome: outcome_from_result(result, &sources, &ir.types),
        log,
        host_calls,
        fuel_used,
    }
}

fn load_plugin(scenario: &PluginScenario) -> Plugin {
    let mut provider = MemoryProvider::new();
    let mut engine = Engine::new();
    for (path, text) in &scenario.files {
        provider.insert(path.clone(), text.clone());
        if let Some(name) = path.strip_suffix(".mti") {
            engine.interface(name.replace('/', "."), text.clone());
        }
    }
    if let Some(hosts) = &scenario.hosts {
        hosts.register(&mut engine);
    }
    let manifest = parse_manifest(&scenario.manifest).expect("snapshot manifest");
    assert_eq!(
        manifest.entry, "main.mt",
        "{}: snapshot plugins must use main.mt",
        scenario.id
    );
    engine
        .load(&scenario.manifest, &provider)
        .unwrap_or_else(|diagnostics| {
            let sources = SourceMap::new();
            panic!(
                "{}: plugin failed to load:\n{}",
                scenario.id,
                diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.render(&sources))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        })
}

fn run_plugin(plugin: &Plugin, scenario: &PluginScenario, limits: ExecLimits) -> PluginObservation {
    let program = plugin.program();
    let types = plugin.types();
    let mut state: Option<Value> = None;
    let mut steps = Vec::new();
    let mut total_fuel = 0u64;
    for step in &scenario.steps {
        if let Some(hosts) = &scenario.hosts {
            hosts.clear_trace();
        }
        let mut dispatcher = scenario
            .hosts
            .as_ref()
            .map(|hosts| hosts.dispatcher(program));
        match step {
            PluginStep::Init => {
                let Some(init) = program.init else {
                    steps.push(StepRecord {
                        action: "init".into(),
                        hook: None,
                        args: Vec::new(),
                        state: None,
                        outcome: Some(OutcomeRecord::NoHandler),
                        log: Vec::new(),
                        host_calls: Vec::new(),
                        fuel_used: 0,
                    });
                    continue;
                };
                let args = plugin.init_args();
                let (result, log, fuel_used) = run_with_runtime(
                    program,
                    limits,
                    dispatcher
                        .as_mut()
                        .map(|dispatcher| dispatcher as &mut dyn HostDispatcher),
                    |runtime| runtime.invoke(init, args),
                );
                if let Ok(result) = &result {
                    state = Some(result.value.clone());
                }
                let host_calls = scenario
                    .hosts
                    .as_ref()
                    .map(|hosts| hosts.take_calls(types))
                    .unwrap_or_default();
                total_fuel = total_fuel.saturating_add(fuel_used);
                steps.push(StepRecord {
                    action: "init".into(),
                    hook: None,
                    args: Vec::new(),
                    state: state.as_ref().map(|value| value_repr(value, types, true)),
                    outcome: Some(match result {
                        Ok(result) => outcome_from_value(&result.value, types),
                        Err(trap) => outcome_from_trap(&trap, plugin.sources()),
                    }),
                    log,
                    host_calls,
                    fuel_used,
                });
            }
            PluginStep::Call { hook, args } => {
                let args = resolve_args(plugin, args);
                let rendered = args
                    .iter()
                    .map(|value| value_repr(value, types, false))
                    .collect();
                let (result, log, fuel_used) = run_with_runtime(
                    program,
                    limits,
                    dispatcher
                        .as_mut()
                        .map(|dispatcher| dispatcher as &mut dyn HostDispatcher),
                    |runtime| runtime.invoke_handler(hook, &mut state, args),
                );
                let host_calls = scenario
                    .hosts
                    .as_ref()
                    .map(|hosts| hosts.take_calls(types))
                    .unwrap_or_default();
                total_fuel = total_fuel.saturating_add(fuel_used);
                steps.push(StepRecord {
                    action: "call".into(),
                    hook: Some((*hook).into()),
                    args: rendered,
                    state: state.as_ref().map(|value| value_repr(value, types, true)),
                    outcome: Some(match result {
                        Ok(Some(value)) => outcome_from_value(&value, types),
                        Ok(None) => OutcomeRecord::NoHandler,
                        Err(trap) => outcome_from_trap(&trap, plugin.sources()),
                    }),
                    log,
                    host_calls,
                    fuel_used,
                });
            }
            PluginStep::Shutdown => {
                let (result, log, fuel_used) = run_with_runtime(
                    program,
                    limits,
                    dispatcher
                        .as_mut()
                        .map(|dispatcher| dispatcher as &mut dyn HostDispatcher),
                    |runtime| runtime.invoke_handler("shutdown", &mut state, Vec::new()),
                );
                let host_calls = scenario
                    .hosts
                    .as_ref()
                    .map(|hosts| hosts.take_calls(types))
                    .unwrap_or_default();
                total_fuel = total_fuel.saturating_add(fuel_used);
                steps.push(StepRecord {
                    action: "shutdown".into(),
                    hook: Some("shutdown".into()),
                    args: Vec::new(),
                    state: state.as_ref().map(|value| value_repr(value, types, true)),
                    outcome: Some(match result {
                        Ok(Some(value)) => outcome_from_value(&value, types),
                        Ok(None) => OutcomeRecord::NoHandler,
                        Err(trap) => outcome_from_trap(&trap, plugin.sources()),
                    }),
                    log,
                    host_calls,
                    fuel_used,
                });
            }
        }
    }
    PluginObservation {
        steps,
        fuel_used: total_fuel,
    }
}

fn run_plugin_ir(
    plugin: &Plugin,
    scenario: &PluginScenario,
    limits: ExecLimits,
) -> PluginObservation {
    let sources = plugin.sources();
    let ir = lower(plugin.program(), sources).expect("IR plugin fixture lowers");
    verify(&ir).expect("IR plugin fixture verifies");
    let types = &ir.types;
    let mut state: Option<Value> = None;
    let mut steps = Vec::new();
    let mut total_fuel = 0u64;
    for step in &scenario.steps {
        if let Some(hosts) = &scenario.hosts {
            hosts.clear_trace();
        }
        let mut dispatcher = scenario
            .hosts
            .as_ref()
            .map(|hosts| hosts.ir_dispatcher(&ir));
        match step {
            PluginStep::Init => {
                let Some(init) = ir.entries.init else {
                    steps.push(StepRecord {
                        action: "init".into(),
                        hook: None,
                        args: Vec::new(),
                        state: None,
                        outcome: Some(OutcomeRecord::NoHandler),
                        log: Vec::new(),
                        host_calls: Vec::new(),
                        fuel_used: 0,
                    });
                    continue;
                };
                let args = plugin.init_args();
                let (result, log, fuel_used) = run_with_ir_runtime(
                    &ir,
                    limits,
                    dispatcher
                        .as_mut()
                        .map(|dispatcher| dispatcher as &mut dyn IrHostDispatcher),
                    |runtime| runtime.invoke(init, args),
                );
                if let Ok(result) = &result {
                    state = Some(result.value.clone());
                }
                let host_calls = scenario
                    .hosts
                    .as_ref()
                    .map(|hosts| hosts.take_calls(types))
                    .unwrap_or_default();
                total_fuel = total_fuel.saturating_add(fuel_used);
                steps.push(StepRecord {
                    action: "init".into(),
                    hook: None,
                    args: Vec::new(),
                    state: state.as_ref().map(|value| value_repr(value, types, true)),
                    outcome: Some(match result {
                        Ok(result) => outcome_from_value(&result.value, types),
                        Err(trap) => outcome_from_trap(&trap, sources),
                    }),
                    log,
                    host_calls,
                    fuel_used,
                });
            }
            PluginStep::Call { hook, args } => {
                let args = resolve_args(plugin, args);
                let rendered = args
                    .iter()
                    .map(|value| value_repr(value, types, false))
                    .collect();
                let (result, log, fuel_used) = run_with_ir_runtime(
                    &ir,
                    limits,
                    dispatcher
                        .as_mut()
                        .map(|dispatcher| dispatcher as &mut dyn IrHostDispatcher),
                    |runtime| runtime.invoke_handler(hook, &mut state, args),
                );
                let host_calls = scenario
                    .hosts
                    .as_ref()
                    .map(|hosts| hosts.take_calls(types))
                    .unwrap_or_default();
                total_fuel = total_fuel.saturating_add(fuel_used);
                steps.push(StepRecord {
                    action: "call".into(),
                    hook: Some((*hook).into()),
                    args: rendered,
                    state: state.as_ref().map(|value| value_repr(value, types, true)),
                    outcome: Some(match result {
                        Ok(Some(value)) => outcome_from_value(&value, types),
                        Ok(None) => OutcomeRecord::NoHandler,
                        Err(trap) => outcome_from_trap(&trap, sources),
                    }),
                    log,
                    host_calls,
                    fuel_used,
                });
            }
            PluginStep::Shutdown => {
                let (result, log, fuel_used) = run_with_ir_runtime(
                    &ir,
                    limits,
                    dispatcher
                        .as_mut()
                        .map(|dispatcher| dispatcher as &mut dyn IrHostDispatcher),
                    |runtime| runtime.invoke_handler("shutdown", &mut state, Vec::new()),
                );
                let host_calls = scenario
                    .hosts
                    .as_ref()
                    .map(|hosts| hosts.take_calls(types))
                    .unwrap_or_default();
                total_fuel = total_fuel.saturating_add(fuel_used);
                steps.push(StepRecord {
                    action: "shutdown".into(),
                    hook: Some("shutdown".into()),
                    args: Vec::new(),
                    state: state.as_ref().map(|value| value_repr(value, types, true)),
                    outcome: Some(match result {
                        Ok(Some(value)) => outcome_from_value(&value, types),
                        Ok(None) => OutcomeRecord::NoHandler,
                        Err(trap) => outcome_from_trap(&trap, sources),
                    }),
                    log,
                    host_calls,
                    fuel_used,
                });
            }
        }
    }
    PluginObservation {
        steps,
        fuel_used: total_fuel,
    }
}

fn resolve_args(plugin: &Plugin, args: &[ArgSpec]) -> Vec<Value> {
    args.iter()
        .map(|arg| match arg {
            ArgSpec::Value(value) => value.clone(),
            ArgSpec::Handle { type_name, token } => plugin
                .make_handle(type_name, *token)
                .unwrap_or_else(|| panic!("handle type `{type_name}` is not declared")),
        })
        .collect()
}

fn run_with_runtime<'p, 'h, R>(
    program: &'p CheckedProgram,
    limits: ExecLimits,
    dispatcher: Option<&'h mut dyn HostDispatcher>,
    run: impl FnOnce(&mut Runtime<'p, 'h>) -> R,
) -> (R, Vec<String>, u64) {
    let mut runtime = match dispatcher {
        Some(dispatcher) => Runtime::with_host(program, limits, dispatcher),
        None => Runtime::new(program, limits),
    };
    let result = run(&mut runtime);
    let log = runtime.take_log();
    let fuel_used = runtime.fuel_used();
    (result, log, fuel_used)
}

fn run_with_ir_runtime<'p, 'h, R>(
    program: &'p IrProgram,
    limits: ExecLimits,
    dispatcher: Option<&'h mut dyn IrHostDispatcher>,
    run: impl FnOnce(&mut IrRuntime<'p, 'h>) -> R,
) -> (R, Vec<String>, u64) {
    let mut runtime = match dispatcher {
        Some(dispatcher) => IrRuntime::with_host(program, limits, dispatcher),
        None => IrRuntime::new(program, limits),
    };
    let result = run(&mut runtime);
    let log = runtime.take_log();
    let fuel_used = runtime.fuel_used();
    (result, log, fuel_used)
}

pub(crate) fn outcome_from_result(
    result: Result<martite_runtime::MainResult, Trap>,
    sources: &SourceMap,
    types: &TypeTable,
) -> OutcomeRecord {
    match result {
        Ok(result) => outcome_from_value(&result.value, types),
        Err(trap) => outcome_from_trap(&trap, sources),
    }
}

fn outcome_from_value(value: &Value, types: &TypeTable) -> OutcomeRecord {
    OutcomeRecord::Value {
        show: value_repr(value, types, true),
    }
}

fn outcome_from_trap(trap: &Trap, sources: &SourceMap) -> OutcomeRecord {
    OutcomeRecord::Trap {
        trap: trap_record(trap, sources),
    }
}

fn trap_record(trap: &Trap, sources: &SourceMap) -> TrapRecord {
    let mut record = TrapRecord {
        kind: trap_kind(trap).into(),
        message: Some(trap.panic_message()),
        source: None,
        line: None,
        column: None,
        span: None,
        text: None,
        trace: Vec::new(),
    };
    if let Trap::Panic {
        source,
        span,
        trace,
        ..
    } = trap
    {
        record.trace = trace.clone();
        if let (Some(source), Some(span)) = (source, span) {
            record_source(&mut record, sources, *source, *span);
        }
    }
    record
}

fn record_source(record: &mut TrapRecord, sources: &SourceMap, source: SourceId, span: Span) {
    if let Some(file) = sources.get(source) {
        let (line, column) = file.line_col(span.start);
        record.source = Some(file.name().into());
        record.line = Some(line);
        record.column = Some(column);
        record.span = Some(SpanRecord {
            start: span.start,
            end: span.end,
        });
        record.text = Some(file.slice(span).into());
    }
}

fn trap_kind(trap: &Trap) -> &'static str {
    match trap {
        Trap::Panic { .. } => "panic",
        Trap::OutOfFuel => "out_of_fuel",
        Trap::OutOfMemory => "out_of_memory",
        Trap::DepthExceeded => "depth_exceeded",
        Trap::Deadline => "deadline",
        Trap::HostFault(_) => "host_fault",
    }
}

pub fn value_repr(value: &Value, types: &TypeTable, top: bool) -> String {
    match value {
        Value::Handle(handle) => {
            let name = types
                .get(handle.type_id)
                .map(|def| def.name.as_ref())
                .unwrap_or("Handle");
            format!("<{name}:{}>", handle.token)
        }
        _ => martite_runtime::show(value, types, top),
    }
}

pub fn write_records(path: &std::path::Path, records: &[SnapshotRecord]) {
    let mut output = String::new();
    for record in records {
        output.push_str(&serde_json::to_string(record).expect("serialize snapshot"));
        output.push('\n');
    }
    std::fs::write(path, output).expect("write snapshot");
}

pub fn read_records(path: &std::path::Path) -> Vec<SnapshotRecord> {
    let text = std::fs::read_to_string(path).expect("read snapshot");
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("parse snapshot line"))
        .collect()
}

/// Load every JSONL snapshot in `root`, keyed by fixture id.
pub fn read_snapshot_dir(root: &std::path::Path) -> BTreeMap<String, SnapshotRecord> {
    let Ok(entries) = std::fs::read_dir(root) else {
        panic!(
            "missing snapshot directory {}; run:\n  cargo test -p martite-conformance --test snapshots -- --ignored update_snapshots",
            root.display()
        );
    };
    let mut entries: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    entries.sort();
    let mut snapshots = BTreeMap::new();
    for path in entries {
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        for record in read_records(&path) {
            let previous = snapshots.insert(record.id.clone(), record);
            assert!(previous.is_none(), "duplicate snapshot id in {path:?}");
        }
    }
    snapshots
}
