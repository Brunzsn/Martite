use std::collections::HashMap;

use martite_base::{SourceId, Span};
use martite_resolve::{BuiltinItem, DefId, DefInfo, ModuleId, ModuleTarget, ResolvedProgram};
use martite_syntax::{Block, FnDecl, LambdaBody, LambdaParam, NodeId};

use crate::ty::{BindingId, Ty, TypeId, TypeTable};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreludeVariant {
    None,
    Some,
    Ok,
    Err,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinFn {
    Print,
    Panic,
    Assert,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinReceiver {
    List,
    Map,
    Str,
    Int,
    Float,
    Range,
    Option,
    Result,
}

#[derive(Debug, Clone, Copy)]
pub enum Resolution {
    Local(BindingId),
    Const(DefId),
    Fn(DefId),
    Handler(DefId),
    Test(DefId),
    Variant(TypeId, usize),
    PreludeVariant(PreludeVariant),
    TypeName(TypeId),
    Module(ModuleTarget),
    ModuleItem(DefId),
    BuiltinItem(BuiltinItem),
    BuiltinFn(BuiltinFn),
    StructField(usize),
    TupleField(u32),
    Method(DefId),
    BuiltinMethod {
        receiver: BuiltinReceiver,
        name: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatResolution {
    Variant(TypeId, usize),
    Struct(TypeId),
    Prelude(PreludeVariant),
}

#[derive(Debug, Clone)]
pub struct BindingInfo {
    pub name: String,
    pub ty: Ty,
    pub mutable: bool,
    pub declaration: NodeId,
    pub scope_depth: usize,
    pub is_self: bool,
}

/// Immutable lambda metadata owned by the checked program. Closures store the
/// lambda's `NodeId` and this map supplies the AST without copying it per
/// evaluation.
#[derive(Debug, Clone)]
pub struct LambdaInfo {
    pub params: Vec<LambdaParam>,
    pub body: LambdaBody,
    pub captures: Vec<BindingId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FnKind {
    Free,
    Method(TypeId),
    Handler,
    Test,
    Host,
}

#[derive(Debug, Clone)]
pub struct ParamSig {
    pub node: NodeId,
    pub binding: BindingId,
    pub name: String,
    pub ty: Ty,
    pub mutable: bool,
    pub is_self: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct FnSig {
    pub def: DefId,
    pub node: NodeId,
    pub module: ModuleId,
    pub name: String,
    pub params: Vec<ParamSig>,
    pub ret: Ty,
    pub generics: Vec<martite_syntax::GenericParam>,
    pub type_params: Vec<crate::ty::TypeParam>,
    pub kind: FnKind,
    pub span: Span,
    pub is_pub: bool,
}

impl FnSig {
    pub fn self_param(&self) -> Option<&ParamSig> {
        self.params.first().filter(|p| p.is_self)
    }

    pub fn has_self(&self) -> bool {
        self.self_param().is_some()
    }

    pub fn self_mutable(&self) -> bool {
        self.self_param().map(|p| p.mutable).unwrap_or(false)
    }

    pub fn call_params(&self) -> &[ParamSig] {
        if self.has_self() {
            &self.params[1..]
        } else {
            &self.params
        }
    }
}

#[derive(Debug, Clone)]
pub struct HookSig {
    pub name: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub module: ModuleId,
    pub span: Span,
    pub source: SourceId,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Unit,
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
    Tuple(Vec<ConstValue>),
    List(Vec<ConstValue>),
    Map(Vec<(ConstValue, ConstValue)>),
    Struct(TypeId, Vec<(String, ConstValue)>),
    Enum(TypeId, usize, Vec<ConstValue>),
    Prelude(PreludeVariant, Vec<ConstValue>),
}

#[derive(Debug, Clone)]
pub struct ConstInfo {
    pub def: DefId,
    pub ty: Ty,
    pub value: ConstValue,
    pub node: NodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Library,
    Script,
    Plugin,
}

#[derive(Debug, Clone)]
pub struct CheckedProgram {
    pub resolved: ResolvedProgram,
    pub types: TypeTable,
    pub functions: HashMap<DefId, FnSig>,
    pub methods: HashMap<(TypeId, String), DefId>,
    pub consts: HashMap<DefId, ConstInfo>,
    pub expr_types: HashMap<NodeId, Ty>,
    pub resolutions: HashMap<NodeId, Resolution>,
    pub pattern_resolutions: HashMap<NodeId, PatResolution>,
    pub bindings: HashMap<BindingId, BindingInfo>,
    pub pattern_bindings: HashMap<NodeId, BindingId>,
    pub param_bindings: HashMap<NodeId, BindingId>,
    pub lambdas: HashMap<NodeId, LambdaInfo>,
    pub handlers: HashMap<String, DefId>,
    pub hooks: HashMap<String, HookSig>,
    pub tests: Vec<DefId>,
    pub state_type: Option<Ty>,
    pub init: Option<DefId>,
    pub shutdown: Option<DefId>,
    pub main: Option<DefId>,
    pub entry_kind: EntryKind,
    pub json_type: Option<TypeId>,
}

impl CheckedProgram {
    pub fn fn_sig(&self, def: DefId) -> Option<&FnSig> {
        self.functions.get(&def)
    }

    pub fn type_def(&self, id: TypeId) -> Option<&crate::ty::TypeDef> {
        self.types.get(id)
    }

    pub fn def(&self, id: DefId) -> &DefInfo {
        self.resolved.def(id)
    }

    pub fn function_block(&self, def: DefId) -> Option<&Block> {
        let info = self.resolved.def(def);
        let module = self.resolved.module(info.module);
        let item = module.ast.items.get(info.item_index)?;
        match item {
            martite_syntax::Item::Fn(f) if f.id == info.node => f.body.as_ref(),
            martite_syntax::Item::On(o) if o.id == info.node => Some(&o.body),
            martite_syntax::Item::Test(t) if t.id == info.node => Some(&t.body),
            martite_syntax::Item::Impl(i) => i
                .methods
                .iter()
                .find(|m| m.id == info.node)
                .and_then(|m| m.body.as_ref()),
            _ => None,
        }
    }

    pub fn function_decl(&self, def: DefId) -> Option<&FnDecl> {
        let info = self.resolved.def(def);
        let module = self.resolved.module(info.module);
        let item = module.ast.items.get(info.item_index)?;
        match item {
            martite_syntax::Item::Fn(f) if f.id == info.node => Some(f),
            martite_syntax::Item::Impl(i) => i.methods.iter().find(|m| m.id == info.node),
            _ => None,
        }
    }

    pub fn type_of(&self, node: NodeId) -> Option<&Ty> {
        self.expr_types.get(&node)
    }

    pub fn resolution(&self, node: NodeId) -> Option<&Resolution> {
        self.resolutions.get(&node)
    }

    pub fn handler(&self, name: &str) -> Option<DefId> {
        self.handlers.get(name).copied()
    }

    pub fn lambda(&self, node: NodeId) -> Option<&LambdaInfo> {
        self.lambdas.get(&node)
    }
}

pub type TypeParams = Vec<crate::ty::TypeParam>;
