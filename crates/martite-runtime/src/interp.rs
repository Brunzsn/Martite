use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use martite_base::{SourceId, Span};
use martite_check::{
    BindingId, BuiltinFn, BuiltinItem, BuiltinReceiver, CheckedProgram, ConstValue, DefId, FnKind,
    FnSig, PatResolution, PreludeVariant, Resolution,
};
use martite_syntax::{
    AssignOp, BinaryOp, Block, Expr, ExprKind, LambdaBody, MatchBody, PatKind, Pattern, Stmt,
    StmtKind, StrExprPart, UnaryOp,
};
use smallvec::{SmallVec, smallvec};

use crate::cost::{comparison_cost, show_slots};
use crate::ops::{show, value_eq, value_ord};
use crate::value::{
    Closure, ClosureTarget, EnumValue, MAX_VALUE_DEPTH, OrderedMap, StructValue, Value,
    key_from_value, key_to_value, key_value_size, value_depth_exceeds,
};

#[derive(Debug, Clone, Copy)]
pub struct ExecLimits {
    pub fuel: u64,
    pub memory: u64,
    pub depth: u32,
    pub deadline: Option<Duration>,
}

impl Default for ExecLimits {
    fn default() -> Self {
        Self {
            fuel: 1_000_000,
            memory: 8 << 20,
            depth: 200,
            deadline: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostFault {
    pub message: String,
}

impl HostFault {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostContext {
    pub charged: u64,
}

impl HostContext {
    pub fn charge(&mut self, units: u64) {
        self.charged = self.charged.saturating_add(units);
    }
}

pub trait HostDispatcher {
    fn call(
        &mut self,
        def: DefId,
        args: &[Value],
        cx: &mut HostContext,
    ) -> Result<Value, HostFault>;
}

#[derive(Debug, Clone)]
pub enum Trap {
    Panic {
        message: String,
        source: Option<SourceId>,
        span: Option<Span>,
        trace: Vec<String>,
    },
    OutOfFuel,
    OutOfMemory,
    DepthExceeded,
    Deadline,
    HostFault(String),
}

impl Trap {
    pub fn panic_message(&self) -> String {
        match self {
            Trap::Panic { message, .. } => message.clone(),
            Trap::OutOfFuel => "out of fuel".into(),
            Trap::OutOfMemory => "out of memory".into(),
            Trap::DepthExceeded => "maximum call depth exceeded".into(),
            Trap::Deadline => "deadline exceeded".into(),
            Trap::HostFault(message) => format!("host fault: {message}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CallResult {
    pub value: Value,
    /// Values of the `mut` parameters after the call, in declaration order.
    /// Immutable parameters are not copied out.
    pub params: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct MainResult {
    pub value: Value,
    pub log: Vec<String>,
}

pub struct Runtime<'p, 'h> {
    program: &'p CheckedProgram,
    limits: ExecLimits,
    fuel: u64,
    start: Instant,
    ticks: u64,
    frames: Vec<Frame<'p>>,
    log: Vec<String>,
    host: Option<&'h mut dyn HostDispatcher>,
    /// Live bytes at the last exact scan.
    census_bytes: u64,
    /// Allocation bytes recorded since the last exact scan. This is a
    /// conservative upper bound: freed values are only subtracted by a scan.
    pending_bytes: u64,
}

struct Frame<'p> {
    locals: Locals,
    /// Undo log for bindings introduced by an attempted match pattern. Each
    /// entry restores the previous value, or removes the binding when it was
    /// absent.
    undo: Vec<(BindingId, Option<Value>)>,
    captures: Option<Rc<HashMap<BindingId, Value>>>,
    source: SourceId,
    name: &'p str,
}

/// Per-frame local bindings. `BindingId`s are dense, but not contiguous per
/// function, so this uses a small inline list instead of a hash map. Lookups
/// are linear over the (typically tiny) set of live bindings.
#[derive(Default)]
struct Locals {
    slots: SmallVec<[(BindingId, Value); 8]>,
}

impl Locals {
    fn get(&self, id: BindingId) -> Option<&Value> {
        self.slots
            .iter()
            .find(|(binding, _)| *binding == id)
            .map(|(_, value)| value)
    }

    fn insert(&mut self, id: BindingId, value: Value) -> Option<Value> {
        if let Some((_, existing)) = self.slots.iter_mut().find(|(binding, _)| *binding == id) {
            return Some(std::mem::replace(existing, value));
        }
        self.slots.push((id, value));
        None
    }

    fn remove(&mut self, id: BindingId) -> Option<Value> {
        let index = self.slots.iter().position(|(binding, _)| *binding == id)?;
        Some(self.slots.swap_remove(index).1)
    }

    fn values(&self) -> impl Iterator<Item = &Value> {
        self.slots.iter().map(|(_, value)| value)
    }
}

type CallArgs = SmallVec<[Value; 4]>;
type Writebacks = SmallVec<[(usize, Value); 2]>;

enum EvalError {
    Trap(Trap),
    Return(Value),
    Break,
    Continue,
}

type EvalResult<T> = Result<T, EvalError>;

struct Place {
    root: BindingId,
    accessors: Vec<Accessor>,
}

enum Accessor {
    Field(usize),
    Tuple(u32),
    Index(Value),
}

impl<'p, 'h> Runtime<'p, 'h> {
    pub fn new(program: &'p CheckedProgram, limits: ExecLimits) -> Self {
        Self {
            program,
            limits,
            fuel: limits.fuel,
            start: Instant::now(),
            ticks: 0,
            frames: Vec::new(),
            log: Vec::new(),
            host: None,
            census_bytes: 0,
            pending_bytes: 0,
        }
    }

    pub fn with_host(
        program: &'p CheckedProgram,
        limits: ExecLimits,
        host: &'h mut dyn HostDispatcher,
    ) -> Self {
        let mut runtime = Self::new(program, limits);
        runtime.host = Some(host);
        runtime
    }

    pub fn set_host(&mut self, host: &'h mut dyn HostDispatcher) {
        self.host = Some(host);
    }

    pub fn limits(&self) -> ExecLimits {
        self.limits
    }

    pub fn set_limits(&mut self, limits: ExecLimits) {
        self.limits = limits;
    }

    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }

    pub fn invoke(&mut self, def: DefId, args: Vec<Value>) -> Result<CallResult, Trap> {
        self.reset();
        self.note_values(&args);
        let (value, updated) = self.call_def(def, CallArgs::from_vec(args))?;
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
        let Some(def) = self.program.handler(name) else {
            return Ok(None);
        };
        self.reset();
        if let Some(state) = state.as_ref() {
            self.note_values(std::slice::from_ref(state));
        }
        self.note_values(&args);
        let program = self.program;
        let sig = program
            .fn_sig(def)
            .ok_or_else(|| self.internal_panic("missing handler signature"))?;
        let has_self = sig.has_self();
        let self_mutable = sig.self_mutable();
        let mut call_args = CallArgs::new();
        if has_self {
            call_args.push(state.clone().unwrap_or(Value::Unit));
        }
        call_args.extend(args);
        let (value, mut updated) = self.call_def_sig(def, sig, call_args)?;
        if has_self && self_mutable {
            *state = take_writeback(&mut updated, 0);
        }
        Ok(Some(value))
    }

    pub fn run_main(&mut self) -> Result<MainResult, Trap> {
        let main = self.program.main.ok_or_else(|| Trap::Panic {
            message: "no `main` function".into(),
            source: None,
            span: None,
            trace: Vec::new(),
        })?;
        self.reset();
        let (value, _) = self.call_def(main, CallArgs::new())?;
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

    fn internal_panic(&self, message: impl Into<String>) -> Trap {
        Trap::Panic {
            message: message.into(),
            source: self.frames.last().map(|f| f.source),
            span: None,
            trace: self.frames.iter().map(|f| f.name.to_string()).collect(),
        }
    }

    fn panic(&self, message: impl Into<String>, span: Span) -> Trap {
        Trap::Panic {
            message: message.into(),
            source: self.frames.last().map(|f| f.source),
            span: Some(span),
            trace: self.frames.iter().map(|f| f.name.to_string()).collect(),
        }
    }

    fn trap<T>(&self, trap: Trap) -> EvalResult<T> {
        Err(EvalError::Trap(trap))
    }

    fn charge(&mut self, units: u64) -> EvalResult<()> {
        self.fuel = self.fuel.saturating_sub(units);
        if self.fuel == 0 {
            return self.trap(Trap::OutOfFuel);
        }
        self.ticks += 1;
        if self.ticks % 1024 == 0 {
            if let Some(deadline) = self.limits.deadline {
                if self.start.elapsed() >= deadline {
                    return self.trap(Trap::Deadline);
                }
            }
        }
        if self.ticks % 256 == 0 {
            self.maybe_check_memory()?;
        }
        Ok(())
    }

    /// Record bytes allocated since the last scan. O(1); the exact live-heap
    /// scan happens only when the conservative total could exceed the cap.
    fn note_alloc(&mut self, bytes: u64) {
        self.pending_bytes = self.pending_bytes.saturating_add(bytes);
    }

    fn note_value(&mut self, value: &Value) {
        let bytes = value.size_bytes(&mut HashSet::new());
        self.note_alloc(bytes);
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

    /// Validate a value that is leaving its frame. The exact scan runs only
    /// when the conservative allocation total could cross the cap.
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
            for value in frame.locals.values() {
                total = total.saturating_add(value.size_bytes(&mut seen));
            }
            if let Some(captures) = &frame.captures {
                for value in captures.values() {
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

    fn call_def(&mut self, def: DefId, args: CallArgs) -> Result<(Value, Writebacks), Trap> {
        let program = self.program;
        let sig = program
            .fn_sig(def)
            .ok_or_else(|| self.internal_panic("missing function signature"))?;
        self.call_def_sig(def, sig, args)
    }

    fn call_def_sig(
        &mut self,
        def: DefId,
        sig: &'p FnSig,
        args: CallArgs,
    ) -> Result<(Value, Writebacks), Trap> {
        if self.frames.len() >= self.limits.depth as usize {
            return Err(Trap::DepthExceeded);
        }
        if sig.kind == FnKind::Host {
            return Ok((self.call_host(def, args)?, Writebacks::new()));
        }
        let program = self.program;
        let body = program
            .function_block(def)
            .ok_or_else(|| self.internal_panic(format!("function `{}` has no body", sig.name)))?;
        if args.len() != sig.params.len() {
            return Err(self.internal_panic(format!(
                "`{}` expected {} arguments, got {}",
                sig.name,
                sig.params.len(),
                args.len()
            )));
        }
        let source = program.resolved.module(sig.module).source;
        let mut locals = Locals::default();
        for (param, value) in sig.params.iter().zip(args) {
            locals.insert(param.binding, value);
        }
        self.frames.push(Frame {
            locals,
            undo: Vec::new(),
            captures: None,
            source,
            name: sig.name.as_str(),
        });
        if let Err(trap) = self.maybe_check_memory() {
            self.frames.pop();
            return Err(match trap {
                EvalError::Trap(trap) => trap,
                _ => Trap::OutOfMemory,
            });
        }
        let result = match self.eval_block(body) {
            Ok(value) => Ok(value),
            Err(EvalError::Return(value)) => Ok(value),
            Err(EvalError::Trap(trap)) => Err(trap),
            Err(EvalError::Break) | Err(EvalError::Continue) => {
                Err(self.internal_panic("`break`/`continue` escaped a loop"))
            }
        };
        let updated = if result.is_ok() {
            let frame = self.frames.last_mut().expect("frame");
            sig.params
                .iter()
                .enumerate()
                .filter(|(_, param)| param.mutable)
                .map(|(index, param)| {
                    (
                        index,
                        frame.locals.remove(param.binding).unwrap_or(Value::Unit),
                    )
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

    fn call_host(&mut self, def: DefId, args: CallArgs) -> Result<Value, Trap> {
        if self.deadline_due() {
            return Err(Trap::Deadline);
        }
        let Some(host) = self.host.as_deref_mut() else {
            return Err(Trap::Panic {
                message: "no test double".into(),
                source: None,
                span: None,
                trace: Vec::new(),
            });
        };
        let mut cx = HostContext { charged: 0 };
        let result = host.call(def, &args, &mut cx);
        self.fuel = self.fuel.saturating_sub(cx.charged.saturating_add(1));
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

    fn call_closure(&mut self, closure: &Closure, args: CallArgs) -> Result<Value, Trap> {
        match &closure.target {
            ClosureTarget::Def(def) => self.call_def(*def, args).map(|(value, _)| value),
            ClosureTarget::Lambda { node } => {
                let program = self.program;
                let Some(lambda) = program.lambda(*node) else {
                    return Err(self.internal_panic("missing lambda body"));
                };
                let params = &lambda.params;
                let body = &lambda.body;
                if params.len() != args.len() {
                    return Err(self.internal_panic("lambda argument count mismatch"));
                }
                if self.frames.len() >= self.limits.depth as usize {
                    return Err(Trap::DepthExceeded);
                }
                let source = self.frames.last().map(|f| f.source).unwrap_or(SourceId(0));
                self.frames.push(Frame {
                    locals: Locals::default(),
                    undo: Vec::new(),
                    captures: closure.captures.clone(),
                    source,
                    name: "<lambda>",
                });
                for (index, (param, value)) in params.iter().zip(args.iter()).enumerate() {
                    match lambda.param_bindings.get(index).copied().flatten() {
                        Some(id) => {
                            self.current_locals_mut().insert(id, value.clone());
                        }
                        None => self.bind_pattern(&param.pattern, value),
                    }
                }
                if let Err(trap) = self.maybe_check_memory() {
                    self.frames.pop();
                    return Err(match trap {
                        EvalError::Trap(trap) => trap,
                        _ => Trap::OutOfMemory,
                    });
                }
                let result = match body {
                    LambdaBody::Block(block) => match self.eval_block(block) {
                        Ok(v) => Ok(v),
                        Err(EvalError::Return(v)) => Ok(v),
                        Err(EvalError::Trap(t)) => Err(t),
                        Err(EvalError::Break) | Err(EvalError::Continue) => {
                            Err(self.internal_panic("loop control escaped a lambda"))
                        }
                    },
                    LambdaBody::Expr(expr) => match self.eval_expr(expr) {
                        Ok(v) => Ok(v),
                        Err(EvalError::Return(v)) => Ok(v),
                        Err(EvalError::Trap(t)) => Err(t),
                        Err(EvalError::Break) | Err(EvalError::Continue) => {
                            Err(self.internal_panic("loop control escaped a lambda"))
                        }
                    },
                };
                self.frames.pop();
                match result {
                    Ok(value) => {
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
                    Err(error) => Err(error),
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Statements
    // ------------------------------------------------------------------

    fn eval_block(&mut self, block: &Block) -> EvalResult<Value> {
        let mut value = Value::Unit;
        for (i, stmt) in block.stmts.iter().enumerate() {
            let is_last = i + 1 == block.stmts.len();
            if is_last {
                if let StmtKind::Expr { expr, assign: None } = &stmt.kind {
                    value = self.eval_expr(expr)?;
                    continue;
                }
            }
            self.eval_stmt(stmt)?;
            value = Value::Unit;
        }
        Ok(value)
    }

    fn eval_stmt(&mut self, stmt: &Stmt) -> EvalResult<Value> {
        match &stmt.kind {
            StmtKind::Let { pattern, value, .. } => {
                let value = self.eval_expr(value)?;
                self.bind_pattern(pattern, &value);
                self.maybe_check_memory()?;
                Ok(Value::Unit)
            }
            StmtKind::While { cond, body } => {
                loop {
                    self.charge(1)?;
                    let condition = self.eval_expr(cond)?;
                    if !matches!(condition, Value::Bool(true)) {
                        break;
                    }
                    match self.eval_block(body) {
                        Ok(_) => {}
                        Err(EvalError::Break) => break,
                        Err(EvalError::Continue) => continue,
                        Err(other) => return Err(other),
                    }
                }
                Ok(Value::Unit)
            }
            StmtKind::For {
                pattern,
                iter,
                body,
            } => {
                let iterable = self.eval_expr(iter)?;
                self.eval_for(pattern, &iterable, body)?;
                Ok(Value::Unit)
            }
            StmtKind::Return(value) => {
                let value = match value {
                    Some(expr) => self.eval_expr(expr)?,
                    None => Value::Unit,
                };
                Err(EvalError::Return(value))
            }
            StmtKind::Break => Err(EvalError::Break),
            StmtKind::Continue => Err(EvalError::Continue),
            StmtKind::Expr { expr, assign } => {
                if let Some((op, rhs)) = assign {
                    let place = self.eval_place(expr)?;
                    let lhs = if matches!(op, AssignOp::Assign) {
                        None
                    } else {
                        Some(self.read_place(&place)?)
                    };
                    let rhs = self.eval_expr(rhs)?;
                    let value = match op {
                        AssignOp::Assign => rhs,
                        AssignOp::Add => {
                            self.binary_values(BinaryOp::Add, lhs.unwrap(), rhs, stmt.span)?
                        }
                        AssignOp::Sub => {
                            self.binary_values(BinaryOp::Sub, lhs.unwrap(), rhs, stmt.span)?
                        }
                        AssignOp::Mul => {
                            self.binary_values(BinaryOp::Mul, lhs.unwrap(), rhs, stmt.span)?
                        }
                        AssignOp::Div => {
                            self.binary_values(BinaryOp::Div, lhs.unwrap(), rhs, stmt.span)?
                        }
                        AssignOp::Rem => {
                            self.binary_values(BinaryOp::Rem, lhs.unwrap(), rhs, stmt.span)?
                        }
                    };
                    self.write_place(place, value)?;
                    self.maybe_check_memory()?;
                } else {
                    self.eval_expr(expr)?;
                    self.maybe_check_memory()?;
                }
                Ok(Value::Unit)
            }
        }
    }

    fn eval_for(&mut self, pattern: &Pattern, iterable: &Value, body: &Block) -> EvalResult<()> {
        match iterable {
            Value::List(items) => {
                let items = items.clone();
                for item in items.iter() {
                    self.charge(1)?;
                    self.bind_pattern(pattern, item);
                    match self.eval_block(body) {
                        Ok(_) | Err(EvalError::Continue) => {}
                        Err(EvalError::Break) => break,
                        Err(other) => return Err(other),
                    }
                }
            }
            Value::Map(map) => {
                let entries: Vec<(crate::value::KeyValue, Value)> = map
                    .entries()
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect();
                for (key, value) in entries {
                    self.charge(1)?;
                    let pair = Value::tuple(vec![key_to_value(&key), value]);
                    self.note_alloc(16 + 8 * 2);
                    self.bind_pattern(pattern, &pair);
                    match self.eval_block(body) {
                        Ok(_) | Err(EvalError::Continue) => {}
                        Err(EvalError::Break) => break,
                        Err(other) => return Err(other),
                    }
                }
            }
            Value::Str(text) => {
                let chars: Vec<char> = text.chars().collect();
                for ch in chars {
                    self.charge(1)?;
                    self.note_alloc(16 + ch.len_utf8() as u64);
                    let value = Value::Str(Rc::from(ch.to_string()));
                    self.bind_pattern(pattern, &value);
                    match self.eval_block(body) {
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
                let last = if *inclusive {
                    Some(*end)
                } else {
                    end.checked_sub(1)
                };
                if let Some(last) = last {
                    let mut i = *start;
                    while i <= last {
                        self.charge(1)?;
                        self.bind_pattern(pattern, &Value::Int(i));
                        match self.eval_block(body) {
                            Ok(_) | Err(EvalError::Continue) => {}
                            Err(EvalError::Break) => return Ok(()),
                            Err(other) => return Err(other),
                        }
                        if i == i64::MAX {
                            break;
                        }
                        i += 1;
                    }
                }
            }
            _ => {
                return self.trap(self.internal_panic("value is not iterable"));
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // Expressions
    // ------------------------------------------------------------------

    fn eval_expr(&mut self, expr: &Expr) -> EvalResult<Value> {
        self.eval_expr_inner(expr)
    }

    fn check_value_depth(&self, value: &Value, _span: Span) -> EvalResult<()> {
        if value_depth_exceeds(value, MAX_VALUE_DEPTH) {
            Err(EvalError::Trap(Trap::DepthExceeded))
        } else {
            Ok(())
        }
    }

    fn eval_expr_inner(&mut self, expr: &Expr) -> EvalResult<Value> {
        self.charge(1)?;
        match &expr.kind {
            ExprKind::Int(v) => Ok(Value::Int(*v)),
            ExprKind::Float(v) => Ok(Value::Float(*v)),
            ExprKind::Bool(v) => Ok(Value::Bool(*v)),
            ExprKind::Unit => Ok(Value::Unit),
            ExprKind::Str(parts) => self.eval_string(parts, expr.span),
            ExprKind::Var(_) => self.eval_var(expr),
            ExprKind::Tuple(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval_expr(item)?);
                }
                self.charge(values.len() as u64)?;
                self.note_alloc(16 + 8 * values.len() as u64);
                let value = Value::tuple(values);
                self.check_value_depth(&value, expr.span)?;
                Ok(value)
            }
            ExprKind::List(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval_expr(item)?);
                }
                self.charge(values.len() as u64)?;
                self.note_alloc(16 + 8 * values.len() as u64);
                let value = Value::list(values);
                self.check_value_depth(&value, expr.span)?;
                Ok(value)
            }
            ExprKind::Map(entries) => {
                let mut map = OrderedMap::new();
                for (k, v) in entries {
                    let key = self.eval_expr(k)?;
                    let value = self.eval_expr(v)?;
                    let key = key_from_value(&key)
                        .map_err(|msg| EvalError::Trap(self.panic(msg, k.span)))?;
                    map.insert(key, value);
                }
                self.charge(map.len() as u64 * 2)?;
                let key_bytes: u64 = map.iter().map(|(key, _)| key_value_size(key)).sum();
                self.note_alloc(16 + 16 * map.len() as u64 + key_bytes);
                let value = Value::map(map);
                self.check_value_depth(&value, expr.span)?;
                Ok(value)
            }
            ExprKind::StructLit { fields, base, .. } => {
                let value = self.eval_struct_lit(expr, fields, base.as_deref())?;
                self.check_value_depth(&value, expr.span)?;
                Ok(value)
            }
            ExprKind::Unary { op, expr: inner } => {
                let value = self.eval_expr(inner)?;
                match (op, value) {
                    (UnaryOp::Neg, Value::Int(v)) => v
                        .checked_neg()
                        .map(Value::Int)
                        .ok_or_else(|| self.panic("integer overflow", expr.span))
                        .map_err(EvalError::Trap),
                    (UnaryOp::Neg, Value::Float(v)) => Ok(Value::Float(-v)),
                    (UnaryOp::Not, Value::Bool(v)) => Ok(Value::Bool(!v)),
                    _ => self.trap(self.internal_panic("invalid unary operand")),
                }
            }
            ExprKind::Binary { op, lhs, rhs } => self.eval_binary(*op, lhs, rhs, expr.span),
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                let start = self.eval_expr(start)?;
                let end = self.eval_expr(end)?;
                match (start, end) {
                    (Value::Int(start), Value::Int(end)) => Ok(Value::Range {
                        start,
                        end,
                        inclusive: *inclusive,
                    }),
                    _ => self.trap(self.internal_panic("range bounds must be int")),
                }
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cond = self.eval_expr(cond)?;
                if matches!(cond, Value::Bool(true)) {
                    self.eval_block(then_branch)
                } else if let Some(else_branch) = else_branch {
                    self.eval_expr(else_branch)
                } else {
                    Ok(Value::Unit)
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                let value = self.eval_expr(scrutinee)?;
                for arm in arms {
                    let mark = self.locals_mark();
                    if self.match_pattern(&arm.pattern, &value)? {
                        if let Some(guard) = &arm.guard {
                            let pass = self.eval_expr(guard)?;
                            if !matches!(pass, Value::Bool(true)) {
                                self.rollback_locals(mark);
                                continue;
                            }
                        }
                        self.commit_locals(mark);
                        return match &arm.body {
                            MatchBody::Block(block) => self.eval_block(block),
                            MatchBody::Expr(expr) => self.eval_expr(expr),
                        };
                    }
                    self.rollback_locals(mark);
                }
                self.trap(self.internal_panic("match fell through"))
            }
            ExprKind::Lambda { .. } => {
                let program = self.program;
                let Some(info) = program.lambda(expr.id) else {
                    return self.trap(self.internal_panic("missing lambda metadata"));
                };
                let captures = if info.captures.is_empty() {
                    None
                } else {
                    let mut values = HashMap::with_capacity(info.captures.len());
                    for id in &info.captures {
                        let value = self.local_value(*id).cloned().ok_or_else(|| {
                            EvalError::Trap(self.internal_panic("missing captured binding"))
                        })?;
                        values.insert(*id, value);
                    }
                    Some(Rc::new(values))
                };
                self.note_alloc(16);
                Ok(Value::Fn(Rc::new(Closure {
                    target: ClosureTarget::Lambda { node: expr.id },
                    captures,
                })))
            }
            ExprKind::Block(block) => self.eval_block(block),
            ExprKind::Field { .. } => self.eval_field(expr),
            ExprKind::Call { .. } => self.eval_call(expr),
            ExprKind::Index { base, index } => {
                let base = self.eval_expr(base)?;
                let index = self.eval_expr(index)?;
                self.index_value(base, index, expr.span)
            }
            ExprKind::Try(inner) => {
                let value = self.eval_expr(inner)?;
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
            ExprKind::Convert { to, expr: inner } => {
                let value = self.eval_expr(inner)?;
                match (to, value) {
                    (martite_syntax::PrimTy::Int, Value::Float(f)) => {
                        if !f.is_finite()
                            || f >= 9_223_372_036_854_775_808.0
                            || f < -9_223_372_036_854_775_808.0
                        {
                            self.trap(self.panic("float is out of int range", expr.span))
                        } else {
                            Ok(Value::Int(f.trunc() as i64))
                        }
                    }
                    (martite_syntax::PrimTy::Float, Value::Int(v)) => Ok(Value::Float(v as f64)),
                    (martite_syntax::PrimTy::Str, value) => {
                        self.charge(show_slots(&value))?;
                        let text = show(&value, &self.program.types, true);
                        self.note_alloc(16 + text.len() as u64);
                        Ok(Value::str(text))
                    }
                    _ => self.trap(self.internal_panic("invalid conversion")),
                }
            }
        }
    }

    fn eval_string(&mut self, parts: &[StrExprPart], span: Span) -> EvalResult<Value> {
        let mut out = String::new();
        let mut slot_cost = 0;
        for part in parts {
            match part {
                StrExprPart::Text(text) => out.push_str(text),
                StrExprPart::Expr(expr) => {
                    let value = self.eval_expr(expr)?;
                    slot_cost += show_slots(&value);
                    out.push_str(&show(&value, &self.program.types, true));
                }
            }
        }
        let _ = span;
        self.charge(slot_cost)?;
        self.charge((out.len() as u64).div_ceil(8))?;
        self.note_alloc(16 + out.len() as u64);
        Ok(Value::str(out))
    }

    fn eval_var(&mut self, expr: &Expr) -> EvalResult<Value> {
        let ExprKind::Var(_) = &expr.kind else {
            unreachable!()
        };
        match self.program.resolution(expr.id).copied() {
            Some(Resolution::Local(id)) => self
                .local_value(id)
                .cloned()
                .ok_or_else(|| EvalError::Trap(self.internal_panic("unbound local"))),
            Some(Resolution::Const(def)) => self.materialize_const(def, expr.span),
            Some(Resolution::Fn(def))
            | Some(Resolution::Handler(def))
            | Some(Resolution::Test(def)) => {
                self.note_alloc(16);
                Ok(Value::Fn(Rc::new(Closure {
                    target: ClosureTarget::Def(def),
                    captures: None,
                })))
            }
            Some(Resolution::PreludeVariant(PreludeVariant::None)) => Ok(Value::Option(None)),
            Some(Resolution::PreludeVariant(variant)) => {
                self.note_alloc(16);
                Ok(Value::Result(match variant {
                    PreludeVariant::Ok => Ok(Box::new(Value::Unit)),
                    PreludeVariant::Err => Err(Box::new(Value::Unit)),
                    _ => Ok(Box::new(Value::Unit)),
                }))
            }
            Some(Resolution::BuiltinFn(function)) => Ok(Value::Builtin(function)),
            Some(Resolution::ModuleItem(def)) => self.value_of_def(def, expr.span),
            Some(Resolution::BuiltinItem(item)) => self.value_of_builtin_item(&item),
            Some(Resolution::Variant(id, index)) => {
                let payload = self
                    .program
                    .types
                    .variant(id, index)
                    .map(|v| v.payload.len())
                    .unwrap_or(0);
                if payload == 0 {
                    self.note_alloc(16);
                    Ok(Value::Enum(Rc::new(EnumValue {
                        type_id: id,
                        variant: index,
                        payload: Vec::new(),
                    })))
                } else {
                    self.trap(self.internal_panic("variant constructor used as a value"))
                }
            }
            Some(Resolution::BuiltinMethod { .. }) | Some(Resolution::Method(_)) => {
                self.trap(self.internal_panic("method used as a value"))
            }
            _ => self.trap(self.internal_panic("name has no runtime value")),
        }
    }

    fn value_of_def(&mut self, def: DefId, span: Span) -> EvalResult<Value> {
        if let Some(info) = self.program.consts.get(&def) {
            let value = info.value.clone();
            let value = self.const_to_value(&value, span);
            self.note_value(&value);
            return Ok(value);
        }
        self.note_alloc(16);
        Ok(Value::Fn(Rc::new(Closure {
            target: ClosureTarget::Def(def),
            captures: None,
        })))
    }

    fn value_of_builtin_item(&self, item: &BuiltinItem) -> EvalResult<Value> {
        match (item.module, item.name) {
            ("std.math", "PI") => Ok(Value::Float(std::f64::consts::PI)),
            ("std.math", "EULER") => Ok(Value::Float(std::f64::consts::E)),
            ("std.json", "Json") => self.trap(self.internal_panic("type used as a value")),
            (module, name) => Ok(Value::BuiltinModuleFn {
                module: module.to_string(),
                name: name.to_string(),
            }),
        }
    }

    fn eval_field(&mut self, expr: &Expr) -> EvalResult<Value> {
        let ExprKind::Field { base, .. } = &expr.kind else {
            unreachable!()
        };
        match self.program.resolution(expr.id).copied() {
            Some(Resolution::StructField(index)) => {
                let value = self.eval_expr(base)?;
                match value {
                    Value::Struct(struct_value) => Ok(struct_value
                        .fields
                        .get(index)
                        .cloned()
                        .unwrap_or(Value::Unit)),
                    _ => self.trap(self.internal_panic("field access on non-struct")),
                }
            }
            Some(Resolution::TupleField(index)) => {
                let value = self.eval_expr(base)?;
                match value {
                    Value::Tuple(items) => {
                        Ok(items.get(index as usize).cloned().unwrap_or(Value::Unit))
                    }
                    _ => self.trap(self.internal_panic("tuple field access on non-tuple")),
                }
            }
            Some(Resolution::Variant(id, index)) => {
                self.note_alloc(16);
                Ok(Value::Enum(Rc::new(EnumValue {
                    type_id: id,
                    variant: index,
                    payload: Vec::new(),
                })))
            }
            Some(Resolution::ModuleItem(def)) => self.value_of_def(def, expr.span),
            Some(Resolution::BuiltinItem(item)) => self.value_of_builtin_item(&item),
            Some(Resolution::Method(_)) | Some(Resolution::BuiltinMethod { .. }) => {
                self.trap(self.internal_panic("method used as a value"))
            }
            _ => self.trap(self.panic("field has no runtime value", expr.span)),
        }
    }

    fn eval_struct_lit(
        &mut self,
        expr: &Expr,
        fields: &[martite_syntax::FieldInit],
        base: Option<&Expr>,
    ) -> EvalResult<Value> {
        let program = self.program;
        let Some(Resolution::TypeName(type_id)) = program.resolution(expr.id).copied() else {
            return self.trap(self.internal_panic("struct literal has no type"));
        };
        let decl_count = match program.types.get(type_id).map(|def| &def.kind) {
            Some(martite_check::TypeDefKind::Struct { fields }) => fields.len(),
            _ => return self.trap(self.internal_panic("unknown struct type")),
        };
        let mut out = vec![Value::Unit; decl_count];
        if let Some(base) = base {
            if let Value::Struct(struct_value) = self.eval_expr(base)? {
                if struct_value.type_id == type_id {
                    for (slot, value) in out.iter_mut().zip(struct_value.fields.iter()) {
                        *slot = value.clone();
                    }
                }
            }
        }
        if let Some(slots) = program.struct_lit_fields.get(&expr.id) {
            for &(source, decl) in slots {
                let Some(field) = fields.get(source) else {
                    continue;
                };
                let Some(value_expr) = &field.value else {
                    continue;
                };
                let value = self.eval_expr(value_expr)?;
                if let Some(slot) = out.get_mut(decl) {
                    *slot = value;
                }
            }
        }
        self.charge(out.len() as u64)?;
        self.note_alloc(16 + 8 * out.len() as u64);
        Ok(Value::Struct(Rc::new(StructValue {
            type_id,
            fields: out,
        })))
    }

    // ------------------------------------------------------------------
    // Calls
    // ------------------------------------------------------------------

    fn eval_call(&mut self, expr: &Expr) -> EvalResult<Value> {
        let ExprKind::Call { callee, args } = &expr.kind else {
            unreachable!()
        };
        if let ExprKind::Field { base, .. } = &callee.kind {
            match self.program.resolution(callee.id).copied() {
                Some(Resolution::Method(def)) => {
                    return self.call_method(expr, def, base, args);
                }
                Some(Resolution::BuiltinMethod { receiver, name }) => {
                    return self.call_builtin_method(expr, receiver, &name, base, args);
                }
                Some(Resolution::Variant(id, index)) => {
                    return self.construct_variant(id, index, args, expr.span);
                }
                Some(Resolution::ModuleItem(def)) => {
                    return self.call_named_fn(expr, def, args);
                }
                Some(Resolution::BuiltinItem(item)) => {
                    return self.call_builtin_module(&item, args, expr.span);
                }
                _ => {}
            }
        }
        if let ExprKind::Var(_) = &callee.kind {
            match self.program.resolution(callee.id).copied() {
                Some(Resolution::Fn(def))
                | Some(Resolution::Handler(def))
                | Some(Resolution::Test(def)) => {
                    return self.call_named_fn(expr, def, args);
                }
                Some(Resolution::BuiltinFn(function)) => {
                    return self.call_builtin(function, args, expr.span);
                }
                Some(Resolution::PreludeVariant(variant)) => {
                    return self.construct_prelude(variant, args, expr.span);
                }
                Some(Resolution::ModuleItem(def)) => {
                    return self.call_named_fn(expr, def, args);
                }
                Some(Resolution::BuiltinItem(item)) => {
                    return self.call_builtin_module(&item, args, expr.span);
                }
                _ => {}
            }
        }
        let callee_value = self.eval_expr(callee)?;
        let mut values = CallArgs::with_capacity(args.len());
        for arg in args {
            if arg.mutable {
                return self.trap(self.internal_panic("function values have no mut parameters"));
            }
            values.push(self.eval_expr(&arg.expr)?);
        }
        match callee_value {
            Value::Fn(closure) => self.call_closure(&closure, values).map_err(EvalError::Trap),
            Value::Builtin(function) => self.call_builtin_values(function, values, expr.span),
            Value::BuiltinModuleFn { module, name } => {
                self.call_builtin_module_values(&module, &name, values, expr.span)
            }
            _ => self.trap(self.internal_panic("callee is not callable")),
        }
    }

    fn call_named_fn(
        &mut self,
        expr: &Expr,
        def: DefId,
        args: &[martite_syntax::Arg],
    ) -> EvalResult<Value> {
        let program = self.program;
        let sig = program
            .fn_sig(def)
            .ok_or_else(|| EvalError::Trap(self.internal_panic("missing function")))?;
        if sig.params.len() != args.len() {
            return self.trap(self.internal_panic("argument count mismatch"));
        }
        let mut values = CallArgs::with_capacity(args.len());
        let mut writebacks: SmallVec<[(Place, usize); 2]> = SmallVec::new();
        for (i, arg) in args.iter().enumerate() {
            if arg.mutable {
                let place = self.eval_place(&arg.expr)?;
                let value = self.read_place(&place)?;
                values.push(value);
                writebacks.push((place, i));
            } else {
                values.push(self.eval_expr(&arg.expr)?);
            }
        }
        let (value, mut updated) = self
            .call_def_sig(def, sig, values)
            .map_err(EvalError::Trap)?;
        for (place, index) in writebacks {
            if let Some(value) = take_writeback(&mut updated, index) {
                self.write_place(place, value)?;
            }
        }
        let _ = expr;
        Ok(value)
    }

    fn call_method(
        &mut self,
        expr: &Expr,
        def: DefId,
        receiver: &Expr,
        args: &[martite_syntax::Arg],
    ) -> EvalResult<Value> {
        let program = self.program;
        let sig = program
            .fn_sig(def)
            .ok_or_else(|| EvalError::Trap(self.internal_panic("missing method")))?;
        if sig.params.len() != args.len() + 1 {
            return self.trap(self.internal_panic("method argument count mismatch"));
        }
        let mut values = CallArgs::with_capacity(args.len() + 1);
        let mut receiver_place = None;
        if sig.self_mutable() {
            let place = self.eval_place(receiver)?;
            values.push(self.read_place(&place)?);
            receiver_place = Some(place);
        } else {
            values.push(self.eval_expr(receiver)?);
        }
        let mut writebacks: SmallVec<[(Place, usize); 2]> = SmallVec::new();
        for (i, arg) in args.iter().enumerate() {
            if arg.mutable {
                let place = self.eval_place(&arg.expr)?;
                values.push(self.read_place(&place)?);
                writebacks.push((place, i + 1));
            } else {
                values.push(self.eval_expr(&arg.expr)?);
            }
        }
        let (value, mut updated) = self
            .call_def_sig(def, sig, values)
            .map_err(EvalError::Trap)?;
        if sig.self_mutable() {
            if let (Some(place), Some(new_value)) =
                (receiver_place, take_writeback(&mut updated, 0))
            {
                self.write_place(place, new_value)?;
            }
        }
        for (place, index) in writebacks {
            if let Some(value) = take_writeback(&mut updated, index) {
                self.write_place(place, value)?;
            }
        }
        let _ = expr;
        Ok(value)
    }

    fn construct_variant(
        &mut self,
        type_id: martite_check::TypeId,
        index: usize,
        args: &[martite_syntax::Arg],
        span: Span,
    ) -> EvalResult<Value> {
        let mut payload = Vec::new();
        for arg in args {
            if arg.mutable {
                return self.trap(self.internal_panic("variant arguments cannot be mut"));
            }
            payload.push(self.eval_expr(&arg.expr)?);
        }
        self.charge(payload.len() as u64)?;
        self.note_alloc(16 + 8 * payload.len() as u64);
        let value = Value::Enum(Rc::new(EnumValue {
            type_id,
            variant: index,
            payload,
        }));
        self.check_value_depth(&value, span)?;
        Ok(value)
    }

    fn construct_prelude(
        &mut self,
        variant: PreludeVariant,
        args: &[martite_syntax::Arg],
        span: Span,
    ) -> EvalResult<Value> {
        let mut values = CallArgs::with_capacity(args.len());
        for arg in args {
            values.push(self.eval_expr(&arg.expr)?);
        }
        let value = match variant {
            PreludeVariant::None => Value::Option(None),
            PreludeVariant::Some => {
                self.note_alloc(16);
                Value::Option(Some(Box::new(
                    values.into_iter().next().unwrap_or(Value::Unit),
                )))
            }
            PreludeVariant::Ok => {
                self.note_alloc(16);
                Value::Result(Ok(Box::new(
                    values.into_iter().next().unwrap_or(Value::Unit),
                )))
            }
            PreludeVariant::Err => {
                self.note_alloc(16);
                Value::Result(Err(Box::new(
                    values.into_iter().next().unwrap_or(Value::Unit),
                )))
            }
        };
        self.check_value_depth(&value, span)?;
        Ok(value)
    }

    // ------------------------------------------------------------------
    // Places
    // ------------------------------------------------------------------

    fn eval_place(&mut self, expr: &Expr) -> EvalResult<Place> {
        match &expr.kind {
            ExprKind::Var(_) => {
                let root = match self.program.resolution(expr.id) {
                    Some(Resolution::Local(id)) => *id,
                    _ => return self.trap(self.internal_panic("not a place")),
                };
                Ok(Place {
                    root,
                    accessors: Vec::new(),
                })
            }
            ExprKind::Field { base, name } => {
                let mut place = self.eval_place(base)?;
                let accessor = match self.program.resolution(expr.id).copied() {
                    Some(Resolution::StructField(index)) => Accessor::Field(index),
                    Some(Resolution::TupleField(index)) => Accessor::Tuple(index),
                    _ => {
                        let _ = name;
                        return self.trap(self.internal_panic("field is not assignable"));
                    }
                };
                place.accessors.push(accessor);
                Ok(place)
            }
            ExprKind::Index { base, index } => {
                let mut place = self.eval_place(base)?;
                let index = self.eval_expr(index)?;
                place.accessors.push(Accessor::Index(index));
                Ok(place)
            }
            _ => self.trap(self.internal_panic("expression is not a place")),
        }
    }

    fn read_place(&self, place: &Place) -> EvalResult<Value> {
        let mut value = self
            .local_value(place.root)
            .cloned()
            .ok_or_else(|| EvalError::Trap(self.internal_panic("unbound place root")))?;
        for accessor in &place.accessors {
            value = match (value, accessor) {
                (Value::Struct(struct_value), Accessor::Field(index)) => {
                    struct_value.fields.get(*index).cloned().ok_or_else(|| {
                        EvalError::Trap(self.internal_panic("struct field out of range"))
                    })?
                }
                (Value::Tuple(items), Accessor::Tuple(index)) => {
                    items.get(*index as usize).cloned().ok_or_else(|| {
                        EvalError::Trap(self.internal_panic("tuple field out of range"))
                    })?
                }
                (Value::List(items), Accessor::Index(Value::Int(index))) => {
                    let index = *index;
                    if index < 0 || index as usize >= items.len() {
                        return self.trap(self.internal_panic("index out of bounds"));
                    }
                    items[index as usize].clone()
                }
                (Value::Map(map), Accessor::Index(key)) => {
                    let key = key_from_value(key)
                        .map_err(|msg| EvalError::Trap(self.internal_panic(msg)))?;
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
        self.current_locals_mut().insert(place.root, root);
        Ok(())
    }

    fn take_place_root(&mut self, place: &Place) -> EvalResult<Value> {
        if let Some(value) = self.current_locals_mut().remove(place.root) {
            Ok(value)
        } else {
            self.local_value(place.root)
                .cloned()
                .ok_or_else(|| EvalError::Trap(self.internal_panic("unbound place root")))
        }
    }

    fn cow_vec_mut<'a>(
        &mut self,
        items: &'a mut Rc<Vec<Value>>,
        slots: usize,
    ) -> EvalResult<&'a mut Vec<Value>> {
        if Rc::strong_count(items) != 1 {
            self.charge(slots as u64)?;
            self.note_alloc(16 + 8 * slots as u64);
        }
        Ok(Rc::make_mut(items))
    }

    fn cow_map_mut<'a>(
        &mut self,
        map: &'a mut Rc<OrderedMap>,
        slots: usize,
    ) -> EvalResult<&'a mut OrderedMap> {
        if Rc::strong_count(map) != 1 {
            self.charge(slots as u64)?;
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
            self.charge(slots as u64)?;
            self.note_alloc(16 + 8 * slots as u64);
        }
        Ok(Rc::make_mut(value))
    }

    fn place_child_mut<'a>(
        &mut self,
        value: &'a mut Value,
        accessor: &Accessor,
    ) -> EvalResult<&'a mut Value> {
        match (value, accessor) {
            (Value::Struct(struct_value), Accessor::Field(index)) => {
                let slots = struct_value.fields.len();
                let struct_value = self.cow_struct_mut(struct_value, slots)?;
                struct_value.fields.get_mut(*index).ok_or_else(|| {
                    EvalError::Trap(self.internal_panic("struct field out of range"))
                })
            }
            (Value::Tuple(items), Accessor::Tuple(index)) => {
                let slots = items.len();
                let items = self.cow_vec_mut(items, slots)?;
                items
                    .get_mut(*index as usize)
                    .ok_or_else(|| EvalError::Trap(self.internal_panic("tuple field out of range")))
            }
            (Value::List(items), Accessor::Index(Value::Int(index))) => {
                let slots = items.len();
                let items = self.cow_vec_mut(items, slots)?;
                let index = *index;
                if index < 0 || index as usize >= items.len() {
                    return Err(EvalError::Trap(self.internal_panic("index out of bounds")));
                }
                Ok(&mut items[index as usize])
            }
            (Value::Map(map), Accessor::Index(key)) => {
                let key =
                    key_from_value(key).map_err(|msg| EvalError::Trap(self.internal_panic(msg)))?;
                let slots = map.len().saturating_mul(2);
                let map = self.cow_map_mut(map, slots)?;
                map.get_mut(&key)
                    .ok_or_else(|| EvalError::Trap(self.internal_panic("missing map key")))
            }
            _ => Err(EvalError::Trap(self.internal_panic("invalid place access"))),
        }
    }

    fn take_path_value(&mut self, value: &mut Value, accessors: &[Accessor]) -> EvalResult<Value> {
        let Some((first, rest)) = accessors.split_first() else {
            return Ok(std::mem::replace(value, Value::Unit));
        };
        let slot = self.place_child_mut(value, first)?;
        self.take_path_value(slot, rest)
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
                    return Err(EvalError::Trap(self.internal_panic("invalid place access")));
                };
                let slots = struct_value.fields.len();
                let struct_value = self.cow_struct_mut(struct_value, slots)?;
                let slot = struct_value.fields.get_mut(*index).ok_or_else(|| {
                    EvalError::Trap(self.internal_panic("struct field out of range"))
                })?;
                self.set_path(slot, rest, new_value, child_remaining)
            }
            Accessor::Tuple(index) => {
                let Value::Tuple(items) = value else {
                    return Err(EvalError::Trap(self.internal_panic("invalid place access")));
                };
                let slots = items.len();
                let items = self.cow_vec_mut(items, slots)?;
                let slot = items.get_mut(*index as usize).ok_or_else(|| {
                    EvalError::Trap(self.internal_panic("tuple field out of range"))
                })?;
                self.set_path(slot, rest, new_value, child_remaining)
            }
            Accessor::Index(index_value) => match value {
                Value::List(items) => {
                    let Value::Int(index) = index_value else {
                        return Err(EvalError::Trap(self.internal_panic("invalid place access")));
                    };
                    let slots = items.len();
                    let items = self.cow_vec_mut(items, slots)?;
                    let index = *index;
                    if index < 0 || index as usize >= items.len() {
                        return Err(EvalError::Trap(self.internal_panic("index out of bounds")));
                    }
                    self.set_path(&mut items[index as usize], rest, new_value, child_remaining)
                }
                Value::Map(map) => {
                    if value_depth_exceeds(index_value, child_remaining) {
                        return Err(EvalError::Trap(Trap::DepthExceeded));
                    }
                    let key = key_from_value(index_value)
                        .map_err(|msg| EvalError::Trap(self.internal_panic(msg)))?;
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
                        let slot = map.get_mut(&key).ok_or_else(|| {
                            EvalError::Trap(self.internal_panic("missing map key"))
                        })?;
                        self.set_path(slot, rest, new_value, child_remaining)
                    }
                }
                _ => Err(EvalError::Trap(self.internal_panic("invalid place access"))),
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

    // ------------------------------------------------------------------
    // Builtin calls (math/json/list/...)
    // ------------------------------------------------------------------

    fn call_builtin(
        &mut self,
        function: BuiltinFn,
        args: &[martite_syntax::Arg],
        span: Span,
    ) -> EvalResult<Value> {
        let mut values = CallArgs::with_capacity(args.len());
        for arg in args {
            values.push(self.eval_expr(&arg.expr)?);
        }
        self.call_builtin_values(function, values, span)
    }

    fn call_builtin_values(
        &mut self,
        function: BuiltinFn,
        values: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        match function {
            BuiltinFn::Print => {
                let value = values.into_iter().next().unwrap_or(Value::Unit);
                self.charge(show_slots(&value))?;
                let text = show(&value, &self.program.types, true);
                self.charge((text.len() as u64).div_ceil(8))?;
                self.log.push(text);
                self.maybe_check_memory()?;
                Ok(Value::Unit)
            }
            BuiltinFn::Panic => {
                let message = values
                    .into_iter()
                    .next()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| "panic".into());
                self.trap(self.panic(message, span))
            }
            BuiltinFn::Assert => {
                let mut iter = values.into_iter();
                let condition = iter.next().unwrap_or(Value::Bool(false));
                let message = iter
                    .next()
                    .and_then(|v| v.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| "assertion failed".into());
                if matches!(condition, Value::Bool(true)) {
                    Ok(Value::Unit)
                } else {
                    self.trap(self.panic(message, span))
                }
            }
        }
    }

    fn call_builtin_module(
        &mut self,
        item: &BuiltinItem,
        args: &[martite_syntax::Arg],
        span: Span,
    ) -> EvalResult<Value> {
        let mut values = CallArgs::with_capacity(args.len());
        for arg in args {
            values.push(self.eval_expr(&arg.expr)?);
        }
        self.call_builtin_module_values(item.module, item.name, values, span)
    }

    fn call_builtin_module_values(
        &mut self,
        module: &str,
        name: &str,
        values: CallArgs,
        span: Span,
    ) -> EvalResult<Value> {
        match (module, name) {
            ("std.math", "min") | ("std.math", "max") => {
                let mut iter = values.into_iter();
                let a = iter.next().unwrap_or(Value::Unit);
                let b = iter.next().unwrap_or(Value::Unit);
                let ord = value_ord(&a, &b);
                Ok(
                    if (name == "min" && ord.is_le()) || (name == "max" && ord.is_ge()) {
                        a
                    } else {
                        b
                    },
                )
            }
            ("std.math", "clamp") => {
                let mut iter = values.into_iter();
                let x = iter.next().unwrap_or(Value::Unit);
                let lo = iter.next().unwrap_or(Value::Unit);
                let hi = iter.next().unwrap_or(Value::Unit);
                if value_ord(&x, &lo).is_lt() {
                    Ok(lo)
                } else if value_ord(&x, &hi).is_gt() {
                    Ok(hi)
                } else {
                    Ok(x)
                }
            }
            ("std.math", name @ ("sin" | "cos" | "tan" | "exp" | "ln" | "log10")) => {
                let Value::Float(x) = values.first().cloned().unwrap_or(Value::Float(0.0)) else {
                    return self.trap(self.internal_panic("expected float"));
                };
                Ok(Value::Float(match name {
                    "sin" => libm::sin(x),
                    "cos" => libm::cos(x),
                    "tan" => libm::tan(x),
                    "exp" => libm::exp(x),
                    "ln" => libm::log(x),
                    _ => libm::log10(x),
                }))
            }
            ("std.math", "atan2") => {
                let mut iter = values.into_iter();
                let Value::Float(y) = iter.next().unwrap_or(Value::Float(0.0)) else {
                    return self.trap(self.internal_panic("expected float"));
                };
                let Value::Float(x) = iter.next().unwrap_or(Value::Float(0.0)) else {
                    return self.trap(self.internal_panic("expected float"));
                };
                Ok(Value::Float(libm::atan2(y, x)))
            }
            ("std.json", "parse") => {
                let text = values
                    .first()
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.charge((text.len() as u64).div_ceil(8))?;
                if text.len() as u64 > self.limits.memory {
                    return Err(EvalError::Trap(Trap::OutOfMemory));
                }
                let json_type = self.program.json_type;
                match crate::json::parse(&text, json_type) {
                    Ok(value) => {
                        self.check_value_depth(&value, span)?;
                        self.note_value(&value);
                        self.note_alloc(16);
                        Ok(Value::Result(Ok(Box::new(value))))
                    }
                    Err(message) => {
                        self.note_alloc(16 + 16 + message.len() as u64);
                        Ok(Value::Result(Err(Box::new(Value::str(message)))))
                    }
                }
            }
            ("std.json", "stringify") => {
                let value = values.into_iter().next().unwrap_or(Value::Unit);
                let text = crate::json::stringify(&value, &self.program.types);
                self.charge((text.len() as u64).div_ceil(8))?;
                self.note_alloc(16 + text.len() as u64);
                Ok(Value::str(text))
            }
            _ => self.trap(self.panic("unknown builtin function", span)),
        }
    }

    // ------------------------------------------------------------------
    // Pattern matching
    // ------------------------------------------------------------------

    fn bind_pattern(&mut self, pattern: &Pattern, value: &Value) {
        match &pattern.kind {
            PatKind::Bind(_) => {
                if let Some(id) = self.program.pattern_bindings.get(&pattern.id).copied() {
                    self.current_locals_mut().insert(id, value.clone());
                }
            }
            PatKind::Tuple(items) => {
                if let Value::Tuple(values) = value {
                    for (item, value) in items.iter().zip(values.iter()) {
                        self.bind_pattern(item, value);
                    }
                }
            }
            PatKind::Variant { args, .. } => {
                let Some(payload) = variant_payload(value) else {
                    return;
                };
                for (arg, value) in args.iter().zip(payload.iter()) {
                    self.bind_pattern(arg, value);
                }
            }
            PatKind::Struct { fields, .. } => {
                let Value::Struct(struct_value) = value else {
                    return;
                };
                let program = self.program;
                let Some(slots) = program.struct_pattern_fields.get(&pattern.id) else {
                    return;
                };
                for &(source, decl) in slots {
                    let Some(field) = fields.get(source) else {
                        continue;
                    };
                    let Some(value) = struct_value.fields.get(decl) else {
                        continue;
                    };
                    self.bind_pattern(&field.pattern, value);
                }
            }
            PatKind::Or(alternatives) => {
                if let Some(first) = alternatives.first() {
                    self.bind_pattern(first, value);
                }
            }
            _ => {}
        }
    }

    fn match_pattern(&mut self, pattern: &Pattern, value: &Value) -> EvalResult<bool> {
        match &pattern.kind {
            PatKind::Wild => Ok(true),
            PatKind::Bind(_) => {
                if let Some(id) = self.program.pattern_bindings.get(&pattern.id).copied() {
                    self.bind_match_local(id, value.clone());
                }
                Ok(true)
            }
            PatKind::Int(v) => Ok(matches!(value, Value::Int(actual) if actual == v)),
            PatKind::Bool(v) => Ok(matches!(value, Value::Bool(actual) if actual == v)),
            PatKind::Str(s) => Ok(matches!(value, Value::Str(actual) if actual.as_ref() == s)),
            PatKind::Tuple(items) => {
                let Value::Tuple(values) = value else {
                    return Ok(false);
                };
                if items.len() != values.len() {
                    return Ok(false);
                }
                for (item, value) in items.iter().zip(values.iter()) {
                    if !self.match_pattern(item, value)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            PatKind::Variant { args, .. } => {
                let Some(payload) = variant_payload(value) else {
                    return Ok(false);
                };
                let matched = match self.program.pattern_resolutions.get(&pattern.id) {
                    Some(PatResolution::Prelude(PreludeVariant::Some)) => {
                        matches!(value, Value::Option(Some(_)))
                    }
                    Some(PatResolution::Prelude(PreludeVariant::None)) => {
                        matches!(value, Value::Option(None))
                    }
                    Some(PatResolution::Prelude(PreludeVariant::Ok)) => {
                        matches!(value, Value::Result(Ok(_)))
                    }
                    Some(PatResolution::Prelude(PreludeVariant::Err)) => {
                        matches!(value, Value::Result(Err(_)))
                    }
                    Some(PatResolution::Variant(id, index)) => {
                        matches!(value, Value::Enum(e) if e.type_id == *id && e.variant == *index)
                    }
                    _ => true,
                };
                if !matched {
                    return Ok(false);
                }
                for (arg, value) in args.iter().zip(payload.iter()) {
                    if !self.match_pattern(arg, value)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            PatKind::Struct { fields, .. } => {
                let Value::Struct(struct_value) = value else {
                    return Ok(false);
                };
                let program = self.program;
                let Some(slots) = program.struct_pattern_fields.get(&pattern.id) else {
                    return Ok(false);
                };
                for &(source, decl) in slots {
                    let Some(field) = fields.get(source) else {
                        return Ok(false);
                    };
                    let Some(value) = struct_value.fields.get(decl) else {
                        return Ok(false);
                    };
                    if !self.match_pattern(&field.pattern, value)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            PatKind::Or(alternatives) => {
                let mark = self.locals_mark();
                for alternative in alternatives {
                    if self.match_pattern(alternative, value)? {
                        return Ok(true);
                    }
                    self.rollback_locals(mark);
                }
                Ok(false)
            }
        }
    }

    fn local_value(&self, id: BindingId) -> Option<&Value> {
        let frame = self.frames.last()?;
        frame.locals.get(id).or_else(|| {
            frame
                .captures
                .as_ref()
                .and_then(|captures| captures.get(&id))
        })
    }

    fn current_locals_mut(&mut self) -> &mut Locals {
        &mut self.frames.last_mut().expect("frame").locals
    }

    fn locals_mark(&self) -> usize {
        self.frames.last().expect("frame").undo.len()
    }

    fn bind_match_local(&mut self, id: BindingId, value: Value) {
        let frame = self.frames.last_mut().expect("frame");
        let previous = frame.locals.insert(id, value);
        frame.undo.push((id, previous));
    }

    fn rollback_locals(&mut self, mark: usize) {
        let frame = self.frames.last_mut().expect("frame");
        while frame.undo.len() > mark {
            let (id, previous) = frame.undo.pop().expect("undo entry");
            match previous {
                Some(value) => {
                    frame.locals.insert(id, value);
                }
                None => {
                    frame.locals.remove(id);
                }
            }
        }
    }

    fn commit_locals(&mut self, mark: usize) {
        self.frames.last_mut().expect("frame").undo.truncate(mark);
    }

    // ------------------------------------------------------------------
    // Arithmetic and values
    // ------------------------------------------------------------------

    fn eval_binary(
        &mut self,
        op: BinaryOp,
        lhs: &Expr,
        rhs: &Expr,
        span: Span,
    ) -> EvalResult<Value> {
        match op {
            BinaryOp::And => {
                let lhs = self.eval_expr(lhs)?;
                if !matches!(lhs, Value::Bool(true)) {
                    return Ok(Value::Bool(false));
                }
                self.eval_expr(rhs)
            }
            BinaryOp::Or => {
                let lhs = self.eval_expr(lhs)?;
                if matches!(lhs, Value::Bool(true)) {
                    return Ok(Value::Bool(true));
                }
                self.eval_expr(rhs)
            }
            BinaryOp::Coalesce => {
                let lhs = self.eval_expr(lhs)?;
                match lhs {
                    Value::Option(Some(value)) => Ok(*value),
                    Value::Option(None) => self.eval_expr(rhs),
                    _ => self.trap(self.internal_panic("`??` applied to non-option")),
                }
            }
            _ => {
                let lhs = self.eval_expr(lhs)?;
                let rhs = self.eval_expr(rhs)?;
                self.binary_values(op, lhs, rhs, span)
            }
        }
    }

    fn binary_values(
        &mut self,
        op: BinaryOp,
        lhs: Value,
        rhs: Value,
        span: Span,
    ) -> EvalResult<Value> {
        use BinaryOp::*;
        match op {
            Eq => {
                let cost = comparison_cost(&lhs).max(comparison_cost(&rhs));
                self.charge(cost)?;
                Ok(Value::Bool(value_eq(&lhs, &rhs)))
            }
            Ne => {
                let cost = comparison_cost(&lhs).max(comparison_cost(&rhs));
                self.charge(cost)?;
                Ok(Value::Bool(!value_eq(&lhs, &rhs)))
            }
            Lt | Le | Gt | Ge => {
                let cost = comparison_cost(&lhs).max(comparison_cost(&rhs));
                self.charge(cost)?;
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
                    self.charge((text.len() as u64).div_ceil(8))?;
                    self.note_alloc(16 + text.len() as u64);
                    Ok(Value::str(text))
                }
                _ => self.trap(self.internal_panic("invalid arithmetic operands")),
            },
            And | Or | Coalesce => self.trap(self.internal_panic("unexpected short-circuit op")),
        }
    }

    fn index_value(&mut self, base: Value, index: Value, span: Span) -> EvalResult<Value> {
        match (base, index) {
            (Value::List(items), Value::Int(i)) => {
                if i < 0 || i as usize >= items.len() {
                    return self.trap(self.panic("index out of bounds", span));
                }
                Ok(items[i as usize].clone())
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
                let (start, end) = match slice_bounds(start, end, inclusive, len) {
                    Some(bounds) => bounds,
                    None => return self.trap(self.panic("invalid list slice", span)),
                };
                let slice = items[start..end].to_vec();
                self.charge(slice.len() as u64)?;
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
                let (start, end) = match slice_bounds(start, end, inclusive, len) {
                    Some(bounds) => bounds,
                    None => return self.trap(self.panic("invalid string slice", span)),
                };
                let out: String = chars[start..end].iter().collect();
                self.charge((out.len() as u64).div_ceil(8))?;
                self.note_alloc(16 + out.len() as u64);
                Ok(Value::str(out))
            }
            (Value::Map(map), key) => {
                let key =
                    key_from_value(&key).map_err(|msg| EvalError::Trap(self.panic(msg, span)))?;
                map.get(&key)
                    .cloned()
                    .ok_or_else(|| EvalError::Trap(self.panic("missing map key", span)))
            }
            _ => self.trap(self.panic("invalid index operation", span)),
        }
    }

    fn materialize_const(&mut self, def: DefId, span: Span) -> EvalResult<Value> {
        let value = self
            .program
            .consts
            .get(&def)
            .map(|c| c.value.clone())
            .ok_or_else(|| EvalError::Trap(self.internal_panic("unknown constant")))?;
        let value = self.const_to_value(&value, span);
        self.note_value(&value);
        Ok(value)
    }

    fn const_to_value(&self, value: &ConstValue, span: Span) -> Value {
        let _ = span;
        match value {
            ConstValue::Unit => Value::Unit,
            ConstValue::Int(v) => Value::Int(*v),
            ConstValue::Float(v) => Value::Float(*v),
            ConstValue::Bool(v) => Value::Bool(*v),
            ConstValue::Str(v) => Value::str(v.clone()),
            ConstValue::Tuple(items) => {
                Value::tuple(items.iter().map(|v| self.const_to_value(v, span)).collect())
            }
            ConstValue::List(items) => {
                Value::list(items.iter().map(|v| self.const_to_value(v, span)).collect())
            }
            ConstValue::Map(entries) => {
                let mut map = OrderedMap::new();
                for (k, v) in entries {
                    let key_value = self.const_to_value(k, span);
                    if let Ok(key) = key_from_value(&key_value) {
                        map.insert(key, self.const_to_value(v, span));
                    }
                }
                Value::map(map)
            }
            ConstValue::Struct(type_id, fields) => {
                let mut values = Vec::new();
                if let Some(def) = self.program.types.get(*type_id) {
                    if let martite_check::TypeDefKind::Struct { fields: def_fields } = &def.kind {
                        for field in def_fields {
                            let value = fields
                                .iter()
                                .find(|(name, _)| name == &field.name)
                                .map(|(_, v)| self.const_to_value(v, span))
                                .unwrap_or(Value::Unit);
                            values.push(value);
                        }
                    }
                }
                Value::Struct(Rc::new(StructValue {
                    type_id: *type_id,
                    fields: values,
                }))
            }
            ConstValue::Enum(type_id, variant, payload) => Value::Enum(Rc::new(EnumValue {
                type_id: *type_id,
                variant: *variant,
                payload: payload
                    .iter()
                    .map(|v| self.const_to_value(v, span))
                    .collect(),
            })),
            ConstValue::Prelude(variant, payload) => {
                let value = payload
                    .first()
                    .map(|v| self.const_to_value(v, span))
                    .unwrap_or(Value::Unit);
                match variant {
                    PreludeVariant::None => Value::Option(None),
                    PreludeVariant::Some => Value::Option(Some(Box::new(value))),
                    PreludeVariant::Ok => Value::Result(Ok(Box::new(value))),
                    PreludeVariant::Err => Value::Result(Err(Box::new(value))),
                }
            }
        }
    }

    // ------------------------------------------------------------------
    // Builtin methods are continued in builtins.rs
    // ------------------------------------------------------------------
}

include!("builtins.rs");

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
