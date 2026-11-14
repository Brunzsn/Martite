//! Shared execution types for both evaluators.
//!
//! The IR evaluator is the default runtime path. The frozen AST evaluator,
//! retained behind the temporary `ast-eval` feature for #115's escape hatch,
//! uses these same limits, traps and host types so observable behavior does
//! not depend on which evaluator ran.

use std::rc::Rc;
use std::time::Duration;

use martite_base::{SourceId, Span};
use martite_ir::TypeId;

use crate::value::{HandleValue, Value};

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

/// Per-call context passed to a host function.
///
/// The runtime constructs this value. `handle_type` records the opaque type
/// the host function is allowed to create, if any.
#[derive(Debug, Clone)]
pub struct HostContext {
    pub charged: u64,
    handle_type: Option<TypeId>,
}

impl HostContext {
    pub(crate) fn new(handle_type: Option<TypeId>) -> Self {
        Self {
            charged: 0,
            handle_type,
        }
    }

    pub fn charge(&mut self, units: u64) {
        self.charged = self.charged.saturating_add(units);
    }

    /// Create an opaque handle of the host function's declared return type.
    ///
    /// The runtime enables this when the host function returns a direct
    /// opaque interface type, or an `Option` or `Result` whose success
    /// payload is one. Calling it from any other host function is a host
    /// bug and returns a [`HostFault`].
    pub fn make_handle(&self, token: impl Into<String>) -> Result<Value, HostFault> {
        let Some(type_id) = self.handle_type else {
            return Err(HostFault::new(
                "make_handle requires the host function to return an opaque handle type",
            ));
        };
        Ok(Value::Handle(Rc::new(HandleValue {
            type_id,
            token: token.into(),
        })))
    }
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
