//! Bidirectional type checker for Martite.

mod checker;
pub mod exhaustive;
pub mod program;
pub mod ty;

pub use checker::check;
pub use martite_resolve::{BuiltinItem, BuiltinItemId, DefId, DefKind, ModuleId, ModuleTarget};
pub use program::{
    BindingInfo, BuiltinFn, BuiltinReceiver, CheckedProgram, ConstInfo, ConstValue, EntryKind,
    FnKind, FnSig, HookSig, LambdaInfo, ParamSig, PatResolution, PreludeVariant, Resolution,
};
pub use ty::{
    BindingId, FieldDef, Infer, Ty, TypeDef, TypeDefKind, TypeId, TypeParam, TypeTable, VariantDef,
};

use martite_base::Diagnostic;

#[derive(Debug, Clone, Default)]
pub struct CheckOutput {
    pub program: Option<CheckedProgram>,
    pub diagnostics: Vec<Diagnostic>,
}
