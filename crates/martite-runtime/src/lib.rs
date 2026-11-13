//! Runtime values, standard library and the AST interpreter for Martite.
//!
//! Values and the standard library use the IR-owned vocabulary in
//! [`martite_ir`]: `FnId`, `TypeId`, `BuiltinFnId` and `BuiltinItemId`.
//! `interp` is the remaining transitional `martite-check` consumer; it is
//! replaced by the IR evaluator in #108-#115 and removed in #116.

pub mod interp;
pub mod json;
pub mod ops;
pub mod value;

pub use interp::{
    CallResult, ExecLimits, HostContext, HostDispatcher, HostFault, MainResult, Runtime, Trap,
};
pub use ops::{show, value_eq, value_ord};
pub use value::{
    Closure, EnumValue, FnValue, HandleValue, KeyValue, MAX_VALUE_DEPTH, MapValue, OrderedMap,
    SeqValue, StructValue, Value, ValueMeta, key_from_value, key_to_value, value_depth_exceeds,
};
