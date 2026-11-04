use std::collections::HashMap;
use std::fmt;

use martite_resolve::{DefId, ModuleId};
use martite_syntax::Bound;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BindingId(pub u32);

#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    Int,
    Float,
    Bool,
    Str,
    Unit,
    Range,
    Never,
    Error,
    Tuple(Vec<Ty>),
    List(Box<Ty>),
    Map(Box<Ty>, Box<Ty>),
    Option(Box<Ty>),
    Result(Box<Ty>, Box<Ty>),
    Fn(Vec<Ty>, Box<Ty>),
    Named(TypeId, Vec<Ty>),
    /// Opaque generic parameter of the function currently being checked.
    Param(u32),
    /// Inference variable local to one call expression.
    Var(u32),
}

impl Ty {
    pub fn list(inner: Ty) -> Ty {
        Ty::List(Box::new(inner))
    }

    pub fn option(inner: Ty) -> Ty {
        Ty::Option(Box::new(inner))
    }

    pub fn named(id: TypeId, args: Vec<Ty>) -> Ty {
        Ty::Named(id, args)
    }

    pub fn unit_fn(params: Vec<Ty>) -> Ty {
        Ty::Fn(params, Box::new(Ty::Unit))
    }

    pub fn is_error(&self) -> bool {
        matches!(self, Ty::Error)
    }

    pub fn is_never(&self) -> bool {
        matches!(self, Ty::Never)
    }

    pub fn substitute(&self, vars: &[Option<Ty>]) -> Ty {
        match self {
            Ty::Var(i) => match vars.get(*i as usize).and_then(|v| v.clone()) {
                Some(t) => t.substitute(vars),
                None => self.clone(),
            },
            Ty::Tuple(items) => Ty::Tuple(items.iter().map(|t| t.substitute(vars)).collect()),
            Ty::List(t) => Ty::list(t.substitute(vars)),
            Ty::Map(k, v) => Ty::Map(Box::new(k.substitute(vars)), Box::new(v.substitute(vars))),
            Ty::Option(t) => Ty::option(t.substitute(vars)),
            Ty::Result(t, e) => {
                Ty::Result(Box::new(t.substitute(vars)), Box::new(e.substitute(vars)))
            }
            Ty::Fn(params, ret) => Ty::Fn(
                params.iter().map(|t| t.substitute(vars)).collect(),
                Box::new(ret.substitute(vars)),
            ),
            Ty::Named(id, args) => {
                Ty::Named(*id, args.iter().map(|t| t.substitute(vars)).collect())
            }
            _ => self.clone(),
        }
    }

    pub fn has_var(&self) -> bool {
        match self {
            Ty::Var(_) => true,
            Ty::Tuple(items) => items.iter().any(Ty::has_var),
            Ty::List(t) => t.has_var(),
            Ty::Map(k, v) => k.has_var() || v.has_var(),
            Ty::Option(t) => t.has_var(),
            Ty::Result(t, e) => t.has_var() || e.has_var(),
            Ty::Fn(params, ret) => params.iter().any(Ty::has_var) || ret.has_var(),
            Ty::Named(_, args) => args.iter().any(Ty::has_var),
            _ => false,
        }
    }

    pub fn display<'a>(&'a self, types: &'a TypeTable) -> TyDisplay<'a> {
        TyDisplay { ty: self, types }
    }
}

pub struct TyDisplay<'a> {
    ty: &'a Ty,
    types: &'a TypeTable,
}

impl fmt::Display for TyDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.ty.fmt_with(self.types, f)
    }
}

impl Ty {
    fn fmt_with(&self, types: &TypeTable, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Int => f.write_str("int"),
            Ty::Float => f.write_str("float"),
            Ty::Bool => f.write_str("bool"),
            Ty::Str => f.write_str("str"),
            Ty::Unit => f.write_str("()"),
            Ty::Range => f.write_str("range"),
            Ty::Never => f.write_str("Never"),
            Ty::Error => f.write_str("<error>"),
            Ty::Tuple(items) => {
                f.write_str("(")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    item.fmt_with(types, f)?;
                }
                if items.len() == 1 {
                    f.write_str(",")?;
                }
                f.write_str(")")
            }
            Ty::List(t) => write!(f, "list[{}]", t.display(types)),
            Ty::Map(k, v) => write!(f, "map[{}, {}]", k.display(types), v.display(types)),
            Ty::Option(t) => write!(f, "{}?", t.display(types)),
            Ty::Result(t, e) => write!(f, "Result[{}, {}]", t.display(types), e.display(types)),
            Ty::Fn(params, ret) => {
                f.write_str("fn(")?;
                for (i, p) in params.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    p.fmt_with(types, f)?;
                }
                f.write_str(") -> ")?;
                ret.fmt_with(types, f)
            }
            Ty::Named(id, args) => {
                let def = types.get(*id);
                f.write_str(def.map(|d| d.name.as_str()).unwrap_or("<type>"))?;
                if !args.is_empty() {
                    f.write_str("[")?;
                    for (i, a) in args.iter().enumerate() {
                        if i > 0 {
                            f.write_str(", ")?;
                        }
                        a.fmt_with(types, f)?;
                    }
                    f.write_str("]")?;
                }
                Ok(())
            }
            Ty::Param(i) => write!(f, "T{i}"),
            Ty::Var(i) => write!(f, "?{i}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeParam {
    pub name: String,
    pub bounds: Vec<Bound>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldDef {
    pub name: String,
    pub ty: Ty,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VariantDef {
    pub name: String,
    pub payload: Vec<Ty>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TypeDefKind {
    Struct { fields: Vec<FieldDef> },
    Enum { variants: Vec<VariantDef> },
    Opaque,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeDef {
    pub id: TypeId,
    pub name: String,
    pub module: Option<ModuleId>,
    pub kind: TypeDefKind,
    pub params: Vec<TypeParam>,
    pub is_interface: bool,
    pub is_builtin: bool,
    pub def: Option<DefId>,
}

#[derive(Debug, Clone, Default)]
pub struct TypeTable {
    pub defs: Vec<TypeDef>,
}

impl TypeTable {
    pub fn add(&mut self, mut def: TypeDef) -> TypeId {
        let id = TypeId(self.defs.len() as u32);
        def.id = id;
        self.defs.push(def);
        id
    }

    pub fn get(&self, id: TypeId) -> Option<&TypeDef> {
        self.defs.get(id.0 as usize)
    }

    pub fn get_mut(&mut self, id: TypeId) -> Option<&mut TypeDef> {
        self.defs.get_mut(id.0 as usize)
    }

    pub fn field_index(&self, id: TypeId, name: &str) -> Option<usize> {
        match &self.get(id)?.kind {
            TypeDefKind::Struct { fields } => fields.iter().position(|f| f.name == name),
            _ => None,
        }
    }

    pub fn field(&self, id: TypeId, name: &str) -> Option<&FieldDef> {
        match &self.get(id)?.kind {
            TypeDefKind::Struct { fields } => fields.iter().find(|f| f.name == name),
            _ => None,
        }
    }

    pub fn variant_index(&self, id: TypeId, name: &str) -> Option<usize> {
        match &self.get(id)?.kind {
            TypeDefKind::Enum { variants } => variants.iter().position(|v| v.name == name),
            _ => None,
        }
    }

    pub fn variant(&self, id: TypeId, index: usize) -> Option<&VariantDef> {
        match &self.get(id)?.kind {
            TypeDefKind::Enum { variants } => variants.get(index),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Infer {
    vars: Vec<Option<Ty>>,
}

impl Infer {
    pub fn fresh(&mut self) -> Ty {
        let id = self.vars.len() as u32;
        self.vars.push(None);
        Ty::Var(id)
    }

    pub fn resolve(&self, ty: &Ty) -> Ty {
        ty.substitute(&self.vars)
    }

    pub fn unify(&mut self, a: &Ty, b: &Ty) -> bool {
        let a = self.resolve(a);
        let b = self.resolve(b);
        match (&a, &b) {
            (Ty::Error, _) | (_, Ty::Error) | (Ty::Never, _) | (_, Ty::Never) => true,
            (Ty::Var(i), Ty::Var(j)) if i == j => true,
            (Ty::Var(i), other) | (other, Ty::Var(i)) => {
                if other.has_var() && contains_var(other, *i) {
                    return false;
                }
                self.vars[*i as usize] = Some(other.clone());
                true
            }
            (Ty::Int, Ty::Int)
            | (Ty::Float, Ty::Float)
            | (Ty::Bool, Ty::Bool)
            | (Ty::Str, Ty::Str)
            | (Ty::Unit, Ty::Unit)
            | (Ty::Range, Ty::Range) => true,
            (Ty::Tuple(a), Ty::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(x, y)| self.unify(x, y))
            }
            (Ty::List(a), Ty::List(b)) => self.unify(a, b),
            (Ty::Map(ak, av), Ty::Map(bk, bv)) => self.unify(ak, bk) && self.unify(av, bv),
            (Ty::Option(a), Ty::Option(b)) => self.unify(a, b),
            (Ty::Result(at, ae), Ty::Result(bt, be)) => self.unify(at, bt) && self.unify(ae, be),
            (Ty::Fn(ap, ar), Ty::Fn(bp, br)) => {
                ap.len() == bp.len()
                    && ap.iter().zip(bp).all(|(x, y)| self.unify(x, y))
                    && self.unify(ar, br)
            }
            (Ty::Named(ai, aargs), Ty::Named(bi, bargs)) => {
                ai == bi
                    && aargs.len() == bargs.len()
                    && aargs.iter().zip(bargs).all(|(x, y)| self.unify(x, y))
            }
            (Ty::Param(i), Ty::Param(j)) => i == j,
            _ => false,
        }
    }

    pub fn matches(&self, a: &Ty, b: &Ty) -> bool {
        let mut clone = self.clone();
        clone.unify(a, b)
    }
}

fn contains_var(ty: &Ty, var: u32) -> bool {
    match ty {
        Ty::Var(i) => *i == var,
        Ty::Tuple(items) => items.iter().any(|t| contains_var(t, var)),
        Ty::List(t) | Ty::Option(t) => contains_var(t, var),
        Ty::Map(k, v) | Ty::Result(k, v) => contains_var(k, var) || contains_var(v, var),
        Ty::Fn(params, ret) => {
            params.iter().any(|t| contains_var(t, var)) || contains_var(ret, var)
        }
        Ty::Named(_, args) => args.iter().any(|t| contains_var(t, var)),
        _ => false,
    }
}

pub fn type_vars_from_params(params: &[TypeParam], infer: &mut Infer) -> Vec<Ty> {
    params.iter().map(|_| infer.fresh()).collect()
}

pub fn instantiate_params(ty: &Ty, vars: &[Ty]) -> Ty {
    match ty {
        Ty::Param(i) => vars.get(*i as usize).cloned().unwrap_or(Ty::Error),
        Ty::Tuple(items) => Ty::Tuple(items.iter().map(|t| instantiate_params(t, vars)).collect()),
        Ty::List(t) => Ty::list(instantiate_params(t, vars)),
        Ty::Map(k, v) => Ty::Map(
            Box::new(instantiate_params(k, vars)),
            Box::new(instantiate_params(v, vars)),
        ),
        Ty::Option(t) => Ty::option(instantiate_params(t, vars)),
        Ty::Result(t, e) => Ty::Result(
            Box::new(instantiate_params(t, vars)),
            Box::new(instantiate_params(e, vars)),
        ),
        Ty::Fn(params, ret) => Ty::Fn(
            params.iter().map(|t| instantiate_params(t, vars)).collect(),
            Box::new(instantiate_params(ret, vars)),
        ),
        Ty::Named(id, args) => Ty::Named(
            *id,
            args.iter().map(|t| instantiate_params(t, vars)).collect(),
        ),
        _ => ty.clone(),
    }
}

pub struct TypeResolver<'a> {
    pub types: &'a TypeTable,
    pub def_to_type: &'a HashMap<DefId, TypeId>,
    pub builtin_json: Option<TypeId>,
    pub type_param_names: &'a [String],
    pub imported_builtin_types: &'a HashMap<String, TypeId>,
}

impl TypeResolver<'_> {
    pub fn resolve(
        &self,
        ty: &martite_syntax::TyExpr,
        symbols: &martite_resolve::Symbols,
    ) -> Result<Ty, String> {
        use martite_syntax::{PrimTy, TyKind};
        match &ty.kind {
            TyKind::Prim(p) => Ok(match p {
                PrimTy::Int => Ty::Int,
                PrimTy::Float => Ty::Float,
                PrimTy::Bool => Ty::Bool,
                PrimTy::Str => Ty::Str,
                PrimTy::Range => Ty::Range,
            }),
            TyKind::Unit => Ok(Ty::Unit),
            TyKind::Optional(inner) => Ok(Ty::option(self.resolve(inner, symbols)?)),
            TyKind::Tuple(items) => Ok(Ty::Tuple(
                items
                    .iter()
                    .map(|t| self.resolve(t, symbols))
                    .collect::<Result<_, _>>()?,
            )),
            TyKind::Fn { params, ret } => Ok(Ty::Fn(
                params
                    .iter()
                    .map(|t| self.resolve(t, symbols))
                    .collect::<Result<_, _>>()?,
                Box::new(match ret {
                    Some(r) => self.resolve(r, symbols)?,
                    None => Ty::Unit,
                }),
            )),
            TyKind::Named { name, args } => {
                if let Some(index) = self.type_param_names.iter().position(|n| n == name) {
                    if !args.is_empty() {
                        return Err(format!("type parameter `{name}` cannot take arguments"));
                    }
                    return Ok(Ty::Param(index as u32));
                }
                let resolved_args: Vec<Ty> = args
                    .iter()
                    .map(|t| self.resolve(t, symbols))
                    .collect::<Result<_, _>>()?;
                match name.as_str() {
                    "Option" => {
                        if resolved_args.len() != 1 {
                            return Err("Option expects 1 type argument".into());
                        }
                        Ok(Ty::option(resolved_args[0].clone()))
                    }
                    "Result" => {
                        if resolved_args.len() != 2 {
                            return Err("Result expects 2 type arguments".into());
                        }
                        Ok(Ty::Result(
                            Box::new(resolved_args[0].clone()),
                            Box::new(resolved_args[1].clone()),
                        ))
                    }
                    "list" => {
                        if resolved_args.len() != 1 {
                            return Err("list expects 1 type argument".into());
                        }
                        Ok(Ty::list(resolved_args[0].clone()))
                    }
                    "map" => {
                        if resolved_args.len() != 2 {
                            return Err("map expects 2 type arguments".into());
                        }
                        Ok(Ty::Map(
                            Box::new(resolved_args[0].clone()),
                            Box::new(resolved_args[1].clone()),
                        ))
                    }
                    _ => {
                        if let Some(item) = symbols.builtin_items.get(name) {
                            if item.module == "std.json" && item.name == "Json" {
                                let id = self.builtin_json.ok_or("Json is unavailable")?;
                                return Ok(Ty::Named(id, Vec::new()));
                            }
                        }
                        if let Some(def) = symbols.types.get(name) {
                            let id = self.def_to_type.get(def).copied().ok_or("unknown type")?;
                            let expected = self.types.get(id).map(|d| d.params.len()).unwrap_or(0);
                            if resolved_args.len() != expected {
                                return Err(format!(
                                    "type `{name}` expects {expected} type argument(s), found {}",
                                    resolved_args.len()
                                ));
                            }
                            Ok(Ty::Named(id, resolved_args))
                        } else if let Some(id) = self.imported_builtin_types.get(name) {
                            Ok(Ty::Named(*id, resolved_args))
                        } else {
                            Err(format!("unknown type `{name}`"))
                        }
                    }
                }
            }
        }
    }
}
