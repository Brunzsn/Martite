//! Recursive evaluator for the resolved tree IR.
//!
//! This is the first evaluator slice (#108): values, places, scalar operators,
//! aggregates and core control flow. Calls, builtins, hosts and mutable
//! writeback land in #109; closures, captures and `match` land in #110.
//!
//! The evaluator only reads [`martite_ir::IrProgram`]. Frame slots are a dense
//! `Vec<Option<Value>>` sized by `IrFn::locals`; there are no binding-id scans
//! and no per-frame hash maps. `None` marks a slot taken by `SlotRead` with
//! [`martite_ir::AccessMode::Take`], which is a `Reduce`-class feature and not
//! emitted by migration lowering.

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Instant;

use martite_base::{SourceId, Span};
use martite_fuel as fuel;
use martite_ir::{
    AccessMode, BuiltinFnId, BuiltinItemId, ChildRange, ConstId, ConstValue, FnId, IrFn, IrId,
    IrProgram, NodeKind, Pattern, PreludeVariant, Slot, StrId, StructLayoutId, Ty, TypeDefKind,
    TypeId,
};
use smallvec::SmallVec;

use crate::interp::{CallResult, ExecLimits, MainResult, Trap};
use crate::ops::{show, value_eq, value_ord, write_show};
use crate::value::{
    EnumValue, FnValue, MAX_VALUE_DEPTH, MapValue, OrderedMap, SeqValue, StructValue, Value,
    key_from_value, key_value_size, value_depth_exceeds,
};

type EvalResult<T> = Result<T, EvalError>;

#[derive(Debug)]
enum EvalError {
    Trap(Trap),
    Return(Value),
    Break,
    Continue,
}

struct Frame<'p> {
    function: FnId,
    /// One entry per declared local. `None` is a slot whose value was taken.
    slots: Vec<Option<Value>>,
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

/// Executes one [`IrProgram`] with the same limits and observable accounting
/// rules as the AST interpreter.
pub struct IrRuntime<'p> {
    program: &'p IrProgram,
    limits: ExecLimits,
    fuel: u64,
    start: Instant,
    ticks: u64,
    frames: Vec<Frame<'p>>,
    log: Vec<String>,
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

impl<'p> IrRuntime<'p> {
    pub fn new(program: &'p IrProgram, limits: ExecLimits) -> Self {
        Self {
            program,
            limits,
            fuel: limits.fuel,
            start: Instant::now(),
            ticks: 0,
            frames: Vec::new(),
            log: Vec::new(),
            census_bytes: 0,
            pending_bytes: 0,
            const_values: vec![None; program.consts.len()],
            literal_values: vec![None; program.strs.len()],
        }
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
        let value = self.call_function(function, args)?;
        Ok(CallResult {
            value,
            params: Vec::new(),
        })
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
        let value = self.call_function(main, Vec::new())?;
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

    fn current_function(&self) -> &'p IrFn {
        let frame = self.frames.last().expect("evaluation frame");
        self.program
            .function(frame.function)
            .expect("frame function exists")
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

    fn call_function(&mut self, function: FnId, args: Vec<Value>) -> Result<Value, Trap> {
        if self.frames.len() >= self.limits.depth as usize {
            return Err(Trap::DepthExceeded);
        }
        let program = self.program;
        let function_info = program
            .function(function)
            .ok_or_else(|| self.internal_panic(format!("missing function #{:?}", function)))?;
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
        if let Some(param) = function_info
            .params
            .iter()
            .find(|param| param.pattern.is_some())
        {
            return Err(self.internal_panic(format!(
                "destructured parameter `{}` is not implemented until #110",
                param.name
            )));
        }

        let mut slots = vec![None; function_info.locals as usize];
        for (param, value) in function_info.params.iter().zip(args) {
            let Some(slot) = slots.get_mut(param.slot.index()) else {
                return Err(self.internal_panic("parameter slot is out of range"));
            };
            *slot = Some(value);
        }
        let frame = Frame {
            function,
            slots,
            source: function_info.source,
            name: function_info.name.as_ref(),
        };
        self.frames.push(frame);
        if let Err(trap) = self.maybe_check_memory() {
            self.frames.pop();
            return Err(match trap {
                EvalError::Trap(trap) => trap,
                _ => Trap::OutOfMemory,
            });
        }

        let body = function_info.body.expect("body checked above");
        let result = match self.eval(body) {
            Ok(value) => Ok(value),
            Err(EvalError::Return(value)) => Ok(value),
            Err(EvalError::Trap(trap)) => Err(trap),
            Err(EvalError::Break) | Err(EvalError::Continue) => {
                Err(self.internal_panic("`break`/`continue` escaped a loop"))
            }
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
        Ok(value)
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
        let Some(node) = function.node(id).copied() else {
            return self.trap(self.internal_panic("IR node id is out of range"));
        };
        let span = function.span(id).unwrap_or(Span::empty(0));
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

            NodeKind::SlotRead { slot, mode } => self.read_slot(slot, mode),
            NodeKind::CaptureRead(_) => {
                self.trap(self.internal_panic("capture reads are not implemented until #110"))
            }
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

            NodeKind::CallDirect { .. }
            | NodeKind::CallMethod { .. }
            | NodeKind::CallValue { .. }
            | NodeKind::CallBuiltinMethod { .. }
            | NodeKind::CallBuiltinModule { .. }
            | NodeKind::CallHost { .. } => {
                self.trap(self.internal_panic("calls are not implemented until #109"))
            }
            NodeKind::CallBuiltinFn { builtin, first_arg } => {
                self.eval_builtin_fn(builtin, first_arg, id, span)
            }

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
            NodeKind::Match { .. } => {
                self.trap(self.internal_panic("match is not implemented until #110"))
            }
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

            NodeKind::MakeClosure(_) => {
                self.trap(self.internal_panic("closures are not implemented until #110"))
            }

            NodeKind::StrInterp(range) => self.eval_string(range),
            NodeKind::Assert(range) => self.eval_assert(range, span),
            NodeKind::Panic(range) => self.eval_panic(range, span),
            NodeKind::MutArg(_) => {
                self.trap(self.internal_panic("mutable arguments are not implemented until #109"))
            }
            NodeKind::Charge(units) => {
                self.charge(u64::from(units))?;
                Ok(Value::Unit)
            }
        }
    }

    fn eval_int_constant(&mut self, id: ConstId) -> EvalResult<Value> {
        match self.program.constant(id) {
            Some(ConstValue::Int(value)) => Ok(Value::Int(*value)),
            _ => self.trap(self.internal_panic("int literal has an invalid const pool entry")),
        }
    }

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

    fn read_slot(&mut self, slot: Slot, mode: AccessMode) -> EvalResult<Value> {
        let index = slot.index();
        let some_frame = self
            .frames
            .last()
            .is_some_and(|frame| index < frame.slots.len());
        if !some_frame {
            return self.trap(self.internal_panic("slot is out of range"));
        }
        let frame = self.frames.last_mut().expect("evaluation frame");
        match mode {
            AccessMode::Share => Ok(frame.slots[index]
                .clone()
                .expect("verifier guarantees every shared slot read is initialized")),
            AccessMode::Take => Ok(frame.slots[index]
                .take()
                .expect("verifier guarantees every taken slot is initialized")),
        }
    }

    fn set_slot(&mut self, slot: Slot, value: Value) {
        if let Some(cell) = self
            .frames
            .last_mut()
            .expect("evaluation frame")
            .slots
            .get_mut(slot.index())
        {
            *cell = Some(value);
        }
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

    fn eval_block(&mut self, range: ChildRange) -> EvalResult<Value> {
        let function = self.current_function();
        let mut value = Value::Unit;
        for offset in 0..range.len {
            let Some(child) = function
                .children
                .get(range.start as usize + offset as usize)
                .copied()
            else {
                return self.trap(self.internal_panic("block child is out of range"));
            };
            value = self.eval(child)?;
        }
        Ok(value)
    }

    fn eval_if(&mut self, range: ChildRange) -> EvalResult<Value> {
        let function = self.current_function();
        let Some(cond) = function.children.get(range.start as usize).copied() else {
            return self.trap(self.internal_panic("if has no condition"));
        };
        let Some(then_branch) = function.children.get(range.start as usize + 1).copied() else {
            return self.trap(self.internal_panic("if has no then branch"));
        };
        let else_branch = (range.len >= 3)
            .then(|| function.children.get(range.start as usize + 2).copied())
            .flatten();
        let cond = self.eval(cond)?;
        if matches!(cond, Value::Bool(true)) {
            self.eval(then_branch)
        } else if let Some(else_branch) = else_branch {
            self.eval(else_branch)
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

    fn eval_builtin_fn(
        &mut self,
        builtin: BuiltinFnId,
        first_arg: u32,
        node: IrId,
        _span: Span,
    ) -> EvalResult<Value> {
        if builtin != BuiltinFnId::PRINT {
            return self.trap(self.internal_panic("builtin calls are not implemented until #109"));
        }
        let function = self.current_function();
        let value = match function.children.get(first_arg as usize).copied() {
            Some(arg) => self.eval(arg)?,
            None => Value::Unit,
        };
        self.charge(fuel::show_cost(value.meta().slots))?;
        let mut text = String::new();
        write_show(&mut text, &value, &self.program.types, true);
        self.charge(fuel::string_cost(text.len() as u64))?;
        self.log.push(text);
        self.safepoint(node)?;
        Ok(Value::Unit)
    }

    fn check_value_depth(&self, value: &Value, _span: Span) -> EvalResult<()> {
        if value_depth_exceeds(value, MAX_VALUE_DEPTH) {
            Err(EvalError::Trap(Trap::DepthExceeded))
        } else {
            Ok(())
        }
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

impl<'p> IrRuntime<'p> {
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
    use super::{Frame, IrRuntime};
    use martite_ir::{
        Charge, ChildRange, ConstValue, Effects, Entries, FnId, FnKind, IrFn, IrId, IrProgram,
        Node, NodeKind, Slot, TyId,
    };

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
            function: function.id,
            slots: vec![None; function.locals as usize],
            source: function.source,
            name: function.name.as_ref(),
        };
        assert_eq!(frame.slots.len(), 7);
        assert!(frame.slots.iter().all(Option::is_none));
    }

    #[test]
    fn new_runtime_sizes_the_pools() {
        let program = IrProgram::default();
        let runtime = IrRuntime::new(&program, crate::interp::ExecLimits::default());
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
        let mut runtime = IrRuntime::new(&program, crate::interp::ExecLimits::default());
        let result = runtime.run_main().expect("slot test runs");
        assert_eq!(result.value, crate::value::Value::Int(8));
    }
}
