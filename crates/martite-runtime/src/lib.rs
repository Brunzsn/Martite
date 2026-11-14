//! Runtime values, standard library and the IR evaluator for Martite.
//!
//! Values and the standard library use the IR-owned vocabulary in
//! [`martite_ir`]: `FnId`, `TypeId`, `BuiltinFnId` and `BuiltinItemId`.
//! [`IrRuntime`] executes [`martite_ir::IrProgram`] and is the only execution
//! path. The runtime does not depend on `martite-check` or `martite-syntax`;
//! behavior is pinned by the snapshots in `martite-conformance`.

pub mod exec;
pub mod ir_eval;
pub mod json;
pub mod ops;
pub mod value;

pub use exec::{CallResult, ExecLimits, HostContext, HostFault, MainResult, Trap};
pub use ir_eval::{IrHostDispatcher, IrRuntime};
pub use ops::{show, value_eq, value_ord};
pub use value::{
    Closure, EnumValue, FnValue, HandleValue, KeyValue, MAX_VALUE_DEPTH, MapValue, OrderedMap,
    SeqValue, StructValue, Value, ValueMeta, key_from_value, key_to_value, value_depth_exceeds,
};
