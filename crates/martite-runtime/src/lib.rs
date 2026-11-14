//! Runtime values, standard library and the IR evaluator for Martite.
//!
//! Values and the standard library use the IR-owned vocabulary in
//! [`martite_ir`]: `FnId`, `TypeId`, `BuiltinFnId` and `BuiltinItemId`.
//! [`IrRuntime`] executes [`martite_ir::IrProgram`] and is the default
//! execution path.
//!
//! The frozen AST evaluator lives behind the temporary `ast-eval` feature for
//! #115's `--ast` escape hatch, the conformance oracle and #116's retirement
//! window. A default build does not depend on `martite-check`.

pub mod exec;
pub mod ir_eval;
pub mod json;
pub mod ops;
pub mod value;

#[cfg(feature = "ast-eval")]
pub mod interp;

pub use exec::{CallResult, ExecLimits, HostContext, HostFault, MainResult, Trap};
#[cfg(feature = "ast-eval")]
pub use interp::{HostDispatcher, Runtime};
pub use ir_eval::{IrHostDispatcher, IrRuntime};
pub use ops::{show, value_eq, value_ord};
pub use value::{
    Closure, EnumValue, FnValue, HandleValue, KeyValue, MAX_VALUE_DEPTH, MapValue, OrderedMap,
    SeqValue, StructValue, Value, ValueMeta, key_from_value, key_to_value, value_depth_exceeds,
};
