//! Shared execution types for the IR evaluator.
//!
//! Limits, traps and host types are runtime-owned and are not tied to the
//! checker or syntax tree.

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

/// Identity of the plugin whose code is currently running.
///
/// The embedding attaches a caller to [`crate::IrRuntime`] before invoking
/// plugin code. A runtime used directly without an attached caller reports
/// `None` through [`HostContext::caller`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostCaller {
    name: Rc<str>,
    version: Rc<str>,
}

impl HostCaller {
    /// Create a caller identity from the plugin's manifest name and version.
    pub fn new(name: impl Into<Rc<str>>, version: impl Into<Rc<str>>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
        }
    }

    /// The manifest name of the calling plugin.
    pub fn name(&self) -> &str {
        self.name.as_ref()
    }

    /// The manifest version of the calling plugin.
    pub fn version(&self) -> &str {
        self.version.as_ref()
    }
}

/// Per-call context passed to a host function.
///
/// The runtime constructs this value. `handle_type` records the opaque type
/// the host function is allowed to create, if any, and `caller` identifies the
/// plugin whose call is in progress.
#[derive(Debug, Clone)]
pub struct HostContext {
    pub charged: u64,
    handle_type: Option<TypeId>,
    caller: Option<HostCaller>,
}

impl HostContext {
    pub(crate) fn new(handle_type: Option<TypeId>, caller: Option<HostCaller>) -> Self {
        Self {
            charged: 0,
            handle_type,
            caller,
        }
    }

    /// The plugin that invoked this host function, when the embedding attached
    /// an identity to the runtime.
    pub fn caller(&self) -> Option<&HostCaller> {
        self.caller.as_ref()
    }

    /// The manifest name of the calling plugin, when known.
    pub fn plugin_name(&self) -> Option<&str> {
        self.caller().map(HostCaller::name)
    }

    /// The manifest version of the calling plugin, when known.
    pub fn plugin_version(&self) -> Option<&str> {
        self.caller().map(HostCaller::version)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_context_reports_the_attached_caller() {
        let caller = HostCaller::new("word-count", "1.2.3");
        let cx = HostContext::new(None, Some(caller));
        assert_eq!(cx.plugin_name(), Some("word-count"));
        assert_eq!(cx.plugin_version(), Some("1.2.3"));
        assert_eq!(cx.caller().map(HostCaller::name), Some("word-count"));
    }

    #[test]
    fn host_context_without_a_caller_reports_none() {
        let cx = HostContext::new(None, None);
        assert!(cx.caller().is_none());
        assert!(cx.plugin_name().is_none());
        assert!(cx.plugin_version().is_none());
    }
}
