//! Tree-walking interpreter and standard library for Martite.

mod cost;
pub mod interp;
pub mod json;
pub mod ops;
pub mod value;

pub use interp::{
    CallResult, ExecLimits, HostContext, HostDispatcher, HostFault, MainResult, Runtime, Trap,
};
pub use ops::{show, value_eq, value_ord};
pub use value::{
    Closure, ClosureTarget, EnumValue, HandleValue, KeyValue, MAX_VALUE_DEPTH, MapValue,
    OrderedMap, SeqValue, StructValue, Value, ValueMeta, key_from_value, key_to_value,
    value_depth_exceeds,
};
