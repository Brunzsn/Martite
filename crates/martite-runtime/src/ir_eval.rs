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

mod builtins;

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Instant;

use martite_base::{SourceId, Span};
use martite_fuel as fuel;
use martite_ir::{
    AccessMode, BuiltinFnId, BuiltinItemId, CaptureIndex, CaptureSource, ChildRange, ConstId,
    ConstValue, FnId, FnKind, HostId, IrFn, IrId, IrProgram, MatchSetId, Node, NodeKind, Pattern,
    PreludeVariant, Slot, StrId, StructLayoutId, Ty, TypeDefKind, TypeId,
};
use smallvec::SmallVec;

use crate::exec::{CallResult, ExecLimits, HostContext, HostFault, MainResult, Trap};
use crate::ops::{show, value_eq, value_ord, write_show};
use crate::value::{
    Closure, EnumValue, FnValue, MAX_VALUE_DEPTH, MapValue, OrderedMap, SeqValue, StructValue,
    Value, key_from_value, key_value_size, value_depth_exceeds,
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
    Trap(Trap),
    Return(Value),
    Break,
    Continue,
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

enum Accessor {
    Field(usize),
    Tuple(u32),
    Index(Value),
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
    /// Live bytes at the last exact scan.
    census_bytes: u64,
    /// Allocation bytes recorded since the last exact scan.
    pending_bytes: u64,
    /// Immutable values materialized from the const pool, indexed by
    /// [`ConstId`]. Program data is not part of the memory census, but the
    /// first materialization is charged to `pending_bytes`.
    const_values: Vec<Option<Value>>,
    /// Static string values, indexed by [`StrId`]. The payload is shared after
    /// the first use; every use still charges its bytes.
    literal_values: Vec<Option<Value>>,
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
            census_bytes: 0,
            pending_bytes: 0,
            const_values: vec![None; program.consts.len()],
            literal_values: vec![None; program.strs.len()],
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
        self.reset();
        if let Some(state) = state.as_ref() {
            self.note_values(std::slice::from_ref(state));
        }
        self.note_values(&args);
        let program = self.program;
        let info = program
            .function(function)
            .ok_or_else(|| self.internal_panic("missing handler function"))?;
        let has_self = info.params.first().is_some_and(|param| param.is_self);
        let self_mutable = info.params.first().is_some_and(|param| param.mutable);
        let mut call_args = CallArgs::with_capacity(args.len() + usize::from(has_self));
        if has_self {
            call_args.push(state.clone().unwrap_or(Value::Unit));
        }
        call_args.extend(args);
        let (value, mut updated) = self.call_function_with_writebacks(function, call_args)?;
        if has_self && self_mutable {
            *state = take_writeback(&mut updated, 0);
        }
        Ok(Some(value))
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
    }

    // ------------------------------------------------------------------
    // Frames and traps
    // ------------------------------------------------------------------

    #[inline]
    fn current_function(&self) -> &'p IrFn {
        self.current.expect("evaluation frame")
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
        Err(EvalError::Trap(trap))
    }

    fn call_function(&mut self, function: FnId, args: CallArgs) -> Result<Value, Trap> {
        self.call_function_with_writebacks(function, args)
            .map(|(value, _)| value)
    }

    fn call_function_with_writebacks(
        &mut self,
        function: FnId,
        args: CallArgs,
    ) -> Result<(Value, Writebacks), Trap> {
        self.call_function_with_writebacks_and_captures(function, args, None)
    }

    /// Calls a closure value. Lambda parameters cannot be `mut`, so a closure
    /// call never produces writeback values.
    #[inline]
    fn call_closure(&mut self, closure: &Closure, args: CallArgs) -> Result<Value, Trap> {
        let program = self.program;
        let Some(function_info) = program.function(closure.target) else {
            return Err(self.internal_panic(format!("missing function #{:?}", closure.target)));
        };
        self.call_closure_function(function_info, closure.captures.clone(), args)
    }

    /// Call a closure whose target function has already been resolved. The
    /// higher-order builtins use this to hoist the target lookup out of their
    /// per-element loops.
    #[inline]
    fn call_closure_function(
        &mut self,
        function_info: &'p IrFn,
        captures: Option<Rc<[Value]>>,
        args: CallArgs,
    ) -> Result<Value, Trap> {
        if matches!(function_info.kind, FnKind::Lambda) {
            return self.call_lambda(function_info, captures, args);
        }
        self.call_function_with_writebacks_and_captures(function_info.id, args, captures)
            .map(|(value, _)| value)
    }

    /// Specialized lambda call. It skips the mutable-writeback scan and the
    /// non-lambda capture checks that every higher-order builtin would
    /// otherwise repeat per element.
    #[inline]
    fn call_lambda(
        &mut self,
        function_info: &'p IrFn,
        captures: Option<Rc<[Value]>>,
        args: CallArgs,
    ) -> Result<Value, Trap> {
        if self.frames.len() >= self.limits.depth as usize {
            return Err(Trap::DepthExceeded);
        }
        if args.len() != function_info.params.len() {
            return Err(self.internal_panic(format!(
                "lambda `{}` expected {} arguments, got {}",
                function_info.name,
                function_info.params.len(),
                args.len()
            )));
        }
        debug_assert_eq!(
            captures.as_ref().map_or(0, |captures| captures.len()),
            function_info.captures.len(),
            "lambda capture count"
        );

        let mut slots = SmallVec::from_elem(None, function_info.locals as usize);
        if self.simple_params[function_info.id.index()] {
            for (param, value) in function_info.params.iter().zip(args) {
                slots[param.slot.index()] = Some(value);
            }
        } else {
            let mut pattern_params: SmallVec<[(&Pattern, Value); 2]> = SmallVec::new();
            for (param, value) in function_info.params.iter().zip(args) {
                if let Some(pattern_id) = param.pattern {
                    let Some(pattern) = function_info.pattern(pattern_id) else {
                        return Err(self.internal_panic("lambda parameter pattern is out of range"));
                    };
                    pattern_params.push((pattern, value));
                } else {
                    slots[param.slot.index()] = Some(value);
                }
            }
            self.frames.push(Frame {
                slots,
                captures,
                source: function_info.source,
                name: function_info.name.as_ref(),
            });
            for (pattern, value) in pattern_params {
                if let Err(error) = self.bind_pattern(pattern, &value) {
                    self.frames.pop();
                    return Err(match error {
                        EvalError::Trap(trap) => trap,
                        _ => self.internal_panic("lambda parameter binding failed"),
                    });
                }
            }
            return self.finish_lambda_call(function_info);
        }

        self.frames.push(Frame {
            slots,
            captures,
            source: function_info.source,
            name: function_info.name.as_ref(),
        });
        self.finish_lambda_call(function_info)
    }

    /// Fast path for plain functions with no mutable or destructured
    /// parameters. It avoids the generic writeback scan and capture checks.
    #[inline]
    fn call_simple_function(
        &mut self,
        function_info: &'p IrFn,
        args: CallArgs,
    ) -> Result<Value, Trap> {
        if self.frames.len() >= self.limits.depth as usize {
            return Err(Trap::DepthExceeded);
        }
        if args.len() != function_info.params.len() {
            return Err(self.internal_panic(format!(
                "`{}` expected {} arguments, got {}",
                function_info.name,
                function_info.params.len(),
                args.len()
            )));
        }
        let mut slots = SmallVec::from_elem(None, function_info.locals as usize);
        for (param, value) in function_info.params.iter().zip(args) {
            slots[param.slot.index()] = Some(value);
        }
        self.frames.push(Frame {
            slots,
            captures: None,
            source: function_info.source,
            name: function_info.name.as_ref(),
        });
        if let Err(trap) = self.maybe_check_memory() {
            self.frames.pop();
            return Err(match trap {
                EvalError::Trap(trap) => trap,
                _ => Trap::OutOfMemory,
            });
        }
        let body = function_info.body.expect("simple function body");
        let previous = self.current;
        self.current = Some(function_info);
        let result = match self.eval_body(body) {
            Ok(value) => Ok(value),
            Err(EvalError::Return(value)) => Ok(value),
            Err(EvalError::Trap(trap)) => Err(trap),
            Err(EvalError::Break) | Err(EvalError::Continue) => {
                Err(self.internal_panic("`break`/`continue` escaped a loop"))
            }
        };
        self.current = previous;
        self.frames.pop();
        let value = result?;
        self.maybe_check_returned_value(&value)
            .map_err(|error| match error {
                EvalError::Trap(trap) => trap,
                _ => Trap::OutOfMemory,
            })?;
        if value_depth_exceeds(&value, MAX_VALUE_DEPTH) {
            return Err(Trap::DepthExceeded);
        }
        Ok(value)
    }

    #[inline]
    fn finish_lambda_call(&mut self, function_info: &'p IrFn) -> Result<Value, Trap> {
        if let Err(trap) = self.maybe_check_memory() {
            self.frames.pop();
            return Err(match trap {
                EvalError::Trap(trap) => trap,
                _ => Trap::OutOfMemory,
            });
        }
        let body = function_info.body.expect("lambda body");
        let previous = self.current;
        self.current = Some(function_info);
        let result = match self.eval_body(body) {
            Ok(value) => Ok(value),
            Err(EvalError::Return(value)) => Ok(value),
            Err(EvalError::Trap(trap)) => Err(trap),
            Err(EvalError::Break) | Err(EvalError::Continue) => {
                Err(self.internal_panic("loop control escaped a lambda"))
            }
        };
        self.current = previous;
        self.frames.pop();
        let value = result?;
        self.maybe_check_returned_value(&value)
            .map_err(|error| match error {
                EvalError::Trap(trap) => trap,
                _ => Trap::OutOfMemory,
            })?;
        if value_depth_exceeds(&value, MAX_VALUE_DEPTH) {
            return Err(Trap::DepthExceeded);
        }
        Ok(value)
    }

    fn call_function_with_writebacks_and_captures(
        &mut self,
        function: FnId,
        args: CallArgs,
        captures: Option<Rc<[Value]>>,
    ) -> Result<(Value, Writebacks), Trap> {
        if self.frames.len() >= self.limits.depth as usize {
            return Err(Trap::DepthExceeded);
        }
        let program = self.program;
        let function_info = program
            .function(function)
            .ok_or_else(|| self.internal_panic(format!("missing function #{:?}", function)))?;
        if let FnKind::Host(host) = function_info.kind {
            return Ok((self.call_host(host, &args)?, Writebacks::new()));
        }
        if function_info.body.is_none() {
            return Err(
                self.internal_panic(format!("function `{}` has no body", function_info.name))
            );
        }
        if args.len() != function_info.params.len() {
            return Err(self.internal_panic(format!(
                "`{}` expected {} arguments, got {}",
                function_info.name,
                function_info.params.len(),
                args.len()
            )));
        }
        let captures = if matches!(function_info.kind, FnKind::Lambda) {
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

        if !matches!(function_info.kind, FnKind::Lambda)
            && function_info.body.is_some()
            && self.simple_params[function.index()]
            && !self.mutable_params[function.index()]
        {
            return self
                .call_simple_function(function_info, args)
                .map(|value| (value, Writebacks::new()));
        }

        let mut slots = SmallVec::from_elem(None, function_info.locals as usize);
        let mut pattern_params: SmallVec<[(&Pattern, Value); 2]> = SmallVec::new();
        for (param, value) in function_info.params.iter().zip(args) {
            if let Some(pattern_id) = param.pattern {
                let Some(pattern) = function_info.pattern(pattern_id) else {
                    return Err(self.internal_panic("parameter pattern is out of range"));
                };
                pattern_params.push((pattern, value));
            } else {
                let Some(slot) = slots.get_mut(param.slot.index()) else {
                    return Err(self.internal_panic("parameter slot is out of range"));
                };
                *slot = Some(value);
            }
        }

        let frame = Frame {
            slots,
            captures,
            source: function_info.source,
            name: function_info.name.as_ref(),
        };
        self.frames.push(frame);
        for (pattern, value) in pattern_params {
            if let Err(error) = self.bind_pattern(pattern, &value) {
                let trap = match error {
                    EvalError::Trap(trap) => trap,
                    _ => self.internal_panic("lambda parameter binding failed"),
                };
                self.frames.pop();
                return Err(trap);
            }
        }
        if let Err(trap) = self.maybe_check_memory() {
            self.frames.pop();
            return Err(match trap {
                EvalError::Trap(trap) => trap,
                _ => Trap::OutOfMemory,
            });
        }

        let body = function_info.body.expect("body checked above");
        let previous = self.current;
        self.current = Some(function_info);
        let result = match self.eval(body) {
            Ok(value) => Ok(value),
            Err(EvalError::Return(value)) => Ok(value),
            Err(EvalError::Trap(trap)) => Err(trap),
            Err(EvalError::Break) | Err(EvalError::Continue) => {
                Err(self.internal_panic("`break`/`continue` escaped a loop"))
            }
        };
        self.current = previous;
        let updated: Writebacks = if result.is_ok() && self.mutable_params[function.index()] {
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
                EvalError::Trap(trap) => trap,
                _ => Trap::OutOfMemory,
            })?;
        if value_depth_exceeds(&value, MAX_VALUE_DEPTH) {
            return Err(Trap::DepthExceeded);
        }
        Ok((value, updated))
    }

    #[inline]
    fn call_closure_value(&mut self, function: &FnValue, args: CallArgs) -> Result<Value, Trap> {
        match function {
            FnValue::Def(id) => self.call_function(*id, args),
            FnValue::Closure(closure) => self.call_closure(closure, args),
        }
    }

    fn call_host(&mut self, host: HostId, args: &[Value]) -> Result<Value, Trap> {
        if self.deadline_due() {
            return Err(Trap::Deadline);
        }
        let program = self.program;
        let Some(info) = program.host(host) else {
            return Err(self.internal_panic(format!("missing host function #{:?}", host)));
        };
        let Some(dispatcher) = self.host.as_deref_mut() else {
            return Err(Trap::Panic {
                message: "no test double".into(),
                source: None,
                span: None,
                trace: Vec::new(),
            });
        };
        let mut cx = HostContext::new(info.opaque_return);
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

    fn maybe_check_memory(&mut self) -> EvalResult<()> {
        if self.census_bytes.saturating_add(self.pending_bytes) > self.limits.memory {
            self.refresh_memory(None)
        } else {
            Ok(())
        }
    }

    fn maybe_check_returned_value(&mut self, value: &Value) -> EvalResult<()> {
        if self.census_bytes.saturating_add(self.pending_bytes) > self.limits.memory {
            self.refresh_memory(Some(value))
        } else {
            Ok(())
        }
    }

    fn refresh_memory(&mut self, extra: Option<&Value>) -> EvalResult<()> {
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
        if total > self.limits.memory {
            Err(EvalError::Trap(Trap::OutOfMemory))
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
            Err(EvalError::Trap(Trap::Deadline))
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
            NodeKind::Field { base, index } => self.eval_field(base, index),
            NodeKind::TupleField { base, index } => self.eval_tuple_field(base, index),
            NodeKind::Index { base, index } => {
                let base = self.eval(base)?;
                let index = self.eval(index)?;
                self.index_value(base, index, span)
            }

            NodeKind::Neg(inner) => self.eval_neg(inner, span),
            NodeKind::Not(inner) => {
                let value = self.eval(inner)?;
                match value {
                    Value::Bool(value) => Ok(Value::Bool(!value)),
                    _ => self.trap(self.internal_panic("invalid unary operand")),
                }
            }
            NodeKind::Add { lhs, rhs } => self.eval_binary_values(BinaryKind::Add, lhs, rhs, span),
            NodeKind::Sub { lhs, rhs } => self.eval_binary_values(BinaryKind::Sub, lhs, rhs, span),
            NodeKind::Mul { lhs, rhs } => self.eval_binary_values(BinaryKind::Mul, lhs, rhs, span),
            NodeKind::Div { lhs, rhs } => self.eval_binary_values(BinaryKind::Div, lhs, rhs, span),
            NodeKind::Rem { lhs, rhs } => self.eval_binary_values(BinaryKind::Rem, lhs, rhs, span),
            NodeKind::Eq { lhs, rhs } => self.eval_binary_values(BinaryKind::Eq, lhs, rhs, span),
            NodeKind::Ne { lhs, rhs } => self.eval_binary_values(BinaryKind::Ne, lhs, rhs, span),
            NodeKind::Lt { lhs, rhs } => self.eval_binary_values(BinaryKind::Lt, lhs, rhs, span),
            NodeKind::Le { lhs, rhs } => self.eval_binary_values(BinaryKind::Le, lhs, rhs, span),
            NodeKind::Gt { lhs, rhs } => self.eval_binary_values(BinaryKind::Gt, lhs, rhs, span),
            NodeKind::Ge { lhs, rhs } => self.eval_binary_values(BinaryKind::Ge, lhs, rhs, span),
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
                let value = self.eval(inner)?;
                self.charge(fuel::show_cost(value.meta().slots))?;
                let mut text = String::new();
                write_show(&mut text, &value, &self.program.types, true);
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
                self.charge(u64::from(units))?;
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
                for item in items {
                    values.push(self.const_to_value(item)?);
                }
                Value::tuple(values)
            }
            ConstValue::List(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.const_to_value(item)?);
                }
                Value::list(values)
            }
            ConstValue::Map(entries) => {
                let mut map = OrderedMap::new();
                for (key, value) in entries {
                    let key_value = self.const_to_value(key)?;
                    if let Ok(key) = key_from_value(&key_value) {
                        map.insert(key, self.const_to_value(value)?);
                    }
                }
                Value::map(map)
            }
            ConstValue::Struct(type_id, fields) => {
                let mut values = Vec::with_capacity(fields.len());
                for field in fields {
                    values.push(self.const_to_value(field)?);
                }
                Value::Struct(Rc::new(StructValue::new(*type_id, values)))
            }
            ConstValue::Enum(type_id, variant, payload) => {
                let mut values = Vec::with_capacity(payload.len());
                for item in payload {
                    values.push(self.const_to_value(item)?);
                }
                Value::Enum(Rc::new(EnumValue::new(*type_id, *variant as usize, values)))
            }
            ConstValue::Prelude(variant, payload) => {
                let mut values = Vec::with_capacity(payload.len());
                for item in payload {
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
                .ok_or_else(|| EvalError::Trap(self.panic("integer overflow", span))),
            Value::Float(value) => Ok(Value::Float(-value)),
            _ => self.trap(self.internal_panic("invalid unary operand")),
        }
    }

    #[inline]
    fn eval_binary_values(
        &mut self,
        op: BinaryKind,
        lhs: IrId,
        rhs: IrId,
        span: Span,
    ) -> EvalResult<Value> {
        let lhs = self.eval(lhs)?;
        let rhs = self.eval(rhs)?;
        self.binary_values(op, lhs, rhs, span)
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
                let lhs_meta = lhs.meta();
                let rhs_meta = rhs.meta();
                let cost = fuel::comparison_cost(lhs_meta.slots, lhs_meta.string_slots)
                    .max(fuel::comparison_cost(rhs_meta.slots, rhs_meta.string_slots));
                self.charge(cost)?;
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
                        .ok_or_else(|| EvalError::Trap(self.panic("integer overflow", span)))
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
        Ok(value)
    }

    fn eval_map(&mut self, range: ChildRange, span: Span) -> EvalResult<Value> {
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
            let key = key_from_value(&key)
                .map_err(|message| EvalError::Trap(self.panic(message, key_span)))?;
            map.insert(key, value);
        }
        self.charge(fuel::map_cost(map.len() as u64))?;
        let key_bytes: u64 = map.iter().map(|(key, _)| key_value_size(key)).sum();
        self.note_alloc(16 + 16 * map.len() as u64 + key_bytes);
        let value = Value::map(map);
        self.check_value_depth(&value, span)?;
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
                for (key, value) in map.iter() {
                    self.charge(per_iter)?;
                    let pair = Value::tuple(vec![crate::value::key_to_value(key), value.clone()]);
                    self.note_alloc(16 + 8 * 2);
                    self.bind_pattern(pattern, &pair)?;
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

    fn eval_string(&mut self, range: ChildRange) -> EvalResult<Value> {
        let function = self.current_function();
        let parts = function.str_part_slice(range);
        let mut out = String::new();
        let mut show_slots = 0u64;
        for part in parts {
            match part {
                martite_ir::StrPart::Text(id) => {
                    if let Some(text) = self.program.string(*id) {
                        out.push_str(text);
                    }
                }
                martite_ir::StrPart::Expr(expr) => {
                    let value = self.eval(*expr)?;
                    show_slots += fuel::show_cost(value.meta().slots);
                    write_show(&mut out, &value, &self.program.types, true);
                }
            }
        }
        self.charge(show_slots)?;
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
            Err(EvalError::Trap(Trap::DepthExceeded))
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
            .call_function_with_writebacks(target, values)
            .map_err(EvalError::Trap)?;
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
                .map_err(EvalError::Trap),
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
            EvalError::Trap(self.internal_panic("host parameter count exceeds u32"))
        })?;
        let values = self.eval_plain_children(first_arg, count)?;
        if self.frames.len() >= self.limits.depth as usize {
            return self.trap(Trap::DepthExceeded);
        }
        self.call_host(host, &values).map_err(EvalError::Trap)
    }

    // ------------------------------------------------------------------
    // Places
    // ------------------------------------------------------------------

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

    fn read_place(&self, place: &Place) -> EvalResult<Value> {
        let mut value = self
            .frames
            .last()
            .and_then(|frame| frame.slots.get(place.root.index()))
            .and_then(Option::as_ref)
            .cloned()
            .ok_or_else(|| EvalError::Trap(self.internal_panic("unbound place root")))?;
        for accessor in &place.accessors {
            value = match (value, accessor) {
                (Value::Struct(value), Accessor::Field(index)) => {
                    value.fields.get(*index).cloned().ok_or_else(|| {
                        EvalError::Trap(self.internal_panic("struct field out of range"))
                    })?
                }
                (Value::Tuple(items), Accessor::Tuple(index)) => {
                    items.get(*index as usize).cloned().ok_or_else(|| {
                        EvalError::Trap(self.internal_panic("tuple field out of range"))
                    })?
                }
                (Value::List(items), Accessor::Index(Value::Int(index))) => {
                    if *index < 0 || *index as usize >= items.len() {
                        return self.trap(self.internal_panic("index out of bounds"));
                    }
                    items[*index as usize].clone()
                }
                (Value::Map(map), Accessor::Index(key)) => {
                    let key = key_from_value(key)
                        .map_err(|message| EvalError::Trap(self.internal_panic(message)))?;
                    map.get(&key)
                        .cloned()
                        .ok_or_else(|| EvalError::Trap(self.internal_panic("missing map key")))?
                }
                _ => return self.trap(self.internal_panic("invalid place access")),
            };
        }
        Ok(value)
    }

    fn write_place(&mut self, place: Place, value: Value) -> EvalResult<()> {
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
            return Err(EvalError::Trap(self.internal_panic("unbound place root")));
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
                return Err(EvalError::Trap(Trap::DepthExceeded));
            }
            *value = new_value;
            return Ok(());
        };
        if remaining == 0 {
            return Err(EvalError::Trap(Trap::DepthExceeded));
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
                        EvalError::Trap(self.internal_panic("struct field out of range"))
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
                        EvalError::Trap(self.internal_panic("tuple field out of range"))
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
                        return Err(EvalError::Trap(Trap::DepthExceeded));
                    }
                    let key = key_from_value(index_value)
                        .map_err(|message| EvalError::Trap(self.internal_panic(message)))?;
                    let slots = map.len().saturating_mul(2);
                    let map = self.cow_map_mut(map, slots)?;
                    if rest.is_empty() {
                        if value_depth_exceeds(&new_value, child_remaining) {
                            return Err(EvalError::Trap(Trap::DepthExceeded));
                        }
                        if map.get(&key).is_none() {
                            self.note_alloc(16 + key_value_size(&key));
                        }
                        map.insert(key, new_value);
                        Ok(())
                    } else {
                        let old = map
                            .get(&key)
                            .ok_or_else(|| EvalError::Trap(self.internal_panic("missing map key")))?
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
                        EvalError::Trap(self.internal_panic("struct field out of range"))
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
                        EvalError::Trap(self.internal_panic("tuple field out of range"))
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
                    let key = key_from_value(index_value)
                        .map_err(|message| EvalError::Trap(self.internal_panic(message)))?;
                    let slots = map.len().saturating_mul(2);
                    let map = self.cow_map_mut(map, slots)?;
                    let old = map
                        .get(&key)
                        .ok_or_else(|| EvalError::Trap(self.internal_panic("missing map key")))?
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
        name: &str,
        args: &[Value],
        path_depth: usize,
    ) -> EvalResult<()> {
        let item_limit = MAX_VALUE_DEPTH.saturating_sub(path_depth).saturating_sub(1);
        let list_limit = MAX_VALUE_DEPTH.saturating_sub(path_depth);
        let check = |value: &Value, limit: usize| -> EvalResult<()> {
            if value_depth_exceeds(value, limit) {
                Err(EvalError::Trap(Trap::DepthExceeded))
            } else {
                Ok(())
            }
        };
        match name {
            "push" => {
                if let Some(value) = args.first() {
                    check(value, item_limit)?;
                }
            }
            "insert" => {
                if let Some(value) = args.get(1) {
                    check(value, item_limit)?;
                }
            }
            "extend" => {
                if let Some(value) = args.first() {
                    check(value, list_limit)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn index_value(&mut self, base: Value, index: Value, span: Span) -> EvalResult<Value> {
        match (base, index) {
            (Value::List(items), Value::Int(index)) => {
                if index < 0 || index as usize >= items.len() {
                    return self.trap(self.panic("index out of bounds", span));
                }
                Ok(items[index as usize].clone())
            }
            (
                Value::List(items),
                Value::Range {
                    start,
                    end,
                    inclusive,
                },
            ) => {
                let len = items.len() as i64;
                let Some((start, end)) = slice_bounds(start, end, inclusive, len) else {
                    return self.trap(self.panic("invalid list slice", span));
                };
                let slice = items[start..end].to_vec();
                self.charge(fuel::slot_cost(slice.len() as u64))?;
                self.note_alloc(16 + 8 * slice.len() as u64);
                let value = Value::list(slice);
                self.check_value_depth(&value, span)?;
                Ok(value)
            }
            (
                Value::Str(text),
                Value::Range {
                    start,
                    end,
                    inclusive,
                },
            ) => {
                let chars: Vec<char> = text.chars().collect();
                let len = chars.len() as i64;
                let Some((start, end)) = slice_bounds(start, end, inclusive, len) else {
                    return self.trap(self.panic("invalid string slice", span));
                };
                let out: String = chars[start..end].iter().collect();
                self.charge(fuel::string_cost(out.len() as u64))?;
                self.note_alloc(16 + out.len() as u64);
                Ok(Value::str(out))
            }
            (Value::Map(map), key) => {
                let key = key_from_value(&key)
                    .map_err(|message| EvalError::Trap(self.panic(message, span)))?;
                map.get(&key)
                    .cloned()
                    .ok_or_else(|| EvalError::Trap(self.panic("missing map key", span)))
            }
            _ => self.trap(self.panic("invalid index operation", span)),
        }
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
    use super::{CallArgs, Frame, IrRuntime};
    use crate::exec::{ExecLimits, Trap};
    use crate::value::{Closure, FnValue, Value};
    use martite_ir::{
        AccessMode, CaptureIndex, CaptureInfo, CaptureSource, Charge, ChildRange, ConstId,
        ConstValue, Effects, Entries, FnId, FnKind, IrFn, IrId, IrProgram, Node, NodeKind,
        ParamInfo, Pattern, PatternId, Slot, SlotInfo, StrId, TyId,
    };
    use smallvec::SmallVec;
    use std::rc::Rc;

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
}
