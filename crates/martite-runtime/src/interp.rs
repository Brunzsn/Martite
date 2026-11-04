use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use martite_base::{SourceId, Span};
use martite_check::{
    BindingId, BuiltinFn, BuiltinItem, BuiltinReceiver, CheckedProgram, ConstValue, DefId, DefKind,
    FnKind, PatResolution, PreludeVariant, Resolution,
};
use martite_syntax::{
    AssignOp, BinaryOp, Block, Expr, ExprKind, LambdaBody, MatchBody, PatKind, Pattern, Stmt,
    StmtKind, StrExprPart, UnaryOp,
};

use crate::ops::{show, value_eq, value_ord};
use crate::value::{
    key_from_value, key_to_value, Closure, ClosureTarget, EnumValue, OrderedMap, StructValue,
    Value,
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
    frames: Vec<Frame>,
    log: Vec<String>,
    host: Option<&'h mut dyn HostDispatcher>,
}

struct Frame {
    locals: HashMap<BindingId, Value>,
    source: SourceId,
    name: String,
}

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
        let (value, params) = self.call_def(def, args, None)?;
        Ok(CallResult { value, params })
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
        let sig = self
            .program
            .fn_sig(def)
            .ok_or_else(|| self.internal_panic("missing handler signature"))?
            .clone();
        let mut call_args = Vec::new();
        if sig.has_self() {
            call_args.push(state.clone().unwrap_or(Value::Unit));
        }
        call_args.extend(args);
        let (value, params) = self.call_def(def, call_args, None)?;
        if sig.self_param().is_some() && sig.self_mutable() {
            *state = params.first().cloned();
        }
        Ok(Some(value))
    }

    pub fn run_main(&mut self) -> Result<MainResult, Trap> {
        let main = self
            .program
            .main
            .ok_or_else(|| Trap::Panic {
                message: "no `main` function".into(),
                source: None,
                span: None,
                trace: Vec::new(),
            })?;
        self.reset();
        let (value, _) = self.call_def(main, Vec::new(), None)?;
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
    }

    fn internal_panic(&self, message: impl Into<String>) -> Trap {
        Trap::Panic {
            message: message.into(),
            source: self.frames.last().map(|f| f.source),
            span: None,
            trace: self.frames.iter().map(|f| f.name.clone()).collect(),
        }
    }

    fn panic(&self, message: impl Into<String>, span: Span) -> Trap {
        Trap::Panic {
            message: message.into(),
            source: self.frames.last().map(|f| f.source),
            span: Some(span),
            trace: self.frames.iter().map(|f| f.name.clone()).collect(),
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
            self.check_memory()?;
        }
        Ok(())
    }

    fn check_memory(&self) -> EvalResult<()> {
        let mut seen = HashSet::new();
        let mut total = 0u64;
        for frame in &self.frames {
            for value in frame.locals.values() {
                total = total.saturating_add(value.size_bytes(&mut seen));
            }
        }
        if total > self.limits.memory {
            Err(EvalError::Trap(Trap::OutOfMemory))
        } else {
            Ok(())
        }
    }

    fn call_def(
        &mut self,
        def: DefId,
        args: Vec<Value>,
        captures: Option<HashMap<BindingId, Value>>,
    ) -> Result<(Value, Vec<Value>), Trap> {
        if self.frames.len() >= self.limits.depth as usize {
            return Err(Trap::DepthExceeded);
        }
        let sig = self
            .program
            .fn_sig(def)
            .ok_or_else(|| self.internal_panic("missing function signature"))?
            .clone();
        if sig.kind == FnKind::Host {
            return self.call_host(def, args);
        }
        let body = self
            .program
            .function_block(def)
            .ok_or_else(|| self.internal_panic(format!("function `{}` has no body", sig.name)))?
            .clone();
        if args.len() != sig.params.len() {
            return Err(self.internal_panic(format!(
                "`{}` expected {} arguments, got {}",
                sig.name,
                sig.params.len(),
                args.len()
            )));
        }
        let source = self.program.resolved.module(sig.module).source;
        let mut locals = captures.unwrap_or_default();
        for (param, value) in sig.params.iter().zip(args.iter()) {
            locals.insert(param.binding, value.clone());
        }
        self.frames.push(Frame {
            locals,
            source,
            name: sig.name.clone(),
        });
        if let Err(trap) = self.check_memory() {
            self.frames.pop();
            return Err(match trap {
                EvalError::Trap(trap) => trap,
                _ => Trap::OutOfMemory,
            });
        }
        let result = match self.eval_block(&body) {
            Ok(value) => Ok(value),
            Err(EvalError::Return(value)) => Ok(value),
            Err(EvalError::Trap(trap)) => Err(trap),
            Err(EvalError::Break) | Err(EvalError::Continue) => Err(self.internal_panic(
                "`break`/`continue` escaped a loop",
            )),
        };
        let updated = {
            let frame = self.frames.last().unwrap();
            sig.params
                .iter()
                .map(|p| frame.locals.get(&p.binding).cloned().unwrap_or(Value::Unit))
                .collect::<Vec<_>>()
        };
        self.frames.pop();
        let value = result?;
        Ok((value, updated))
    }

    fn call_host(&mut self, def: DefId, args: Vec<Value>) -> Result<(Value, Vec<Value>), Trap> {
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
        self.fuel = self.fuel.saturating_sub(1 + cx.charged);
        if self.fuel == 0 {
            return Err(Trap::OutOfFuel);
        }
        match result {
            Ok(value) => Ok((value, args)),
            Err(fault) => Err(Trap::HostFault(fault.message)),
        }
    }

    fn call_closure(
        &mut self,
        closure: &Closure,
        args: Vec<Value>,
    ) -> Result<Value, Trap> {
        match &closure.target {
            ClosureTarget::Def(def) => self
                .call_def(*def, args, Some(closure.captures.clone()))
                .map(|(value, _)| value),
            ClosureTarget::Lambda { params, body } => {
                if params.len() != args.len() {
                    return Err(self.internal_panic("lambda argument count mismatch"));
                }
                if self.frames.len() >= self.limits.depth as usize {
                    return Err(Trap::DepthExceeded);
                }
                let source = self
                    .frames
                    .last()
                    .map(|f| f.source)
                    .unwrap_or(SourceId(0));
                self.frames.push(Frame {
                    locals: closure.captures.clone(),
                    source,
                    name: "<lambda>".into(),
                });
                for (param, value) in params.iter().zip(args.iter()) {
                    self.bind_pattern(&param.pattern, value);
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
                result
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
                self.check_memory()?;
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
                    let lhs = self.read_place(&place)?;
                    let rhs = self.eval_expr(rhs)?;
                    let value = match op {
                        AssignOp::Assign => rhs,
                        AssignOp::Add => self.binary_values(BinaryOp::Add, lhs, rhs, stmt.span)?,
                        AssignOp::Sub => self.binary_values(BinaryOp::Sub, lhs, rhs, stmt.span)?,
                        AssignOp::Mul => self.binary_values(BinaryOp::Mul, lhs, rhs, stmt.span)?,
                        AssignOp::Div => self.binary_values(BinaryOp::Div, lhs, rhs, stmt.span)?,
                        AssignOp::Rem => self.binary_values(BinaryOp::Rem, lhs, rhs, stmt.span)?,
                    };
                    self.write_place(place, value)?;
                } else {
                    self.eval_expr(expr)?;
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
                let entries = map.entries().to_vec();
                for (key, value) in entries {
                    self.charge(1)?;
                    let pair = Value::tuple(vec![key_to_value(&key), value]);
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
                let last = if *inclusive { *end } else { end.saturating_sub(1) };
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
                Ok(Value::tuple(values))
            }
            ExprKind::List(items) => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(self.eval_expr(item)?);
                }
                self.charge(values.len() as u64)?;
                Ok(Value::list(values))
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
                Ok(Value::map(map))
            }
            ExprKind::StructLit { fields, base, .. } => {
                self.eval_struct_lit(expr, fields, base.as_deref())
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
                    let saved = self.current_locals().clone();
                    if self.match_pattern(&arm.pattern, &value)? {
                        if let Some(guard) = &arm.guard {
                            let pass = self.eval_expr(guard)?;
                            if !matches!(pass, Value::Bool(true)) {
                                *self.current_locals_mut() = saved;
                                continue;
                            }
                        }
                        return match &arm.body {
                            MatchBody::Block(block) => self.eval_block(block),
                            MatchBody::Expr(expr) => self.eval_expr(expr),
                        };
                    }
                    *self.current_locals_mut() = saved;
                }
                self.trap(self.internal_panic("match fell through"))
            }
            ExprKind::Lambda { params, body, .. } => Ok(Value::Fn(Rc::new(Closure {
                target: ClosureTarget::Lambda {
                    params: params.clone(),
                    body: body.clone(),
                },
                captures: self.current_locals().clone(),
            }))),
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
                        if !f.is_finite() || f >= 9_223_372_036_854_775_808.0 || f < -9_223_372_036_854_775_808.0
                        {
                            self.trap(self.panic("float is out of int range", expr.span))
                        } else {
                            Ok(Value::Int(f.trunc() as i64))
                        }
                    }
                    (martite_syntax::PrimTy::Float, Value::Int(v)) => Ok(Value::Float(v as f64)),
                    (martite_syntax::PrimTy::Str, value) => {
                        Ok(Value::str(show(&value, &self.program.types, true)))
                    }
                    _ => self.trap(self.internal_panic("invalid conversion")),
                }
            }
        }
    }

    fn eval_string(&mut self, parts: &[StrExprPart], span: Span) -> EvalResult<Value> {
        let mut out = String::new();
        for part in parts {
            match part {
                StrExprPart::Text(text) => out.push_str(text),
                StrExprPart::Expr(expr) => {
                    let value = self.eval_expr(expr)?;
                    out.push_str(&show(&value, &self.program.types, true));
                }
            }
        }
        let _ = span;
        self.charge((out.len() as u64).div_ceil(8))?;
        Ok(Value::str(out))
    }

    fn eval_var(&mut self, expr: &Expr) -> EvalResult<Value> {
        let ExprKind::Var(_) = &expr.kind else {
            unreachable!()
        };
        match self.program.resolution(expr.id).cloned() {
            Some(Resolution::Local(id)) => self
                .current_locals()
                .get(&id)
                .cloned()
                .ok_or_else(|| EvalError::Trap(self.internal_panic("unbound local"))),
            Some(Resolution::Const(def)) => self.materialize_const(def, expr.span),
            Some(Resolution::Fn(def))
            | Some(Resolution::Handler(def))
            | Some(Resolution::Test(def)) => Ok(Value::Fn(Rc::new(Closure {
                target: ClosureTarget::Def(def),
                captures: self.current_locals().clone(),
            }))),
            Some(Resolution::PreludeVariant(PreludeVariant::None)) => {
                Ok(Value::Option(None))
            }
            Some(Resolution::PreludeVariant(variant)) => Ok(Value::Result(match variant {
                PreludeVariant::Ok => Ok(Box::new(Value::Unit)),
                PreludeVariant::Err => Err(Box::new(Value::Unit)),
                _ => Ok(Box::new(Value::Unit)),
            })),
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
            return Ok(self.const_to_value(&value, span));
        }
        Ok(Value::Fn(Rc::new(Closure {
            target: ClosureTarget::Def(def),
            captures: self.current_locals().clone(),
        })))
    }

    fn value_of_builtin_item(&self, item: &BuiltinItem) -> EvalResult<Value> {
        match (item.module.as_str(), item.name.as_str()) {
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
        match self.program.resolution(expr.id).cloned() {
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
                    Value::Tuple(items) => Ok(items.get(index as usize).cloned().unwrap_or(Value::Unit)),
                    _ => self.trap(self.internal_panic("tuple field access on non-tuple")),
                }
            }
            Some(Resolution::Variant(id, index)) => Ok(Value::Enum(Rc::new(EnumValue {
                type_id: id,
                variant: index,
                payload: Vec::new(),
            }))),
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
        let Some(Resolution::TypeName(type_id)) = self.program.resolution(expr.id).cloned() else {
            return self.trap(self.internal_panic("struct literal has no type"));
        };
        let mut values: HashMap<String, Value> = HashMap::new();
        if let Some(base) = base {
            let base = self.eval_expr(base)?;
            if let Value::Struct(struct_value) = base {
                if let Some(def) = self.program.types.get(struct_value.type_id) {
                    if let martite_check::TypeDefKind::Struct { fields } = &def.kind {
                        for (field, value) in fields.iter().zip(struct_value.fields.iter()) {
                            values.insert(field.name.clone(), value.clone());
                        }
                    }
                }
            }
        }
        for field in fields {
            if let Some(value) = &field.value {
                let value = self.eval_expr(value)?;
                values.insert(field.name.clone(), value);
            }
        }
        let Some(def) = self.program.types.get(type_id) else {
            return self.trap(self.internal_panic("unknown struct type"));
        };
        let mut out = Vec::new();
        if let martite_check::TypeDefKind::Struct { fields } = &def.kind {
            for field in fields {
                out.push(values.remove(&field.name).unwrap_or(Value::Unit));
            }
        }
        self.charge(out.len() as u64)?;
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
            match self.program.resolution(callee.id).cloned() {
                Some(Resolution::Method(def)) => {
                    return self.call_method(expr, def, base, args);
                }
                Some(Resolution::BuiltinMethod { receiver, name }) => {
                    return self.call_builtin_method(expr, receiver, &name, base, args);
                }
                Some(Resolution::Variant(id, index)) => {
                    return self.construct_variant(id, index, args);
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
            match self.program.resolution(callee.id).cloned() {
                Some(Resolution::Fn(def))
                | Some(Resolution::Handler(def))
                | Some(Resolution::Test(def)) => {
                    return self.call_named_fn(expr, def, args);
                }
                Some(Resolution::BuiltinFn(function)) => {
                    return self.call_builtin(function, args, expr.span);
                }
                Some(Resolution::PreludeVariant(variant)) => {
                    return self.construct_prelude(variant, args);
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
        let mut values = Vec::with_capacity(args.len());
        for arg in args {
            if arg.mutable {
                return self.trap(self.internal_panic("function values have no mut parameters"));
            }
            values.push(self.eval_expr(&arg.expr)?);
        }
        match callee_value {
            Value::Fn(closure) => self
                .call_closure(&closure, values)
                .map_err(EvalError::Trap),
            Value::Builtin(function) => self.call_builtin_values(function, values, expr.span),
            Value::BuiltinModuleFn { module, name } => {
                let item = BuiltinItem {
                    module,
                    name,
                    kind: DefKind::Fn,
                };
                self.call_builtin_module_values(&item, values, expr.span)
            }
            _ => self.trap(self.internal_panic("callee is not callable")),
        }
    }

    fn call_named_fn(&mut self, expr: &Expr, def: DefId, args: &[martite_syntax::Arg]) -> EvalResult<Value> {
        let sig = self
            .program
            .fn_sig(def)
            .cloned()
            .ok_or_else(|| EvalError::Trap(self.internal_panic("missing function")))?;
        if sig.params.len() != args.len() {
            return self.trap(self.internal_panic("argument count mismatch"));
        }
        let mut values = Vec::with_capacity(args.len());
        let mut writebacks: Vec<(Place, usize)> = Vec::new();
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
        let (value, params) = self
            .call_def(def, values, None)
            .map_err(EvalError::Trap)?;
        for (place, index) in writebacks {
            if let Some(value) = params.get(index).cloned() {
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
        let sig = self
            .program
            .fn_sig(def)
            .cloned()
            .ok_or_else(|| EvalError::Trap(self.internal_panic("missing method")))?;
        if sig.params.len() != args.len() + 1 {
            return self.trap(self.internal_panic("method argument count mismatch"));
        }
        let mut values = Vec::with_capacity(args.len() + 1);
        let mut receiver_place = None;
        if sig.self_mutable() {
            let place = self.eval_place(receiver)?;
            values.push(self.read_place(&place)?);
            receiver_place = Some(place);
        } else {
            values.push(self.eval_expr(receiver)?);
        }
        let mut writebacks: Vec<(Place, usize)> = Vec::new();
        for (i, arg) in args.iter().enumerate() {
            if arg.mutable {
                let place = self.eval_place(&arg.expr)?;
                values.push(self.read_place(&place)?);
                writebacks.push((place, i + 1));
            } else {
                values.push(self.eval_expr(&arg.expr)?);
            }
        }
        let (value, params) = self
            .call_def(def, values, None)
            .map_err(EvalError::Trap)?;
        if sig.self_mutable() {
            if let (Some(place), Some(new_value)) = (receiver_place, params.first().cloned()) {
                self.write_place(place, new_value)?;
            }
        }
        for (place, index) in writebacks {
            if let Some(value) = params.get(index).cloned() {
                self.write_place(place, value)?;
            }
        }
        let _ = expr;
        Ok(value)
    }

    fn construct_variant(&mut self, type_id: martite_check::TypeId, index: usize, args: &[martite_syntax::Arg]) -> EvalResult<Value> {
        let mut payload = Vec::new();
        for arg in args {
            if arg.mutable {
                return self.trap(self.internal_panic("variant arguments cannot be mut"));
            }
            payload.push(self.eval_expr(&arg.expr)?);
        }
        self.charge(payload.len() as u64)?;
        Ok(Value::Enum(Rc::new(EnumValue {
            type_id,
            variant: index,
            payload,
        })))
    }

    fn construct_prelude(&mut self, variant: PreludeVariant, args: &[martite_syntax::Arg]) -> EvalResult<Value> {
        let mut values = Vec::new();
        for arg in args {
            values.push(self.eval_expr(&arg.expr)?);
        }
        match variant {
            PreludeVariant::None => Ok(Value::Option(None)),
            PreludeVariant::Some => Ok(Value::Option(Some(Box::new(
                values.into_iter().next().unwrap_or(Value::Unit),
            )))),
            PreludeVariant::Ok => Ok(Value::Result(Ok(Box::new(
                values.into_iter().next().unwrap_or(Value::Unit),
            )))),
            PreludeVariant::Err => Ok(Value::Result(Err(Box::new(
                values.into_iter().next().unwrap_or(Value::Unit),
            )))),
        }
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
                let accessor = match self.program.resolution(expr.id).cloned() {
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
            .current_locals()
            .get(&place.root)
            .cloned()
            .ok_or_else(|| EvalError::Trap(self.internal_panic("unbound place root")))?;
        for accessor in &place.accessors {
            value = match (value, accessor) {
                (Value::Struct(struct_value), Accessor::Field(index)) => struct_value
                    .fields
                    .get(*index)
                    .cloned()
                    .ok_or_else(|| EvalError::Trap(self.internal_panic("struct field out of range")))?,
                (Value::Tuple(items), Accessor::Tuple(index)) => items
                    .get(*index as usize)
                    .cloned()
                    .ok_or_else(|| EvalError::Trap(self.internal_panic("tuple field out of range")))?,
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
        let mut root = self
            .current_locals()
            .get(&place.root)
            .cloned()
            .ok_or_else(|| EvalError::Trap(self.internal_panic("unbound place root")))?;
        set_path(&mut root, &place.accessors, value)
            .map_err(|msg| EvalError::Trap(self.internal_panic(msg)))?;
        self.current_locals_mut().insert(place.root, root);
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
        let mut values = Vec::new();
        for arg in args {
            values.push(self.eval_expr(&arg.expr)?);
        }
        self.call_builtin_values(function, values, span)
    }

    fn call_builtin_values(
        &mut self,
        function: BuiltinFn,
        values: Vec<Value>,
        span: Span,
    ) -> EvalResult<Value> {
        match function {
            BuiltinFn::Print => {
                let value = values.into_iter().next().unwrap_or(Value::Unit);
                let text = show(&value, &self.program.types, true);
                self.charge((text.len() as u64).div_ceil(8))?;
                self.log.push(text);
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
        let mut values = Vec::new();
        for arg in args {
            values.push(self.eval_expr(&arg.expr)?);
        }
        self.call_builtin_module_values(item, values, span)
    }

    fn call_builtin_module_values(
        &mut self,
        item: &BuiltinItem,
        values: Vec<Value>,
        span: Span,
    ) -> EvalResult<Value> {
        match (item.module.as_str(), item.name.as_str()) {
            ("std.math", "min") | ("std.math", "max") => {
                let mut iter = values.into_iter();
                let a = iter.next().unwrap_or(Value::Unit);
                let b = iter.next().unwrap_or(Value::Unit);
                let ord = value_ord(&a, &b);
                Ok(if (item.name == "min" && ord.is_le()) || (item.name == "max" && ord.is_ge()) {
                    a
                } else {
                    b
                })
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
                let json_type = self.program.json_type;
                match crate::json::parse(&text, json_type) {
                    Ok(value) => Ok(Value::Result(Ok(Box::new(value)))),
                    Err(message) => Ok(Value::Result(Err(Box::new(Value::str(message))))),
                }
            }
            ("std.json", "stringify") => {
                let value = values.into_iter().next().unwrap_or(Value::Unit);
                Ok(Value::str(crate::json::stringify(
                    &value,
                    &self.program.types,
                )))
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
                if let Some(id) = self.program.pattern_bindings.get(&pattern.id) {
                    self.current_locals_mut().insert(*id, value.clone());
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
                let payload: Vec<Value> = match value {
                    Value::Option(Some(v)) => vec![(**v).clone()],
                    Value::Option(None) => Vec::new(),
                    Value::Result(Ok(v)) => vec![(**v).clone()],
                    Value::Result(Err(v)) => vec![(**v).clone()],
                    Value::Enum(e) => e.payload.clone(),
                    _ => Vec::new(),
                };
                for (arg, value) in args.iter().zip(payload.iter()) {
                    self.bind_pattern(arg, value);
                }
            }
            PatKind::Struct { fields, .. } => {
                if let Value::Struct(struct_value) = value {
                    if let Some(def) = self.program.types.get(struct_value.type_id) {
                        if let martite_check::TypeDefKind::Struct { fields: def_fields } = &def.kind
                        {
                            for field in fields {
                                if let Some(index) =
                                    def_fields.iter().position(|f| f.name == field.name)
                                {
                                    if let Some(value) = struct_value.fields.get(index) {
                                        self.bind_pattern(&field.pattern, value);
                                    }
                                }
                            }
                        }
                    }
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
                if let Some(id) = self.program.pattern_bindings.get(&pattern.id) {
                    self.current_locals_mut().insert(*id, value.clone());
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
                let payload: Vec<Value> = match value {
                    Value::Option(Some(v)) => vec![(**v).clone()],
                    Value::Option(None) => Vec::new(),
                    Value::Result(Ok(v)) => vec![(**v).clone()],
                    Value::Result(Err(v)) => vec![(**v).clone()],
                    Value::Enum(e) => e.payload.clone(),
                    _ => return Ok(false),
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
                let Some(def) = self.program.types.get(struct_value.type_id) else {
                    return Ok(false);
                };
                let martite_check::TypeDefKind::Struct { fields: def_fields } = &def.kind else {
                    return Ok(false);
                };
                for field in fields {
                    let Some(index) = def_fields.iter().position(|f| f.name == field.name) else {
                        return Ok(false);
                    };
                    let Some(value) = struct_value.fields.get(index) else {
                        return Ok(false);
                    };
                    if !self.match_pattern(&field.pattern, value)? {
                        return Ok(false);
                    }
                }
                Ok(true)
            }
            PatKind::Or(alternatives) => {
                let saved = self.current_locals().clone();
                for alternative in alternatives {
                    *self.current_locals_mut() = saved.clone();
                    if self.match_pattern(alternative, value)? {
                        return Ok(true);
                    }
                }
                *self.current_locals_mut() = saved;
                Ok(false)
            }
        }
    }

    fn current_locals(&self) -> &HashMap<BindingId, Value> {
        &self.frames.last().expect("frame").locals
    }

    fn current_locals_mut(&mut self) -> &mut HashMap<BindingId, Value> {
        &mut self.frames.last_mut().expect("frame").locals
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
            (Value::List(items), Value::Range { start, end, inclusive }) => {
                let len = items.len() as i64;
                let (start, end) = match slice_bounds(start, end, inclusive, len) {
                    Some(bounds) => bounds,
                    None => return self.trap(self.panic("invalid list slice", span)),
                };
                let slice = items[start..end].to_vec();
                self.charge(slice.len() as u64)?;
                Ok(Value::list(slice))
            }
            (Value::Str(text), Value::Range { start, end, inclusive }) => {
                let chars: Vec<char> = text.chars().collect();
                let len = chars.len() as i64;
                let (start, end) = match slice_bounds(start, end, inclusive, len) {
                    Some(bounds) => bounds,
                    None => return self.trap(self.panic("invalid string slice", span)),
                };
                let out: String = chars[start..end].iter().collect();
                self.charge((out.len() as u64).div_ceil(8))?;
                Ok(Value::str(out))
            }
            (Value::Map(map), key) => {
                let key = key_from_value(&key)
                    .map_err(|msg| EvalError::Trap(self.panic(msg, span)))?;
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
        Ok(self.const_to_value(&value, span))
    }

    fn const_to_value(&self, value: &ConstValue, span: Span) -> Value {
        let _ = span;
        match value {
            ConstValue::Unit => Value::Unit,
            ConstValue::Int(v) => Value::Int(*v),
            ConstValue::Float(v) => Value::Float(*v),
            ConstValue::Bool(v) => Value::Bool(*v),
            ConstValue::Str(v) => Value::str(v.clone()),
            ConstValue::Tuple(items) => Value::tuple(
                items
                    .iter()
                    .map(|v| self.const_to_value(v, span))
                    .collect(),
            ),
            ConstValue::List(items) => Value::list(
                items
                    .iter()
                    .map(|v| self.const_to_value(v, span))
                    .collect(),
            ),
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

fn slice_bounds(start: i64, end: i64, inclusive: bool, len: i64) -> Option<(usize, usize)> {
    if start < 0 || end < start {
        return None;
    }
    let end = if inclusive {
        end.checked_add(1)?
    } else {
        end
    };
    if end > len {
        return None;
    }
    Some((start as usize, end as usize))
}

fn set_path(value: &mut Value, path: &[Accessor], new_value: Value) -> Result<(), String> {
    let Some((first, rest)) = path.split_first() else {
        *value = new_value;
        return Ok(());
    };
    match (value, first) {
        (Value::Struct(struct_value), Accessor::Field(index)) => {
            let struct_value = Rc::make_mut(struct_value);
            let slot = struct_value
                .fields
                .get_mut(*index)
                .ok_or_else(|| "struct field out of range".to_string())?;
            set_path(slot, rest, new_value)
        }
        (Value::Tuple(items), Accessor::Tuple(index)) => {
            let items = Rc::make_mut(items);
            let slot = items
                .get_mut(*index as usize)
                .ok_or_else(|| "tuple field out of range".to_string())?;
            set_path(slot, rest, new_value)
        }
        (Value::List(items), Accessor::Index(Value::Int(index))) => {
            let items = Rc::make_mut(items);
            if *index < 0 || *index as usize >= items.len() {
                return Err("index out of bounds".into());
            }
            set_path(&mut items[*index as usize], rest, new_value)
        }
        (Value::Map(map), Accessor::Index(key)) => {
            let key = key_from_value(key)?;
            let map = Rc::make_mut(map);
            if rest.is_empty() {
                map.insert(key, new_value);
                Ok(())
            } else {
                let slot = map
                    .get_mut(&key)
                    .ok_or_else(|| "missing map key".to_string())?;
                set_path(slot, rest, new_value)
            }
        }
        _ => Err("invalid place access".into()),
    }
}
