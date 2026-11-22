//! Recursive evaluator for the resolved tree IR and the default runtime path.
//!
//! It executes values, places, scalar operators, aggregates, control flow,
//! calls, builtins, hosts, mutable writeback, closures, captures, `match` and
//! pattern binding.
//!
//! The evaluator only reads [`martite_ir::IrProgram`]. Frame slots are a dense
//! inline `SmallVec` sized by `IrFn::locals`; there are no binding-id scans and
//! no per-frame hash maps. `None` marks a slot taken by `SlotRead` with
//! [`martite_ir::AccessMode::Take`], which is a `Reduce`-class feature and not
//! emitted by migration lowering.
//!
//! Host dispatch uses [`IrHostDispatcher`], whose argument is a [`HostId`].
//! This keeps the evaluator independent of checker definition ids; embedders
//! map their host registry to the IR host table once, at setup time.
//!
//! All calls funnel through `call_resolved`. The `FnId`, closure and `FnValue`
//! entry points only resolve their target; frame setup and the return path
//! exist once. Plain parameter lists fill their slots directly, while
//! destructured lambda parameters take the pattern path.

mod builtins;

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Instant;

use martite_base::{SourceId, Span};
use martite_fuel as fuel;
use martite_ir::{
    AccessMode, BuiltinFnId, BuiltinItemId, BuiltinMethodId, CaptureIndex, CaptureSource,
    ChildRange, ConstId, ConstValue, Effects, FnId, FnKind, HostId, IrFn, IrId, IrProgram,
    MatchSetId, Node, NodeKind, Pattern, PreludeVariant, Slot, StrId, StructLayoutId, Ty,
    TypeDefKind, TypeId,
};
use smallvec::SmallVec;

use crate::exec::{CallResult, ExecLimits, HostCaller, HostContext, HostFault, MainResult, Trap};
use crate::ops::{ShowOutput, show, value_eq, value_ord, write_show};
use crate::value::{
    Closure, EnumValue, FnValue, KeyRef, MAX_VALUE_DEPTH, MapValue, OrderedMap, SeqValue,
    StructValue, Value, key_from_value, key_value_size, map_key_cost, value_depth_exceeds,
};

/// Host functions resolved by the IR. Dispatch is by [`HostId`]; the embedder
/// builds the mapping from its own registry once.
pub trait IrHostDispatcher {
    fn call(
        &mut self,
        host: HostId,
        args: &[Value],
        cx: &mut HostContext,
    ) -> Result<Value, HostFault>;
}

type EvalResult<T> = Result<T, EvalError>;
type CallArgs = SmallVec<[Value; 4]>;
type Writebacks = SmallVec<[(usize, Value); 2]>;

/// Previous slot values for the bindings of an attempted match arm. Entries
/// are restored in reverse order when a guard fails.
type BindingUndo = SmallVec<[(Slot, Option<Value>); 8]>;

#[derive(Debug)]
enum EvalError {
    Trap(Box<Trap>),
    Return(Value),
    Break,
    Continue,
}

impl From<Trap> for EvalError {
    #[inline]
    fn from(trap: Trap) -> Self {
        Self::Trap(Box::new(trap))
    }
}

struct Frame<'p> {
    /// One entry per declared local. `None` is a slot whose value was taken.
    slots: SmallVec<[Option<Value>; 8]>,
    /// Dense capture values in the lambda's capture order.
    captures: Option<Rc<[Value]>>,
    source: SourceId,
    name: &'p str,
}

struct Place {
    root: Slot,
    accessors: SmallVec<[Accessor; 4]>,
}

/// Root of a stable place read: a frame slot or a closure capture.
#[derive(Clone, Copy)]
enum PlaceRoot {
    Slot(Slot),
    Capture(CaptureIndex),
}

enum Accessor {
    Field(usize),
    Tuple(u32),
    Index(Value),
}

/// Specialized index operation selected from the IR base and index node types.
///
/// `General` keeps the historical sequential match for unresolved or
/// non-container checker types (`Ty::Never`, `Ty::Param`, `Ty::Named`, a
/// missing type-table entry, or an index kind that does not fit one of the
/// four container rules).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexKind {
    ListIndex,
    ListSlice,
    StrSlice,
    MapIndex,
    General,
}

impl IndexKind {
    fn of(base: Option<&Ty>, index: Option<&Ty>) -> Self {
        match (base, index) {
            (Some(Ty::List(_)), Some(Ty::Int)) => Self::ListIndex,
            (Some(Ty::List(_)), Some(Ty::Range)) => Self::ListSlice,
            (Some(Ty::Str), Some(Ty::Range)) => Self::StrSlice,
            (Some(Ty::Map(_, _)), Some(_)) => Self::MapIndex,
            _ => Self::General,
        }
    }
}

/// One table per function, parallel to `IrFn::nodes`.
///
/// The classification is a runtime cache of the checker expression types that
/// lowering already copied into `Node::ty`; the evaluator does not need the
/// checker or the syntax tree to rebuild it.
fn build_index_kinds(program: &IrProgram) -> Vec<Vec<IndexKind>> {
    program
        .fns
        .iter()
        .map(|function| {
            function
                .nodes
                .iter()
                .map(|node| match node.kind {
                    NodeKind::Index { base, index } => {
                        let base_ty = function
                            .node(base)
                            .and_then(|base| program.types.get_ty(base.ty));
                        let index_ty = function
                            .node(index)
                            .and_then(|index| program.types.get_ty(index.ty));
                        IndexKind::of(base_ty, index_ty)
                    }
                    _ => IndexKind::General,
                })
                .collect()
        })
        .collect()
}

/// Specialized binary operation selected from the IR operand node types.
///
/// `General(op)` keeps the historical value-shape match for unresolved or
/// non-scalar checker types (`Ty::Never`, `Ty::Param`, `Ty::Named`, a missing
/// type-table entry, or an operand combination that does not fit one of the
/// scalar rules). A specialized arm that sees unexpected value shapes falls
/// back to `General` as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BinaryDispatch {
    IntAdd,
    IntSub,
    IntMul,
    IntDiv,
    IntRem,
    FloatAdd,
    FloatSub,
    FloatMul,
    FloatDiv,
    FloatRem,
    StrConcat,
    IntCompare(BinaryKind),
    FloatCompare(BinaryKind),
    StrCompare(BinaryKind),
    General(BinaryKind),
}

impl BinaryDispatch {
    fn of(op: BinaryKind, lhs: Option<&Ty>, rhs: Option<&Ty>) -> Self {
        use BinaryKind::*;
        match (op, lhs, rhs) {
            (Add, Some(Ty::Int), Some(Ty::Int)) => Self::IntAdd,
            (Sub, Some(Ty::Int), Some(Ty::Int)) => Self::IntSub,
            (Mul, Some(Ty::Int), Some(Ty::Int)) => Self::IntMul,
            (Div, Some(Ty::Int), Some(Ty::Int)) => Self::IntDiv,
            (Rem, Some(Ty::Int), Some(Ty::Int)) => Self::IntRem,
            (Add, Some(Ty::Float), Some(Ty::Float)) => Self::FloatAdd,
            (Sub, Some(Ty::Float), Some(Ty::Float)) => Self::FloatSub,
            (Mul, Some(Ty::Float), Some(Ty::Float)) => Self::FloatMul,
            (Div, Some(Ty::Float), Some(Ty::Float)) => Self::FloatDiv,
            (Rem, Some(Ty::Float), Some(Ty::Float)) => Self::FloatRem,
            (Add, Some(Ty::Str), Some(Ty::Str)) => Self::StrConcat,
            (op @ (Eq | Ne | Lt | Le | Gt | Ge), Some(Ty::Int), Some(Ty::Int)) => {
                Self::IntCompare(op)
            }
            (op @ (Eq | Ne | Lt | Le | Gt | Ge), Some(Ty::Float), Some(Ty::Float)) => {
                Self::FloatCompare(op)
            }
            (op @ (Eq | Ne | Lt | Le | Gt | Ge), Some(Ty::Str), Some(Ty::Str)) => {
                Self::StrCompare(op)
            }
            (op, _, _) => Self::General(op),
        }
    }
}

/// One table per function, parallel to `IrFn::nodes`.
///
/// Entries are only present for the eleven binary node kinds; every other node
/// stays `None` and is never consulted by the evaluator.
fn build_binary_dispatches(program: &IrProgram) -> Vec<Vec<Option<BinaryDispatch>>> {
    program
        .fns
        .iter()
        .map(|function| {
            function
                .nodes
                .iter()
                .map(|node| {
                    let (op, lhs, rhs) = match node.kind {
                        NodeKind::Add { lhs, rhs } => (BinaryKind::Add, lhs, rhs),
                        NodeKind::Sub { lhs, rhs } => (BinaryKind::Sub, lhs, rhs),
                        NodeKind::Mul { lhs, rhs } => (BinaryKind::Mul, lhs, rhs),
                        NodeKind::Div { lhs, rhs } => (BinaryKind::Div, lhs, rhs),
                        NodeKind::Rem { lhs, rhs } => (BinaryKind::Rem, lhs, rhs),
                        NodeKind::Eq { lhs, rhs } => (BinaryKind::Eq, lhs, rhs),
                        NodeKind::Ne { lhs, rhs } => (BinaryKind::Ne, lhs, rhs),
                        NodeKind::Lt { lhs, rhs } => (BinaryKind::Lt, lhs, rhs),
                        NodeKind::Le { lhs, rhs } => (BinaryKind::Le, lhs, rhs),
                        NodeKind::Gt { lhs, rhs } => (BinaryKind::Gt, lhs, rhs),
                        NodeKind::Ge { lhs, rhs } => (BinaryKind::Ge, lhs, rhs),
                        _ => return None,
                    };
                    let lhs_ty = function
                        .node(lhs)
                        .and_then(|lhs| program.types.get_ty(lhs.ty));
                    let rhs_ty = function
                        .node(rhs)
                        .and_then(|rhs| program.types.get_ty(rhs.ty));
                    Some(BinaryDispatch::of(op, lhs_ty, rhs_ty))
                })
                .collect()
        })
        .collect()
}

/// Executes one [`IrProgram`] with the limits and observable accounting rules
/// pinned by the conformance snapshots.
pub struct IrRuntime<'p, 'h> {
    program: &'p IrProgram,
    limits: ExecLimits,
    fuel: u64,
    start: Instant,
    ticks: u64,
    frames: Vec<Frame<'p>>,
    current: Option<&'p IrFn>,
    /// Per-function parameter shape, computed once from the IR.
    simple_params: Vec<bool>,
    mutable_params: Vec<bool>,
    log: Vec<String>,
    host: Option<&'h mut dyn IrHostDispatcher>,
    caller: Option<HostCaller>,
    /// Live bytes at the last exact scan.
    census_bytes: u64,
    /// Allocation bytes recorded since the last exact scan.
    pending_bytes: u64,
    /// Nominal bytes held by the host-visible print log.
    log_bytes: u64,
    /// Number of exact scans. Builders compare this before and after a build:
    /// a scan while an aggregate lived only in a Rust local discarded the
    /// pending bytes for its children, so the finished value must be scanned.
    scan_epoch: u64,
    /// Immutable values materialized from the const pool, indexed by
    /// [`ConstId`]. Program data is not part of the memory census, but the
    /// first materialization is charged to `pending_bytes`.
    const_values: Vec<Option<Value>>,
    /// Static string values, indexed by [`StrId`]. The payload is shared after
    /// the first use; every use still charges its bytes.
    literal_values: Vec<Option<Value>>,
    /// `IndexKind` per function and node, derived once from the IR node types.
    index_kinds: Vec<Vec<IndexKind>>,
    /// `BinaryDispatch` per function and node, derived once from the IR node
    /// types. `None` for non-binary nodes.
    binary_dispatches: Vec<Vec<Option<BinaryDispatch>>>,
}

impl<'p, 'h> IrRuntime<'p, 'h> {
    pub fn new(program: &'p IrProgram, limits: ExecLimits) -> Self {
        Self {
            program,
            limits,
            fuel: limits.fuel,
            start: Instant::now(),
            ticks: 0,
            frames: Vec::new(),
            current: None,
            simple_params: program
                .fns
                .iter()
                .map(|function| function.params.iter().all(|param| param.pattern.is_none()))
                .collect(),
            mutable_params: program
                .fns
                .iter()
                .map(|function| function.params.iter().any(|param| param.mutable))
                .collect(),
            log: Vec::new(),
            host: None,
            caller: None,
            census_bytes: 0,
            pending_bytes: 0,
            log_bytes: 0,
            scan_epoch: 0,
            const_values: vec![None; program.consts.len()],
            literal_values: vec![None; program.strs.len()],
            index_kinds: build_index_kinds(program),
            binary_dispatches: build_binary_dispatches(program),
        }
    }

    pub fn with_host(
        program: &'p IrProgram,
        limits: ExecLimits,
        host: &'h mut dyn IrHostDispatcher,
    ) -> Self {
        let mut runtime = Self::new(program, limits);
        runtime.host = Some(host);
        runtime
    }

    pub fn set_host(&mut self, host: &'h mut dyn IrHostDispatcher) {
        self.host = Some(host);
    }

    /// Attach the identity reported by [`HostContext::caller`] for every host
    /// function invoked by this runtime.
    pub fn set_caller(&mut self, caller: HostCaller) {
        self.caller = Some(caller);
    }

    pub fn limits(&self) -> ExecLimits {
        self.limits
    }

    /// Fuel consumed by the current run, including host-call charges.
    pub fn fuel_used(&self) -> u64 {
        self.limits.fuel.saturating_sub(self.fuel)
    }

    pub fn set_limits(&mut self, limits: ExecLimits) {
        self.limits = limits;
    }

    pub fn take_log(&mut self) -> Vec<String> {
        self.log_bytes = 0;
        std::mem::take(&mut self.log)
    }

    pub fn invoke(&mut self, function: FnId, args: Vec<Value>) -> Result<CallResult, Trap> {
        self.reset();
        self.note_values(&args);
        let (value, updated) =
            self.call_function_with_writebacks(function, CallArgs::from_vec(args))?;
        Ok(CallResult {
            value,
            params: updated.into_iter().map(|(_, value)| value).collect(),
        })
    }

    pub fn invoke_handler(
        &mut self,
        name: &str,
        state: &mut Option<Value>,
        args: Vec<Value>,
    ) -> Result<Option<Value>, Trap> {
        let Some(function) = self
            .program
            .entries
            .handlers
            .iter()
            .find(|(handler, _)| handler.as_ref() == name)
            .map(|(_, function)| *function)
        else {
            return Ok(None);
        };
        Ok(Some(self.invoke_entry(function, state, args)?))
    }

    /// Run the `shutdown` entry when the plugin declares one.
    ///
    /// Returns `false` when the plugin has no `shutdown` handler.
    pub fn invoke_shutdown(&mut self, state: &mut Option<Value>) -> Result<bool, Trap> {
        let Some(function) = self.program.entries.shutdown else {
            return Ok(false);
        };
        self.invoke_entry(function, state, Vec::new())?;
        Ok(true)
    }

    fn invoke_entry(
        &mut self,
        function: FnId,
        state: &mut Option<Value>,
        args: Vec<Value>,
    ) -> Result<Value, Trap> {
        self.reset();
        if let Some(state) = state.as_ref() {
            self.note_values(std::slice::from_ref(state));
        }
        self.note_values(&args);
        let program = self.program;
        let info = program
            .function(function)
            .ok_or_else(|| self.internal_panic("missing entry function"))?;
        let has_self = info.params.first().is_some_and(|param| param.is_self);
        let self_mutable = info.params.first().is_some_and(|param| param.mutable);
        let mut call_args = CallArgs::with_capacity(args.len() + usize::from(has_self));
        if has_self {
            call_args.push(state.clone().unwrap_or(Value::Unit));
        }
        call_args.extend(args);
        let (value, mut updated) = self.call_resolved(info, call_args, None)?;
        if has_self && self_mutable {
            *state = take_writeback(&mut updated, 0);
        }
        Ok(value)
    }

    pub fn run_main(&mut self) -> Result<MainResult, Trap> {
        let Some(main) = self.program.entries.main else {
            return Err(Trap::Panic {
                message: "no `main` function".into(),
                source: None,
                span: None,
                trace: Vec::new(),
            });
        };
        self.reset();
        let value = self.call_function(main, CallArgs::new())?;
        Ok(MainResult {
            value,
            log: self.take_log(),
        })
    }

    fn reset(&mut self) {
        self.fuel = self.limits.fuel;
        self.start = Instant::now();
        self.ticks = 0;
        self.frames.clear();
        self.census_bytes = 0;
        self.pending_bytes = 0;
        // The log survives a reset until the host takes it, so it stays part of
        // the memory census for every call.
        self.log_bytes = self.log.iter().map(|text| 16 + text.len() as u64).sum();
    }

    // ------------------------------------------------------------------
    // Frames and traps
    // ------------------------------------------------------------------

    #[inline]
    fn current_function(&self) -> &'p IrFn {
        self.current.expect("evaluation frame")
    }

    #[inline]
    fn index_kind(&self, id: IrId) -> IndexKind {
        let function = self.current_function();
        self.index_kinds
            .get(function.id.index())
            .and_then(|kinds| kinds.get(id.index()))
            .copied()
            .unwrap_or(IndexKind::General)
    }

    #[inline]
    fn binary_dispatch(&self, id: IrId, op: BinaryKind) -> BinaryDispatch {
        let function = self.current_function();
        self.binary_dispatches
            .get(function.id.index())
            .and_then(|kinds| kinds.get(id.index()))
            .copied()
            .flatten()
            .unwrap_or(BinaryDispatch::General(op))
    }

    fn internal_panic(&self, message: impl Into<String>) -> Trap {
        Trap::Panic {
            message: message.into(),
            source: self.frames.last().map(|frame| frame.source),
            span: None,
            trace: self
                .frames
                .iter()
                .map(|frame| frame.name.to_string())
                .collect(),
        }
    }

    fn panic(&self, message: impl Into<String>, span: Span) -> Trap {
        Trap::Panic {
            message: message.into(),
            source: self.frames.last().map(|frame| frame.source),
            span: Some(span),
            trace: self
                .frames
                .iter()
                .map(|frame| frame.name.to_string())
                .collect(),
        }
    }

    fn trap<T>(&self, trap: Trap) -> EvalResult<T> {
        Err(EvalError::from(trap))
    }

    /// Borrow a value for a map lookup without allocating an owned key.
    fn map_key_ref<'v>(&self, value: &'v Value) -> EvalResult<KeyRef<'v>> {
        KeyRef::new(value).map_err(|message| EvalError::from(self.internal_panic(message)))
    }

    /// Borrow a value for a map lookup, reporting invalid keys at `span`.
    fn map_key_ref_at<'v>(&self, value: &'v Value, span: Span) -> EvalResult<KeyRef<'v>> {
        KeyRef::new(value).map_err(|message| EvalError::from(self.panic(message, span)))
    }

    fn function_info(&self, function: FnId) -> Result<&'p IrFn, Trap> {
        self.program
            .function(function)
            .ok_or_else(|| self.internal_panic(format!("missing function #{:?}", function)))
    }

    /// The single call implementation. Every entry point resolves its target
    /// and then delegates here, so depth, argument, capture, frame and return
    /// handling exist in exactly one place.
    fn call_resolved(
        &mut self,
        function_info: &'p IrFn,
        args: CallArgs,
        captures: Option<Rc<[Value]>>,
    ) -> Result<(Value, Writebacks), Trap> {
        if self.frames.len() >= self.limits.depth as usize {
            return Err(Trap::DepthExceeded);
        }
        if let FnKind::Host(host) = function_info.kind {
            return Ok((self.call_host(host, &args)?, Writebacks::new()));
        }
        let Some(body) = function_info.body else {
            return Err(
                self.internal_panic(format!("function `{}` has no body", function_info.name))
            );
        };
        let lambda = matches!(function_info.kind, FnKind::Lambda);
        if args.len() != function_info.params.len() {
            if lambda {
                return Err(self.internal_panic(format!(
                    "lambda `{}` expected {} arguments, got {}",
                    function_info.name,
                    function_info.params.len(),
                    args.len()
                )));
            }
            return Err(self.internal_panic(format!(
                "`{}` expected {} arguments, got {}",
                function_info.name,
                function_info.params.len(),
                args.len()
            )));
        }
        let captures = if lambda {
            let provided = captures.as_ref().map_or(0, |captures| captures.len());
            if provided != function_info.captures.len() {
                return Err(self.internal_panic(format!(
                    "lambda `{}` expected {} captures, got {}",
                    function_info.name,
                    function_info.captures.len(),
                    provided
                )));
            }
            captures
        } else {
            if captures.is_some() {
                return Err(self.internal_panic("captures on a non-lambda function"));
            }
            None
        };

        self.push_frame(function_info, args, captures)?;
        let escaped = if lambda {
            "loop control escaped a lambda"
        } else {
            "`break`/`continue` escaped a loop"
        };
        let result = self.run_body(function_info, body, escaped);
        self.pop_frame(function_info, result)
    }

    fn call_function(&mut self, function: FnId, args: CallArgs) -> Result<Value, Trap> {
        let function_info = self.function_info(function)?;
        self.call_resolved(function_info, args, None)
            .map(|(value, _)| value)
    }

    fn call_function_with_writebacks(
        &mut self,
        function: FnId,
        args: CallArgs,
    ) -> Result<(Value, Writebacks), Trap> {
        let function_info = self.function_info(function)?;
        self.call_resolved(function_info, args, None)
    }

    /// Calls a closure value. Lambda parameters cannot be `mut`, so a closure
    /// call never produces writeback values.
    #[inline]
    fn call_closure(&mut self, closure: &Closure, args: CallArgs) -> Result<Value, Trap> {
        let function_info = self.function_info(closure.target)?;
        self.call_resolved(function_info, args, closure.captures.clone())
            .map(|(value, _)| value)
    }

    #[inline]
    fn call_closure_value(&mut self, function: &FnValue, args: CallArgs) -> Result<Value, Trap> {
        match function {
            FnValue::Def(id) => self.call_function(*id, args),
            FnValue::Closure(closure) => self.call_closure(closure, args),
        }
    }

    /// Builds and pushes one frame, binds its parameters, and runs the shared
    /// pre-body memory check. The frame is popped again on every failure path.
    fn push_frame(
        &mut self,
        function_info: &'p IrFn,
        args: CallArgs,
        captures: Option<Rc<[Value]>>,
    ) -> Result<(), Trap> {
        // Plain parameter lists are the common case. Filling their slots
        // directly avoids the pattern vector and the bounds-checked path that
        // only destructured lambda parameters need.
        let simple = self.simple_params[function_info.id.index()];
        let mut slots = SmallVec::from_elem(None, function_info.locals as usize);
        let mut pattern_params: SmallVec<[(&Pattern, Value); 2]> = SmallVec::new();
        for (param, value) in function_info.params.iter().zip(args) {
            if simple {
                slots[param.slot.index()] = Some(value);
                continue;
            }
            let Some(pattern_id) = param.pattern else {
                let Some(slot) = slots.get_mut(param.slot.index()) else {
                    return Err(self.internal_panic("parameter slot is out of range"));
                };
                *slot = Some(value);
                continue;
            };
            let Some(pattern) = function_info.pattern(pattern_id) else {
                let message = if matches!(function_info.kind, FnKind::Lambda) {
                    "lambda parameter pattern is out of range"
                } else {
                    "parameter pattern is out of range"
                };
                return Err(self.internal_panic(message));
            };
            pattern_params.push((pattern, value));
        }

        self.frames.push(Frame {
            slots,
            captures,
            source: function_info.source,
            name: function_info.name.as_ref(),
        });
        for (pattern, value) in pattern_params {
            if let Err(error) = self.bind_pattern(pattern, &value) {
                let trap = match error {
                    EvalError::Trap(trap) => *trap,
                    _ => self.internal_panic("lambda parameter binding failed"),
                };
                self.frames.pop();
                return Err(trap);
            }
        }
        if let Err(trap) = self.maybe_check_memory() {
            self.frames.pop();
            return Err(match trap {
                EvalError::Trap(trap) => *trap,
                _ => Trap::OutOfMemory,
            });
        }
        Ok(())
    }

    /// Runs a pushed frame's body, restoring `current` on every path. Function
    /// bodies lower to zero-charge `Block`s, so `eval_body`'s transparent-block
    /// skip is safe here for every call kind.
    #[inline]
    fn run_body(
        &mut self,
        function_info: &'p IrFn,
        body: IrId,
        escaped_loop: &'static str,
    ) -> Result<Value, Trap> {
        let previous = self.current;
        self.current = Some(function_info);
        let result = match self.eval_body(body) {
            Ok(value) | Err(EvalError::Return(value)) => Ok(value),
            Err(EvalError::Trap(trap)) => Err(*trap),
            Err(EvalError::Break) | Err(EvalError::Continue) => {
                Err(self.internal_panic(escaped_loop))
            }
        };
        self.current = previous;
        result
    }

    /// The single return path: extracts mutable writebacks while the frame is
    /// still live, pops the frame, then applies the returned-value memory and
    /// depth checks in the order the snapshots pin.
    fn pop_frame(
        &mut self,
        function_info: &'p IrFn,
        result: Result<Value, Trap>,
    ) -> Result<(Value, Writebacks), Trap> {
        let updated: Writebacks = if result.is_ok() && self.mutable_params[function_info.id.index()]
        {
            let frame = self.frames.last_mut().expect("evaluation frame");
            function_info
                .params
                .iter()
                .enumerate()
                .filter(|(_, param)| param.mutable)
                .map(|(index, param)| {
                    let value = frame
                        .slots
                        .get_mut(param.slot.index())
                        .and_then(|slot| slot.take())
                        .unwrap_or(Value::Unit);
                    (index, value)
                })
                .collect()
        } else {
            Writebacks::new()
        };
        self.frames.pop();
        let value = result?;
        self.maybe_check_returned_value(&value)
            .map_err(|error| match error {
                EvalError::Trap(trap) => *trap,
                _ => Trap::OutOfMemory,
            })?;
        if value_depth_exceeds(&value, MAX_VALUE_DEPTH) {
            return Err(Trap::DepthExceeded);
        }
        Ok((value, updated))
    }

    fn call_host(&mut self, host: HostId, args: &[Value]) -> Result<Value, Trap> {
        if self.deadline_due() {
            return Err(Trap::Deadline);
        }
        let program = self.program;
        let Some(info) = program.host(host) else {
            return Err(self.internal_panic(format!("missing host function #{:?}", host)));
        };
        let caller = self.caller.clone();
        let Some(dispatcher) = self.host.as_deref_mut() else {
            return Err(Trap::Panic {
                message: "no test double".into(),
                source: None,
                span: None,
                trace: Vec::new(),
            });
        };
        let mut cx = HostContext::new(info.opaque_return, caller);
        let result = dispatcher.call(host, args, &mut cx);
        self.fuel = self.fuel.saturating_sub(fuel::host_call_cost(cx.charged));
        if self.fuel == 0 {
            return Err(Trap::OutOfFuel);
        }
        if self.deadline_due() {
            return Err(Trap::Deadline);
        }
        match result {
            Ok(value) => {
                if value_depth_exceeds(&value, MAX_VALUE_DEPTH) {
                    return Err(Trap::DepthExceeded);
                }
                self.note_value(&value);
                Ok(value)
            }
            Err(fault) => Err(Trap::HostFault(fault.message)),
        }
    }

    // ------------------------------------------------------------------
    // Limits and memory accounting
    // ------------------------------------------------------------------

    #[inline]
    fn charge(&mut self, units: u64) -> EvalResult<()> {
        self.fuel = self.fuel.saturating_sub(units);
        if self.fuel == 0 {
            return self.trap(Trap::OutOfFuel);
        }
        self.ticks += 1;
        if self.ticks & 255 == 0 {
            if self.ticks & 1023 == 0 && self.deadline_due() {
                return self.trap(Trap::Deadline);
            }
            self.maybe_check_memory()?;
        }
        Ok(())
    }

    fn note_alloc(&mut self, bytes: u64) {
        self.pending_bytes = self.pending_bytes.saturating_add(bytes);
    }

    fn note_value(&mut self, value: &Value) {
        self.note_alloc(value.meta().bytes);
    }

    fn note_values(&mut self, values: &[Value]) {
        for value in values {
            self.note_value(value);
        }
    }

    fn note_log(&mut self, text: String) {
        self.log_bytes = self.log_bytes.saturating_add(16 + text.len() as u64);
        self.log.push(text);
    }

    fn used_bytes(&self) -> u64 {
        self.census_bytes
            .saturating_add(self.pending_bytes)
            .saturating_add(self.log_bytes)
    }

    fn maybe_check_memory(&mut self) -> EvalResult<()> {
        if self.used_bytes() > self.limits.memory {
            self.refresh_memory(None)
        } else {
            Ok(())
        }
    }

    fn maybe_check_returned_value(&mut self, value: &Value) -> EvalResult<()> {
        if self.used_bytes() > self.limits.memory {
            self.refresh_memory(Some(value))
        } else {
            Ok(())
        }
    }

    /// Bytes an output builder may still allocate without crossing the limit.
    ///
    /// If the running estimate is already over the limit, an exact scan is
    /// attempted first: pending bytes can overestimate when a temporary was
    /// dropped, and a builder should not fail because of a dead allocation.
    fn memory_headroom(&mut self, extra: Option<&Value>) -> EvalResult<u64> {
        if self.used_bytes() > self.limits.memory {
            self.refresh_memory(extra)?;
        }
        Ok(self.limits.memory.saturating_sub(self.used_bytes()))
    }

    fn ensure_output_capacity(&mut self, bytes: u64) -> EvalResult<()> {
        if bytes > self.memory_headroom(None)? {
            return self.trap(Trap::OutOfMemory);
        }
        Ok(())
    }

    /// Scan an aggregate that just finished its Rust-local build.
    ///
    /// A scan that fires while the aggregate is under construction resets
    /// `pending_bytes` without seeing the children, because they are reachable
    /// only from the builder's local. Re-scanning the finished value recovers
    /// those bytes before the aggregate escapes.
    fn finish_aggregate(&mut self, value: &Value, build_epoch: u64) -> EvalResult<()> {
        if self.scan_epoch != build_epoch || self.used_bytes() > self.limits.memory {
            self.refresh_memory(Some(value))?;
        }
        Ok(())
    }

    /// Re-scan a value that stayed in a Rust local across an exact scan.
    ///
    /// Scans only see frames, so a value held by a builder or an interpolation
    /// expression can lose its `pending_bytes` when a scan fires between its
    /// evaluation and its first use.
    fn pin_value(&mut self, value: &Value, eval_epoch: u64) -> EvalResult<()> {
        if self.scan_epoch != eval_epoch {
            self.refresh_memory(Some(value))?;
        }
        Ok(())
    }

    fn refresh_memory(&mut self, extra: Option<&Value>) -> EvalResult<()> {
        self.scan_epoch = self.scan_epoch.wrapping_add(1);
        let mut seen = HashSet::new();
        let mut total = 0u64;
        for frame in &self.frames {
            for value in frame.slots.iter().flatten() {
                total = total.saturating_add(value.size_bytes(&mut seen));
            }
            if let Some(captures) = &frame.captures {
                for value in captures.iter() {
                    total = total.saturating_add(value.size_bytes(&mut seen));
                }
            }
        }
        if let Some(value) = extra {
            total = total.saturating_add(value.size_bytes(&mut seen));
        }
        self.census_bytes = total;
        self.pending_bytes = 0;
        let live = total.saturating_add(self.log_bytes);
        if live > self.limits.memory {
            Err(EvalError::from(Trap::OutOfMemory))
        } else {
            Ok(())
        }
    }

    fn deadline_due(&self) -> bool {
        self.limits
            .deadline
            .is_some_and(|deadline| self.start.elapsed() >= deadline)
    }

    fn ensure_deadline(&self) -> EvalResult<()> {
        if self.deadline_due() {
            Err(EvalError::from(Trap::Deadline))
        } else {
            Ok(())
        }
    }

    #[inline]
    fn safepoint(&mut self, id: IrId) -> EvalResult<()> {
        if self.current_function().is_safepoint(id) {
            self.maybe_check_memory()
        } else {
            Ok(())
        }
    }

    // ------------------------------------------------------------------
    // Nodes
    // ------------------------------------------------------------------

    fn eval(&mut self, id: IrId) -> EvalResult<Value> {
        let function = self.current_function();
        // The verifier guarantees every child id, span and side-vector entry
        // is in range. Direct indexing keeps the interpreter's hottest path
        // branch-free.
        let node = &function.nodes[id.index()];
        let span = function.spans[id.index()];
        if node.charge.pre != 0 {
            self.charge(u64::from(node.charge.pre))?;
        }

        match node.kind {
            NodeKind::Unit => Ok(Value::Unit),
            NodeKind::Int(constant) => self.eval_int_constant(constant),
            NodeKind::Float(constant) => self.eval_float_constant(constant),
            NodeKind::Bool(value) => Ok(Value::Bool(value)),
            NodeKind::Str(string) => self.eval_static_string(string),
            NodeKind::ConstRef(constant) => self.materialize_const(constant),
            NodeKind::FnRef(function) => Ok(Value::Fn(FnValue::Def(function))),
            NodeKind::BuiltinFnRef(function) => Ok(Value::Builtin(function)),
            NodeKind::BuiltinModuleRef(item) => self.builtin_item_value(item),

            NodeKind::SlotRead { slot, mode } => {
                let frame = self.frames.last_mut().expect("evaluation frame");
                let cell = &mut frame.slots[slot.index()];
                match mode {
                    AccessMode::Share => Ok(cell
                        .clone()
                        .expect("verifier guarantees every shared slot read is initialized")),
                    AccessMode::Take => Ok(cell
                        .take()
                        .expect("verifier guarantees every taken slot read is initialized")),
                }
            }
            NodeKind::CaptureRead(index) => self.read_capture(index),
            NodeKind::SlotPlace(_) => self.trap(self.internal_panic("slot place used as a value")),
            NodeKind::SlotWrite { slot, value } | NodeKind::SlotInit { slot, value } => {
                let value = self.eval(value)?;
                self.set_slot(slot, value);
                Ok(Value::Unit)
            }
            NodeKind::Field { base, index } => match self.eval_place_read(id)? {
                Some(value) => Ok(value),
                None => self.eval_field(base, index),
            },
            NodeKind::TupleField { base, index } => match self.eval_place_read(id)? {
                Some(value) => Ok(value),
                None => self.eval_tuple_field(base, index),
            },
            NodeKind::Index { base, index } => {
                if let Some(value) = self.eval_place_read(id)? {
                    return Ok(value);
                }
                let base = self.eval(base)?;
                let index = self.eval(index)?;
                let kind = self.index_kind(id);
                self.index_value_kind(kind, base, index, span)
            }

            NodeKind::Neg(inner) => self.eval_neg(inner, span),
            NodeKind::Not(inner) => {
                let value = self.eval(inner)?;
                match value {
                    Value::Bool(value) => Ok(Value::Bool(!value)),
                    _ => self.trap(self.internal_panic("invalid unary operand")),
                }
            }
            NodeKind::Add { lhs, rhs } => {
                self.eval_binary_values(id, BinaryKind::Add, lhs, rhs, span)
            }
            NodeKind::Sub { lhs, rhs } => {
                self.eval_binary_values(id, BinaryKind::Sub, lhs, rhs, span)
            }
            NodeKind::Mul { lhs, rhs } => {
                self.eval_binary_values(id, BinaryKind::Mul, lhs, rhs, span)
            }
            NodeKind::Div { lhs, rhs } => {
                self.eval_binary_values(id, BinaryKind::Div, lhs, rhs, span)
            }
            NodeKind::Rem { lhs, rhs } => {
                self.eval_binary_values(id, BinaryKind::Rem, lhs, rhs, span)
            }
            NodeKind::Eq { lhs, rhs } => {
                self.eval_binary_values(id, BinaryKind::Eq, lhs, rhs, span)
            }
            NodeKind::Ne { lhs, rhs } => {
                self.eval_binary_values(id, BinaryKind::Ne, lhs, rhs, span)
            }
            NodeKind::Lt { lhs, rhs } => {
                self.eval_binary_values(id, BinaryKind::Lt, lhs, rhs, span)
            }
            NodeKind::Le { lhs, rhs } => {
                self.eval_binary_values(id, BinaryKind::Le, lhs, rhs, span)
            }
            NodeKind::Gt { lhs, rhs } => {
                self.eval_binary_values(id, BinaryKind::Gt, lhs, rhs, span)
            }
            NodeKind::Ge { lhs, rhs } => {
                self.eval_binary_values(id, BinaryKind::Ge, lhs, rhs, span)
            }
            NodeKind::And { lhs, rhs } => {
                let lhs = self.eval(lhs)?;
                if !matches!(lhs, Value::Bool(true)) {
                    return Ok(Value::Bool(false));
                }
                self.eval(rhs)
            }
            NodeKind::Or { lhs, rhs } => {
                let lhs = self.eval(lhs)?;
                if matches!(lhs, Value::Bool(true)) {
                    return Ok(Value::Bool(true));
                }
                self.eval(rhs)
            }
            NodeKind::Coalesce { lhs, rhs } => {
                let lhs = self.eval(lhs)?;
                match lhs {
                    Value::Option(Some(value)) => Ok(*value),
                    Value::Option(None) => self.eval(rhs),
                    _ => self.trap(self.internal_panic("`??` applied to non-option")),
                }
            }

            NodeKind::CallDirect { target, first_arg } => self.eval_call(target, first_arg),
            NodeKind::CallMethod {
                target,
                first_child,
            } => self.eval_call(target, first_child),
            NodeKind::CallValue { first_child, argc } => {
                self.eval_call_value(first_child, argc, span)
            }
            NodeKind::CallBuiltinFn { builtin, first_arg } => {
                self.eval_builtin_fn_call(builtin, first_arg, span)
            }
            NodeKind::CallBuiltinMethod {
                method,
                first_child,
            } => self.call_builtin_method(method, first_child, span),
            NodeKind::CallBuiltinModule { item, first_arg } => {
                self.eval_builtin_module_call(item, first_arg, span)
            }
            NodeKind::CallHost { host, first_arg } => self.eval_host_call(host, first_arg),

            NodeKind::Struct { type_id, layout } => {
                self.eval_struct(type_id, layout, false, span, node.charge.post)
            }
            NodeKind::StructUpdate { type_id, layout } => {
                self.eval_struct(type_id, layout, true, span, node.charge.post)
            }
            NodeKind::EnumVariant {
                variant,
                first_payload,
            } => self.eval_enum(variant, first_payload, node.ty, node.charge.post, span),
            NodeKind::EnumValue { variant } => self.eval_enum_value(variant, node.ty, span),
            NodeKind::PreludeVariant {
                variant,
                first_payload,
            } => self.eval_prelude(variant, first_payload, span),
            NodeKind::Tuple(range) => self.eval_sequence(range, true, span, node.charge.post),
            NodeKind::List(range) => self.eval_sequence(range, false, span, node.charge.post),
            NodeKind::Map(range) => self.eval_map(range, span),
            NodeKind::Range { start, end } => self.eval_range(start, end, false),
            NodeKind::InclusiveRange { start, end } => self.eval_range(start, end, true),

            NodeKind::Block(range) | NodeKind::ExprBlock(range) => self.eval_block(range),
            NodeKind::If(range) => self.eval_if(range),
            NodeKind::While { cond, body } => {
                self.eval_while(cond, body, u64::from(node.charge.per_iter))
            }
            NodeKind::For {
                pattern,
                first_child,
            } => self.eval_for(pattern, first_child, u64::from(node.charge.per_iter)),
            NodeKind::Break => Err(EvalError::Break),
            NodeKind::Continue => Err(EvalError::Continue),
            NodeKind::Return(value) => {
                let value = match value {
                    Some(value) => self.eval(value)?,
                    None => Value::Unit,
                };
                Err(EvalError::Return(value))
            }
            NodeKind::Match {
                match_set,
                scrutinee,
            } => self.eval_match(match_set, scrutinee),
            NodeKind::Try(inner) => self.eval_try(inner),

            NodeKind::Let { pattern, value } => {
                let value = self.eval(value)?;
                let function = self.current_function();
                let Some(pattern) = function.pattern(pattern) else {
                    return self.trap(self.internal_panic("let has no binding pattern"));
                };
                self.bind_pattern(pattern, &value)?;
                self.safepoint(id)?;
                Ok(Value::Unit)
            }
            NodeKind::Assign { place, value } => {
                let place = self.eval_place(place)?;
                let value = self.eval(value)?;
                self.write_place(place, value)?;
                self.safepoint(id)?;
                Ok(Value::Unit)
            }
            kind @ (NodeKind::AddAssign { .. }
            | NodeKind::SubAssign { .. }
            | NodeKind::MulAssign { .. }
            | NodeKind::DivAssign { .. }
            | NodeKind::RemAssign { .. }) => {
                let (place, value) = match kind {
                    NodeKind::AddAssign { place, value }
                    | NodeKind::SubAssign { place, value }
                    | NodeKind::MulAssign { place, value }
                    | NodeKind::DivAssign { place, value }
                    | NodeKind::RemAssign { place, value } => (place, value),
                    _ => unreachable!(),
                };
                let place = self.eval_place(place)?;
                let lhs = self.read_place(&place)?;
                let rhs = self.eval(value)?;
                let op = match kind {
                    NodeKind::AddAssign { .. } => BinaryKind::Add,
                    NodeKind::SubAssign { .. } => BinaryKind::Sub,
                    NodeKind::MulAssign { .. } => BinaryKind::Mul,
                    NodeKind::DivAssign { .. } => BinaryKind::Div,
                    NodeKind::RemAssign { .. } => BinaryKind::Rem,
                    _ => unreachable!(),
                };
                let value = self.binary_values(op, lhs, rhs, span)?;
                self.write_place(place, value)?;
                self.safepoint(id)?;
                Ok(Value::Unit)
            }
            NodeKind::ExprStmt(inner) => {
                let value = self.eval(inner)?;
                self.safepoint(id)?;
                Ok(value)
            }

            NodeKind::ConvertInt(inner) => self.eval_convert_int(inner, span),
            NodeKind::ConvertFloat(inner) => {
                let value = self.eval(inner)?;
                match value {
                    Value::Int(value) => Ok(Value::Float(value as f64)),
                    _ => self.trap(self.internal_panic("invalid conversion")),
                }
            }
            NodeKind::ConvertStr(inner) => {
                let epoch = self.scan_epoch;
                let value = self.eval(inner)?;
                self.charge(fuel::show_cost(value.meta().slots))?;
                self.pin_value(&value, epoch)?;
                let text = self.render_show(&value, true)?;
                self.note_alloc(16 + text.len() as u64);
                Ok(Value::str(text))
            }

            NodeKind::MakeClosure(target) => self.eval_make_closure(target),

            NodeKind::StrInterp(range) => self.eval_string(range),
            NodeKind::Assert(range) => self.eval_assert(range, span),
            NodeKind::Panic(range) => self.eval_panic(range, span),
            NodeKind::MutArg(_) => {
                self.trap(self.internal_panic("mutable argument used as a value"))
            }
            NodeKind::Charge(units) => {
                // Charging one unit at a time preserves the per-charge tick
                // cadence (and therefore safepoint timing) of the nodes a
                // `Preserve` pass removed. Lowering never emits this node.
                for _ in 0..units {
                    self.charge(1)?;
                }
                Ok(Value::Unit)
            }
        }
    }

    #[inline]
    fn eval_int_constant(&mut self, id: ConstId) -> EvalResult<Value> {
        match self.program.constant(id) {
            Some(ConstValue::Int(value)) => Ok(Value::Int(*value)),
            _ => self.trap(self.internal_panic("int literal has an invalid const pool entry")),
        }
    }

    #[inline]
    fn eval_float_constant(&mut self, id: ConstId) -> EvalResult<Value> {
        match self.program.constant(id) {
            Some(ConstValue::Float(value)) => Ok(Value::Float(*value)),
            _ => self.trap(self.internal_panic("float literal has an invalid const pool entry")),
        }
    }

    fn eval_static_string(&mut self, id: StrId) -> EvalResult<Value> {
        let program = self.program;
        let Some(text) = program.string(id) else {
            return self.trap(self.internal_panic("string literal has an invalid pool entry"));
        };
        self.charge(fuel::string_cost(text.len() as u64))?;
        if let Some(value) = &self.literal_values[id.index()] {
            return Ok(value.clone());
        }
        let value = Value::str(text);
        self.note_alloc(16 + text.len() as u64);
        self.literal_values[id.index()] = Some(value.clone());
        Ok(value)
    }

    fn materialize_const(&mut self, id: ConstId) -> EvalResult<Value> {
        if let Some(value) = &self.const_values[id.index()] {
            return Ok(value.clone());
        }
        let Some(constant) = self.program.constant(id) else {
            return self.trap(self.internal_panic("constant has an invalid pool entry"));
        };
        let value = self.const_to_value(constant)?;
        self.note_value(&value);
        self.const_values[id.index()] = Some(value.clone());
        Ok(value)
    }

    fn const_to_value(&self, value: &ConstValue) -> EvalResult<Value> {
        Ok(match value {
            ConstValue::Unit => Value::Unit,
            ConstValue::Int(value) => Value::Int(*value),
            ConstValue::Float(value) => Value::Float(*value),
            ConstValue::Bool(value) => Value::Bool(*value),
            ConstValue::Str(id) => Value::str(self.program.string(*id).unwrap_or("")),
            ConstValue::Tuple(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items.iter() {
                    values.push(self.const_to_value(item)?);
                }
                Value::tuple(values)
            }
            ConstValue::List(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items.iter() {
                    values.push(self.const_to_value(item)?);
                }
                Value::list(values)
            }
            ConstValue::Map(entries) => {
                let mut map = OrderedMap::new();
                for (key, value) in entries.iter() {
                    let key_value = self.const_to_value(key)?;
                    if let Ok(key) = key_from_value(&key_value) {
                        map.insert(key, self.const_to_value(value)?);
                    }
                }
                Value::map(map)
            }
            ConstValue::Struct(type_id, fields) => {
                let mut values = Vec::with_capacity(fields.len());
                for field in fields.iter() {
                    values.push(self.const_to_value(field)?);
                }
                Value::Struct(Rc::new(StructValue::new(*type_id, values)))
            }
            ConstValue::Enum(type_id, variant, payload) => {
                let mut values = Vec::with_capacity(payload.len());
                for item in payload.iter() {
                    values.push(self.const_to_value(item)?);
                }
                Value::Enum(Rc::new(EnumValue::new(*type_id, *variant as usize, values)))
            }
            ConstValue::Prelude(variant, payload) => {
                let mut values = Vec::with_capacity(payload.len());
                for item in payload.iter() {
                    values.push(self.const_to_value(item)?);
                }
                let payload = values.into_iter().next().unwrap_or(Value::Unit);
                match variant {
                    PreludeVariant::None => Value::Option(None),
                    PreludeVariant::Some => Value::Option(Some(Box::new(payload))),
                    PreludeVariant::Ok => Value::Result(Ok(Box::new(payload))),
                    PreludeVariant::Err => Value::Result(Err(Box::new(payload))),
                }
            }
        })
    }

    fn builtin_item_value(&mut self, item: BuiltinItemId) -> EvalResult<Value> {
        match item {
            BuiltinItemId::MATH_PI => Ok(Value::Float(std::f64::consts::PI)),
            BuiltinItemId::MATH_EULER => Ok(Value::Float(std::f64::consts::E)),
            BuiltinItemId::JSON => self.trap(self.internal_panic("type used as a value")),
            _ => Ok(Value::BuiltinModuleFn(item)),
        }
    }

    #[inline]
    fn read_slot(&mut self, slot: Slot, mode: AccessMode) -> EvalResult<Value> {
        let frame = self.frames.last_mut().expect("evaluation frame");
        let cell = &mut frame.slots[slot.index()];
        match mode {
            AccessMode::Share => Ok(cell
                .clone()
                .expect("verifier guarantees every shared slot read is initialized")),
            AccessMode::Take => Ok(cell
                .take()
                .expect("verifier guarantees every taken slot is initialized")),
        }
    }

    #[inline]
    fn set_slot(&mut self, slot: Slot, value: Value) {
        self.frames.last_mut().expect("evaluation frame").slots[slot.index()] = Some(value);
    }

    fn eval_field(&mut self, base: IrId, index: u32) -> EvalResult<Value> {
        let value = self.eval(base)?;
        match value {
            Value::Struct(value) => Ok(value
                .fields
                .get(index as usize)
                .cloned()
                .unwrap_or(Value::Unit)),
            _ => self.trap(self.internal_panic("field access on non-struct")),
        }
    }

    fn eval_tuple_field(&mut self, base: IrId, index: u32) -> EvalResult<Value> {
        let value = self.eval(base)?;
        match value {
            Value::Tuple(items) => Ok(items.get(index as usize).cloned().unwrap_or(Value::Unit)),
            _ => self.trap(self.internal_panic("tuple field access on non-tuple")),
        }
    }

    fn eval_neg(&mut self, inner: IrId, span: Span) -> EvalResult<Value> {
        let value = self.eval(inner)?;
        match value {
            Value::Int(value) => value
                .checked_neg()
                .map(Value::Int)
                .ok_or_else(|| EvalError::from(self.panic("integer overflow", span))),
            Value::Float(value) => Ok(Value::Float(-value)),
            _ => self.trap(self.internal_panic("invalid unary operand")),
        }
    }

    #[inline]
    fn eval_binary_values(
        &mut self,
        id: IrId,
        op: BinaryKind,
        lhs: IrId,
        rhs: IrId,
        span: Span,
    ) -> EvalResult<Value> {
        let lhs = self.eval(lhs)?;
        let rhs = self.eval(rhs)?;
        let dispatch = self.binary_dispatch(id, op);
        self.binary_values_kind(dispatch, lhs, rhs, span)
    }

    /// Dispatch on the `BinaryDispatch` derived from the IR operand types.
    ///
    /// A specialized arm only handles its own value shape; anything else falls
    /// back to `binary_values` so malformed or unresolved IR keeps the generic
    /// rules (and the same traps and fuel).
    fn binary_values_kind(
        &mut self,
        dispatch: BinaryDispatch,
        lhs: Value,
        rhs: Value,
        span: Span,
    ) -> EvalResult<Value> {
        use BinaryKind::*;
        match dispatch {
            BinaryDispatch::IntAdd => self.int_arithmetic(Add, lhs, rhs, span),
            BinaryDispatch::IntSub => self.int_arithmetic(Sub, lhs, rhs, span),
            BinaryDispatch::IntMul => self.int_arithmetic(Mul, lhs, rhs, span),
            BinaryDispatch::IntDiv => self.int_arithmetic(Div, lhs, rhs, span),
            BinaryDispatch::IntRem => self.int_arithmetic(Rem, lhs, rhs, span),
            BinaryDispatch::FloatAdd => self.float_arithmetic(Add, lhs, rhs, span),
            BinaryDispatch::FloatSub => self.float_arithmetic(Sub, lhs, rhs, span),
            BinaryDispatch::FloatMul => self.float_arithmetic(Mul, lhs, rhs, span),
            BinaryDispatch::FloatDiv => self.float_arithmetic(Div, lhs, rhs, span),
            BinaryDispatch::FloatRem => self.float_arithmetic(Rem, lhs, rhs, span),
            BinaryDispatch::StrConcat => match (lhs, rhs) {
                (Value::Str(lhs), Value::Str(rhs)) => self.concat_strings(lhs, rhs),
                (lhs, rhs) => self.binary_values(Add, lhs, rhs, span),
            },
            BinaryDispatch::IntCompare(op) => self.int_compare(op, lhs, rhs, span),
            BinaryDispatch::FloatCompare(op) => self.float_compare(op, lhs, rhs, span),
            BinaryDispatch::StrCompare(op) => {
                if let (Value::Str(_), Value::Str(_)) = (&lhs, &rhs) {
                    self.charge(comparison_charge(&lhs, &rhs))?;
                    let result = match op {
                        Eq => value_eq(&lhs, &rhs),
                        Ne => !value_eq(&lhs, &rhs),
                        Lt => value_ord(&lhs, &rhs).is_lt(),
                        Le => value_ord(&lhs, &rhs).is_le(),
                        Gt => value_ord(&lhs, &rhs).is_gt(),
                        Ge => value_ord(&lhs, &rhs).is_ge(),
                        _ => unreachable!(),
                    };
                    Ok(Value::Bool(result))
                } else {
                    self.binary_values(op, lhs, rhs, span)
                }
            }
            BinaryDispatch::General(op) => self.binary_values(op, lhs, rhs, span),
        }
    }

    fn int_arithmetic(
        &mut self,
        op: BinaryKind,
        lhs: Value,
        rhs: Value,
        span: Span,
    ) -> EvalResult<Value> {
        match (lhs, rhs) {
            (Value::Int(a), Value::Int(b)) => {
                let result = match op {
                    BinaryKind::Add => a.checked_add(b),
                    BinaryKind::Sub => a.checked_sub(b),
                    BinaryKind::Mul => a.checked_mul(b),
                    BinaryKind::Div => {
                        if b == 0 {
                            return self.trap(self.panic("division by zero", span));
                        }
                        a.checked_div(b)
                    }
                    BinaryKind::Rem => {
                        if b == 0 {
                            return self.trap(self.panic("division by zero", span));
                        }
                        a.checked_rem(b)
                    }
                    _ => unreachable!(),
                };
                result
                    .map(Value::Int)
                    .ok_or_else(|| EvalError::from(self.panic("integer overflow", span)))
            }
            (lhs, rhs) => self.binary_values(op, lhs, rhs, span),
        }
    }

    fn float_arithmetic(
        &mut self,
        op: BinaryKind,
        lhs: Value,
        rhs: Value,
        span: Span,
    ) -> EvalResult<Value> {
        match (lhs, rhs) {
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(match op {
                BinaryKind::Add => a + b,
                BinaryKind::Sub => a - b,
                BinaryKind::Mul => a * b,
                BinaryKind::Div => a / b,
                BinaryKind::Rem => a % b,
                _ => unreachable!(),
            })),
            (lhs, rhs) => self.binary_values(op, lhs, rhs, span),
        }
    }

    fn int_compare(
        &mut self,
        op: BinaryKind,
        lhs: Value,
        rhs: Value,
        span: Span,
    ) -> EvalResult<Value> {
        match (lhs, rhs) {
            (Value::Int(a), Value::Int(b)) => {
                self.charge(0)?;
                Ok(Value::Bool(match op {
                    BinaryKind::Eq => a == b,
                    BinaryKind::Ne => a != b,
                    BinaryKind::Lt => a < b,
                    BinaryKind::Le => a <= b,
                    BinaryKind::Gt => a > b,
                    BinaryKind::Ge => a >= b,
                    _ => unreachable!(),
                }))
            }
            (lhs, rhs) => self.binary_values(op, lhs, rhs, span),
        }
    }

    fn float_compare(
        &mut self,
        op: BinaryKind,
        lhs: Value,
        rhs: Value,
        span: Span,
    ) -> EvalResult<Value> {
        match (lhs, rhs) {
            (Value::Float(a), Value::Float(b)) => {
                self.charge(0)?;
                // IEEE comparisons make every ordered comparison false for
                // NaN, matching the general `partial_cmp` fallback.
                Ok(Value::Bool(match op {
                    BinaryKind::Eq => a == b,
                    BinaryKind::Ne => a != b,
                    BinaryKind::Lt => a < b,
                    BinaryKind::Le => a <= b,
                    BinaryKind::Gt => a > b,
                    BinaryKind::Ge => a >= b,
                    _ => unreachable!(),
                }))
            }
            (lhs, rhs) => self.binary_values(op, lhs, rhs, span),
        }
    }

    fn concat_strings(&mut self, a: Rc<str>, b: Rc<str>) -> EvalResult<Value> {
        let mut text = String::with_capacity(a.len() + b.len());
        text.push_str(&a);
        text.push_str(&b);
        self.charge(fuel::string_cost(text.len() as u64))?;
        self.note_alloc(16 + text.len() as u64);
        Ok(Value::str(text))
    }

    #[inline]
    fn binary_values(
        &mut self,
        op: BinaryKind,
        lhs: Value,
        rhs: Value,
        span: Span,
    ) -> EvalResult<Value> {
        use BinaryKind::*;
        match op {
            Eq | Ne | Lt | Le | Gt | Ge => {
                self.charge(comparison_charge(&lhs, &rhs))?;
                match op {
                    Eq => Ok(Value::Bool(value_eq(&lhs, &rhs))),
                    Ne => Ok(Value::Bool(!value_eq(&lhs, &rhs))),
                    Lt | Le | Gt | Ge => {
                        let ord = match (&lhs, &rhs) {
                            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
                            _ => Some(value_ord(&lhs, &rhs)),
                        };
                        let Some(ord) = ord else {
                            return Ok(Value::Bool(false));
                        };
                        Ok(Value::Bool(match op {
                            Lt => ord.is_lt(),
                            Le => ord.is_le(),
                            Gt => ord.is_gt(),
                            Ge => ord.is_ge(),
                            _ => false,
                        }))
                    }
                    _ => unreachable!(),
                }
            }
            Add | Sub | Mul | Div | Rem => match (&lhs, &rhs) {
                (Value::Int(a), Value::Int(b)) => {
                    let result = match op {
                        Add => a.checked_add(*b),
                        Sub => a.checked_sub(*b),
                        Mul => a.checked_mul(*b),
                        Div => {
                            if *b == 0 {
                                return self.trap(self.panic("division by zero", span));
                            }
                            a.checked_div(*b)
                        }
                        Rem => {
                            if *b == 0 {
                                return self.trap(self.panic("division by zero", span));
                            }
                            a.checked_rem(*b)
                        }
                        _ => None,
                    };
                    result
                        .map(Value::Int)
                        .ok_or_else(|| EvalError::from(self.panic("integer overflow", span)))
                }
                (Value::Float(a), Value::Float(b)) => Ok(Value::Float(match op {
                    Add => a + b,
                    Sub => a - b,
                    Mul => a * b,
                    Div => a / b,
                    Rem => a % b,
                    _ => unreachable!(),
                })),
                (Value::Str(a), Value::Str(b)) if op == Add => {
                    let mut text = String::with_capacity(a.len() + b.len());
                    text.push_str(a);
                    text.push_str(b);
                    self.charge(fuel::string_cost(text.len() as u64))?;
                    self.note_alloc(16 + text.len() as u64);
                    Ok(Value::str(text))
                }
                _ => self.trap(self.internal_panic("invalid arithmetic operands")),
            },
        }
    }

    fn eval_range(&mut self, start: IrId, end: IrId, inclusive: bool) -> EvalResult<Value> {
        let start = self.eval(start)?;
        let end = self.eval(end)?;
        match (start, end) {
            (Value::Int(start), Value::Int(end)) => Ok(Value::Range {
                start,
                end,
                inclusive,
            }),
            _ => self.trap(self.internal_panic("range bounds must be int")),
        }
    }

    fn eval_sequence(
        &mut self,
        range: ChildRange,
        tuple: bool,
        span: Span,
        post: u32,
    ) -> EvalResult<Value> {
        let epoch = self.scan_epoch;
        let function = self.current_function();
        let mut values = Vec::with_capacity(range.len as usize);
        for offset in 0..range.len {
            let Some(child) = function
                .children
                .get(range.start as usize + offset as usize)
                .copied()
            else {
                return self.trap(self.internal_panic("aggregate child is out of range"));
            };
            values.push(self.eval(child)?);
        }
        self.charge(u64::from(post))?;
        self.note_alloc(16 + 8 * values.len() as u64);
        let value = if tuple {
            Value::tuple(values)
        } else {
            Value::list(values)
        };
        self.check_value_depth(&value, span)?;
        self.finish_aggregate(&value, epoch)?;
        Ok(value)
    }

    fn eval_map(&mut self, range: ChildRange, span: Span) -> EvalResult<Value> {
        let epoch = self.scan_epoch;
        let function = self.current_function();
        let mut map = OrderedMap::new();
        for offset in (0..range.len).step_by(2) {
            let Some(key_id) = function
                .children
                .get(range.start as usize + offset as usize)
                .copied()
            else {
                return self.trap(self.internal_panic("map key child is out of range"));
            };
            let Some(value_id) = function
                .children
                .get(range.start as usize + offset as usize + 1)
                .copied()
            else {
                return self.trap(self.internal_panic("map value child is out of range"));
            };
            let key = self.eval(key_id)?;
            let value = self.eval(value_id)?;
            let key_span = function.span(key_id).unwrap_or(span);
            self.charge(map_key_cost(&key))?;
            let key = key_from_value(&key)
                .map_err(|message| EvalError::from(self.panic(message, key_span)))?;
            map.insert(key, value);
        }
        self.charge(fuel::map_cost(map.len() as u64))?;
        let key_bytes: u64 = map.iter().map(|(key, _)| key_value_size(key)).sum();
        self.note_alloc(16 + 16 * map.len() as u64 + key_bytes);
        let value = Value::map(map);
        self.check_value_depth(&value, span)?;
        self.finish_aggregate(&value, epoch)?;
        Ok(value)
    }

    fn eval_struct(
        &mut self,
        type_id: TypeId,
        layout_id: StructLayoutId,
        update: bool,
        span: Span,
        post: u32,
    ) -> EvalResult<Value> {
        let epoch = self.scan_epoch;
        let program = self.program;
        let Some(def) = program.types.get(type_id) else {
            return self.trap(self.internal_panic("unknown struct type"));
        };
        let TypeDefKind::Struct {
            fields: declaration,
        } = &def.kind
        else {
            return self.trap(self.internal_panic("struct literal has a non-struct type"));
        };
        let declaration_len = declaration.len();
        let function = self.current_function();
        let Some(layout) = function.struct_layout(layout_id) else {
            return self.trap(self.internal_panic("struct literal has no layout"));
        };

        let mut fields = vec![Value::Unit; declaration_len];
        let value_start = if update {
            let Some(base_id) = function.children.get(layout.first_child as usize).copied() else {
                return self.trap(self.internal_panic("struct update has no base"));
            };
            let base = self.eval(base_id)?;
            if let Value::Struct(base) = base
                && base.type_id == type_id
            {
                for (slot, value) in fields.iter_mut().zip(base.fields.iter()) {
                    *slot = value.clone();
                }
            }
            layout.first_child + 1
        } else {
            layout.first_child
        };

        for (index, declaration_slot) in layout.fields.iter().enumerate() {
            let Some(child) = function.children.get(value_start as usize + index).copied() else {
                return self.trap(self.internal_panic("struct field child is out of range"));
            };
            let value = self.eval(child)?;
            if let Some(slot) = fields.get_mut(*declaration_slot as usize) {
                *slot = value;
            }
        }

        self.charge(u64::from(post))?;
        self.note_alloc(16 + 8 * declaration_len as u64);
        let value = Value::Struct(Rc::new(StructValue::new(type_id, fields)));
        self.check_value_depth(&value, span)?;
        self.finish_aggregate(&value, epoch)?;
        Ok(value)
    }

    fn eval_enum(
        &mut self,
        variant: u32,
        first_payload: u32,
        ty: martite_ir::TyId,
        post: u32,
        span: Span,
    ) -> EvalResult<Value> {
        let epoch = self.scan_epoch;
        let Some(Ty::Named(type_id, _)) = self.program.types.get_ty(ty) else {
            return self.trap(self.internal_panic("enum variant has no named type"));
        };
        let type_id = *type_id;
        let payload_count = self
            .program
            .types
            .variant(type_id, variant as usize)
            .map(|variant| variant.payload.len())
            .unwrap_or(0);
        let function = self.current_function();
        let mut payload = Vec::with_capacity(payload_count);
        for offset in 0..payload_count {
            let Some(child) = function
                .children
                .get(first_payload as usize + offset)
                .copied()
            else {
                return self.trap(self.internal_panic("enum payload child is out of range"));
            };
            payload.push(self.eval(child)?);
        }
        self.charge(u64::from(post))?;
        self.note_alloc(16 + 8 * payload_count as u64);
        let value = Value::Enum(Rc::new(EnumValue::new(type_id, variant as usize, payload)));
        self.check_value_depth(&value, span)?;
        self.finish_aggregate(&value, epoch)?;
        Ok(value)
    }

    /// Zero-payload variant used as a value. The interpreter allocates it
    /// without taking a charge, unlike the constructor form.
    fn eval_enum_value(
        &mut self,
        variant: u32,
        ty: martite_ir::TyId,
        span: Span,
    ) -> EvalResult<Value> {
        let Some(Ty::Named(type_id, _)) = self.program.types.get_ty(ty) else {
            return self.trap(self.internal_panic("enum value has no named type"));
        };
        let type_id = *type_id;
        self.note_alloc(16);
        let value = Value::Enum(Rc::new(EnumValue::new(
            type_id,
            variant as usize,
            Vec::new(),
        )));
        self.check_value_depth(&value, span)?;
        Ok(value)
    }

    fn eval_prelude(
        &mut self,
        variant: PreludeVariant,
        first_payload: u32,
        span: Span,
    ) -> EvalResult<Value> {
        let payload = if matches!(variant, PreludeVariant::None) {
            None
        } else {
            let function = self.current_function();
            let Some(child) = function.children.get(first_payload as usize).copied() else {
                return self.trap(self.internal_panic("prelude payload child is out of range"));
            };
            Some(self.eval(child)?)
        };
        let value = match variant {
            PreludeVariant::None => Value::Option(None),
            PreludeVariant::Some => {
                self.note_alloc(16);
                Value::Option(Some(Box::new(payload.unwrap_or(Value::Unit))))
            }
            PreludeVariant::Ok => {
                self.note_alloc(16);
                Value::Result(Ok(Box::new(payload.unwrap_or(Value::Unit))))
            }
            PreludeVariant::Err => {
                self.note_alloc(16);
                Value::Result(Err(Box::new(payload.unwrap_or(Value::Unit))))
            }
        };
        self.check_value_depth(&value, span)?;
        Ok(value)
    }

    /// Evaluate a function or branch body. A single-statement block and its
    /// trailing `ExprStmt` are transparent: neither charges nor safepoints, so
    /// callers can evaluate the inner value directly.
    #[inline]
    fn eval_body(&mut self, body: IrId) -> EvalResult<Value> {
        let function = self.current_function();
        if let NodeKind::Block(range) = function.nodes[body.index()].kind {
            if range.len == 0 {
                return Ok(Value::Unit);
            }
            if range.len == 1 {
                let child = function.children[range.start as usize];
                if let NodeKind::ExprStmt(inner) = function.nodes[child.index()].kind {
                    return self.eval(inner);
                }
                return self.eval(child);
            }
        }
        self.eval(body)
    }

    fn eval_block(&mut self, range: ChildRange) -> EvalResult<Value> {
        let function = self.current_function();
        let mut value = Value::Unit;
        for offset in 0..range.len {
            let child = function.children[range.start as usize + offset as usize];
            // A block's trailing expression statement has no charge and is not
            // a safepoint, so evaluating its inner node directly avoids one
            // dispatch without changing observable behavior.
            let is_tail = offset + 1 == range.len;
            if is_tail
                && let Node {
                    kind: NodeKind::ExprStmt(inner),
                    ..
                } = function.nodes[child.index()]
            {
                value = self.eval(inner)?;
                continue;
            }
            value = self.eval(child)?;
        }
        Ok(value)
    }

    fn eval_if(&mut self, range: ChildRange) -> EvalResult<Value> {
        let function = self.current_function();
        let cond = function.children[range.start as usize];
        let then_branch = function.children[range.start as usize + 1];
        let else_branch = (range.len >= 3).then(|| function.children[range.start as usize + 2]);
        let cond = self.eval(cond)?;
        if matches!(cond, Value::Bool(true)) {
            self.eval_body(then_branch)
        } else if let Some(else_branch) = else_branch {
            self.eval_body(else_branch)
        } else {
            Ok(Value::Unit)
        }
    }

    fn eval_while(&mut self, cond: IrId, body: IrId, per_iter: u64) -> EvalResult<Value> {
        loop {
            self.charge(per_iter)?;
            let condition = self.eval(cond)?;
            if !matches!(condition, Value::Bool(true)) {
                break;
            }
            match self.eval(body) {
                Ok(_) => {}
                Err(EvalError::Break) => break,
                Err(EvalError::Continue) => continue,
                Err(other) => return Err(other),
            }
        }
        Ok(Value::Unit)
    }

    fn eval_for(
        &mut self,
        pattern: martite_ir::PatternId,
        first_child: u32,
        per_iter: u64,
    ) -> EvalResult<Value> {
        let function = self.current_function();
        let Some(pattern) = function.pattern(pattern) else {
            return self.trap(self.internal_panic("for loop has no pattern"));
        };
        let Some(iterable) = function.children.get(first_child as usize).copied() else {
            return self.trap(self.internal_panic("for loop has no iterable"));
        };
        let Some(body) = function.children.get(first_child as usize + 1).copied() else {
            return self.trap(self.internal_panic("for loop has no body"));
        };
        let iterable = self.eval(iterable)?;
        match iterable {
            Value::List(items) => {
                for item in items.iter() {
                    self.charge(per_iter)?;
                    self.bind_pattern(pattern, item)?;
                    match self.eval(body) {
                        Ok(_) | Err(EvalError::Continue) => {}
                        Err(EvalError::Break) => break,
                        Err(other) => return Err(other),
                    }
                }
            }
            Value::Map(map) => {
                // Destructuring a pair from a map is the common case. Bind the
                // borrowed entry directly instead of materialising a temporary
                // `(key, value)` tuple for every iteration.
                let pair = match pattern {
                    Pattern::Tuple(items) if items.len() == 2 => Some((&items[0], &items[1])),
                    _ => None,
                };
                for (key, value) in map.iter() {
                    self.charge(per_iter)?;
                    match pair {
                        Some((key_pattern, value_pattern)) => match key_pattern {
                            Pattern::Wild => {
                                // The entry tuple is no longer materialised, but
                                // keep its nominal accounting so tight memory
                                // limits behave exactly as before.
                                self.note_alloc(16 + 8 * 2);
                                self.bind_pattern(value_pattern, value)?;
                            }
                            Pattern::Bind { .. } => {
                                let key = crate::value::key_to_value(key);
                                self.note_alloc(16 + 8 * 2);
                                self.bind_pattern(key_pattern, &key)?;
                                self.bind_pattern(value_pattern, value)?;
                            }
                            _ => {
                                let pair = Value::tuple(vec![
                                    crate::value::key_to_value(key),
                                    value.clone(),
                                ]);
                                self.note_alloc(16 + 8 * 2);
                                self.bind_pattern(pattern, &pair)?;
                            }
                        },
                        None => {
                            let pair =
                                Value::tuple(vec![crate::value::key_to_value(key), value.clone()]);
                            self.note_alloc(16 + 8 * 2);
                            self.bind_pattern(pattern, &pair)?;
                        }
                    }
                    match self.eval(body) {
                        Ok(_) | Err(EvalError::Continue) => {}
                        Err(EvalError::Break) => break,
                        Err(other) => return Err(other),
                    }
                }
            }
            Value::Str(text) => {
                for ch in text.chars() {
                    self.charge(per_iter)?;
                    self.note_alloc(16 + ch.len_utf8() as u64);
                    let value = Value::Str(Rc::from(ch.to_string()));
                    self.bind_pattern(pattern, &value)?;
                    match self.eval(body) {
                        Ok(_) | Err(EvalError::Continue) => {}
                        Err(EvalError::Break) => break,
                        Err(other) => return Err(other),
                    }
                }
            }
            Value::Range {
                start,
                end,
                inclusive,
            } => {
                let last = if inclusive {
                    Some(end)
                } else {
                    end.checked_sub(1)
                };
                if let Some(last) = last {
                    let mut value = start;
                    while value <= last {
                        self.charge(per_iter)?;
                        self.bind_pattern(pattern, &Value::Int(value))?;
                        match self.eval(body) {
                            Ok(_) | Err(EvalError::Continue) => {}
                            Err(EvalError::Break) => return Ok(Value::Unit),
                            Err(other) => return Err(other),
                        }
                        if value == i64::MAX {
                            break;
                        }
                        value += 1;
                    }
                }
            }
            _ => return self.trap(self.internal_panic("value is not iterable")),
        }
        Ok(Value::Unit)
    }

    #[inline]
    fn eval_make_closure(&mut self, target: FnId) -> EvalResult<Value> {
        let program = self.program;
        let Some(function) = program.function(target) else {
            return self.trap(self.internal_panic("closure target is out of range"));
        };
        if !matches!(function.kind, FnKind::Lambda) {
            return self.trap(self.internal_panic("closure target is not a lambda"));
        }
        let mut captured = Vec::with_capacity(function.captures.len());
        for capture in &function.captures {
            let value = match capture.source {
                CaptureSource::Slot(slot) => self.read_slot(slot, AccessMode::Share)?,
                CaptureSource::Capture(index) => self.read_capture(index)?,
            };
            captured.push(value);
        }
        self.note_alloc(16);
        let captures = if captured.is_empty() {
            None
        } else {
            Some(Rc::<[Value]>::from(captured))
        };
        Ok(Value::Fn(FnValue::Closure(Rc::new(Closure::new(
            target, captures,
        )))))
    }

    #[inline]
    fn read_capture(&self, index: CaptureIndex) -> EvalResult<Value> {
        let frame = self.frames.last().expect("evaluation frame");
        let captures = frame
            .captures
            .as_ref()
            .expect("verifier guarantees lambda captures are present");
        Ok(captures[index.index()].clone())
    }

    fn eval_match(&mut self, match_set: MatchSetId, scrutinee: IrId) -> EvalResult<Value> {
        let value = self.eval(scrutinee)?;
        let function = self.current_function();
        let Some(set) = function.match_set(match_set) else {
            return self.trap(self.internal_panic("match has no arm table"));
        };
        for arm in &set.arms {
            let Some(pattern) = function.pattern(arm.pattern) else {
                return self.trap(self.internal_panic("match arm pattern is out of range"));
            };
            if !self.test_pattern(pattern, &value)? {
                continue;
            }
            let mut undo = BindingUndo::new();
            self.bind_match_pattern(pattern, &value, &mut undo)?;
            let passes = match arm.guard {
                Some(guard) => matches!(self.eval(guard)?, Value::Bool(true)),
                None => true,
            };
            if !passes {
                self.rollback_bindings(&mut undo)?;
                continue;
            }
            // Drop the pre-arm values before the body, as the snapshots pin;
            // retaining them would add observable references.
            drop(undo);
            return self.eval(arm.body);
        }
        self.trap(self.internal_panic("match fell through"))
    }

    /// Pure pattern test: no writes, no charges, no allocation notes.
    fn test_pattern(&self, pattern: &Pattern, value: &Value) -> EvalResult<bool> {
        match pattern {
            Pattern::Wild | Pattern::Bind { .. } => Ok(true),
            Pattern::Int(expected) => Ok(matches!(value, Value::Int(actual) if actual == expected)),
            Pattern::Bool(expected) => {
                Ok(matches!(value, Value::Bool(actual) if actual == expected))
            }
            Pattern::Str(expected) => Ok(match value {
                Value::Str(actual) => self
                    .program
                    .string(*expected)
                    .is_some_and(|text| actual.as_ref() == text),
                _ => false,
            }),
            Pattern::Tuple(items) => {
                let Value::Tuple(values) = value else {
                    return Ok(false);
                };
                if items.len() != values.len() {
                    return Ok(false);
                }
                for (pattern, value) in items.iter().zip(values.iter()) {
                    if !self.test_pattern(pattern, value)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Pattern::Variant {
                type_id,
                variant,
                args,
            } => {
                let Value::Enum(enum_value) = value else {
                    return Ok(false);
                };
                if enum_value.type_id != *type_id || enum_value.variant != *variant as usize {
                    return Ok(false);
                }
                for (pattern, value) in args.iter().zip(enum_value.payload.iter()) {
                    if !self.test_pattern(pattern, value)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Pattern::Prelude { variant, args } => {
                let Some(payload) = prelude_payload(value, *variant) else {
                    return Ok(false);
                };
                for (pattern, value) in args.iter().zip(payload.iter()) {
                    if !self.test_pattern(pattern, value)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Pattern::Struct { fields, .. } => {
                let Value::Struct(struct_value) = value else {
                    return Ok(false);
                };
                for (declaration, pattern) in fields {
                    let Some(field) = struct_value.fields.get(*declaration as usize) else {
                        return Ok(false);
                    };
                    if !self.test_pattern(pattern, field)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            Pattern::Or(alternatives) => {
                for alternative in alternatives {
                    if self.test_pattern(alternative, value)? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }

    fn bind_match_pattern(
        &mut self,
        pattern: &Pattern,
        value: &Value,
        undo: &mut BindingUndo,
    ) -> EvalResult<()> {
        match pattern {
            Pattern::Wild | Pattern::Int(_) | Pattern::Bool(_) | Pattern::Str(_) => {}
            Pattern::Bind { slot, .. } => {
                self.bind_match_slot(*slot, value.clone(), undo)?;
            }
            Pattern::Tuple(items) => {
                if let Value::Tuple(values) = value {
                    for (pattern, value) in items.iter().zip(values.iter()) {
                        self.bind_match_pattern(pattern, value, undo)?;
                    }
                }
            }
            Pattern::Variant { args, .. } => {
                if let Value::Enum(enum_value) = value {
                    for (pattern, value) in args.iter().zip(enum_value.payload.iter()) {
                        self.bind_match_pattern(pattern, value, undo)?;
                    }
                }
            }
            Pattern::Prelude { variant, args } => {
                if let Some(payload) = prelude_payload(value, *variant) {
                    for (pattern, value) in args.iter().zip(payload.iter()) {
                        self.bind_match_pattern(pattern, value, undo)?;
                    }
                }
            }
            Pattern::Struct { fields, .. } => {
                if let Value::Struct(struct_value) = value {
                    for (declaration, pattern) in fields {
                        if let Some(field) = struct_value.fields.get(*declaration as usize) {
                            self.bind_match_pattern(pattern, field, undo)?;
                        }
                    }
                }
            }
            Pattern::Or(alternatives) => {
                for alternative in alternatives {
                    if self.test_pattern(alternative, value)? {
                        return self.bind_match_pattern(alternative, value, undo);
                    }
                }
                return self.trap(self.internal_panic("or-pattern alternative did not match"));
            }
        }
        Ok(())
    }

    fn bind_match_slot(
        &mut self,
        slot: Slot,
        value: Value,
        undo: &mut BindingUndo,
    ) -> EvalResult<()> {
        let index = slot.index();
        if !self
            .frames
            .last()
            .is_some_and(|frame| index < frame.slots.len())
        {
            return self.trap(self.internal_panic("match binding slot is out of range"));
        }
        let frame = self.frames.last_mut().expect("evaluation frame");
        undo.push((slot, frame.slots[index].clone()));
        frame.slots[index] = Some(value);
        Ok(())
    }

    /// Restores the bindings of a failed guard to their exact pre-arm state.
    /// Slots are never otherwise cleared before frame exit, so a later
    /// iteration can observe the restored value.
    fn rollback_bindings(&mut self, undo: &mut BindingUndo) -> EvalResult<()> {
        while let Some((slot, previous)) = undo.pop() {
            let index = slot.index();
            if !self
                .frames
                .last()
                .is_some_and(|frame| index < frame.slots.len())
            {
                return self.trap(self.internal_panic("match binding slot is out of range"));
            }
            let frame = self.frames.last_mut().expect("evaluation frame");
            frame.slots[index] = previous;
        }
        Ok(())
    }

    fn eval_try(&mut self, inner: IrId) -> EvalResult<Value> {
        let value = self.eval(inner)?;
        match value {
            Value::Option(Some(value)) => Ok(*value),
            Value::Option(None) => Err(EvalError::Return(Value::Option(None))),
            Value::Result(Ok(value)) => Ok(*value),
            Value::Result(Err(value)) => {
                self.note_alloc(16);
                Err(EvalError::Return(Value::Result(Err(value))))
            }
            other => self.trap(self.internal_panic(format!(
                "`?` applied to non-option value {}",
                show(&other, &self.program.types, true)
            ))),
        }
    }

    fn eval_convert_int(&mut self, inner: IrId, span: Span) -> EvalResult<Value> {
        let value = self.eval(inner)?;
        match value {
            Value::Float(value) => {
                if !value.is_finite()
                    || !(-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&value)
                {
                    self.trap(self.panic("float is out of int range", span))
                } else {
                    Ok(Value::Int(value.trunc() as i64))
                }
            }
            _ => self.trap(self.internal_panic("invalid conversion")),
        }
    }

    fn render_show(&mut self, value: &Value, top: bool) -> EvalResult<String> {
        // Leave room for the string's own `Rc<str>` header; the text itself is
        // the only allocation this builder may grow into.
        let limit = self.memory_headroom(Some(value))?.saturating_sub(16);
        let limit = limit.min(usize::MAX as u64) as usize;
        let mut out = ShowOutput::new(limit);
        write_show(&mut out, value, &self.program.types, top);
        if out.is_overflowed() {
            return self.trap(Trap::OutOfMemory);
        }
        Ok(out.into_string())
    }

    fn eval_string(&mut self, range: ChildRange) -> EvalResult<Value> {
        let function = self.current_function();
        let parts = function.str_part_slice(range);
        let limit = self.memory_headroom(None)?.saturating_sub(16);
        let limit = limit.min(usize::MAX as u64) as usize;
        let mut out = ShowOutput::new(limit);
        for part in parts {
            match part {
                martite_ir::StrPart::Text(id) => {
                    if let Some(text) = self.program.string(*id) {
                        out.push_str(text);
                    }
                }
                martite_ir::StrPart::Expr(expr) => {
                    let epoch = self.scan_epoch;
                    let value = self.eval(*expr)?;
                    // Charge the show cost before the expansion allocates. A
                    // shared DAG can expand exponentially, so charging after
                    // the write let it allocate far past the fuel budget.
                    self.charge(fuel::show_cost(value.meta().slots))?;
                    self.pin_value(&value, epoch)?;
                    // Evaluating the expression may itself have grown the live
                    // heap; tighten the output cap before writing.
                    let limit = self.memory_headroom(Some(&value))?.saturating_sub(16);
                    out.set_limit(limit.min(usize::MAX as u64) as usize);
                    write_show(&mut out, &value, &self.program.types, true);
                }
            }
            if out.is_overflowed() {
                return self.trap(Trap::OutOfMemory);
            }
        }
        if out.is_overflowed() {
            return self.trap(Trap::OutOfMemory);
        }
        let out = out.into_string();
        self.charge(fuel::string_cost(out.len() as u64))?;
        self.note_alloc(16 + out.len() as u64);
        Ok(Value::str(out))
    }

    fn eval_assert(&mut self, range: ChildRange, span: Span) -> EvalResult<Value> {
        let function = self.current_function();
        let Some(condition) = function.children.get(range.start as usize).copied() else {
            return self.trap(self.internal_panic("assert has no condition"));
        };
        let Some(message) = function.children.get(range.start as usize + 1).copied() else {
            return self.trap(self.internal_panic("assert has no message"));
        };
        let condition = self.eval(condition)?;
        let message = self.eval(message)?;
        if matches!(condition, Value::Bool(true)) {
            Ok(Value::Unit)
        } else {
            let message = message
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| "assertion failed".into());
            self.trap(self.panic(message, span))
        }
    }

    fn eval_panic(&mut self, range: ChildRange, span: Span) -> EvalResult<Value> {
        let function = self.current_function();
        let Some(message) = function.children.get(range.start as usize).copied() else {
            return self.trap(self.internal_panic("panic has no message"));
        };
        let message = self.eval(message)?;
        let message = message
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| "panic".into());
        self.trap(self.panic(message, span))
    }

    fn check_value_depth(&self, value: &Value, _span: Span) -> EvalResult<()> {
        if value_depth_exceeds(value, MAX_VALUE_DEPTH) {
            Err(EvalError::from(Trap::DepthExceeded))
        } else {
            Ok(())
        }
    }

    // ------------------------------------------------------------------
    // Calls
    // ------------------------------------------------------------------

    fn eval_call(&mut self, target: FnId, first_child: u32) -> EvalResult<Value> {
        let program = self.program;
        let Some(function_info) = program.function(target) else {
            return self.trap(self.internal_panic("call target is out of range"));
        };
        let params = &function_info.params;
        let function = self.current_function();
        let mut values = CallArgs::with_capacity(params.len());
        let mut writebacks: SmallVec<[(Place, usize); 2]> = SmallVec::new();
        for (index, param) in params.iter().enumerate() {
            let child = function.children[first_child as usize + index];
            if param.mutable {
                let node = function.nodes[child.index()];
                let NodeKind::MutArg(place) = node.kind else {
                    return self
                        .trap(self.internal_panic("mutable parameter needs a place argument"));
                };
                let place = self.eval_place(place)?;
                let value = self.read_place(&place)?;
                values.push(value);
                writebacks.push((place, index));
            } else {
                values.push(self.eval(child)?);
            }
        }
        let (value, mut updated) = self
            .call_resolved(function_info, values, None)
            .map_err(EvalError::from)?;
        for (place, index) in writebacks {
            if let Some(value) = take_writeback(&mut updated, index) {
                self.write_place(place, value)?;
            }
        }
        Ok(value)
    }

    fn eval_call_value(&mut self, first_child: u32, argc: u32, span: Span) -> EvalResult<Value> {
        let function = self.current_function();
        let Some(callee_id) = function.children.get(first_child as usize).copied() else {
            return self.trap(self.internal_panic("value call has no callee"));
        };
        let callee = self.eval(callee_id)?;
        let mut values = CallArgs::with_capacity(argc as usize);
        for index in 0..argc as usize {
            let Some(child) = function
                .children
                .get(first_child as usize + 1 + index)
                .copied()
            else {
                return self.trap(self.internal_panic("value call argument is out of range"));
            };
            let Some(node) = function.node(child).copied() else {
                return self.trap(self.internal_panic("value call argument is out of range"));
            };
            if matches!(node.kind, NodeKind::MutArg(_)) {
                return self
                    .trap(self.internal_panic("function values have no mutable parameters"));
            }
            values.push(self.eval(child)?);
        }
        match callee {
            Value::Fn(function) => self
                .call_closure_value(&function, values)
                .map_err(EvalError::from),
            Value::Builtin(function) => self.call_builtin_fn_values(function, &values, span),
            Value::BuiltinModuleFn(item) => self.call_builtin_item_values(item, &values, span),
            _ => self.trap(self.internal_panic("callee is not callable")),
        }
    }

    fn eval_plain_children(&mut self, first: u32, count: u32) -> EvalResult<CallArgs> {
        let function = self.current_function();
        let mut values = CallArgs::with_capacity(count as usize);
        for index in 0..count as usize {
            let Some(child) = function.children.get(first as usize + index).copied() else {
                return self.trap(self.internal_panic("call argument is out of range"));
            };
            let Some(node) = function.node(child).copied() else {
                return self.trap(self.internal_panic("call argument is out of range"));
            };
            if matches!(node.kind, NodeKind::MutArg(_)) {
                return self.trap(self.internal_panic("mutable argument on a value-only call"));
            }
            values.push(self.eval(child)?);
        }
        Ok(values)
    }

    fn eval_builtin_fn_call(
        &mut self,
        builtin: BuiltinFnId,
        first_arg: u32,
        span: Span,
    ) -> EvalResult<Value> {
        let Some(arity) = builtin.arity() else {
            return self.trap(self.internal_panic("unknown builtin function"));
        };
        let values = self.eval_plain_children(first_arg, arity)?;
        self.call_builtin_fn_values(builtin, &values, span)
    }

    fn eval_builtin_module_call(
        &mut self,
        item: BuiltinItemId,
        first_arg: u32,
        span: Span,
    ) -> EvalResult<Value> {
        let Some(arity) = item.arity() else {
            return self.trap(self.internal_panic("unknown builtin module item"));
        };
        let values = self.eval_plain_children(first_arg, arity)?;
        self.call_builtin_item_values(item, &values, span)
    }

    fn eval_host_call(&mut self, host: HostId, first_arg: u32) -> EvalResult<Value> {
        let program = self.program;
        let Some(info) = program.host(host) else {
            return self.trap(self.internal_panic("host function is out of range"));
        };
        let count = u32::try_from(info.params.len()).map_err(|_| {
            EvalError::from(self.internal_panic("host parameter count exceeds u32"))
        })?;
        let values = self.eval_plain_children(first_arg, count)?;
        if self.frames.len() >= self.limits.depth as usize {
            return self.trap(Trap::DepthExceeded);
        }
        self.call_host(host, &values).map_err(EvalError::from)
    }

    // ------------------------------------------------------------------
    // Places
    // ------------------------------------------------------------------

    /// Evaluate a value read of a stable access chain without cloning the
    /// intermediate containers.
    ///
    /// A chain is stable when it is rooted at a shared slot or capture read
    /// and consists only of struct/tuple fields and list indices. Index
    /// expressions must be free of calls, host calls and writes: those could
    /// observe or mutate the root while the old evaluator held its clone.
    ///
    /// Returns `None` before charging anything when the chain is not stable,
    /// so the general evaluator takes over with unchanged fuel and traps.
    fn eval_place_read(&mut self, id: IrId) -> EvalResult<Option<Value>> {
        let Some((root, chain)) = self.stable_place_chain(id) else {
            return Ok(None);
        };
        // The general path charges each chain node on entry, outermost first.
        // Share that order so fuel, ticks and deadline checks stay identical.
        // The outermost node was already charged by `eval` before this call.
        let function = self.current_function();
        for node_id in chain.iter().skip(1) {
            let charge = function.nodes[node_id.index()].charge.pre;
            if charge != 0 {
                self.charge(u64::from(charge))?;
            }
        }
        // Resolve accessors from the innermost one out. After each step, walk
        // the resolved prefix so a list bound trap happens before any later
        // index expression runs, exactly as the recursive evaluator did.
        let mut accessors: SmallVec<[Accessor; 8]> = SmallVec::new();
        for position in (0..chain.len().saturating_sub(1)).rev() {
            let node_id = chain[position];
            let node = self.current_function().nodes[node_id.index()];
            let accessor = match node.kind {
                NodeKind::Field { index, .. } => Accessor::Field(index as usize),
                NodeKind::TupleField { index, .. } => Accessor::Tuple(index),
                NodeKind::Index { index, .. } => Accessor::Index(self.eval(index)?),
                _ => unreachable!("stable place chains contain only accessors"),
            };
            accessors.push(accessor);
            let span = self.current_function().spans[node_id.index()];
            let value = self.place_leaf(root, &accessors, span)?;
            if position == 0 {
                return Ok(Some(value.clone()));
            }
        }
        Ok(None)
    }

    /// Collect a stable access chain and validate its root before any charge.
    ///
    /// `chain` is ordered outermost to root: the order in which the general
    /// evaluator charges the expression nodes.
    fn stable_place_chain(&self, id: IrId) -> Option<(PlaceRoot, SmallVec<[IrId; 8]>)> {
        let function = self.current_function();
        let frame = self.frames.last()?;
        let mut chain = SmallVec::new();
        let mut current = id;
        loop {
            let node = function.node(current)?;
            match node.kind {
                NodeKind::Field { base, .. } | NodeKind::TupleField { base, .. } => {
                    chain.push(current);
                    current = base;
                }
                NodeKind::Index { base, index } => {
                    if self.index_kind(current) != IndexKind::ListIndex {
                        return None;
                    }
                    let index = function.node(index)?;
                    if index.effects.intersects(
                        Effects::CALLS
                            | Effects::HOST
                            | Effects::WRITES_LOCAL
                            | Effects::WRITES_HEAP,
                    ) {
                        return None;
                    }
                    chain.push(current);
                    current = base;
                }
                NodeKind::SlotRead {
                    slot,
                    mode: AccessMode::Share,
                } => {
                    frame.slots.get(slot.index())?.as_ref()?;
                    chain.push(current);
                    return Some((PlaceRoot::Slot(slot), chain));
                }
                NodeKind::CaptureRead(index) => {
                    frame.captures.as_ref()?.get(index.index())?;
                    chain.push(current);
                    return Some((PlaceRoot::Capture(index), chain));
                }
                _ => return None,
            }
        }
    }

    /// Borrow the value at `accessors` below `root`.
    ///
    /// The caller only adds one accessor at a time, so an already-traversed
    /// prefix cannot fail: the only trap left is the newest accessor's list
    /// bound or an invalid shape.
    fn place_leaf<'a>(
        &'a self,
        root: PlaceRoot,
        accessors: &[Accessor],
        span: Span,
    ) -> EvalResult<&'a Value> {
        let frame = self.frames.last().expect("evaluation frame");
        let mut value = match root {
            PlaceRoot::Slot(slot) => frame
                .slots
                .get(slot.index())
                .and_then(Option::as_ref)
                .ok_or_else(|| EvalError::from(self.internal_panic("unbound place root")))?,
            PlaceRoot::Capture(index) => frame
                .captures
                .as_ref()
                .and_then(|captures| captures.get(index.index()))
                .ok_or_else(|| EvalError::from(self.internal_panic("capture is out of range")))?,
        };
        for accessor in accessors {
            value = match (value, accessor) {
                (Value::Struct(struct_value), Accessor::Field(index)) => {
                    struct_value.fields.get(*index).ok_or_else(|| {
                        EvalError::from(self.internal_panic("struct field out of range"))
                    })?
                }
                (Value::Tuple(items), Accessor::Tuple(index)) => {
                    items.get(*index as usize).ok_or_else(|| {
                        EvalError::from(self.internal_panic("tuple field out of range"))
                    })?
                }
                (Value::List(items), Accessor::Index(Value::Int(index))) => {
                    if *index < 0 || *index as usize >= items.len() {
                        return self.trap(self.panic("index out of bounds", span));
                    }
                    &items[*index as usize]
                }
                _ => return self.trap(self.internal_panic("invalid place access")),
            };
        }
        Ok(value)
    }

    fn eval_place(&mut self, id: IrId) -> EvalResult<Place> {
        let function = self.current_function();
        let Some(node) = function.node(id).copied() else {
            return self.trap(self.internal_panic("place node is out of range"));
        };
        match node.kind {
            NodeKind::SlotPlace(slot) => Ok(Place {
                root: slot,
                accessors: SmallVec::new(),
            }),
            NodeKind::Field { base, index } => {
                let mut place = self.eval_place(base)?;
                place.accessors.push(Accessor::Field(index as usize));
                Ok(place)
            }
            NodeKind::TupleField { base, index } => {
                let mut place = self.eval_place(base)?;
                place.accessors.push(Accessor::Tuple(index));
                Ok(place)
            }
            NodeKind::Index { base, index } => {
                let mut place = self.eval_place(base)?;
                let index = self.eval(index)?;
                place.accessors.push(Accessor::Index(index));
                Ok(place)
            }
            _ => self.trap(self.internal_panic("expression is not a place")),
        }
    }

    fn read_place(&mut self, place: &Place) -> EvalResult<Value> {
        if let Some(value) = self.read_place_borrowed(place) {
            return Ok(value);
        }
        self.read_place_cloned(place)
    }

    /// Borrowed walk for struct/tuple fields and list indices.
    ///
    /// Returns `None` when the chain reaches a map or a shape the old path
    /// must diagnose; map keys still go through `map_key_ref` there.
    fn read_place_borrowed(&self, place: &Place) -> Option<Value> {
        let frame = self.frames.last()?;
        let mut value = frame.slots.get(place.root.index())?.as_ref()?;
        for accessor in &place.accessors {
            value = match (value, accessor) {
                (Value::Struct(struct_value), Accessor::Field(index)) => {
                    struct_value.fields.get(*index)?
                }
                (Value::Tuple(items), Accessor::Tuple(index)) => items.get(*index as usize)?,
                (Value::List(items), Accessor::Index(Value::Int(index))) => {
                    if *index < 0 {
                        return None;
                    }
                    items.get(*index as usize)?
                }
                _ => return None,
            };
        }
        Some(value.clone())
    }

    fn read_place_cloned(&mut self, place: &Place) -> EvalResult<Value> {
        let mut value = self
            .frames
            .last()
            .and_then(|frame| frame.slots.get(place.root.index()))
            .and_then(Option::as_ref)
            .cloned()
            .ok_or_else(|| EvalError::from(self.internal_panic("unbound place root")))?;
        for accessor in &place.accessors {
            value = match (value, accessor) {
                (Value::Struct(value), Accessor::Field(index)) => {
                    value.fields.get(*index).cloned().ok_or_else(|| {
                        EvalError::from(self.internal_panic("struct field out of range"))
                    })?
                }
                (Value::Tuple(items), Accessor::Tuple(index)) => {
                    items.get(*index as usize).cloned().ok_or_else(|| {
                        EvalError::from(self.internal_panic("tuple field out of range"))
                    })?
                }
                (Value::List(items), Accessor::Index(Value::Int(index))) => {
                    if *index < 0 || *index as usize >= items.len() {
                        return self.trap(self.internal_panic("index out of bounds"));
                    }
                    items[*index as usize].clone()
                }
                (Value::Map(map), Accessor::Index(key)) => {
                    self.charge(map_key_cost(key))?;
                    let key = self.map_key_ref(key)?;
                    map.get(&key)
                        .cloned()
                        .ok_or_else(|| EvalError::from(self.internal_panic("missing map key")))?
                }
                _ => return self.trap(self.internal_panic("invalid place access")),
            };
        }
        Ok(value)
    }

    fn write_place(&mut self, place: Place, value: Value) -> EvalResult<()> {
        if place.accessors.is_empty() {
            let present = self
                .frames
                .last()
                .and_then(|frame| frame.slots.get(place.root.index()))
                .is_some_and(Option::is_some);
            if !present {
                return Err(EvalError::from(self.internal_panic("unbound place root")));
            }
            if value_depth_exceeds(&value, MAX_VALUE_DEPTH) {
                return Err(EvalError::from(Trap::DepthExceeded));
            }
            self.set_slot(place.root, value);
            return Ok(());
        }
        let mut root = self.take_place_root(&place)?;
        self.set_path(&mut root, &place.accessors, value, MAX_VALUE_DEPTH)?;
        self.set_slot(place.root, root);
        Ok(())
    }

    fn take_place_root(&mut self, place: &Place) -> EvalResult<Value> {
        let index = place.root.index();
        let present = self
            .frames
            .last()
            .and_then(|frame| frame.slots.get(index))
            .is_some_and(Option::is_some);
        if !present {
            return Err(EvalError::from(self.internal_panic("unbound place root")));
        }
        let frame = self.frames.last_mut().expect("evaluation frame");
        Ok(frame.slots[index]
            .take()
            .expect("place root was checked above"))
    }

    fn cow_vec_mut<'a>(
        &mut self,
        items: &'a mut Rc<SeqValue>,
        slots: usize,
    ) -> EvalResult<&'a mut SeqValue> {
        if Rc::strong_count(items) != 1 {
            self.charge(fuel::slot_cost(slots as u64))?;
            self.note_alloc(16 + 8 * slots as u64);
        }
        Ok(Rc::make_mut(items))
    }

    fn cow_map_mut<'a>(
        &mut self,
        map: &'a mut Rc<MapValue>,
        slots: usize,
    ) -> EvalResult<&'a mut MapValue> {
        if Rc::strong_count(map) != 1 {
            self.charge(fuel::slot_cost(slots as u64))?;
            self.note_alloc(16 + 8 * slots as u64);
        }
        Ok(Rc::make_mut(map))
    }

    fn cow_struct_mut<'a>(
        &mut self,
        value: &'a mut Rc<StructValue>,
        slots: usize,
    ) -> EvalResult<&'a mut StructValue> {
        if Rc::strong_count(value) != 1 {
            self.charge(fuel::slot_cost(slots as u64))?;
            self.note_alloc(16 + 8 * slots as u64);
        }
        Ok(Rc::make_mut(value))
    }

    fn set_path(
        &mut self,
        value: &mut Value,
        accessors: &[Accessor],
        new_value: Value,
        remaining: usize,
    ) -> EvalResult<()> {
        let Some((first, rest)) = accessors.split_first() else {
            if value_depth_exceeds(&new_value, remaining) {
                return Err(EvalError::from(Trap::DepthExceeded));
            }
            *value = new_value;
            return Ok(());
        };
        if remaining == 0 {
            return Err(EvalError::from(Trap::DepthExceeded));
        }
        let child_remaining = remaining - 1;
        match first {
            Accessor::Field(index) => {
                let Value::Struct(struct_value) = value else {
                    return self.trap(self.internal_panic("invalid place access"));
                };
                let slots = struct_value.fields.len();
                let struct_value = self.cow_struct_mut(struct_value, slots)?;
                let old = struct_value
                    .fields
                    .get(*index)
                    .ok_or_else(|| {
                        EvalError::from(self.internal_panic("struct field out of range"))
                    })?
                    .meta();
                let slot = struct_value.field_mut(*index).expect("field was checked");
                let result = self.set_path(slot, rest, new_value, child_remaining);
                let new = slot.meta();
                struct_value.note_field_replaced(old, new);
                result
            }
            Accessor::Tuple(index) => {
                let Value::Tuple(items) = value else {
                    return self.trap(self.internal_panic("invalid place access"));
                };
                let slots = items.len();
                let items = self.cow_vec_mut(items, slots)?;
                let index = *index as usize;
                let old = items
                    .get(index)
                    .ok_or_else(|| {
                        EvalError::from(self.internal_panic("tuple field out of range"))
                    })?
                    .meta();
                let slot = items.slot_mut(index).expect("field was checked");
                let result = self.set_path(slot, rest, new_value, child_remaining);
                let new = slot.meta();
                items.note_child_replaced(old, new);
                result
            }
            Accessor::Index(index_value) => match value {
                Value::List(items) => {
                    let Value::Int(index) = index_value else {
                        return self.trap(self.internal_panic("invalid place access"));
                    };
                    let slots = items.len();
                    let items = self.cow_vec_mut(items, slots)?;
                    let index = *index;
                    if index < 0 || index as usize >= items.len() {
                        return self.trap(self.internal_panic("index out of bounds"));
                    }
                    let index = index as usize;
                    let old = items[index].meta();
                    let slot = items.slot_mut(index).expect("index was checked");
                    let result = self.set_path(slot, rest, new_value, child_remaining);
                    let new = slot.meta();
                    items.note_child_replaced(old, new);
                    result
                }
                Value::Map(map) => {
                    if value_depth_exceeds(index_value, child_remaining) {
                        return Err(EvalError::from(Trap::DepthExceeded));
                    }
                    self.charge(map_key_cost(index_value))?;
                    let key = key_from_value(index_value)
                        .map_err(|message| EvalError::from(self.internal_panic(message)))?;
                    let slots = map.len().saturating_mul(2);
                    let map = self.cow_map_mut(map, slots)?;
                    if rest.is_empty() {
                        if value_depth_exceeds(&new_value, child_remaining) {
                            return Err(EvalError::from(Trap::DepthExceeded));
                        }
                        if map.get(&key).is_none() {
                            self.note_alloc(16 + key_value_size(&key));
                        }
                        map.insert(key, new_value);
                        Ok(())
                    } else {
                        let old = map
                            .get(&key)
                            .ok_or_else(|| EvalError::from(self.internal_panic("missing map key")))?
                            .meta();
                        let slot = map.value_mut(&key).expect("key was checked");
                        let result = self.set_path(slot, rest, new_value, child_remaining);
                        let new = slot.meta();
                        map.note_value_replaced(&key, old, new);
                        result
                    }
                }
                _ => self.trap(self.internal_panic("invalid place access")),
            },
        }
    }

    /// Move the value at `accessors` out of `value`, leaving `Unit`.
    ///
    /// Copy-on-write charges apply to shared parents, exactly as in `set_path`.
    /// Mutable builtin receivers use this before applying the method so the
    /// receiver itself is uniquely owned when the method mutates it.
    fn take_path_value(&mut self, value: &mut Value, accessors: &[Accessor]) -> EvalResult<Value> {
        let Some((first, rest)) = accessors.split_first() else {
            return Ok(std::mem::replace(value, Value::Unit));
        };
        match first {
            Accessor::Field(index) => {
                let Value::Struct(struct_value) = value else {
                    return self.trap(self.internal_panic("invalid place access"));
                };
                let slots = struct_value.fields.len();
                let struct_value = self.cow_struct_mut(struct_value, slots)?;
                let old = struct_value
                    .fields
                    .get(*index)
                    .ok_or_else(|| {
                        EvalError::from(self.internal_panic("struct field out of range"))
                    })?
                    .meta();
                let taken = {
                    let slot = struct_value.field_mut(*index).expect("field was checked");
                    self.take_path_value(slot, rest)?
                };
                let new = struct_value.fields[*index].meta();
                struct_value.note_field_replaced(old, new);
                Ok(taken)
            }
            Accessor::Tuple(index) => {
                let Value::Tuple(items) = value else {
                    return self.trap(self.internal_panic("invalid place access"));
                };
                let slots = items.len();
                let items = self.cow_vec_mut(items, slots)?;
                let index = *index as usize;
                let old = items
                    .get(index)
                    .ok_or_else(|| {
                        EvalError::from(self.internal_panic("tuple field out of range"))
                    })?
                    .meta();
                let taken = {
                    let slot = items.slot_mut(index).expect("field was checked");
                    self.take_path_value(slot, rest)?
                };
                let new = items[index].meta();
                items.note_child_replaced(old, new);
                Ok(taken)
            }
            Accessor::Index(index_value) => match value {
                Value::List(items) => {
                    let Value::Int(index) = index_value else {
                        return self.trap(self.internal_panic("invalid place access"));
                    };
                    let slots = items.len();
                    let items = self.cow_vec_mut(items, slots)?;
                    let index = *index;
                    if index < 0 || index as usize >= items.len() {
                        return self.trap(self.internal_panic("index out of bounds"));
                    }
                    let index = index as usize;
                    let old = items[index].meta();
                    let taken = {
                        let slot = items.slot_mut(index).expect("index was checked");
                        self.take_path_value(slot, rest)?
                    };
                    let new = items[index].meta();
                    items.note_child_replaced(old, new);
                    Ok(taken)
                }
                Value::Map(map) => {
                    self.charge(map_key_cost(index_value))?;
                    let key = key_from_value(index_value)
                        .map_err(|message| EvalError::from(self.internal_panic(message)))?;
                    let slots = map.len().saturating_mul(2);
                    let map = self.cow_map_mut(map, slots)?;
                    let old = map
                        .get(&key)
                        .ok_or_else(|| EvalError::from(self.internal_panic("missing map key")))?
                        .meta();
                    let taken = {
                        let slot = map.value_mut(&key).expect("key was checked");
                        self.take_path_value(slot, rest)?
                    };
                    let new = map.get(&key).expect("key was checked").meta();
                    map.note_value_replaced(&key, old, new);
                    Ok(taken)
                }
                _ => self.trap(self.internal_panic("invalid place access")),
            },
        }
    }

    fn check_builtin_mutation_depth(
        &self,
        method: BuiltinMethodId,
        args: &[Value],
        path_depth: usize,
    ) -> EvalResult<()> {
        let item_limit = MAX_VALUE_DEPTH.saturating_sub(path_depth).saturating_sub(1);
        let list_limit = MAX_VALUE_DEPTH.saturating_sub(path_depth);
        let check = |value: &Value, limit: usize| -> EvalResult<()> {
            if value_depth_exceeds(value, limit) {
                Err(EvalError::from(Trap::DepthExceeded))
            } else {
                Ok(())
            }
        };
        match method {
            BuiltinMethodId::LIST_PUSH => {
                if let Some(value) = args.first() {
                    check(value, item_limit)?;
                }
            }
            BuiltinMethodId::LIST_INSERT => {
                if let Some(value) = args.get(1) {
                    check(value, item_limit)?;
                }
            }
            BuiltinMethodId::LIST_EXTEND => {
                if let Some(value) = args.first() {
                    check(value, list_limit)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// General entry point, also used by builtins that index a value directly.
    fn index_value(&mut self, base: Value, index: Value, span: Span) -> EvalResult<Value> {
        match (base, index) {
            (Value::List(items), Value::Int(index)) => self.index_list_element(items, index, span),
            (
                Value::List(items),
                Value::Range {
                    start,
                    end,
                    inclusive,
                },
            ) => self.index_list_slice(items, start, end, inclusive, span),
            (
                Value::Str(text),
                Value::Range {
                    start,
                    end,
                    inclusive,
                },
            ) => self.index_str_slice(text, start, end, inclusive, span),
            (Value::Map(map), key) => self.index_map_lookup(map, &key, span),
            _ => self.trap(self.panic("invalid index operation", span)),
        }
    }

    /// Dispatch on the `IndexKind` derived from the IR base and index types.
    ///
    /// A specialized arm only handles its own shape; anything else falls back
    /// to the sequential match so malformed or unresolved IR keeps the generic
    /// rules (and the same traps and fuel).
    fn index_value_kind(
        &mut self,
        kind: IndexKind,
        base: Value,
        index: Value,
        span: Span,
    ) -> EvalResult<Value> {
        match kind {
            IndexKind::ListIndex => match (base, index) {
                (Value::List(items), Value::Int(index)) => {
                    self.index_list_element(items, index, span)
                }
                (base, index) => self.index_value(base, index, span),
            },
            IndexKind::ListSlice => match (base, index) {
                (
                    Value::List(items),
                    Value::Range {
                        start,
                        end,
                        inclusive,
                    },
                ) => self.index_list_slice(items, start, end, inclusive, span),
                (base, index) => self.index_value(base, index, span),
            },
            IndexKind::StrSlice => match (base, index) {
                (
                    Value::Str(text),
                    Value::Range {
                        start,
                        end,
                        inclusive,
                    },
                ) => self.index_str_slice(text, start, end, inclusive, span),
                (base, index) => self.index_value(base, index, span),
            },
            IndexKind::MapIndex => match base {
                Value::Map(map) => self.index_map_lookup(map, &index, span),
                base => self.index_value(base, index, span),
            },
            IndexKind::General => self.index_value(base, index, span),
        }
    }

    fn index_list_element(&self, items: Rc<SeqValue>, index: i64, span: Span) -> EvalResult<Value> {
        if index < 0 || index as usize >= items.len() {
            return self.trap(self.panic("index out of bounds", span));
        }
        Ok(items[index as usize].clone())
    }

    fn index_list_slice(
        &mut self,
        items: Rc<SeqValue>,
        start: i64,
        end: i64,
        inclusive: bool,
        span: Span,
    ) -> EvalResult<Value> {
        let len = items.len() as i64;
        let Some((start, end)) = slice_bounds(start, end, inclusive, len) else {
            return self.trap(self.panic("invalid list slice", span));
        };
        let slice_len = end - start;
        self.charge(fuel::slot_cost(slice_len as u64))?;
        self.ensure_output_capacity(16 + 8 * slice_len as u64)?;
        let slice = items[start..end].to_vec();
        self.note_alloc(16 + 8 * slice.len() as u64);
        let value = Value::list(slice);
        self.check_value_depth(&value, span)?;
        Ok(value)
    }

    fn index_str_slice(
        &mut self,
        text: Rc<str>,
        start: i64,
        end: i64,
        inclusive: bool,
        span: Span,
    ) -> EvalResult<Value> {
        // Slicing scans to find character boundaries. Validate first so an
        // invalid slice keeps its panic (and its fuel) unchanged, then charge
        // the input bytes the scan touched.
        let char_count = text.chars().count() as i64;
        let Some((start, end)) = slice_bounds(start, end, inclusive, char_count) else {
            return self.trap(self.panic("invalid string slice", span));
        };
        self.charge(fuel::string_cost(text.len() as u64))?;
        let start_byte = text
            .char_indices()
            .nth(start)
            .map(|(byte, _)| byte)
            .unwrap_or(text.len());
        let end_byte = if end as i64 == char_count {
            text.len()
        } else {
            text.char_indices()
                .nth(end)
                .map(|(byte, _)| byte)
                .unwrap_or(text.len())
        };
        let out = &text[start_byte..end_byte];
        self.ensure_output_capacity(16 + out.len() as u64)?;
        let out = out.to_string();
        self.note_alloc(16 + out.len() as u64);
        Ok(Value::str(out))
    }

    /// Map-key conversion and lookup stay in one place so key changes are
    /// isolated from the index dispatch.
    fn index_map_lookup(
        &mut self,
        map: Rc<MapValue>,
        key: &Value,
        span: Span,
    ) -> EvalResult<Value> {
        self.charge(map_key_cost(key))?;
        let key = self.map_key_ref_at(key, span)?;
        map.get(&key)
            .cloned()
            .ok_or_else(|| EvalError::from(self.panic("missing map key", span)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BinaryKind {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

/// Fuel a comparison of `lhs` and `rhs` touches.
///
/// Kept as one function so the specialized scalar comparisons and the general
/// value-shape path charge exactly the same amount in the same order.
fn comparison_charge(lhs: &Value, rhs: &Value) -> u64 {
    let lhs_meta = lhs.meta();
    let rhs_meta = rhs.meta();
    fuel::comparison_cost(lhs_meta.slots, lhs_meta.string_slots)
        .max(fuel::comparison_cost(rhs_meta.slots, rhs_meta.string_slots))
}

fn take_writeback(updated: &mut Writebacks, index: usize) -> Option<Value> {
    let position = updated.iter().position(|(slot, _)| *slot == index)?;
    Some(updated.swap_remove(position).1)
}

fn slice_bounds(start: i64, end: i64, inclusive: bool, len: i64) -> Option<(usize, usize)> {
    if start < 0 || end < start {
        return None;
    }
    let end = if inclusive { end.checked_add(1)? } else { end };
    if end > len {
        return None;
    }
    Some((start as usize, end as usize))
}

fn prelude_payload(value: &Value, variant: PreludeVariant) -> Option<&[Value]> {
    match (variant, value) {
        (PreludeVariant::None, Value::Option(None)) => Some(&[]),
        (PreludeVariant::Some, Value::Option(Some(value))) => {
            Some(std::slice::from_ref(value.as_ref()))
        }
        (PreludeVariant::Ok, Value::Result(Ok(value))) => {
            Some(std::slice::from_ref(value.as_ref()))
        }
        (PreludeVariant::Err, Value::Result(Err(value))) => {
            Some(std::slice::from_ref(value.as_ref()))
        }
        _ => None,
    }
}

fn variant_payload(value: &Value) -> Option<&[Value]> {
    match value {
        Value::Option(Some(value)) => Some(std::slice::from_ref(value.as_ref())),
        Value::Option(None) => Some(&[]),
        Value::Result(Ok(value)) => Some(std::slice::from_ref(value.as_ref())),
        Value::Result(Err(value)) => Some(std::slice::from_ref(value.as_ref())),
        Value::Enum(value) => Some(&value.payload),
        _ => None,
    }
}

impl<'p, 'h> IrRuntime<'p, 'h> {
    fn bind_pattern(&mut self, pattern: &Pattern, value: &Value) -> EvalResult<()> {
        match pattern {
            Pattern::Wild => {}
            Pattern::Bind { slot, .. } => self.set_slot(*slot, value.clone()),
            Pattern::Int(_) | Pattern::Bool(_) | Pattern::Str(_) => {}
            Pattern::Tuple(items) => {
                if let Value::Tuple(values) = value {
                    for (pattern, value) in items.iter().zip(values.iter()) {
                        self.bind_pattern(pattern, value)?;
                    }
                }
            }
            Pattern::Variant { args, .. } | Pattern::Prelude { args, .. } => {
                if let Some(payload) = variant_payload(value) {
                    for (pattern, value) in args.iter().zip(payload.iter()) {
                        self.bind_pattern(pattern, value)?;
                    }
                }
            }
            Pattern::Struct { fields, .. } => {
                if let Value::Struct(value) = value {
                    for (declaration, pattern) in fields {
                        if let Some(field) = value.fields.get(*declaration as usize) {
                            self.bind_pattern(pattern, field)?;
                        }
                    }
                }
            }
            Pattern::Or(alternatives) => {
                if let Some(first) = alternatives.first() {
                    self.bind_pattern(first, value)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Accessor, BinaryDispatch, BinaryKind, CallArgs, Frame, IndexKind, IrRuntime,
        MAX_VALUE_DEPTH, Place,
    };
    use crate::exec::{ExecLimits, Trap};
    use crate::value::{Closure, FnValue, KeyValue, OrderedMap, StructValue, Value, map_key_cost};
    use martite_ir::{
        AccessMode, CaptureIndex, CaptureInfo, CaptureSource, Charge, ChildRange, ConstId,
        ConstValue, Effects, Entries, FnId, FnKind, IrFn, IrId, IrProgram, Node, NodeKind,
        ParamInfo, Pattern, PatternId, Slot, SlotInfo, StrId, Ty, TyId, TypeId, TypeTable,
    };
    use smallvec::SmallVec;
    use std::collections::HashSet;
    use std::rc::Rc;

    fn trap(error: super::EvalError) -> Trap {
        match error {
            super::EvalError::Trap(trap) => *trap,
            other => panic!("expected a trap, got {other:?}"),
        }
    }

    #[test]
    fn eval_result_stays_within_its_size_budget() {
        let size = std::mem::size_of::<super::EvalResult<Value>>();
        assert!(
            size <= 32,
            "EvalResult<Value> is {size} bytes; boxed trap payloads should keep it within 32"
        );
    }

    fn expression(kind: NodeKind) -> Node {
        Node::new(kind, TyId::new(0), Charge::expression(), Effects::NONE)
    }

    fn zero(kind: NodeKind) -> Node {
        Node::new(kind, TyId::new(0), Charge::ZERO, Effects::NONE)
    }

    fn finish(mut function: IrFn, nodes: Vec<Node>, children: Vec<IrId>) -> IrFn {
        function.spans = vec![martite_base::Span::empty(0); nodes.len()];
        function.synthetic = vec![None; nodes.len()];
        function.safepoints = vec![false; nodes.len()];
        function.nodes = nodes;
        function.children = children;
        function
    }

    fn empty_function(locals: u32) -> IrFn {
        IrFn {
            id: FnId::new(0),
            name: "test".into(),
            source: martite_base::SourceId(0),
            kind: FnKind::Free,
            params: Vec::new(),
            ret: TyId::new(0),
            locals,
            slot_info: Vec::new(),
            captures: Vec::new(),
            patterns: Vec::new(),
            matches: Vec::new(),
            struct_layouts: Vec::new(),
            str_parts: Vec::new(),
            nodes: Vec::new(),
            spans: Vec::new(),
            synthetic: Vec::new(),
            safepoints: Vec::new(),
            children: Vec::new(),
            body: None,
        }
    }

    #[test]
    fn frames_allocate_exactly_the_declared_slots() {
        let function = empty_function(7);
        let frame = Frame {
            slots: SmallVec::from_elem(None, function.locals as usize),
            captures: None,
            source: function.source,
            name: function.name.as_ref(),
        };
        assert_eq!(frame.slots.len(), 7);
        assert!(frame.slots.iter().all(Option::is_none));
    }

    #[test]
    fn new_runtime_sizes_the_pools() {
        let program = IrProgram::default();
        let runtime = IrRuntime::new(&program, ExecLimits::default());
        assert!(runtime.const_values.is_empty());
        assert!(runtime.literal_values.is_empty());
    }

    #[test]
    fn slot_init_write_and_take_execute_through_slots() {
        let span = martite_base::Span::empty(0);
        let mut function = empty_function(1);
        function.body = Some(IrId::new(8));
        function.nodes = vec![
            Node::new(
                NodeKind::Int(martite_ir::ConstId::new(0)),
                TyId::new(0),
                Charge::expression(),
                Effects::NONE,
            ),
            Node::new(
                NodeKind::SlotInit {
                    slot: Slot::new(0),
                    value: IrId::new(0),
                },
                TyId::new(0),
                Charge::ZERO,
                Effects::NONE,
            ),
            Node::new(
                NodeKind::Int(martite_ir::ConstId::new(1)),
                TyId::new(0),
                Charge::expression(),
                Effects::NONE,
            ),
            Node::new(
                NodeKind::SlotWrite {
                    slot: Slot::new(0),
                    value: IrId::new(2),
                },
                TyId::new(0),
                Charge::ZERO,
                Effects::NONE,
            ),
            Node::new(
                NodeKind::SlotRead {
                    slot: Slot::new(0),
                    mode: martite_ir::AccessMode::Share,
                },
                TyId::new(0),
                Charge::expression(),
                Effects::NONE,
            ),
            Node::new(
                NodeKind::ExprStmt(IrId::new(4)),
                TyId::new(0),
                Charge::ZERO,
                Effects::NONE,
            ),
            Node::new(
                NodeKind::SlotRead {
                    slot: Slot::new(0),
                    mode: martite_ir::AccessMode::Take,
                },
                TyId::new(0),
                Charge::expression(),
                Effects::NONE,
            ),
            Node::new(
                NodeKind::ExprStmt(IrId::new(6)),
                TyId::new(0),
                Charge::ZERO,
                Effects::NONE,
            ),
            Node::new(
                NodeKind::Block(ChildRange::new(0, 4)),
                TyId::new(0),
                Charge::ZERO,
                Effects::NONE,
            ),
        ];
        function.spans = vec![span; function.nodes.len()];
        function.synthetic = vec![None; function.nodes.len()];
        function.safepoints = vec![false; function.nodes.len()];
        function.children = vec![IrId::new(1), IrId::new(3), IrId::new(5), IrId::new(7)];

        let program = IrProgram {
            fns: vec![function],
            consts: vec![ConstValue::Int(7), ConstValue::Int(8)],
            entries: Entries {
                main: Some(FnId::new(0)),
                ..Entries::default()
            },
            ..IrProgram::default()
        };
        let mut runtime = IrRuntime::new(&program, ExecLimits::default());
        let result = runtime.run_main().expect("slot test runs");
        assert_eq!(result.value, crate::value::Value::Int(8));
    }

    #[test]
    fn make_closure_snapshots_slots_and_capture_reads_them() {
        let mut lambda = empty_function(0);
        lambda.id = FnId::new(1);
        lambda.name = "<lambda>".into();
        lambda.kind = FnKind::Lambda;
        lambda.captures = vec![CaptureInfo {
            source: CaptureSource::Slot(Slot::new(0)),
            ty: TyId::new(0),
            name: Some("value".into()),
        }];
        let mut lambda = finish(
            lambda,
            vec![expression(NodeKind::CaptureRead(CaptureIndex::new(0)))],
            Vec::new(),
        );
        lambda.body = Some(IrId::new(0));

        let mut main = empty_function(1);
        main.name = "main".into();
        let mut main = finish(
            main,
            vec![
                expression(NodeKind::Int(ConstId::new(0))),
                zero(NodeKind::SlotInit {
                    slot: Slot::new(0),
                    value: IrId::new(0),
                }),
                expression(NodeKind::MakeClosure(FnId::new(1))),
                zero(NodeKind::Block(ChildRange::new(0, 2))),
            ],
            vec![IrId::new(1), IrId::new(2)],
        );
        main.body = Some(IrId::new(3));

        let program = IrProgram {
            fns: vec![main, lambda],
            consts: vec![ConstValue::Int(7)],
            entries: Entries {
                main: Some(FnId::new(0)),
                ..Entries::default()
            },
            ..IrProgram::default()
        };
        let mut runtime = IrRuntime::new(&program, ExecLimits::default());
        let result = runtime.run_main().expect("closure creation runs");
        let Value::Fn(FnValue::Closure(closure)) = result.value else {
            panic!("main must return a closure");
        };
        assert_eq!(
            closure.capture(CaptureIndex::new(0)),
            Some(&Value::Int(7)),
            "the closure snapshots the creating frame's slot"
        );
        let value = runtime
            .call_closure(&closure, CallArgs::new())
            .expect("closure call runs");
        assert_eq!(value, Value::Int(7));
        let value = runtime
            .call_closure(&closure, CallArgs::new())
            .expect("closure call runs again");
        assert_eq!(value, Value::Int(7), "captures survive repeated calls");
    }

    #[test]
    fn destructured_lambda_parameters_bind_pattern_slots() {
        let mut lambda = empty_function(3);
        lambda.name = "<lambda>".into();
        lambda.kind = FnKind::Lambda;
        lambda.params = vec![ParamInfo {
            slot: Slot::new(0),
            name: "pair".into(),
            ty: TyId::new(0),
            mutable: false,
            is_self: false,
            pattern: Some(PatternId::new(0)),
        }];
        lambda.slot_info = vec![
            SlotInfo {
                ty: TyId::new(0),
                mutable: false,
                name: None,
            },
            SlotInfo {
                ty: TyId::new(0),
                mutable: false,
                name: Some("a".into()),
            },
            SlotInfo {
                ty: TyId::new(0),
                mutable: false,
                name: Some("b".into()),
            },
        ];
        lambda.patterns = vec![Pattern::Tuple(vec![
            Pattern::Bind {
                slot: Slot::new(1),
                ty: TyId::new(0),
            },
            Pattern::Bind {
                slot: Slot::new(2),
                ty: TyId::new(0),
            },
        ])];
        let mut lambda = finish(
            lambda,
            vec![
                expression(NodeKind::SlotRead {
                    slot: Slot::new(1),
                    mode: AccessMode::Share,
                }),
                expression(NodeKind::SlotRead {
                    slot: Slot::new(2),
                    mode: AccessMode::Share,
                }),
                expression(NodeKind::Add {
                    lhs: IrId::new(0),
                    rhs: IrId::new(1),
                }),
            ],
            Vec::new(),
        );
        lambda.body = Some(IrId::new(2));

        let program = IrProgram {
            fns: vec![lambda],
            ..IrProgram::default()
        };
        let mut runtime = IrRuntime::new(&program, ExecLimits::default());
        let closure = Closure::new(FnId::new(0), None);
        let value = runtime
            .call_closure(
                &closure,
                CallArgs::from(vec![Value::tuple(vec![Value::Int(2), Value::Int(3)])]),
            )
            .expect("destructured lambda call runs");
        assert_eq!(value, Value::Int(5));
    }

    #[test]
    fn nested_closures_read_captures_from_the_creating_lambda() {
        let mut inner = empty_function(0);
        inner.id = FnId::new(1);
        inner.name = "<lambda>".into();
        inner.kind = FnKind::Lambda;
        inner.captures = vec![CaptureInfo {
            source: CaptureSource::Capture(CaptureIndex::new(0)),
            ty: TyId::new(0),
            name: Some("value".into()),
        }];
        let mut inner = finish(
            inner,
            vec![expression(NodeKind::CaptureRead(CaptureIndex::new(0)))],
            Vec::new(),
        );
        inner.body = Some(IrId::new(0));

        let mut outer = empty_function(0);
        outer.id = FnId::new(0);
        outer.name = "<lambda>".into();
        outer.kind = FnKind::Lambda;
        outer.captures = vec![CaptureInfo {
            source: CaptureSource::Slot(Slot::new(0)),
            ty: TyId::new(0),
            name: Some("value".into()),
        }];
        let mut outer = finish(
            outer,
            vec![expression(NodeKind::MakeClosure(FnId::new(1)))],
            Vec::new(),
        );
        outer.body = Some(IrId::new(0));

        let program = IrProgram {
            fns: vec![outer, inner],
            ..IrProgram::default()
        };
        let mut runtime = IrRuntime::new(&program, ExecLimits::default());
        let captures: Rc<[Value]> = Rc::from(vec![Value::Int(11)]);
        let outer = Closure::new(FnId::new(0), Some(captures));
        let value = runtime
            .call_closure(&outer, CallArgs::new())
            .expect("outer lambda call runs");
        let Value::Fn(FnValue::Closure(inner)) = value else {
            panic!("outer lambda must return the inner closure");
        };
        assert_eq!(
            inner.capture(CaptureIndex::new(0)),
            Some(&Value::Int(11)),
            "the inner closure reads the outer lambda's capture"
        );
        assert_eq!(
            runtime
                .call_closure(&inner, CallArgs::new())
                .expect("inner lambda call runs"),
            Value::Int(11)
        );
    }

    #[test]
    fn lambda_traps_use_the_defining_source() {
        let mut lambda = empty_function(0);
        lambda.id = FnId::new(1);
        lambda.name = "<lambda>".into();
        lambda.kind = FnKind::Lambda;
        lambda.source = martite_base::SourceId(0);
        let mut lambda = finish(
            lambda,
            vec![
                expression(NodeKind::Str(StrId::new(0))),
                expression(NodeKind::Panic(ChildRange::new(0, 1))),
            ],
            vec![IrId::new(0)],
        );
        lambda.body = Some(IrId::new(1));

        let program = IrProgram {
            fns: vec![empty_function(1), lambda],
            strs: vec!["boom".into()],
            ..IrProgram::default()
        };
        let mut runtime = IrRuntime::new(&program, ExecLimits::default());
        runtime.frames.push(Frame {
            slots: SmallVec::from_elem(None, 1),
            captures: None,
            source: martite_base::SourceId(1),
            name: "main",
        });
        let closure = Closure::new(FnId::new(1), None);
        let error = runtime
            .call_closure(&closure, CallArgs::new())
            .expect_err("the lambda panics");
        let Trap::Panic {
            message,
            source,
            trace,
            ..
        } = error
        else {
            panic!("expected a panic trap, got {error:?}");
        };
        assert_eq!(message, "boom");
        assert_eq!(source, Some(martite_base::SourceId(0)));
        assert_eq!(trace, vec!["main".to_string(), "<lambda>".to_string()]);
    }

    #[test]
    fn a_trapping_call_leaves_the_runtime_ready_for_the_next_call() {
        let mut boom = empty_function(0);
        boom.name = "boom".into();
        let mut boom = finish(
            boom,
            vec![
                expression(NodeKind::Str(StrId::new(0))),
                expression(NodeKind::Panic(ChildRange::new(0, 1))),
            ],
            vec![IrId::new(0)],
        );
        boom.body = Some(IrId::new(1));

        let mut good = empty_function(0);
        good.id = FnId::new(1);
        good.name = "good".into();
        let mut good = finish(
            good,
            vec![expression(NodeKind::Int(ConstId::new(0)))],
            Vec::new(),
        );
        good.body = Some(IrId::new(0));

        let program = IrProgram {
            fns: vec![boom, good],
            consts: vec![ConstValue::Int(42)],
            strs: vec!["boom".into()],
            ..IrProgram::default()
        };
        let mut runtime = IrRuntime::new(&program, ExecLimits::default());
        assert!(
            runtime
                .call_function(FnId::new(0), CallArgs::new())
                .is_err()
        );
        assert!(runtime.frames.is_empty(), "the trapped frame must pop");
        assert!(runtime.current.is_none(), "current must be restored");
        assert_eq!(
            runtime
                .call_function(FnId::new(1), CallArgs::new())
                .expect("the runtime recovers"),
            Value::Int(42)
        );
    }

    fn accounting_limits(fuel: u64, memory: u64) -> ExecLimits {
        ExecLimits {
            fuel,
            memory,
            depth: 200,
            deadline: None,
        }
    }

    fn shared_dag(levels: usize) -> Value {
        let mut value = Value::Unit;
        for _ in 0..levels {
            value = Value::list(vec![value.clone(), value]);
        }
        value
    }

    fn interpolation_program(levels: usize) -> IrProgram {
        let mut function = empty_function(0);
        function.name = "main".into();
        let mut nodes = vec![expression(NodeKind::Int(ConstId::new(0)))];
        let mut children = Vec::new();
        let mut current = IrId::new(0);
        for _ in 0..levels {
            let start = children.len() as u32;
            children.push(current);
            children.push(current);
            let id = IrId::new(nodes.len() as u32);
            nodes.push(zero(NodeKind::List(ChildRange::new(start, 2))));
            current = id;
        }
        let interp = IrId::new(nodes.len() as u32);
        nodes.push(zero(NodeKind::StrInterp(ChildRange::new(0, 1))));
        let mut function = finish(function, nodes, children);
        function.str_parts = vec![martite_ir::StrPart::Expr(current)];
        function.body = Some(interp);
        IrProgram {
            fns: vec![function],
            consts: vec![ConstValue::Int(1)],
            entries: Entries {
                main: Some(FnId::new(0)),
                ..Entries::default()
            },
            ..IrProgram::default()
        }
    }

    #[test]
    fn interpolation_charges_before_expanding_a_shared_dag() {
        let program = interpolation_program(18);

        let mut runtime = IrRuntime::new(&program, accounting_limits(10_000_000, 4096));
        let error = runtime
            .run_main()
            .expect_err("the expansion must be capped");
        assert!(
            matches!(error, Trap::OutOfMemory),
            "memory cap trap: {error:?}"
        );

        let mut runtime = IrRuntime::new(&program, accounting_limits(1000, 8 << 20));
        let error = runtime
            .run_main()
            .expect_err("show fuel is charged before the write");
        assert!(
            matches!(error, Trap::OutOfFuel),
            "fuel trap before allocation: {error:?}"
        );
    }

    #[test]
    fn interpolation_output_is_unchanged() {
        let program = interpolation_program(3);
        let mut runtime = IrRuntime::new(&program, accounting_limits(1_000_000, 8 << 20));
        let result = runtime.run_main().expect("small interpolation");
        assert_eq!(
            super::show(&result.value, &program.types, true),
            "[[[1, 1], [1, 1]], [[1, 1], [1, 1]]]"
        );
    }

    fn run_interpolation(
        function: IrFn,
        nodes: Vec<Node>,
        children: Vec<IrId>,
        consts: Vec<ConstValue>,
        str_parts: Vec<martite_ir::StrPart>,
        body: IrId,
    ) -> String {
        let mut function = finish(function, nodes, children);
        function.str_parts = str_parts;
        function.body = Some(body);
        let program = IrProgram {
            fns: vec![function],
            consts,
            entries: Entries {
                main: Some(FnId::new(0)),
                ..Entries::default()
            },
            ..IrProgram::default()
        };
        let mut runtime = IrRuntime::new(&program, accounting_limits(1_000_000, 8 << 20));
        let result = runtime.run_main().expect("interpolation runs");
        result
            .value
            .as_str()
            .expect("interpolation returns a string")
            .to_string()
    }

    #[test]
    fn integer_interpolation_is_byte_exact() {
        let cases = [
            (i64::MIN, "-9223372036854775808"),
            (i64::MAX, "9223372036854775807"),
            (-1, "-1"),
            (0, "0"),
            (9, "9"),
            (10, "10"),
            (-9, "-9"),
            (-10, "-10"),
        ];
        for (value, expected) in cases {
            let output = run_interpolation(
                empty_function(0),
                vec![
                    expression(NodeKind::Int(ConstId::new(0))),
                    zero(NodeKind::StrInterp(ChildRange::new(0, 1))),
                ],
                Vec::new(),
                vec![ConstValue::Int(value)],
                vec![martite_ir::StrPart::Expr(IrId::new(0))],
                IrId::new(1),
            );
            assert_eq!(output, expected);
        }
    }

    #[test]
    fn nested_container_interpolation_shows_integers_exactly() {
        let output = run_interpolation(
            empty_function(0),
            vec![
                expression(NodeKind::Int(ConstId::new(0))),
                expression(NodeKind::Int(ConstId::new(1))),
                expression(NodeKind::Int(ConstId::new(2))),
                zero(NodeKind::List(ChildRange::new(0, 2))),
                zero(NodeKind::Tuple(ChildRange::new(2, 2))),
                zero(NodeKind::StrInterp(ChildRange::new(0, 1))),
            ],
            vec![IrId::new(0), IrId::new(1), IrId::new(3), IrId::new(2)],
            vec![
                ConstValue::Int(i64::MIN),
                ConstValue::Int(0),
                ConstValue::Int(10),
            ],
            vec![martite_ir::StrPart::Expr(IrId::new(4))],
            IrId::new(5),
        );
        assert_eq!(output, "([-9223372036854775808, 0], 10)");
    }

    #[test]
    fn shared_dag_show_stays_inside_the_memory_limit() {
        let program = IrProgram::default();
        let value = shared_dag(18);
        let mut runtime = IrRuntime::new(&program, accounting_limits(10_000_000, 4096));
        let error = runtime
            .render_show(&value, true)
            .expect_err("the expansion must be capped");
        assert!(
            matches!(&error, super::EvalError::Trap(trap) if matches!(&**trap, Trap::OutOfMemory)),
            "{error:?}"
        );

        let mut out = crate::ops::ShowOutput::new(4096);
        super::write_show(&mut out, &value, &program.types, true);
        assert!(out.is_overflowed());
        assert!(
            out.capacity() <= 4096,
            "output capacity {} exceeded the memory cap",
            out.capacity()
        );
    }

    #[test]
    fn bounded_show_matches_the_normative_format() {
        let program = IrProgram::default();
        let value = Value::list(vec![
            Value::str("a\"b\n"),
            Value::Int(7),
            Value::tuple(vec![Value::Bool(false)]),
        ]);
        let mut runtime = IrRuntime::new(&program, accounting_limits(1_000_000, 8 << 20));
        let rendered = runtime.render_show(&value, true).expect("small output");
        assert_eq!(rendered, super::show(&value, &program.types, true));
    }

    #[test]
    fn a_shared_aggregate_is_rescanned_after_a_pending_bytes_reset() {
        let program = IrProgram::default();

        let mut runtime = IrRuntime::new(&program, accounting_limits(1_000_000, 4096));
        let epoch = runtime.scan_epoch;
        runtime.note_alloc(1024);
        runtime.refresh_memory(None).expect("the initial scan fits");
        assert_eq!(runtime.pending_bytes, 0);

        let inner = Value::list(vec![Value::str("abcd")]);
        let value = Value::list(vec![inner.clone(), inner]);
        let bytes = value.size_bytes(&mut HashSet::new());
        runtime.note_alloc(16 + 16);
        runtime
            .finish_aggregate(&value, epoch)
            .expect("the finished aggregate is rescanned");
        assert_eq!(runtime.census_bytes, bytes);

        let mut tight = IrRuntime::new(&program, accounting_limits(1_000_000, 50));
        let epoch = tight.scan_epoch;
        tight.note_alloc(16);
        tight.refresh_memory(None).expect("the initial scan fits");
        tight.note_alloc(32);
        let error = tight
            .finish_aggregate(&value, epoch)
            .expect_err("the lost in-flight bytes are recovered");
        assert!(
            matches!(&error, super::EvalError::Trap(trap) if matches!(&**trap, Trap::OutOfMemory)),
            "{error:?}"
        );
    }

    /// Runs one map `for` loop whose body breaks after binding the first entry.
    ///
    /// The map is bound to slot 0; the pattern binds from slot 1 onward. The
    /// returned slots are the frame state after the single iteration.
    fn run_map_for_loop(
        pattern: Pattern,
        locals: u32,
        map: OrderedMap,
    ) -> (Vec<Option<Value>>, u64) {
        let mut function = empty_function(locals);
        function.patterns = vec![pattern];
        let function = finish(
            function,
            vec![
                expression(NodeKind::SlotRead {
                    slot: Slot::new(0),
                    mode: AccessMode::Share,
                }),
                zero(NodeKind::Break),
            ],
            vec![IrId::new(0), IrId::new(1)],
        );
        let program = IrProgram {
            fns: vec![function],
            ..IrProgram::default()
        };
        let mut runtime = IrRuntime::new(&program, ExecLimits::default());
        runtime.current = Some(&program.fns[0]);
        runtime.frames.push(Frame {
            slots: SmallVec::from_elem(None, locals as usize),
            captures: None,
            source: program.fns[0].source,
            name: program.fns[0].name.as_ref(),
        });
        runtime.frames.last_mut().expect("frame").slots[0] = Some(Value::map(map));
        runtime
            .eval_for(PatternId::new(0), 0, 1)
            .expect("map loop runs");
        let slots = runtime.frames.pop().expect("frame").slots;
        (slots.into_iter().collect(), runtime.fuel_used())
    }

    fn bound_int(value: Option<&Value>) -> Option<i64> {
        value.and_then(Value::as_int)
    }

    #[test]
    fn map_place_reads_accept_borrowed_keys() {
        let program = IrProgram::default();
        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("a".into()), Value::Int(3));
        map.insert(KeyValue::Int(7), Value::Int(4));
        map.insert(KeyValue::Tuple(vec![KeyValue::Bool(true)]), Value::Int(5));

        let mut runtime = IrRuntime::new(&program, ExecLimits::default());
        runtime.frames.push(Frame {
            slots: SmallVec::from_elem(None, 1),
            captures: None,
            source: martite_base::SourceId(0),
            name: "test",
        });
        runtime.frames[0].slots[0] = Some(Value::map(map));

        for (index, expected) in [
            (Value::str("a"), 3),
            (Value::Int(7), 4),
            (Value::tuple(vec![Value::Bool(true)]), 5),
        ] {
            let mut accessors: SmallVec<[super::Accessor; 4]> = SmallVec::new();
            accessors.push(super::Accessor::Index(index));
            let place = super::Place {
                root: Slot::new(0),
                accessors,
            };
            let value = runtime.read_place(&place).expect("map place read");
            assert_eq!(value.as_int(), Some(expected));
        }
    }

    #[test]
    fn map_for_loop_binds_wildcard_key_and_value_directly() {
        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("first".into()), Value::Int(10));
        map.insert(KeyValue::Str("second".into()), Value::Int(20));
        let pattern = Pattern::Tuple(vec![
            Pattern::Wild,
            Pattern::Bind {
                slot: Slot::new(1),
                ty: TyId::new(0),
            },
        ]);
        let (slots, fuel) = run_map_for_loop(pattern, 2, map);
        assert_eq!(bound_int(slots[1].as_ref()), Some(10));
        assert_eq!(
            fuel, 2,
            "one expression charge plus one per-iteration charge"
        );
    }

    #[test]
    fn map_for_loop_binds_simple_key_and_value() {
        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("b".into()), Value::Int(1));
        map.insert(KeyValue::Str("a".into()), Value::Int(2));
        let pattern = Pattern::Tuple(vec![
            Pattern::Bind {
                slot: Slot::new(1),
                ty: TyId::new(0),
            },
            Pattern::Bind {
                slot: Slot::new(2),
                ty: TyId::new(0),
            },
        ]);
        let (slots, fuel) = run_map_for_loop(pattern, 3, map);
        assert_eq!(slots[1].as_ref().and_then(Value::as_str), Some("b"));
        assert_eq!(bound_int(slots[2].as_ref()), Some(1));
        assert_eq!(fuel, 2, "the tuple has no fuel charge of its own");
    }

    #[test]
    fn map_for_loop_converts_an_int_key_once() {
        let mut map = OrderedMap::new();
        map.insert(KeyValue::Int(7), Value::Int(1));
        let pattern = Pattern::Tuple(vec![
            Pattern::Bind {
                slot: Slot::new(1),
                ty: TyId::new(0),
            },
            Pattern::Bind {
                slot: Slot::new(2),
                ty: TyId::new(0),
            },
        ]);
        let (slots, _) = run_map_for_loop(pattern, 3, map);
        assert_eq!(bound_int(slots[1].as_ref()), Some(7));
        assert_eq!(bound_int(slots[2].as_ref()), Some(1));
    }

    #[test]
    fn map_for_loop_falls_back_to_an_entry_tuple_for_nested_key_patterns() {
        let mut map = OrderedMap::new();
        map.insert(
            KeyValue::Tuple(vec![KeyValue::Str("nested".into())]),
            Value::Int(5),
        );
        let pattern = Pattern::Tuple(vec![
            Pattern::Tuple(vec![Pattern::Bind {
                slot: Slot::new(1),
                ty: TyId::new(0),
            }]),
            Pattern::Bind {
                slot: Slot::new(2),
                ty: TyId::new(0),
            },
        ]);
        let (slots, _) = run_map_for_loop(pattern, 3, map);
        assert_eq!(slots[1].as_ref().and_then(Value::as_str), Some("nested"));
        assert_eq!(bound_int(slots[2].as_ref()), Some(5));
    }

    #[test]
    fn map_for_loop_binds_wildcard_value_without_materialising_it() {
        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("key".into()), Value::Int(1));
        let pattern = Pattern::Tuple(vec![
            Pattern::Bind {
                slot: Slot::new(1),
                ty: TyId::new(0),
            },
            Pattern::Wild,
        ]);
        let (slots, _) = run_map_for_loop(pattern, 2, map);
        assert_eq!(slots[1].as_ref().and_then(Value::as_str), Some("key"));
    }

    #[test]
    fn map_for_loop_wildcard_pair_binds_nothing() {
        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("key".into()), Value::Int(1));
        let pattern = Pattern::Tuple(vec![Pattern::Wild, Pattern::Wild]);
        let (slots, fuel) = run_map_for_loop(pattern, 2, map);
        assert!(slots.iter().skip(1).all(Option::is_none));
        assert_eq!(fuel, 2);
    }

    #[test]
    fn map_for_loop_keeps_the_whole_entry_pattern_fallback() {
        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("key".into()), Value::Int(9));
        let pattern = Pattern::Bind {
            slot: Slot::new(1),
            ty: TyId::new(0),
        };
        let (slots, _) = run_map_for_loop(pattern, 2, map);
        let Some(Value::Tuple(entry)) = slots[1].as_ref() else {
            panic!("whole-entry pattern must bind the (key, value) tuple");
        };
        assert_eq!(entry[0].as_str(), Some("key"));
        assert_eq!(entry[1].as_int(), Some(9));
    }

    // ------------------------------------------------------------------
    // Borrowed place reads and index specialization (#70, #84)
    // ------------------------------------------------------------------

    struct TestTypes {
        int: TyId,
        float: TyId,
        str_ty: TyId,
        range: TyId,
        list_int: TyId,
        list_struct: TyId,
        map_str_int: TyId,
        struct_ty: TyId,
        param: TyId,
        never: TyId,
    }

    fn test_types() -> (TypeTable, TestTypes) {
        let mut table = TypeTable::default();
        let int = table.add_ty(Ty::Int);
        let float = table.add_ty(Ty::Float);
        let str_ty = table.add_ty(Ty::Str);
        let range = table.add_ty(Ty::Range);
        let struct_ty = table.add_ty(Ty::Named(TypeId::new(0), Vec::new()));
        let list_int = table.add_ty(Ty::List(int));
        let list_struct = table.add_ty(Ty::List(struct_ty));
        let map_str_int = table.add_ty(Ty::Map(str_ty, int));
        let param = table.add_ty(Ty::Param(0));
        let never = table.add_ty(Ty::Never);
        (
            table,
            TestTypes {
                int,
                float,
                str_ty,
                range,
                list_int,
                list_struct,
                map_str_int,
                struct_ty,
                param,
                never,
            },
        )
    }

    fn test_span() -> martite_base::Span {
        martite_base::Span::empty(0)
    }

    /// A `main` that initializes slot 0 from a constant, reads
    /// `slot[index]` and optionally `.field`, and returns the leaf.
    fn chain_read_program(
        constant: ConstValue,
        index: i64,
        field: Option<u32>,
        typed: bool,
    ) -> IrProgram {
        let (types, tys) = if typed {
            let (table, tys) = test_types();
            (table, tys)
        } else {
            let zero = TyId::new(0);
            (
                TypeTable::default(),
                TestTypes {
                    int: zero,
                    float: zero,
                    str_ty: zero,
                    range: zero,
                    list_int: zero,
                    list_struct: zero,
                    map_str_int: zero,
                    struct_ty: zero,
                    param: zero,
                    never: zero,
                },
            )
        };
        let list_ty = if field.is_some() {
            tys.list_struct
        } else {
            tys.list_int
        };
        let mut nodes = Vec::new();
        let base = IrId::new(nodes.len() as u32);
        nodes.push(expression_with_ty(
            NodeKind::ConstRef(ConstId::new(0)),
            list_ty,
        ));
        let init = IrId::new(nodes.len() as u32);
        nodes.push(zero_with_ty(
            NodeKind::SlotInit {
                slot: Slot::new(0),
                value: base,
            },
            tys.int,
        ));
        let index_id = IrId::new(nodes.len() as u32);
        nodes.push(expression_with_ty(NodeKind::Int(ConstId::new(1)), tys.int));
        let root = IrId::new(nodes.len() as u32);
        nodes.push(expression_with_ty(
            NodeKind::SlotRead {
                slot: Slot::new(0),
                mode: AccessMode::Share,
            },
            list_ty,
        ));
        let mut current = IrId::new(nodes.len() as u32);
        let index_ty = if field.is_some() {
            tys.struct_ty
        } else {
            tys.int
        };
        nodes.push(expression_with_ty(
            NodeKind::Index {
                base: root,
                index: index_id,
            },
            index_ty,
        ));
        if let Some(field) = field {
            let field_id = IrId::new(nodes.len() as u32);
            nodes.push(expression_with_ty(
                NodeKind::Field {
                    base: current,
                    index: field,
                },
                tys.int,
            ));
            current = field_id;
        }
        let init_stmt = IrId::new(nodes.len() as u32);
        nodes.push(zero_with_ty(NodeKind::ExprStmt(init), tys.int));
        let value_stmt = IrId::new(nodes.len() as u32);
        nodes.push(zero_with_ty(NodeKind::ExprStmt(current), tys.int));
        let block = IrId::new(nodes.len() as u32);
        nodes.push(zero_with_ty(
            NodeKind::Block(ChildRange::new(0, 2)),
            tys.int,
        ));
        let mut function = finish(empty_function(1), nodes, vec![init_stmt, value_stmt]);
        function.body = Some(block);
        IrProgram {
            fns: vec![function],
            consts: vec![constant, ConstValue::Int(index)],
            types,
            entries: Entries {
                main: Some(FnId::new(0)),
                ..Entries::default()
            },
            ..IrProgram::default()
        }
    }

    fn expression_with_ty(kind: NodeKind, ty: TyId) -> Node {
        Node::new(kind, ty, Charge::expression(), Effects::NONE)
    }

    fn zero_with_ty(kind: NodeKind, ty: TyId) -> Node {
        Node::new(kind, ty, Charge::ZERO, Effects::NONE)
    }

    fn run_program(program: &IrProgram) -> (Value, u64) {
        let mut runtime = IrRuntime::new(program, ExecLimits::default());
        let result = runtime.run_main().expect("program runs");
        (result.value, runtime.fuel_used())
    }

    #[test]
    fn index_kinds_follow_the_ir_node_types() {
        let (types, tys) = test_types();
        let function = empty_function(0);
        let unit = |ty: TyId| Node::new(NodeKind::Unit, ty, Charge::expression(), Effects::NONE);
        let index = |base: IrId, index: IrId, ty: TyId| {
            expression_with_ty(NodeKind::Index { base, index }, ty)
        };
        let nodes = vec![
            unit(tys.list_int),                              // 0
            unit(tys.int),                                   // 1
            unit(tys.range),                                 // 2
            unit(tys.str_ty),                                // 3
            unit(tys.map_str_int),                           // 4
            unit(tys.param),                                 // 5
            unit(tys.never),                                 // 6
            index(IrId::new(0), IrId::new(1), tys.int),      // 7 ListIndex
            index(IrId::new(0), IrId::new(2), tys.list_int), // 8 ListSlice
            index(IrId::new(3), IrId::new(2), tys.str_ty),   // 9 StrSlice
            index(IrId::new(4), IrId::new(3), tys.int),      // 10 MapIndex
            index(IrId::new(5), IrId::new(1), tys.int),      // 11 General (param base)
            index(IrId::new(0), IrId::new(5), tys.int),      // 12 General (param index)
            index(IrId::new(6), IrId::new(1), tys.int),      // 13 General (never base)
        ];
        let mut function = finish(function, nodes, Vec::new());
        function.body = Some(IrId::new(7));
        let program = IrProgram {
            fns: vec![function],
            types,
            ..IrProgram::default()
        };
        let mut runtime = IrRuntime::new(&program, ExecLimits::default());
        runtime.current = program.function(FnId::new(0));
        assert_eq!(runtime.index_kind(IrId::new(7)), IndexKind::ListIndex);
        assert_eq!(runtime.index_kind(IrId::new(8)), IndexKind::ListSlice);
        assert_eq!(runtime.index_kind(IrId::new(9)), IndexKind::StrSlice);
        assert_eq!(runtime.index_kind(IrId::new(10)), IndexKind::MapIndex);
        assert_eq!(runtime.index_kind(IrId::new(11)), IndexKind::General);
        assert_eq!(runtime.index_kind(IrId::new(12)), IndexKind::General);
        assert_eq!(runtime.index_kind(IrId::new(13)), IndexKind::General);
        assert_eq!(runtime.index_kind(IrId::new(0)), IndexKind::General);
    }

    #[test]
    fn borrowed_index_chain_clones_only_the_leaf() {
        let (types, tys) = test_types();
        let function = empty_function(1);
        let nodes = vec![
            expression_with_ty(
                NodeKind::SlotRead {
                    slot: Slot::new(0),
                    mode: AccessMode::Share,
                },
                tys.list_int,
            ),
            expression_with_ty(NodeKind::Int(ConstId::new(0)), tys.int),
            expression_with_ty(
                NodeKind::Index {
                    base: IrId::new(0),
                    index: IrId::new(1),
                },
                tys.int,
            ),
        ];
        let function = finish(function, nodes, Vec::new());
        let program = IrProgram {
            fns: vec![function],
            consts: vec![ConstValue::Int(1)],
            types,
            ..IrProgram::default()
        };
        let mut runtime = IrRuntime::new(&program, ExecLimits::default());
        runtime.current = program.function(FnId::new(0));
        let value = Value::list(vec![Value::Int(0), Value::Int(1), Value::Int(2)]);
        runtime.frames.push(Frame {
            slots: SmallVec::from_elem(Some(value), 1),
            captures: None,
            source: martite_base::SourceId(0),
            name: "test",
        });

        let read = runtime
            .eval_place_read(IrId::new(2))
            .expect("read runs")
            .expect("the chain is stable");
        assert_eq!(read, Value::Int(1));
        assert_eq!(runtime.fuel_used(), 2, "one charge per replaced node");
        let Value::List(items) = runtime.frames[0].slots[0].as_ref().unwrap() else {
            panic!("frame holds the list");
        };
        assert_eq!(
            Rc::strong_count(items),
            1,
            "the read must not retain or leave a clone of the list"
        );
    }

    #[test]
    fn nested_field_and_index_reads_return_the_leaf() {
        let constant = ConstValue::List(Rc::from(vec![
            ConstValue::Struct(
                TypeId::new(0),
                Rc::from(vec![ConstValue::Int(10), ConstValue::Int(11)]),
            ),
            ConstValue::Struct(
                TypeId::new(0),
                Rc::from(vec![ConstValue::Int(20), ConstValue::Int(21)]),
            ),
        ]));
        let typed = chain_read_program(constant.clone(), 1, Some(1), true);
        let (value, fuel) = run_program(&typed);
        assert_eq!(value, Value::Int(21));

        let general = chain_read_program(constant, 1, Some(1), false);
        let (fallback_value, fallback_fuel) = run_program(&general);
        assert_eq!(fallback_value, value);
        assert_eq!(
            fuel, fallback_fuel,
            "the borrowed chain must charge exactly the nodes it replaces"
        );
    }

    #[test]
    fn a_borrowed_read_traps_with_the_general_fuel_and_message() {
        let constant = ConstValue::List(Rc::from(vec![ConstValue::Int(1), ConstValue::Int(2)]));
        let run = |program: &IrProgram| {
            let mut runtime = IrRuntime::new(program, ExecLimits::default());
            let error = runtime.run_main().expect_err("index is out of bounds");
            (error, runtime.fuel_used())
        };
        let (typed_error, typed_fuel) = run(&chain_read_program(constant.clone(), 5, None, true));
        let (general_error, general_fuel) = run(&chain_read_program(constant, 5, None, false));
        let Trap::Panic { message, .. } = &typed_error else {
            panic!("expected a panic trap");
        };
        assert_eq!(message, "index out of bounds");
        let Trap::Panic {
            message: general_message,
            span: general_span,
            ..
        } = general_error
        else {
            panic!("expected a panic trap");
        };
        assert_eq!(general_message, "index out of bounds");
        assert_eq!(general_span, Some(test_span()));
        assert_eq!(typed_fuel, general_fuel);
    }

    #[test]
    fn borrowed_tuple_fields_walk_by_reference() {
        let function = empty_function(1);
        let nodes = vec![
            expression_with_ty(
                NodeKind::SlotRead {
                    slot: Slot::new(0),
                    mode: AccessMode::Share,
                },
                TyId::new(0),
            ),
            expression_with_ty(
                NodeKind::TupleField {
                    base: IrId::new(0),
                    index: 1,
                },
                TyId::new(0),
            ),
            expression_with_ty(
                NodeKind::TupleField {
                    base: IrId::new(1),
                    index: 0,
                },
                TyId::new(0),
            ),
        ];
        let function = finish(function, nodes, Vec::new());
        let program = IrProgram {
            fns: vec![function],
            ..IrProgram::default()
        };
        let mut runtime = IrRuntime::new(&program, ExecLimits::default());
        runtime.current = program.function(FnId::new(0));
        let pair = Value::tuple(vec![
            Value::Int(1),
            Value::tuple(vec![Value::Int(2), Value::Int(3)]),
        ]);
        runtime.frames.push(Frame {
            slots: SmallVec::from_elem(Some(pair), 1),
            captures: None,
            source: martite_base::SourceId(0),
            name: "test",
        });
        let read = runtime
            .eval_place_read(IrId::new(2))
            .expect("read runs")
            .expect("the tuple chain is stable");
        assert_eq!(read, Value::Int(2));
    }

    #[test]
    fn calling_or_writing_index_expressions_take_the_general_path() {
        for effects in [Effects::CALLS, Effects::WRITES_LOCAL, Effects::HOST] {
            let (types, tys) = test_types();
            let function = empty_function(1);
            let nodes = vec![
                expression_with_ty(
                    NodeKind::SlotRead {
                        slot: Slot::new(0),
                        mode: AccessMode::Share,
                    },
                    tys.list_int,
                ),
                Node::new(
                    NodeKind::Int(ConstId::new(0)),
                    tys.int,
                    Charge::expression(),
                    effects,
                ),
                expression_with_ty(
                    NodeKind::Index {
                        base: IrId::new(0),
                        index: IrId::new(1),
                    },
                    tys.int,
                ),
            ];
            let function = finish(function, nodes, Vec::new());
            let program = IrProgram {
                fns: vec![function],
                consts: vec![ConstValue::Int(0)],
                types,
                ..IrProgram::default()
            };
            let mut runtime = IrRuntime::new(&program, ExecLimits::default());
            runtime.current = program.function(FnId::new(0));
            runtime.frames.push(Frame {
                slots: SmallVec::from_elem(Some(Value::list(vec![Value::Int(1)])), 1),
                captures: None,
                source: martite_base::SourceId(0),
                name: "test",
            });
            assert_eq!(
                runtime.eval_place_read(IrId::new(2)).expect("read runs"),
                None,
                "{effects:?} must disable the borrowed read"
            );
            assert_eq!(runtime.fuel_used(), 0, "fallback must not pre-charge");
        }
    }

    #[test]
    fn read_place_borrows_fields_and_lists_but_keeps_map_lookup() {
        let (types, _) = test_types();
        let function = finish(empty_function(1), Vec::new(), Vec::new());
        let program = IrProgram {
            fns: vec![function],
            types,
            ..IrProgram::default()
        };
        let mut runtime = IrRuntime::new(&program, ExecLimits::default());
        runtime.current = program.function(FnId::new(0));
        let item = Value::Struct(Rc::new(StructValue::new(
            TypeId::new(0),
            vec![Value::Int(7)],
        )));
        runtime.frames.push(Frame {
            slots: SmallVec::from_elem(Some(Value::list(vec![item])), 1),
            captures: None,
            source: martite_base::SourceId(0),
            name: "test",
        });
        let place = Place {
            root: Slot::new(0),
            accessors: SmallVec::from_vec(vec![Accessor::Index(Value::Int(0)), Accessor::Field(0)]),
        };
        assert_eq!(
            runtime.read_place(&place).expect("list place"),
            Value::Int(7)
        );
        assert_eq!(runtime.fuel_used(), 0, "list access needs no charge");

        let key = Value::str("a".repeat(64));
        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("a".repeat(64)), Value::Int(9));
        runtime.frames[0].slots[0] = Some(Value::map(map));
        let place = Place {
            root: Slot::new(0),
            accessors: SmallVec::from_vec(vec![Accessor::Index(key.clone())]),
        };
        assert_eq!(
            runtime.read_place(&place).expect("map place"),
            Value::Int(9)
        );
        assert_eq!(runtime.fuel_used(), map_key_cost(&key));

        let missing = Place {
            root: Slot::new(0),
            accessors: SmallVec::from_vec(vec![Accessor::Index(Value::str("missing"))]),
        };
        let error = runtime.read_place(&missing).expect_err("missing key traps");
        let Trap::Panic { message, .. } = trap(error) else {
            panic!("expected a panic trap");
        };
        assert_eq!(message, "missing map key");
    }

    #[test]
    fn specialized_index_kinds_preserve_results_traps_and_fuel() {
        let program = IrProgram::default();
        let span = test_span();
        let list = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);

        let mut runtime = IrRuntime::new(&program, ExecLimits::default());
        assert_eq!(
            runtime
                .index_value_kind(IndexKind::ListIndex, list.clone(), Value::Int(1), span)
                .expect("list index"),
            Value::Int(2)
        );
        assert_eq!(
            runtime
                .index_value_kind(
                    IndexKind::ListSlice,
                    list.clone(),
                    Value::Range {
                        start: 1,
                        end: 3,
                        inclusive: false,
                    },
                    span,
                )
                .expect("list slice"),
            Value::list(vec![Value::Int(2), Value::Int(3)])
        );
        let error = runtime
            .index_value_kind(IndexKind::ListIndex, list.clone(), Value::Int(3), span)
            .expect_err("out of bounds");
        let Trap::Panic {
            message,
            span: Some(found),
            ..
        } = trap(error)
        else {
            panic!("expected an index panic");
        };
        assert_eq!(message, "index out of bounds");
        assert_eq!(found, span);

        assert_eq!(
            runtime
                .index_value_kind(
                    IndexKind::StrSlice,
                    Value::str("héllo"),
                    Value::Range {
                        start: 1,
                        end: 3,
                        inclusive: false,
                    },
                    span,
                )
                .expect("string slice"),
            Value::str("él")
        );

        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("k".into()), Value::Int(9));
        let map = Value::map(map);
        assert_eq!(
            runtime
                .index_value_kind(IndexKind::MapIndex, map.clone(), Value::str("k"), span)
                .expect("map index"),
            Value::Int(9)
        );
        let error = runtime
            .index_value_kind(IndexKind::MapIndex, map, Value::str("missing"), span)
            .expect_err("missing key");
        let Trap::Panic { message, .. } = trap(error) else {
            panic!("expected a map panic");
        };
        assert_eq!(message, "missing map key");
    }

    #[test]
    fn index_kind_dispatch_does_not_change_fuel() {
        let program = IrProgram::default();
        let span = test_span();
        let list = Value::list(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let range = Value::Range {
            start: 0,
            end: 2,
            inclusive: false,
        };

        let mut general = IrRuntime::new(&program, ExecLimits::default());
        general
            .index_value(list.clone(), range.clone(), span)
            .expect("general slice");
        let mut specialized = IrRuntime::new(&program, ExecLimits::default());
        specialized
            .index_value_kind(IndexKind::ListSlice, list, range, span)
            .expect("specialized slice");
        assert_eq!(general.fuel_used(), specialized.fuel_used());

        let key = Value::str("k".repeat(64));
        let mut map = OrderedMap::new();
        map.insert(KeyValue::Str("k".repeat(64)), Value::Int(1));
        let map = Value::map(map);
        let mut general = IrRuntime::new(&program, ExecLimits::default());
        general
            .index_value(map.clone(), key.clone(), span)
            .expect("general map index");
        let mut specialized = IrRuntime::new(&program, ExecLimits::default());
        specialized
            .index_value_kind(IndexKind::MapIndex, map, key, span)
            .expect("specialized map index");
        assert_eq!(general.fuel_used(), specialized.fuel_used());
    }

    // ------------------------------------------------------------------
    // Direct slot writes (#87) and unchanged metadata (#80)
    // ------------------------------------------------------------------

    fn assignment_program(compound: bool, initial: i64, operand: i64) -> IrProgram {
        let kind = if compound {
            NodeKind::AddAssign {
                place: IrId::new(2),
                value: IrId::new(3),
            }
        } else {
            NodeKind::Assign {
                place: IrId::new(2),
                value: IrId::new(3),
            }
        };
        let nodes = vec![
            expression(NodeKind::Int(ConstId::new(0))),
            zero(NodeKind::SlotInit {
                slot: Slot::new(0),
                value: IrId::new(0),
            }),
            zero(NodeKind::SlotPlace(Slot::new(0))),
            expression(NodeKind::Int(ConstId::new(1))),
            expression(kind),
            zero(NodeKind::ExprStmt(IrId::new(4))),
            expression(NodeKind::SlotRead {
                slot: Slot::new(0),
                mode: AccessMode::Share,
            }),
            zero(NodeKind::Block(ChildRange::new(0, 3))),
        ];
        let mut function = finish(
            empty_function(1),
            nodes,
            vec![IrId::new(1), IrId::new(5), IrId::new(6)],
        );
        function.body = Some(IrId::new(7));
        IrProgram {
            fns: vec![function],
            consts: vec![ConstValue::Int(initial), ConstValue::Int(operand)],
            entries: Entries {
                main: Some(FnId::new(0)),
                ..Entries::default()
            },
            ..IrProgram::default()
        }
    }

    // ------------------------------------------------------------------
    // Binary operation specialization (#71)
    // ------------------------------------------------------------------

    /// A `main` that evaluates one binary node over two constants.
    fn binary_program(
        kind: NodeKind,
        result_ty: TyId,
        lhs: ConstValue,
        rhs: ConstValue,
        typed: bool,
    ) -> IrProgram {
        let (types, operand_ty) = if typed {
            let (table, tys) = test_types();
            (table, tys.int)
        } else {
            (TypeTable::default(), TyId::new(0))
        };
        let nodes = vec![
            expression_with_ty(NodeKind::ConstRef(ConstId::new(0)), operand_ty),
            expression_with_ty(NodeKind::ConstRef(ConstId::new(1)), operand_ty),
            expression_with_ty(kind, result_ty),
        ];
        let mut function = finish(empty_function(0), nodes, Vec::new());
        function.body = Some(IrId::new(2));
        IrProgram {
            fns: vec![function],
            consts: vec![lhs, rhs],
            types,
            entries: Entries {
                main: Some(FnId::new(0)),
                ..Entries::default()
            },
            ..IrProgram::default()
        }
    }

    #[test]
    fn simple_and_compound_slot_assignments_update_the_slot_without_extra_fuel() {
        let (value, simple_fuel) = run_program(&assignment_program(false, 1, 2));
        assert_eq!(value, Value::Int(2));
        assert_eq!(simple_fuel, 4, "only charged expression nodes add fuel");

        let (value, compound_fuel) = run_program(&assignment_program(true, 1, 2));
        assert_eq!(value, Value::Int(3));
        assert_eq!(
            compound_fuel, simple_fuel,
            "a compound slot write charges no extra fuel"
        );
    }

    #[test]
    fn empty_accessor_writes_trap_without_removing_the_root() {
        let program = IrProgram::default();
        let mut runtime = IrRuntime::new(&program, ExecLimits::default());
        runtime.frames.push(Frame {
            slots: SmallVec::from_elem(Some(Value::Int(1)), 1),
            captures: None,
            source: martite_base::SourceId(0),
            name: "test",
        });
        let place = || Place {
            root: Slot::new(0),
            accessors: SmallVec::new(),
        };

        runtime
            .write_place(place(), Value::Int(2))
            .expect("simple write");
        assert_eq!(
            runtime.frames.last().expect("frame").slots[0],
            Some(Value::Int(2))
        );
        assert_eq!(runtime.fuel_used(), 0, "the write charges no fuel");

        runtime.frames.last_mut().expect("frame").slots[0] = None;
        let error = runtime
            .write_place(place(), Value::Int(3))
            .expect_err("the missing root traps");
        let Trap::Panic { message, .. } = trap(error) else {
            panic!("expected a panic trap");
        };
        assert_eq!(message, "unbound place root");

        runtime.frames.last_mut().expect("frame").slots[0] = Some(Value::Int(1));
        let mut deep = Value::Unit;
        for _ in 0..MAX_VALUE_DEPTH {
            deep = Value::list(vec![deep]);
        }
        let error = runtime
            .write_place(place(), deep)
            .expect_err("the depth cap traps");
        assert!(matches!(trap(error), Trap::DepthExceeded));
        assert_eq!(
            runtime.frames.last().expect("frame").slots[0],
            Some(Value::Int(1)),
            "the depth check runs before the slot is overwritten"
        );
        assert_eq!(runtime.fuel_used(), 0, "a failed write charges no fuel");
    }

    #[test]
    fn binary_dispatches_follow_the_ir_node_types() {
        let (types, tys) = test_types();
        let function = empty_function(0);
        let unit = |ty: TyId| Node::new(NodeKind::Unit, ty, Charge::expression(), Effects::NONE);
        let binary = |kind: NodeKind, ty: TyId| expression_with_ty(kind, ty);
        let nodes = vec![
            unit(tys.int),       // 0
            unit(tys.float),     // 1
            unit(tys.str_ty),    // 2
            unit(tys.param),     // 3
            unit(tys.never),     // 4
            unit(tys.struct_ty), // 5
            unit(TyId::new(99)), // 6 missing type entry
            binary(
                NodeKind::Add {
                    lhs: IrId::new(0),
                    rhs: IrId::new(0),
                },
                tys.int,
            ), // 7 IntAdd
            binary(
                NodeKind::Sub {
                    lhs: IrId::new(0),
                    rhs: IrId::new(0),
                },
                tys.int,
            ), // 8 IntSub
            binary(
                NodeKind::Mul {
                    lhs: IrId::new(0),
                    rhs: IrId::new(0),
                },
                tys.int,
            ), // 9 IntMul
            binary(
                NodeKind::Div {
                    lhs: IrId::new(0),
                    rhs: IrId::new(0),
                },
                tys.int,
            ), // 10 IntDiv
            binary(
                NodeKind::Rem {
                    lhs: IrId::new(0),
                    rhs: IrId::new(0),
                },
                tys.int,
            ), // 11 IntRem
            binary(
                NodeKind::Add {
                    lhs: IrId::new(1),
                    rhs: IrId::new(1),
                },
                tys.float,
            ), // 12 FloatAdd
            binary(
                NodeKind::Sub {
                    lhs: IrId::new(1),
                    rhs: IrId::new(1),
                },
                tys.float,
            ), // 13 FloatSub
            binary(
                NodeKind::Mul {
                    lhs: IrId::new(1),
                    rhs: IrId::new(1),
                },
                tys.float,
            ), // 14 FloatMul
            binary(
                NodeKind::Div {
                    lhs: IrId::new(1),
                    rhs: IrId::new(1),
                },
                tys.float,
            ), // 15 FloatDiv
            binary(
                NodeKind::Rem {
                    lhs: IrId::new(1),
                    rhs: IrId::new(1),
                },
                tys.float,
            ), // 16 FloatRem
            binary(
                NodeKind::Add {
                    lhs: IrId::new(2),
                    rhs: IrId::new(2),
                },
                tys.str_ty,
            ), // 17 StrConcat
            binary(
                NodeKind::Eq {
                    lhs: IrId::new(0),
                    rhs: IrId::new(0),
                },
                tys.int,
            ), // 18 IntCompare(Eq)
            binary(
                NodeKind::Lt {
                    lhs: IrId::new(0),
                    rhs: IrId::new(0),
                },
                tys.int,
            ), // 19 IntCompare(Lt)
            binary(
                NodeKind::Ge {
                    lhs: IrId::new(1),
                    rhs: IrId::new(1),
                },
                tys.float,
            ), // 20 FloatCompare(Ge)
            binary(
                NodeKind::Eq {
                    lhs: IrId::new(2),
                    rhs: IrId::new(2),
                },
                tys.int,
            ), // 21 StrCompare(Eq)
            binary(
                NodeKind::Le {
                    lhs: IrId::new(2),
                    rhs: IrId::new(2),
                },
                tys.int,
            ), // 22 StrCompare(Le)
            binary(
                NodeKind::Add {
                    lhs: IrId::new(0),
                    rhs: IrId::new(1),
                },
                tys.float,
            ), // 23 General(Add), int/float mismatch
            binary(
                NodeKind::Add {
                    lhs: IrId::new(3),
                    rhs: IrId::new(0),
                },
                tys.int,
            ), // 24 General(Add), param lhs
            binary(
                NodeKind::Add {
                    lhs: IrId::new(4),
                    rhs: IrId::new(0),
                },
                tys.int,
            ), // 25 General(Add), never lhs
            binary(
                NodeKind::Add {
                    lhs: IrId::new(5),
                    rhs: IrId::new(0),
                },
                tys.int,
            ), // 26 General(Add), named lhs
            binary(
                NodeKind::Add {
                    lhs: IrId::new(6),
                    rhs: IrId::new(0),
                },
                tys.int,
            ), // 27 General(Add), missing lhs type
            binary(
                NodeKind::Add {
                    lhs: IrId::new(0),
                    rhs: IrId::new(99),
                },
                tys.int,
            ), // 28 General(Add), missing rhs node
            binary(
                NodeKind::Lt {
                    lhs: IrId::new(0),
                    rhs: IrId::new(1),
                },
                tys.int,
            ), // 29 General(Lt), int/float mismatch
        ];
        let mut function = finish(function, nodes, Vec::new());
        function.body = Some(IrId::new(7));
        let program = IrProgram {
            fns: vec![function],
            types,
            ..IrProgram::default()
        };
        let mut runtime = IrRuntime::new(&program, ExecLimits::default());
        runtime.current = program.function(FnId::new(0));
        let dispatch = |id: u32, op: BinaryKind| runtime.binary_dispatch(IrId::new(id), op);
        assert_eq!(dispatch(7, BinaryKind::Add), BinaryDispatch::IntAdd);
        assert_eq!(dispatch(8, BinaryKind::Sub), BinaryDispatch::IntSub);
        assert_eq!(dispatch(9, BinaryKind::Mul), BinaryDispatch::IntMul);
        assert_eq!(dispatch(10, BinaryKind::Div), BinaryDispatch::IntDiv);
        assert_eq!(dispatch(11, BinaryKind::Rem), BinaryDispatch::IntRem);
        assert_eq!(dispatch(12, BinaryKind::Add), BinaryDispatch::FloatAdd);
        assert_eq!(dispatch(13, BinaryKind::Sub), BinaryDispatch::FloatSub);
        assert_eq!(dispatch(14, BinaryKind::Mul), BinaryDispatch::FloatMul);
        assert_eq!(dispatch(15, BinaryKind::Div), BinaryDispatch::FloatDiv);
        assert_eq!(dispatch(16, BinaryKind::Rem), BinaryDispatch::FloatRem);
        assert_eq!(dispatch(17, BinaryKind::Add), BinaryDispatch::StrConcat);
        assert_eq!(
            dispatch(18, BinaryKind::Eq),
            BinaryDispatch::IntCompare(BinaryKind::Eq)
        );
        assert_eq!(
            dispatch(19, BinaryKind::Lt),
            BinaryDispatch::IntCompare(BinaryKind::Lt)
        );
        assert_eq!(
            dispatch(20, BinaryKind::Ge),
            BinaryDispatch::FloatCompare(BinaryKind::Ge)
        );
        assert_eq!(
            dispatch(21, BinaryKind::Eq),
            BinaryDispatch::StrCompare(BinaryKind::Eq)
        );
        assert_eq!(
            dispatch(22, BinaryKind::Le),
            BinaryDispatch::StrCompare(BinaryKind::Le)
        );
        for id in 23..=28 {
            assert_eq!(
                dispatch(id, BinaryKind::Add),
                BinaryDispatch::General(BinaryKind::Add),
                "node {id} must stay general"
            );
        }
        assert_eq!(
            dispatch(29, BinaryKind::Lt),
            BinaryDispatch::General(BinaryKind::Lt),
            "an int/float comparison mismatch stays general"
        );
        assert_eq!(
            dispatch(0, BinaryKind::Add),
            BinaryDispatch::General(BinaryKind::Add),
            "non-binary nodes have no table entry"
        );
    }

    /// Run one specialized or general binary operation and report its result,
    /// consumed fuel and charge count.
    fn run_binary(
        dispatch: Option<BinaryDispatch>,
        op: BinaryKind,
        lhs: &Value,
        rhs: &Value,
        span: martite_base::Span,
        fuel: u64,
    ) -> (Result<Value, String>, u64, u64) {
        let program = IrProgram::default();
        let mut runtime = IrRuntime::new(
            &program,
            ExecLimits {
                fuel,
                ..ExecLimits::default()
            },
        );
        let result = match dispatch {
            Some(dispatch) => runtime.binary_values_kind(dispatch, lhs.clone(), rhs.clone(), span),
            None => runtime.binary_values(op, lhs.clone(), rhs.clone(), span),
        };
        let outcome = match result {
            Ok(value) => Ok(value),
            Err(super::EvalError::Trap(trap)) => Err(trap.panic_message()),
            Err(_) => Err("unexpected control flow".into()),
        };
        (outcome, runtime.fuel_used(), runtime.ticks)
    }

    #[test]
    fn specialized_binary_dispatch_keeps_values_and_traps() {
        let program = IrProgram::default();
        let span = test_span();
        let mut runtime = IrRuntime::new(&program, ExecLimits::default());
        let cases = [
            (
                BinaryDispatch::IntAdd,
                Value::Int(40),
                Value::Int(2),
                Value::Int(42),
            ),
            (
                BinaryDispatch::IntSub,
                Value::Int(40),
                Value::Int(2),
                Value::Int(38),
            ),
            (
                BinaryDispatch::IntMul,
                Value::Int(6),
                Value::Int(7),
                Value::Int(42),
            ),
            (
                BinaryDispatch::IntDiv,
                Value::Int(7),
                Value::Int(2),
                Value::Int(3),
            ),
            (
                BinaryDispatch::IntRem,
                Value::Int(7),
                Value::Int(2),
                Value::Int(1),
            ),
            (
                BinaryDispatch::FloatAdd,
                Value::Float(1.5),
                Value::Float(2.25),
                Value::Float(3.75),
            ),
            (
                BinaryDispatch::FloatSub,
                Value::Float(1.5),
                Value::Float(2.25),
                Value::Float(-0.75),
            ),
            (
                BinaryDispatch::FloatMul,
                Value::Float(1.5),
                Value::Float(2.0),
                Value::Float(3.0),
            ),
            (
                BinaryDispatch::FloatDiv,
                Value::Float(1.0),
                Value::Float(0.0),
                Value::Float(f64::INFINITY),
            ),
            (
                BinaryDispatch::FloatRem,
                Value::Float(5.5),
                Value::Float(2.0),
                Value::Float(1.5),
            ),
            (
                BinaryDispatch::StrConcat,
                Value::str("ab"),
                Value::str("cd"),
                Value::str("abcd"),
            ),
            (
                BinaryDispatch::IntCompare(BinaryKind::Eq),
                Value::Int(3),
                Value::Int(3),
                Value::Bool(true),
            ),
            (
                BinaryDispatch::IntCompare(BinaryKind::Lt),
                Value::Int(3),
                Value::Int(4),
                Value::Bool(true),
            ),
            (
                BinaryDispatch::FloatCompare(BinaryKind::Lt),
                Value::Float(1.0),
                Value::Float(2.0),
                Value::Bool(true),
            ),
            (
                BinaryDispatch::FloatCompare(BinaryKind::Ge),
                Value::Float(2.0),
                Value::Float(2.0),
                Value::Bool(true),
            ),
            (
                BinaryDispatch::StrCompare(BinaryKind::Lt),
                Value::str("a"),
                Value::str("b"),
                Value::Bool(true),
            ),
            (
                BinaryDispatch::StrCompare(BinaryKind::Ne),
                Value::str("a"),
                Value::str("a"),
                Value::Bool(false),
            ),
        ];
        for (dispatch, lhs, rhs, expected) in cases {
            assert_eq!(
                runtime
                    .binary_values_kind(dispatch, lhs, rhs, span)
                    .expect("specialized operation runs"),
                expected,
                "{dispatch:?}"
            );
        }

        // NaN keeps the historical behavior: equality is false, inequality is
        // true, and every ordered comparison is false.
        let nan = Value::Float(f64::NAN);
        for (op, expected) in [
            (BinaryKind::Eq, false),
            (BinaryKind::Ne, true),
            (BinaryKind::Lt, false),
            (BinaryKind::Le, false),
            (BinaryKind::Gt, false),
            (BinaryKind::Ge, false),
        ] {
            assert_eq!(
                runtime
                    .binary_values_kind(
                        BinaryDispatch::FloatCompare(op),
                        nan.clone(),
                        Value::Float(1.0),
                        span,
                    )
                    .expect("NaN comparison runs"),
                Value::Bool(expected),
                "{op:?}"
            );
        }

        let division_by_zero = runtime
            .binary_values_kind(BinaryDispatch::IntDiv, Value::Int(1), Value::Int(0), span)
            .expect_err("division by zero traps");
        let Trap::Panic {
            message,
            span: Some(found),
            ..
        } = trap(division_by_zero)
        else {
            panic!("expected a division-by-zero panic");
        };
        assert_eq!(message, "division by zero");
        assert_eq!(found, span);

        for (dispatch, lhs, rhs) in [
            (BinaryDispatch::IntAdd, Value::Int(i64::MAX), Value::Int(1)),
            (BinaryDispatch::IntSub, Value::Int(i64::MIN), Value::Int(1)),
            (BinaryDispatch::IntMul, Value::Int(i64::MAX), Value::Int(2)),
            (BinaryDispatch::IntDiv, Value::Int(i64::MIN), Value::Int(-1)),
            (BinaryDispatch::IntRem, Value::Int(i64::MIN), Value::Int(-1)),
        ] {
            let error = runtime
                .binary_values_kind(dispatch, lhs, rhs, span)
                .expect_err("integer overflow traps");
            let Trap::Panic {
                message,
                span: Some(found),
                ..
            } = trap(error)
            else {
                panic!("expected an integer-overflow panic");
            };
            assert_eq!(message, "integer overflow", "{dispatch:?}");
            assert_eq!(found, span);
        }

        // Unexpected value shapes fall back to the general path instead of
        // trapping differently from unspecialized IR.
        assert_eq!(
            runtime
                .binary_values_kind(
                    BinaryDispatch::IntAdd,
                    Value::Float(1.5),
                    Value::Float(2.25),
                    span,
                )
                .expect("mismatched int arm falls back to general add"),
            Value::Float(3.75)
        );
        assert_eq!(
            runtime
                .binary_values_kind(
                    BinaryDispatch::StrConcat,
                    Value::Int(2),
                    Value::Int(3),
                    span,
                )
                .expect("mismatched concat arm falls back to general add"),
            Value::Int(5)
        );
        assert_eq!(
            runtime
                .binary_values_kind(
                    BinaryDispatch::FloatCompare(BinaryKind::Lt),
                    Value::Int(1),
                    Value::Int(2),
                    span,
                )
                .expect("mismatched float compare falls back to general compare"),
            Value::Bool(true)
        );
        let invalid = runtime
            .binary_values_kind(
                BinaryDispatch::IntAdd,
                Value::Bool(true),
                Value::Bool(false),
                span,
            )
            .expect_err("bools are not arithmetic operands");
        let Trap::Panic { message, .. } = trap(invalid) else {
            panic!("expected an invalid-operand panic");
        };
        assert_eq!(message, "invalid arithmetic operands");
    }

    #[test]
    fn specialized_binary_fuel_and_ticks_match_the_general_path() {
        let span = test_span();
        let nan = Value::Float(f64::NAN);
        let cases = vec![
            (
                BinaryDispatch::IntAdd,
                BinaryKind::Add,
                Value::Int(2),
                Value::Int(3),
            ),
            (
                BinaryDispatch::IntSub,
                BinaryKind::Sub,
                Value::Int(2),
                Value::Int(3),
            ),
            (
                BinaryDispatch::IntMul,
                BinaryKind::Mul,
                Value::Int(6),
                Value::Int(7),
            ),
            (
                BinaryDispatch::IntDiv,
                BinaryKind::Div,
                Value::Int(7),
                Value::Int(2),
            ),
            (
                BinaryDispatch::IntRem,
                BinaryKind::Rem,
                Value::Int(7),
                Value::Int(2),
            ),
            (
                BinaryDispatch::IntAdd,
                BinaryKind::Add,
                Value::Int(i64::MAX),
                Value::Int(1),
            ),
            (
                BinaryDispatch::IntSub,
                BinaryKind::Sub,
                Value::Int(i64::MIN),
                Value::Int(1),
            ),
            (
                BinaryDispatch::IntMul,
                BinaryKind::Mul,
                Value::Int(i64::MAX),
                Value::Int(2),
            ),
            (
                BinaryDispatch::IntDiv,
                BinaryKind::Div,
                Value::Int(1),
                Value::Int(0),
            ),
            (
                BinaryDispatch::IntRem,
                BinaryKind::Rem,
                Value::Int(1),
                Value::Int(0),
            ),
            (
                BinaryDispatch::IntDiv,
                BinaryKind::Div,
                Value::Int(i64::MIN),
                Value::Int(-1),
            ),
            (
                BinaryDispatch::IntRem,
                BinaryKind::Rem,
                Value::Int(i64::MIN),
                Value::Int(-1),
            ),
            (
                BinaryDispatch::FloatAdd,
                BinaryKind::Add,
                Value::Float(1.5),
                Value::Float(2.25),
            ),
            (
                BinaryDispatch::FloatSub,
                BinaryKind::Sub,
                Value::Float(1.5),
                Value::Float(2.25),
            ),
            (
                BinaryDispatch::FloatMul,
                BinaryKind::Mul,
                Value::Float(1.5),
                Value::Float(2.0),
            ),
            (
                BinaryDispatch::FloatDiv,
                BinaryKind::Div,
                Value::Float(1.0),
                Value::Float(0.0),
            ),
            (
                BinaryDispatch::FloatRem,
                BinaryKind::Rem,
                Value::Float(5.5),
                Value::Float(2.0),
            ),
            (
                BinaryDispatch::FloatCompare(BinaryKind::Eq),
                BinaryKind::Eq,
                nan.clone(),
                nan.clone(),
            ),
            (
                BinaryDispatch::FloatCompare(BinaryKind::Ne),
                BinaryKind::Ne,
                nan.clone(),
                nan.clone(),
            ),
            (
                BinaryDispatch::FloatCompare(BinaryKind::Lt),
                BinaryKind::Lt,
                nan.clone(),
                Value::Float(1.0),
            ),
            (
                BinaryDispatch::FloatCompare(BinaryKind::Le),
                BinaryKind::Le,
                Value::Float(1.0),
                nan.clone(),
            ),
            (
                BinaryDispatch::FloatCompare(BinaryKind::Gt),
                BinaryKind::Gt,
                Value::Float(2.0),
                Value::Float(1.0),
            ),
            (
                BinaryDispatch::FloatCompare(BinaryKind::Ge),
                BinaryKind::Ge,
                Value::Float(1.0),
                Value::Float(1.0),
            ),
            (
                BinaryDispatch::IntCompare(BinaryKind::Eq),
                BinaryKind::Eq,
                Value::Int(1),
                Value::Int(1),
            ),
            (
                BinaryDispatch::IntCompare(BinaryKind::Ne),
                BinaryKind::Ne,
                Value::Int(1),
                Value::Int(1),
            ),
            (
                BinaryDispatch::IntCompare(BinaryKind::Lt),
                BinaryKind::Lt,
                Value::Int(1),
                Value::Int(2),
            ),
            (
                BinaryDispatch::IntCompare(BinaryKind::Le),
                BinaryKind::Le,
                Value::Int(2),
                Value::Int(2),
            ),
            (
                BinaryDispatch::IntCompare(BinaryKind::Gt),
                BinaryKind::Gt,
                Value::Int(3),
                Value::Int(2),
            ),
            (
                BinaryDispatch::IntCompare(BinaryKind::Ge),
                BinaryKind::Ge,
                Value::Int(2),
                Value::Int(3),
            ),
            (
                BinaryDispatch::StrConcat,
                BinaryKind::Add,
                Value::str("ab"),
                Value::str("cde"),
            ),
            (
                BinaryDispatch::StrCompare(BinaryKind::Eq),
                BinaryKind::Eq,
                Value::str("a"),
                Value::str("a"),
            ),
            (
                BinaryDispatch::StrCompare(BinaryKind::Ne),
                BinaryKind::Ne,
                Value::str("a"),
                Value::str("b"),
            ),
            (
                BinaryDispatch::StrCompare(BinaryKind::Lt),
                BinaryKind::Lt,
                Value::str("a"),
                Value::str("b"),
            ),
            (
                BinaryDispatch::StrCompare(BinaryKind::Le),
                BinaryKind::Le,
                Value::str("b"),
                Value::str("b"),
            ),
            (
                BinaryDispatch::StrCompare(BinaryKind::Gt),
                BinaryKind::Gt,
                Value::str("c"),
                Value::str("b"),
            ),
            (
                BinaryDispatch::StrCompare(BinaryKind::Ge),
                BinaryKind::Ge,
                Value::str("a"),
                Value::str("b"),
            ),
            // Shape mismatches take the general path from inside the arm.
            (
                BinaryDispatch::IntAdd,
                BinaryKind::Add,
                Value::Float(1.5),
                Value::Float(2.25),
            ),
            (
                BinaryDispatch::IntAdd,
                BinaryKind::Add,
                Value::str("a"),
                Value::str("b"),
            ),
            (
                BinaryDispatch::FloatCompare(BinaryKind::Eq),
                BinaryKind::Eq,
                Value::Int(1),
                Value::Int(1),
            ),
            (
                BinaryDispatch::StrCompare(BinaryKind::Lt),
                BinaryKind::Lt,
                Value::Int(1),
                Value::Int(2),
            ),
        ];
        for (dispatch, op, lhs, rhs) in cases {
            assert_eq!(
                run_binary(Some(dispatch), op, &lhs, &rhs, span, 1_000_000),
                run_binary(None, op, &lhs, &rhs, span, 1_000_000),
                "{dispatch:?} must match the general path"
            );
        }
    }

    #[test]
    fn scalar_comparisons_keep_the_zero_charge() {
        let program = IrProgram::default();
        let span = test_span();
        let limits = ExecLimits {
            fuel: 0,
            ..ExecLimits::default()
        };
        let mut specialized = IrRuntime::new(&program, limits);
        let error = specialized
            .binary_values_kind(
                BinaryDispatch::IntCompare(BinaryKind::Lt),
                Value::Int(1),
                Value::Int(2),
                span,
            )
            .expect_err("even a zero-unit comparison charges");
        assert!(matches!(trap(error), Trap::OutOfFuel));
        let mut general = IrRuntime::new(&program, limits);
        let error = general
            .binary_values(BinaryKind::Lt, Value::Int(1), Value::Int(2), span)
            .expect_err("the general comparison charges the same way");
        assert!(matches!(trap(error), Trap::OutOfFuel));
    }

    #[test]
    fn typed_binary_nodes_match_the_general_path() {
        let kind = NodeKind::Add {
            lhs: IrId::new(0),
            rhs: IrId::new(1),
        };
        let typed = binary_program(
            kind.clone(),
            TyId::new(0),
            ConstValue::Int(20),
            ConstValue::Int(22),
            true,
        );
        let general = binary_program(
            kind,
            TyId::new(0),
            ConstValue::Int(20),
            ConstValue::Int(22),
            false,
        );
        let (typed_value, typed_fuel) = run_program(&typed);
        let (general_value, general_fuel) = run_program(&general);
        assert_eq!(typed_value, Value::Int(42));
        assert_eq!(general_value, typed_value);
        assert_eq!(general_fuel, typed_fuel);

        let mut runtime = IrRuntime::new(&typed, ExecLimits::default());
        runtime.current = typed.function(FnId::new(0));
        assert_eq!(
            runtime.binary_dispatch(IrId::new(2), BinaryKind::Add),
            BinaryDispatch::IntAdd
        );
        let mut runtime = IrRuntime::new(&general, ExecLimits::default());
        runtime.current = general.function(FnId::new(0));
        assert_eq!(
            runtime.binary_dispatch(IrId::new(2), BinaryKind::Add),
            BinaryDispatch::General(BinaryKind::Add)
        );
    }
}
