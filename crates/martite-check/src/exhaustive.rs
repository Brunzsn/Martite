use martite_syntax::{MatchArm, PatKind, Pattern};

use crate::ty::{instantiate_params, Ty, TypeId, TypeTable};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Ctor {
    Unit,
    Bool(bool),
    Int(i64),
    Str(String),
    Tuple(usize),
    Struct(TypeId),
    Variant(TypeId, usize),
    OptionSome,
    OptionNone,
    ResultOk,
    ResultErr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum P {
    Wild,
    Ctor(Ctor, Vec<P>),
}

/// True when the match arms cover every value of `ty` (guards are ignored, as
/// required by the language rules).
pub fn is_exhaustive(arms: &[MatchArm], ty: &Ty, types: &TypeTable) -> bool {
    if matches!(ty, Ty::Never | Ty::Error) {
        return true;
    }
    let mut matrix = Vec::new();
    for arm in arms {
        if arm.guard.is_some() {
            continue;
        }
        let mut alternatives = Vec::new();
        expand_or(&arm.pattern, &mut alternatives);
        for pat in alternatives {
            let mut row = Vec::new();
            pattern_rep(&pat, ty, types, &mut row);
            matrix.push(row);
        }
    }
    !useful(&matrix, &[P::Wild], &[ty.clone()], types)
}

fn expand_or<'a>(pat: &'a Pattern, out: &mut Vec<Pattern>) {
    match &pat.kind {
        PatKind::Or(alts) => {
            for alt in alts {
                expand_or(alt, out);
            }
        }
        _ => out.push(pat.clone()),
    }
}

fn pattern_rep(pat: &Pattern, ty: &Ty, types: &TypeTable, out: &mut Vec<P>) {
    match &pat.kind {
        PatKind::Wild | PatKind::Bind(_) => out.push(P::Wild),
        PatKind::Int(v) => out.push(P::Ctor(Ctor::Int(*v), Vec::new())),
        PatKind::Bool(v) => out.push(P::Ctor(Ctor::Bool(*v), Vec::new())),
        PatKind::Str(v) => out.push(P::Ctor(Ctor::Str(v.clone()), Vec::new())),
        PatKind::Tuple(items) => {
            let mut args = Vec::new();
            if let Ty::Tuple(tys) = ty {
                for (item, item_ty) in items.iter().zip(tys) {
                    let mut inner = Vec::new();
                    pattern_rep(item, item_ty, types, &mut inner);
                    args.push(inner.pop().unwrap_or(P::Wild));
                }
            } else {
                args.extend(items.iter().map(|_| P::Wild));
            }
            out.push(P::Ctor(Ctor::Tuple(items.len()), args));
        }
        PatKind::Variant {
            qualifier: _,
            name,
            args,
        } => {
            if let Ty::Option(inner) = ty {
                if name == "Some" {
                    let mut p = Vec::new();
                    if let Some(arg) = args.first() {
                        pattern_rep(arg, inner, types, &mut p);
                    }
                    out.push(P::Ctor(Ctor::OptionSome, p));
                } else {
                    out.push(P::Ctor(Ctor::OptionNone, Vec::new()));
                }
                return;
            }
            if let Ty::Result(ok, err) = ty {
                if name == "Ok" {
                    let mut p = Vec::new();
                    if let Some(arg) = args.first() {
                        pattern_rep(arg, ok, types, &mut p);
                    }
                    out.push(P::Ctor(Ctor::ResultOk, p));
                } else {
                    let mut p = Vec::new();
                    if let Some(arg) = args.first() {
                        pattern_rep(arg, err, types, &mut p);
                    }
                    out.push(P::Ctor(Ctor::ResultErr, p));
                }
                return;
            }
            if let Ty::Named(id, type_args) = ty {
                if let Some(index) = types.variant_index(*id, name) {
                    let payload = types
                        .variant(*id, index)
                        .map(|v| {
                            v.payload
                                .iter()
                                .map(|t| instantiate_params(t, type_args))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let mut p = Vec::new();
                    for (arg, arg_ty) in args.iter().zip(payload.iter()) {
                        let mut inner = Vec::new();
                        pattern_rep(arg, arg_ty, types, &mut inner);
                        p.push(inner.pop().unwrap_or(P::Wild));
                    }
                    out.push(P::Ctor(Ctor::Variant(*id, index), p));
                    return;
                }
            }
            out.push(P::Wild);
        }
        PatKind::Struct { name: _, fields, rest } => {
            let Ty::Named(id, type_args) = ty else {
                out.push(P::Wild);
                return;
            };
            let Some(def) = types.get(*id) else {
                out.push(P::Wild);
                return;
            };
            let crate::ty::TypeDefKind::Struct { fields: def_fields } = &def.kind else {
                out.push(P::Wild);
                return;
            };
            let mut args = Vec::new();
            for field in def_fields {
                if let Some(fp) = fields.iter().find(|f| f.name == field.name) {
                    let field_ty = instantiate_params(&field.ty, type_args);
                    let mut inner = Vec::new();
                    pattern_rep(&fp.pattern, &field_ty, types, &mut inner);
                    args.push(inner.pop().unwrap_or(P::Wild));
                } else {
                    args.push(P::Wild);
                }
            }
            let _ = rest;
            out.push(P::Ctor(Ctor::Struct(*id), args));
        }
        PatKind::Or(_) => out.push(P::Wild),
    }
}

fn useful(matrix: &[Vec<P>], vector: &[P], types: &[Ty], table: &TypeTable) -> bool {
    if vector.is_empty() {
        return matrix.is_empty();
    }
    let p = &vector[0];
    let ty = &types[0];
    match p {
        P::Ctor(ctor, args) => {
            let mut new_matrix = Vec::new();
            for row in matrix {
                let Some(first) = row.first() else { continue };
                match first {
                    P::Wild => {
                        let mut r: Vec<P> = args.iter().map(|_| P::Wild).collect();
                        r.extend_from_slice(&row[1..]);
                        new_matrix.push(r);
                    }
                    P::Ctor(rc, rargs) if rc == ctor => {
                        let mut r = rargs.clone();
                        r.extend_from_slice(&row[1..]);
                        new_matrix.push(r);
                    }
                    _ => {}
                }
            }
            let mut new_types = ctor_arg_types(ctor, ty, table);
            new_types.extend_from_slice(&types[1..]);
            let mut new_vector = args.clone();
            new_vector.extend_from_slice(&vector[1..]);
            useful(&new_matrix, &new_vector, &new_types, table)
        }
        P::Wild => {
            if let Some(ctors) = constructors(ty, table) {
                for ctor in ctors {
                    let mut new_matrix = Vec::new();
                    let arity = ctor_arity(&ctor, ty, table);
                    for row in matrix {
                        let Some(first) = row.first() else { continue };
                        match first {
                            P::Wild => {
                                let mut r: Vec<P> = (0..arity).map(|_| P::Wild).collect();
                                r.extend_from_slice(&row[1..]);
                                new_matrix.push(r);
                            }
                            P::Ctor(rc, rargs) if *rc == ctor => {
                                let mut r = rargs.clone();
                                r.extend_from_slice(&row[1..]);
                                new_matrix.push(r);
                            }
                            _ => {}
                        }
                    }
                    let mut new_types = ctor_arg_types(&ctor, ty, table);
                    new_types.extend_from_slice(&types[1..]);
                    let mut new_vector: Vec<P> = (0..arity).map(|_| P::Wild).collect();
                    new_vector.extend_from_slice(&vector[1..]);
                    if useful(&new_matrix, &new_vector, &new_types, table) {
                        return true;
                    }
                }
                false
            } else {
                let default: Vec<Vec<P>> = matrix
                    .iter()
                    .filter_map(|row| {
                        matches!(row.first(), Some(P::Wild)).then(|| row[1..].to_vec())
                    })
                    .collect();
                useful(&default, &vector[1..], &types[1..], table)
            }
        }
    }
}

fn constructors(ty: &Ty, table: &TypeTable) -> Option<Vec<Ctor>> {
    match ty {
        Ty::Bool => Some(vec![Ctor::Bool(false), Ctor::Bool(true)]),
        Ty::Unit => Some(vec![Ctor::Unit]),
        Ty::Tuple(items) => Some(vec![Ctor::Tuple(items.len())]),
        Ty::Option(_) => Some(vec![Ctor::OptionNone, Ctor::OptionSome]),
        Ty::Result(_, _) => Some(vec![Ctor::ResultOk, Ctor::ResultErr]),
        Ty::Named(id, _) => {
            let def = table.get(*id)?;
            match &def.kind {
                crate::ty::TypeDefKind::Enum { variants } => Some(
                    variants
                        .iter()
                        .enumerate()
                        .map(|(i, _)| Ctor::Variant(*id, i))
                        .collect(),
                ),
                crate::ty::TypeDefKind::Struct { .. } => Some(vec![Ctor::Struct(*id)]),
                crate::ty::TypeDefKind::Opaque => None,
            }
        }
        _ => None,
    }
}

fn ctor_arity(ctor: &Ctor, ty: &Ty, table: &TypeTable) -> usize {
    ctor_arg_types(ctor, ty, table).len()
}

fn ctor_arg_types(ctor: &Ctor, ty: &Ty, table: &TypeTable) -> Vec<Ty> {
    match ctor {
        Ctor::Unit | Ctor::Bool(_) | Ctor::Int(_) | Ctor::Str(_) => Vec::new(),
        Ctor::Tuple(_) => match ty {
            Ty::Tuple(items) => items.clone(),
            _ => Vec::new(),
        },
        Ctor::OptionSome => match ty {
            Ty::Option(inner) => vec![(**inner).clone()],
            _ => Vec::new(),
        },
        Ctor::OptionNone => Vec::new(),
        Ctor::ResultOk | Ctor::ResultErr => match ty {
            Ty::Result(ok, err) => {
                vec![if matches!(ctor, Ctor::ResultOk) {
                    (**ok).clone()
                } else {
                    (**err).clone()
                }]
            }
            _ => Vec::new(),
        },
        Ctor::Struct(id) => {
            let args = match ty {
                Ty::Named(tid, args) if tid == id => args.clone(),
                _ => Vec::new(),
            };
            match table.get(*id).map(|d| &d.kind) {
                Some(crate::ty::TypeDefKind::Struct { fields }) => fields
                    .iter()
                    .map(|f| instantiate_params(&f.ty, &args))
                    .collect(),
                _ => Vec::new(),
            }
        }
        Ctor::Variant(id, index) => {
            let args = match ty {
                Ty::Named(tid, args) if tid == id => args.clone(),
                _ => Vec::new(),
            };
            table
                .variant(*id, *index)
                .map(|v| {
                    v.payload
                        .iter()
                        .map(|t| instantiate_params(t, &args))
                        .collect()
                })
                .unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use martite_base::Span;
    use martite_syntax::NodeId;

    fn pattern(kind: PatKind) -> Pattern {
        Pattern {
            id: NodeId(0),
            span: Span::new(0, 0),
            kind,
        }
    }

    fn arm(kind: PatKind) -> MatchArm {
        MatchArm {
            id: NodeId(0),
            span: Span::new(0, 0),
            pattern: pattern(kind),
            guard: None,
            body: martite_syntax::MatchBody::Expr(martite_syntax::Expr {
                id: NodeId(0),
                span: Span::new(0, 0),
                kind: martite_syntax::ExprKind::Unit,
            }),
        }
    }

    #[test]
    fn bool_needs_both_arms() {
        let table = TypeTable::default();
        assert!(!is_exhaustive(
            &[arm(PatKind::Bool(true))],
            &Ty::Bool,
            &table
        ));
        assert!(is_exhaustive(
            &[arm(PatKind::Bool(true)), arm(PatKind::Bool(false))],
            &Ty::Bool,
            &table
        ));
        assert!(is_exhaustive(
            &[arm(PatKind::Wild)],
            &Ty::Bool,
            &table
        ));
    }

    #[test]
    fn option_needs_both_arms() {
        let table = TypeTable::default();
        let ty = Ty::option(Ty::Int);
        let some = arm(PatKind::Variant {
            qualifier: None,
            name: "Some".into(),
            args: vec![pattern(PatKind::Bind("x".into()))],
        });
        let none = arm(PatKind::Variant {
            qualifier: None,
            name: "None".into(),
            args: Vec::new(),
        });
        assert!(is_exhaustive(&[some, none], &ty, &table));
    }

    #[test]
    fn ints_need_catch_all() {
        let table = TypeTable::default();
        assert!(!is_exhaustive(&[arm(PatKind::Int(1))], &Ty::Int, &table));
        assert!(is_exhaustive(
            &[arm(PatKind::Int(1)), arm(PatKind::Bind("x".into()))],
            &Ty::Int,
            &table
        ));
    }
}
