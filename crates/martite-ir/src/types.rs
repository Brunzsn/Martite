//! IR-owned type identifiers and tables.
//!
//! Two indices are kept separate:
//!
//! - [`TyId`] identifies an erased structural type used by a node, slot or
//!   host signature.
//! - [`TypeId`] identifies a nominal type definition used by runtime values,
//!   method dispatch, constants and plugin state.
//!
//! [`TypeTable::from_checker`] is the migration bridge. It copies definitions
//! in checker declaration order, so a checker `TypeId(i)` and the IR
//! `TypeId(i)` name the same definition. Tooling keeps using the checker ids;
//! only values and lowered programs use the IR table. The bridge is part of
//! the default `lower` feature; runtime builds use the IR table directly.

#[cfg(feature = "lower")]
use martite_check::{CheckedProgram, Ty as CheckTy, TypeDefKind as CheckTypeDefKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TyId(pub u32);

impl TyId {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeId(pub u32);

impl TypeId {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// An erased structural type.
///
/// Generic parameters and arguments are preserved for verification; the
/// runtime never monomorphizes over them. Inference-only checker states
/// (`Error`, `Var`) never reach the IR.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    Int,
    Float,
    Bool,
    Str,
    Unit,
    Range,
    Never,
    Tuple(Vec<TyId>),
    List(TyId),
    Map(TyId, TyId),
    Option(TyId),
    Result(TyId, TyId),
    Fn(Vec<TyId>, TyId),
    Named(TypeId, Vec<TyId>),
    /// Opaque generic parameter of the function currently being checked.
    Param(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FieldDef {
    pub name: Box<str>,
    pub ty: TyId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VariantDef {
    pub name: Box<str>,
    pub payload: Vec<TyId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TypeDefKind {
    Struct { fields: Vec<FieldDef> },
    Enum { variants: Vec<VariantDef> },
    Opaque,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeDef {
    pub name: Box<str>,
    pub kind: TypeDefKind,
    /// Number of generic parameters. Names and bounds stay in the checker.
    pub params: u32,
    pub is_interface: bool,
    pub is_builtin: bool,
}

impl TypeDef {
    pub fn is_opaque(&self) -> bool {
        matches!(self.kind, TypeDefKind::Opaque)
    }
}

/// IR-owned type table.
///
/// Definitions are in checker declaration order. Structural types are
/// interned by [`TypeTable::intern_ty`], so equal types share one [`TyId`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TypeTable {
    pub tys: Vec<Ty>,
    pub defs: Vec<TypeDef>,
}

impl TypeTable {
    pub fn add_ty(&mut self, ty: Ty) -> TyId {
        let id = TyId(self.tys.len() as u32);
        self.tys.push(ty);
        id
    }

    pub fn intern_ty(&mut self, ty: Ty) -> TyId {
        if let Some(index) = self.tys.iter().position(|existing| existing == &ty) {
            return TyId(index as u32);
        }
        self.add_ty(ty)
    }

    pub fn get_ty(&self, id: TyId) -> Option<&Ty> {
        self.tys.get(id.index())
    }

    pub fn add_def(&mut self, def: TypeDef) -> TypeId {
        let id = TypeId(self.defs.len() as u32);
        self.defs.push(def);
        id
    }

    pub fn get(&self, id: TypeId) -> Option<&TypeDef> {
        self.defs.get(id.index())
    }

    pub fn get_mut(&mut self, id: TypeId) -> Option<&mut TypeDef> {
        self.defs.get_mut(id.index())
    }

    pub fn field_index(&self, id: TypeId, name: &str) -> Option<usize> {
        match &self.get(id)?.kind {
            TypeDefKind::Struct { fields } => fields.iter().position(|f| f.name.as_ref() == name),
            _ => None,
        }
    }

    pub fn field(&self, id: TypeId, name: &str) -> Option<&FieldDef> {
        match &self.get(id)?.kind {
            TypeDefKind::Struct { fields } => fields.iter().find(|f| f.name.as_ref() == name),
            _ => None,
        }
    }

    pub fn variant_index(&self, id: TypeId, name: &str) -> Option<usize> {
        match &self.get(id)?.kind {
            TypeDefKind::Enum { variants } => variants.iter().position(|v| v.name.as_ref() == name),
            _ => None,
        }
    }

    pub fn variant(&self, id: TypeId, index: usize) -> Option<&VariantDef> {
        match &self.get(id)?.kind {
            TypeDefKind::Enum { variants } => variants.get(index),
            _ => None,
        }
    }

    /// Copy the checked program's type definitions into IR-owned data.
    ///
    /// The ids are stable by construction: definition `i` in the checker table
    /// becomes definition `i` here. Recursive and generic references are
    /// resolved after the placeholder definitions have been added.
    #[cfg(feature = "lower")]
    pub fn from_checker(program: &CheckedProgram) -> Self {
        let mut table = Self::default();
        for def in &program.types.defs {
            let kind = match def.kind {
                CheckTypeDefKind::Struct { .. } => TypeDefKind::Struct { fields: Vec::new() },
                CheckTypeDefKind::Enum { .. } => TypeDefKind::Enum {
                    variants: Vec::new(),
                },
                CheckTypeDefKind::Opaque => TypeDefKind::Opaque,
            };
            table.add_def(TypeDef {
                name: def.name.clone().into_boxed_str(),
                kind,
                params: def.params.len() as u32,
                is_interface: def.is_interface,
                is_builtin: def.is_builtin,
            });
        }

        for (index, def) in program.types.defs.iter().enumerate() {
            let kind = match &def.kind {
                CheckTypeDefKind::Struct { fields } => TypeDefKind::Struct {
                    fields: fields
                        .iter()
                        .map(|field| FieldDef {
                            name: field.name.clone().into_boxed_str(),
                            ty: table.ty_from_checker(&field.ty),
                        })
                        .collect(),
                },
                CheckTypeDefKind::Enum { variants } => TypeDefKind::Enum {
                    variants: variants
                        .iter()
                        .map(|variant| VariantDef {
                            name: variant.name.clone().into_boxed_str(),
                            payload: variant
                                .payload
                                .iter()
                                .map(|ty| table.ty_from_checker(ty))
                                .collect(),
                        })
                        .collect(),
                },
                CheckTypeDefKind::Opaque => TypeDefKind::Opaque,
            };
            table.defs[index].kind = kind;
        }

        table
    }

    #[cfg(feature = "lower")]
    pub(crate) fn ty_from_checker(&mut self, ty: &CheckTy) -> TyId {
        let converted = match ty {
            CheckTy::Int => Ty::Int,
            CheckTy::Float => Ty::Float,
            CheckTy::Bool => Ty::Bool,
            CheckTy::Str => Ty::Str,
            CheckTy::Unit => Ty::Unit,
            CheckTy::Range => Ty::Range,
            CheckTy::Never => Ty::Never,
            CheckTy::Tuple(items) => Ty::Tuple(
                items
                    .iter()
                    .map(|item| self.ty_from_checker(item))
                    .collect(),
            ),
            CheckTy::List(item) => Ty::List(self.ty_from_checker(item)),
            CheckTy::Map(key, value) => {
                let key = self.ty_from_checker(key);
                let value = self.ty_from_checker(value);
                Ty::Map(key, value)
            }
            CheckTy::Option(item) => Ty::Option(self.ty_from_checker(item)),
            CheckTy::Result(ok, err) => {
                let ok = self.ty_from_checker(ok);
                let err = self.ty_from_checker(err);
                Ty::Result(ok, err)
            }
            CheckTy::Fn(params, ret) => {
                let params = params
                    .iter()
                    .map(|param| self.ty_from_checker(param))
                    .collect();
                let ret = self.ty_from_checker(ret);
                Ty::Fn(params, ret)
            }
            CheckTy::Named(id, args) => Ty::Named(
                TypeId::new(id.0),
                args.iter().map(|arg| self.ty_from_checker(arg)).collect(),
            ),
            CheckTy::Param(index) => Ty::Param(*index),
            CheckTy::Error | CheckTy::Var(_) => {
                debug_assert!(
                    false,
                    "inference-only checker types must not reach the IR type table"
                );
                Ty::Never
            }
        };
        self.intern_ty(converted)
    }
}
