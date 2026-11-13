//! Runtime values, standard library and the AST interpreter for Martite.
//!
//! Values and the standard library use the IR-owned vocabulary in
//! [`martite_ir`]: `FnId`, `TypeId`, `BuiltinFnId` and `BuiltinItemId`.
//! `interp` is the remaining transitional `martite-check` consumer; it is
//! replaced by the IR evaluator in #108-#115 and removed in #116.
//!
//! The opt-in `ir-eval` feature adds [`IrRuntime`], the recursive evaluator for
//! [`martite_ir::IrProgram`]. The AST interpreter remains the default until the
//! migration switches it in #115.

pub mod interp;
pub mod json;
pub mod ops;
pub mod value;

#[cfg(feature = "ir-eval")]
pub mod ir_eval;

pub use interp::{
    CallResult, ExecLimits, HostContext, HostDispatcher, HostFault, MainResult, Runtime, Trap,
};
#[cfg(feature = "ir-eval")]
pub use ir_eval::IrRuntime;
pub use ops::{show, value_eq, value_ord};
pub use value::{
    Closure, EnumValue, FnValue, HandleValue, KeyValue, MAX_VALUE_DEPTH, MapValue, OrderedMap,
    SeqValue, StructValue, Value, ValueMeta, key_from_value, key_to_value, value_depth_exceeds,
};
