use martite_syntax::{FieldPat, MatchArm, PatKind, Pattern};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::ty::{Ty, TypeId, TypeTable, instantiate_params};

/// Upper bound on the number of alternative rows a single arm may expand to.
/// Or-patterns are distributed over every pattern position, so the expansion is
/// a product of the alternatives at each position.
const MAX_PATTERN_ROWS: usize = 1024;

/// Upper bound on the number of pattern columns in one usefulness query.
/// A query wider than this is answered conservatively.
const MAX_WIDTH: usize = 1024;

/// Upper bound on the number of recursive usefulness steps for one match.
/// Exceeding it is conservative: the checker reports E301 and leaves arms
/// un-flagged, but it never certifies an uncovered match or deletes a needed
/// arm.
const MAX_STEPS: u32 = 20_000;

/// Bounds for the memo table. Large sub-problems are not memoized so the memo
/// cannot grow without bound; their cost is still capped by [`MAX_STEPS`].
const MAX_MEMO_ENTRIES: usize = 4096;
const MAX_MEMO_KEY_BYTES: usize = 16 * 1024;

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

fn is_wild(pattern: &P) -> bool {
    matches!(pattern, P::Wild)
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
        let Some(rows) = arm_rows(&arm.pattern, ty, types) else {
            // The pattern expands past the alternative budget. Refuse to
            // certify the match rather than under-approximating coverage.
            return false;
        };
        matrix.extend(rows);
    }
    let query = [P::Wild];
    !Usefulness::new(types).query(&matrix, &query, std::slice::from_ref(ty))
}

/// Per-arm usefulness. An arm is useful when at least one of its alternatives
/// is not covered by the earlier unguarded arms. Guarded arms never add
/// coverage, so a guarded arm covered by an earlier unguarded arm is reported
/// as redundant, while an uncovered guarded arm is useful.
pub fn useful_arms(arms: &[MatchArm], ty: &Ty, types: &TypeTable) -> Vec<bool> {
    if matches!(ty, Ty::Never | Ty::Error) {
        return vec![false; arms.len()];
    }
    let mut matrix: Vec<Vec<P>> = Vec::new();
    let mut out = Vec::with_capacity(arms.len());
    let mut usefulness = Usefulness::new(types);
    for arm in arms {
        let Some(rows) = arm_rows(&arm.pattern, ty, types) else {
            // Unknown coverage: treat the arm as useful so L003 never removes
            // it, and add no coverage from a pattern we could not expand.
            out.push(true);
            continue;
        };
        let mut useful_any = false;
        for row in &rows {
            if usefulness.query(&matrix, row, std::slice::from_ref(ty)) {
                useful_any = true;
                break;
            }
        }
        if arm.guard.is_none() {
            matrix.extend(rows);
        }
        out.push(useful_any);
    }
    out
}

/// Expand one arm's pattern into alternative coverage rows. Or-patterns are
/// expanded recursively at every position (tuple elements, variant payloads,
/// struct fields), so no alternative is ever approximated by a wildcard.
fn arm_rows(pattern: &Pattern, ty: &Ty, types: &TypeTable) -> Option<Vec<Vec<P>>> {
    let mut expander = Expander {
        types,
        overflow: false,
    };
    let mut alternatives = Vec::new();
    expander.pattern(pattern, ty, &mut alternatives);
    if expander.overflow {
        None
    } else {
        Some(
            alternatives
                .into_iter()
                .map(|pattern| vec![pattern])
                .collect(),
        )
    }
}

struct Expander<'a> {
    types: &'a TypeTable,
    overflow: bool,
}

impl<'a> Expander<'a> {
    fn push(&mut self, out: &mut Vec<P>, pattern: P) {
        if out.len() >= MAX_PATTERN_ROWS {
            self.overflow = true;
        } else {
            out.push(pattern);
        }
    }

    /// Push every alternative representation of `pattern` (at one position).
    fn pattern(&mut self, pattern: &Pattern, ty: &Ty, out: &mut Vec<P>) {
        if self.overflow {
            return;
        }
        match &pattern.kind {
            PatKind::Or(alternatives) => {
                for alternative in alternatives {
                    self.pattern(alternative, ty, out);
                    if self.overflow {
                        return;
                    }
                }
            }
            PatKind::Wild | PatKind::Bind(_) => self.push(out, P::Wild),
            PatKind::Int(value) => self.push(out, P::Ctor(Ctor::Int(*value), Vec::new())),
            PatKind::Bool(value) => self.push(out, P::Ctor(Ctor::Bool(*value), Vec::new())),
            PatKind::Str(value) => self.push(out, P::Ctor(Ctor::Str(value.clone()), Vec::new())),
            PatKind::Tuple(items) => {
                let field_tys = match ty {
                    Ty::Tuple(tys) if tys.len() == items.len() => tys.clone(),
                    _ => vec![Ty::Error; items.len()],
                };
                let mut columns = Vec::with_capacity(items.len());
                for (item, item_ty) in items.iter().zip(&field_tys) {
                    columns.push(self.column(Some(item), item_ty));
                }
                for args in self.product(columns) {
                    self.push(out, P::Ctor(Ctor::Tuple(items.len()), args));
                }
            }
            PatKind::Variant {
                qualifier: _,
                name,
                args,
            } => self.variant(name, args, ty, out),
            PatKind::Struct {
                name: _,
                fields,
                rest: _,
            } => self.struct_pattern(ty, fields, out),
        }
    }

    fn variant(&mut self, name: &str, args: &[Pattern], ty: &Ty, out: &mut Vec<P>) {
        let types = self.types;
        match ty {
            Ty::Option(inner) => {
                if name == "Some" {
                    let payload = [(**inner).clone()];
                    let columns = self.columns(args, &payload);
                    for args in self.product(columns) {
                        self.push(out, P::Ctor(Ctor::OptionSome, args));
                    }
                } else {
                    self.push(out, P::Ctor(Ctor::OptionNone, Vec::new()));
                }
            }
            Ty::Result(ok, err) => {
                let (ctor, payload_ty) = if name == "Ok" {
                    (Ctor::ResultOk, (**ok).clone())
                } else {
                    (Ctor::ResultErr, (**err).clone())
                };
                let columns = self.columns(args, std::slice::from_ref(&payload_ty));
                for args in self.product(columns) {
                    self.push(out, P::Ctor(ctor.clone(), args));
                }
            }
            Ty::Named(id, type_args) => {
                let Some(index) = types.variant_index(*id, name) else {
                    // Unknown variant (reported as E304): keep covering it with
                    // a wildcard so exhaustiveness does not add a second error.
                    self.push(out, P::Wild);
                    return;
                };
                let payload: Vec<Ty> = types
                    .variant(*id, index)
                    .map(|variant| {
                        variant
                            .payload
                            .iter()
                            .map(|t| instantiate_params(t, type_args))
                            .collect()
                    })
                    .unwrap_or_default();
                let columns = self.columns(args, &payload);
                for args in self.product(columns) {
                    self.push(out, P::Ctor(Ctor::Variant(*id, index), args));
                }
            }
            _ => self.push(out, P::Wild),
        }
    }

    fn struct_pattern(&mut self, ty: &Ty, fields: &[FieldPat], out: &mut Vec<P>) {
        let types = self.types;
        let Ty::Named(id, type_args) = ty else {
            self.push(out, P::Wild);
            return;
        };
        let Some(def) = types.get(*id) else {
            self.push(out, P::Wild);
            return;
        };
        let crate::ty::TypeDefKind::Struct { fields: def_fields } = &def.kind else {
            self.push(out, P::Wild);
            return;
        };
        let mut columns = Vec::with_capacity(def_fields.len());
        for field in def_fields {
            let field_ty = instantiate_params(&field.ty, type_args);
            let pattern = fields
                .iter()
                .find(|candidate| candidate.name == field.name)
                .map(|candidate| &candidate.pattern);
            columns.push(self.column(pattern, &field_ty));
        }
        for args in self.product(columns) {
            self.push(out, P::Ctor(Ctor::Struct(*id), args));
        }
    }

    /// Expand one column. A missing pattern (a variant payload or a struct
    /// field the source did not mention) covers every value of that column.
    fn column(&mut self, pattern: Option<&Pattern>, ty: &Ty) -> Vec<P> {
        let mut alternatives = Vec::new();
        match pattern {
            Some(pattern) => self.pattern(pattern, ty, &mut alternatives),
            None => alternatives.push(P::Wild),
        }
        alternatives
    }

    fn columns(&mut self, patterns: &[Pattern], tys: &[Ty]) -> Vec<Vec<P>> {
        let mut columns = Vec::with_capacity(tys.len());
        for (index, ty) in tys.iter().enumerate() {
            columns.push(self.column(patterns.get(index), ty));
        }
        columns
    }

    /// Cartesian product of the per-column alternatives, bounded by
    /// [`MAX_PATTERN_ROWS`].
    fn product(&mut self, columns: Vec<Vec<P>>) -> Vec<Vec<P>> {
        let mut rows: Vec<Vec<P>> = vec![Vec::new()];
        for alternatives in columns {
            if self.overflow || alternatives.is_empty() {
                // An empty column (for example an empty or-pattern) has no
                // complete row; overflow makes the whole arm unusable.
                return Vec::new();
            }
            if rows.len().saturating_mul(alternatives.len()) > MAX_PATTERN_ROWS {
                self.overflow = true;
                return Vec::new();
            }
            let mut next = Vec::with_capacity(rows.len() * alternatives.len());
            for prefix in &rows {
                for alternative in &alternatives {
                    let mut row = Vec::with_capacity(prefix.len() + 1);
                    row.extend_from_slice(prefix);
                    row.push(alternative.clone());
                    next.push(row);
                }
            }
            rows = next;
        }
        rows
    }
}

/// Memoized Maranget usefulness with a step budget. The public entry points
/// answer `true` ("useful") whenever the budget is exhausted, which is the
/// conservative direction for both E301 and L003.
struct Usefulness<'a> {
    types: &'a TypeTable,
    steps: u32,
    memo: FxHashMap<String, bool>,
    active: FxHashSet<String>,
}

impl<'a> Usefulness<'a> {
    fn new(types: &'a TypeTable) -> Self {
        Self {
            types,
            steps: 0,
            memo: FxHashMap::default(),
            active: FxHashSet::default(),
        }
    }

    /// `matrix` covers the first `vector.len()` columns; every row has exactly
    /// that width. Returns true when `vector` has a value not matched by any
    /// row of `matrix`.
    fn query(&mut self, matrix: &[Vec<P>], vector: &[P], types: &[Ty]) -> bool {
        // All-wildcard short-circuit: a row that is wildcards everywhere (at
        // this width) covers every remaining value, so the query is not useful.
        if matrix
            .iter()
            .any(|row| row.len() == vector.len() && row.iter().all(is_wild))
        {
            return false;
        }
        if vector.is_empty() {
            return matrix.is_empty();
        }
        if vector.len() > MAX_WIDTH || self.steps >= MAX_STEPS {
            return true;
        }
        self.steps += 1;

        let key = subproblem_key(matrix, vector, types);
        let cacheable = key.len() <= MAX_MEMO_KEY_BYTES && self.memo.len() < MAX_MEMO_ENTRIES;
        if cacheable {
            if let Some(result) = self.memo.get(&key) {
                return *result;
            }
            if !self.active.insert(key.clone()) {
                // A repeated sub-problem while its first evaluation is still
                // on the stack. Answer conservatively instead of recursing
                // forever.
                return true;
            }
        }
        let result = self.useful(matrix, vector, types);
        if cacheable {
            self.active.remove(&key);
            self.memo.insert(key, result);
        }
        result
    }

    fn useful(&mut self, matrix: &[Vec<P>], vector: &[P], types: &[Ty]) -> bool {
        let head = &vector[0];
        let ty = &types[0];
        match head {
            P::Ctor(ctor, args) => {
                let specialized = specialize(matrix, ctor, args.len());
                let mut new_vector = args.clone();
                new_vector.extend_from_slice(&vector[1..]);
                let mut new_types = ctor_arg_types(ctor, ty, self.types);
                new_types.extend_from_slice(&types[1..]);
                self.query(&specialized, &new_vector, &new_types)
            }
            P::Wild => {
                let present = head_constructors(matrix);
                if let Some(all) = constructors(ty, self.types) {
                    let complete = all.iter().all(|ctor| present.contains(ctor))
                        && present.iter().all(|ctor| all.contains(ctor));
                    if complete {
                        for ctor in all {
                            let arity = ctor_arity(&ctor, ty, self.types);
                            let specialized = specialize(matrix, &ctor, arity);
                            let mut new_vector: Vec<P> = vec![P::Wild; arity];
                            new_vector.extend_from_slice(&vector[1..]);
                            let mut new_types = ctor_arg_types(&ctor, ty, self.types);
                            new_types.extend_from_slice(&types[1..]);
                            if self.query(&specialized, &new_vector, &new_types) {
                                return true;
                            }
                        }
                        return false;
                    }
                }
                // Default-matrix branch: wildcard rows do not have to be
                // expanded constructor-by-constructor.
                let specialized = default_matrix(matrix);
                self.query(&specialized, &vector[1..], &types[1..])
            }
        }
    }
}

fn specialize(matrix: &[Vec<P>], ctor: &Ctor, arity: usize) -> Vec<Vec<P>> {
    let mut out = Vec::new();
    for row in matrix {
        let Some(head) = row.first() else {
            continue;
        };
        match head {
            P::Wild => {
                let mut new_row: Vec<P> = vec![P::Wild; arity];
                new_row.extend_from_slice(&row[1..]);
                out.push(new_row);
            }
            P::Ctor(row_ctor, args) if row_ctor == ctor => {
                let mut new_row = args.clone();
                new_row.extend_from_slice(&row[1..]);
                out.push(new_row);
            }
            P::Ctor(..) => {}
        }
    }
    out
}

fn default_matrix(matrix: &[Vec<P>]) -> Vec<Vec<P>> {
    matrix
        .iter()
        .filter_map(|row| match row.first() {
            Some(P::Wild) => Some(row[1..].to_vec()),
            _ => None,
        })
        .collect()
}

fn head_constructors(matrix: &[Vec<P>]) -> Vec<Ctor> {
    let mut out = Vec::new();
    for row in matrix {
        if let Some(P::Ctor(ctor, _)) = row.first() {
            if !out.contains(ctor) {
                out.push(ctor.clone());
            }
        }
    }
    out
}

fn subproblem_key(matrix: &[Vec<P>], vector: &[P], types: &[Ty]) -> String {
    format!("{matrix:?}|{vector:?}|{types:?}")
}

fn constructors(ty: &Ty, table: &TypeTable) -> Option<Vec<Ctor>> {
    match ty {
        Ty::Never => Some(Vec::new()),
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
        Ctor::Tuple(arity) => match ty {
            Ty::Tuple(items) if items.len() == *arity => items.clone(),
            _ => vec![Ty::Error; *arity],
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
    use crate::ty::{FieldDef, TypeDef, TypeDefKind, VariantDef};
    use martite_base::Span;
    use martite_syntax::{FieldPat, MatchBody, NodeId};

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
            body: MatchBody::Expr(martite_syntax::Expr {
                id: NodeId(0),
                span: Span::new(0, 0),
                kind: martite_syntax::ExprKind::Unit,
            }),
        }
    }

    fn or(alternatives: Vec<PatKind>) -> PatKind {
        PatKind::Or(alternatives.into_iter().map(pattern).collect())
    }

    fn variant(name: &str, args: Vec<Pattern>) -> PatKind {
        PatKind::Variant {
            qualifier: None,
            name: name.into(),
            args,
        }
    }

    fn add_type(table: &mut TypeTable, name: &str, kind: TypeDefKind) -> TypeId {
        table.add(TypeDef {
            id: TypeId(0),
            name: name.into(),
            module: None,
            kind,
            params: Vec::new(),
            is_interface: false,
            is_builtin: false,
            def: None,
        })
    }

    fn recursive_enum(table: &mut TypeTable) -> TypeId {
        let id = add_type(
            table,
            "E",
            TypeDefKind::Enum {
                variants: vec![VariantDef {
                    name: "Leaf".into(),
                    payload: Vec::new(),
                }],
            },
        );
        table.get_mut(id).unwrap().kind = TypeDefKind::Enum {
            variants: vec![
                VariantDef {
                    name: "Leaf".into(),
                    payload: Vec::new(),
                },
                VariantDef {
                    name: "Next".into(),
                    payload: vec![Ty::Named(id, Vec::new())],
                },
            ],
        };
        id
    }

    fn int_enum(table: &mut TypeTable) -> TypeId {
        add_type(
            table,
            "E",
            TypeDefKind::Enum {
                variants: vec![
                    VariantDef {
                        name: "A".into(),
                        payload: vec![Ty::Int],
                    },
                    VariantDef {
                        name: "B".into(),
                        payload: Vec::new(),
                    },
                ],
            },
        )
    }

    fn struct_type(table: &mut TypeTable) -> TypeId {
        add_type(
            table,
            "S",
            TypeDefKind::Struct {
                fields: vec![
                    FieldDef {
                        name: "x".into(),
                        ty: Ty::Int,
                    },
                    FieldDef {
                        name: "y".into(),
                        ty: Ty::Bool,
                    },
                ],
            },
        )
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
        assert!(is_exhaustive(&[arm(PatKind::Wild)], &Ty::Bool, &table));
    }

    #[test]
    fn option_needs_both_arms() {
        let table = TypeTable::default();
        let ty = Ty::option(Ty::Int);
        let some = arm(variant("Some", vec![pattern(PatKind::Bind("x".into()))]));
        let none = arm(variant("None", Vec::new()));
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

    #[test]
    fn or_pattern_at_root_covers_both_arms() {
        let table = TypeTable::default();
        let either = arm(or(vec![PatKind::Bool(true), PatKind::Bool(false)]));
        assert!(is_exhaustive(&[either], &Ty::Bool, &table));
    }

    #[test]
    fn recursive_enum_wildcard_terminates() {
        let mut table = TypeTable::default();
        let id = recursive_enum(&mut table);
        let ty = Ty::Named(id, Vec::new());
        assert!(is_exhaustive(&[arm(PatKind::Wild)], &ty, &table));
    }

    #[test]
    fn recursive_enum_explicit_arms_terminate_and_missing_arm_is_reported() {
        let mut table = TypeTable::default();
        let id = recursive_enum(&mut table);
        let ty = Ty::Named(id, Vec::new());
        let leaf = arm(variant("Leaf", Vec::new()));
        let next = arm(variant("Next", vec![pattern(PatKind::Wild)]));
        assert!(is_exhaustive(&[leaf.clone(), next.clone()], &ty, &table));
        assert!(!is_exhaustive(&[leaf.clone()], &ty, &table));
        assert_eq!(useful_arms(&[next.clone()], &ty, &table), vec![true]);
        assert_eq!(useful_arms(&[leaf, next], &ty, &table), vec![true, true]);
    }

    #[test]
    fn wide_tuple_with_wildcard_is_immediate() {
        let table = TypeTable::default();
        let ty = Ty::Tuple(vec![Ty::Bool; 40]);
        assert!(is_exhaustive(&[arm(PatKind::Wild)], &ty, &table));
        let explicit = arm(PatKind::Tuple(vec![pattern(PatKind::Wild); 40]));
        assert!(is_exhaustive(&[explicit], &ty, &table));
        assert!(!is_exhaustive(&[], &ty, &table));
    }

    #[test]
    fn empty_enum_is_exhaustive_without_arms() {
        let mut table = TypeTable::default();
        let id = add_type(
            &mut table,
            "Void",
            TypeDefKind::Enum {
                variants: Vec::new(),
            },
        );
        let ty = Ty::Named(id, Vec::new());
        assert!(is_exhaustive(&[], &ty, &table));
        assert_eq!(useful_arms(&[arm(PatKind::Wild)], &ty, &table), vec![false]);
    }

    #[test]
    fn variant_payload_or_pattern_is_not_a_wildcard() {
        let mut table = TypeTable::default();
        let id = int_enum(&mut table);
        let ty = Ty::Named(id, Vec::new());
        let a = arm(variant(
            "A",
            vec![pattern(or(vec![PatKind::Int(0), PatKind::Int(1)]))],
        ));
        let b = arm(variant("B", Vec::new()));
        assert!(!is_exhaustive(&[a.clone(), b.clone()], &ty, &table));
        assert!(is_exhaustive(
            &[a.clone(), b, arm(PatKind::Wild)],
            &ty,
            &table
        ));
        assert_eq!(
            useful_arms(&[a, arm(PatKind::Wild)], &ty, &table),
            vec![true, true]
        );
    }

    #[test]
    fn tuple_or_pattern_is_not_a_wildcard() {
        let table = TypeTable::default();
        let ty = Ty::Tuple(vec![Ty::Int, Ty::Int]);
        let first = arm(PatKind::Tuple(vec![
            pattern(or(vec![PatKind::Int(0), PatKind::Int(1)])),
            pattern(PatKind::Wild),
        ]));
        assert!(!is_exhaustive(&[first.clone()], &ty, &table));
        assert!(is_exhaustive(
            &[first.clone(), arm(PatKind::Wild)],
            &ty,
            &table
        ));
        assert_eq!(
            useful_arms(&[first, arm(PatKind::Wild)], &ty, &table),
            vec![true, true]
        );
    }

    #[test]
    fn option_payload_or_pattern_is_not_a_wildcard() {
        let table = TypeTable::default();
        let ty = Ty::option(Ty::Int);
        let some = arm(variant(
            "Some",
            vec![pattern(or(vec![PatKind::Int(0), PatKind::Int(1)]))],
        ));
        let none = arm(variant("None", Vec::new()));
        assert!(!is_exhaustive(&[some.clone(), none.clone()], &ty, &table));
        assert!(is_exhaustive(
            &[some.clone(), none, arm(PatKind::Wild)],
            &ty,
            &table
        ));
        assert_eq!(
            useful_arms(&[some, arm(PatKind::Wild)], &ty, &table),
            vec![true, true]
        );
    }

    #[test]
    fn struct_field_or_pattern_is_not_a_wildcard() {
        let mut table = TypeTable::default();
        let id = struct_type(&mut table);
        let ty = Ty::Named(id, Vec::new());
        let struct_pat = arm(PatKind::Struct {
            name: "S".into(),
            fields: vec![FieldPat {
                id: NodeId(0),
                span: Span::new(0, 0),
                name: "x".into(),
                explicit: true,
                pattern: pattern(or(vec![PatKind::Int(0), PatKind::Int(1)])),
            }],
            rest: true,
        });
        assert!(!is_exhaustive(&[struct_pat.clone()], &ty, &table));
        assert_eq!(
            useful_arms(&[struct_pat, arm(PatKind::Wild)], &ty, &table),
            vec![true, true]
        );
    }
}
