//! Structural AST comparison used by `martite fmt --verify`.

use martite_syntax::{
    Arg, AssignOp, BinaryOp, Block, Bound, ConstDecl, EnumDecl, Expr, ExprKind, Field, FieldInit,
    FieldName, FnDecl, GenericParam, ImplDecl, Item, LambdaBody, LambdaParam, MatchArm, MatchBody,
    OnDecl, Param, ParamKind, PatKind, Pattern, PrimTy, Program, Stmt, StmtKind, StrExprPart,
    StructDecl, TestDecl, TyExpr, TyKind, UnaryOp, UseDecl, Variant,
};

use crate::normalize;

/// Compare two programs without considering spans or node IDs. Import runs are
/// compared after sorting, because the formatter canonically sorts them.
pub fn structurally_equal(left: &Program, right: &Program) -> bool {
    if left.is_interface != right.is_interface {
        return false;
    }
    if comment_multiset(left) != comment_multiset(right) {
        return false;
    }
    let cmp = Cmp { left, right };
    cmp.items(&left.items, &right.items)
}

pub fn comment_multiset(program: &Program) -> Vec<(bool, String)> {
    let mut comments: Vec<(bool, String)> = program
        .comments
        .iter()
        .map(|comment| (comment.doc, normalize::comment(comment)))
        .collect();
    comments.sort();
    comments
}

struct Cmp<'a> {
    left: &'a Program,
    right: &'a Program,
}

impl Cmp<'_> {
    fn items(&self, left: &[Item], right: &[Item]) -> bool {
        let mut left_index = 0;
        let mut right_index = 0;
        while left_index < left.len() && right_index < right.len() {
            if matches!(left[left_index], Item::Use(_))
                && matches!(right[right_index], Item::Use(_))
            {
                let mut left_run = Vec::new();
                while left_index < left.len() {
                    if let Item::Use(item) = &left[left_index] {
                        left_run.push(item);
                        left_index += 1;
                    } else {
                        break;
                    }
                }
                let mut right_run = Vec::new();
                while right_index < right.len() {
                    if let Item::Use(item) = &right[right_index] {
                        right_run.push(item);
                        right_index += 1;
                    } else {
                        break;
                    }
                }
                left_run.sort_by(|a, b| a.path.cmp(&b.path));
                right_run.sort_by(|a, b| a.path.cmp(&b.path));
                if left_run.len() != right_run.len()
                    || !left_run
                        .iter()
                        .zip(right_run.iter())
                        .all(|(a, b)| self.use_decl(a, b))
                {
                    return false;
                }
                continue;
            }
            if !self.item(&left[left_index], &right[right_index]) {
                return false;
            }
            left_index += 1;
            right_index += 1;
        }
        left_index == left.len() && right_index == right.len()
    }

    fn item(&self, left: &Item, right: &Item) -> bool {
        match (left, right) {
            (Item::Use(a), Item::Use(b)) => self.use_decl(a, b),
            (Item::Test(a), Item::Test(b)) => self.test(a, b),
            (Item::On(a), Item::On(b)) => self.on(a, b),
            (Item::Impl(a), Item::Impl(b)) => self.impl_block(a, b),
            (Item::Fn(a), Item::Fn(b)) => self.fn_decl(a, b),
            (Item::Struct(a), Item::Struct(b)) => self.struct_decl(a, b),
            (Item::Enum(a), Item::Enum(b)) => self.enum_decl(a, b),
            (Item::Const(a), Item::Const(b)) => self.const_decl(a, b),
            _ => false,
        }
    }

    fn use_decl(&self, left: &UseDecl, right: &UseDecl) -> bool {
        left.path == right.path && left.alias == right.alias
    }

    fn test(&self, left: &TestDecl, right: &TestDecl) -> bool {
        left.name == right.name && self.block(&left.body, &right.body)
    }

    fn on(&self, left: &OnDecl, right: &OnDecl) -> bool {
        left.name == right.name
            && self.params(&left.params, &right.params)
            && self.opt_ty(left.ret.as_ref(), right.ret.as_ref())
            && self.block(&left.body, &right.body)
    }

    fn impl_block(&self, left: &ImplDecl, right: &ImplDecl) -> bool {
        left.type_name == right.type_name
            && self.generics(&left.generics, &right.generics)
            && self.tys(&left.type_args, &right.type_args)
            && left.methods.len() == right.methods.len()
            && left
                .methods
                .iter()
                .zip(right.methods.iter())
                .all(|(a, b)| self.fn_decl(a, b))
    }

    fn fn_decl(&self, left: &FnDecl, right: &FnDecl) -> bool {
        left.is_pub == right.is_pub
            && left.name == right.name
            && self.generics(&left.generics, &right.generics)
            && self.params(&left.params, &right.params)
            && self.opt_ty(left.ret.as_ref(), right.ret.as_ref())
            && self.opt_block(left.body.as_ref(), right.body.as_ref())
    }

    fn struct_decl(&self, left: &StructDecl, right: &StructDecl) -> bool {
        left.is_pub == right.is_pub
            && left.name == right.name
            && left.opaque == right.opaque
            && self.generics(&left.generics, &right.generics)
            && left.fields.len() == right.fields.len()
            && left
                .fields
                .iter()
                .zip(right.fields.iter())
                .all(|(a, b)| self.field(a, b))
    }

    fn enum_decl(&self, left: &EnumDecl, right: &EnumDecl) -> bool {
        left.is_pub == right.is_pub
            && left.name == right.name
            && self.generics(&left.generics, &right.generics)
            && left.variants.len() == right.variants.len()
            && left
                .variants
                .iter()
                .zip(right.variants.iter())
                .all(|(a, b)| self.variant(a, b))
    }

    fn const_decl(&self, left: &ConstDecl, right: &ConstDecl) -> bool {
        left.is_pub == right.is_pub
            && left.name == right.name
            && self.ty(&left.ty, &right.ty)
            && self.expr(&left.value, &right.value)
    }

    fn field(&self, left: &Field, right: &Field) -> bool {
        left.name == right.name && self.ty(&left.ty, &right.ty)
    }

    fn variant(&self, left: &Variant, right: &Variant) -> bool {
        left.name == right.name && self.tys(&left.payload, &right.payload)
    }

    fn generics(&self, left: &[GenericParam], right: &[GenericParam]) -> bool {
        left.len() == right.len()
            && left.iter().zip(right.iter()).all(|(a, b)| {
                a.name == b.name
                    && a.bounds == b.bounds
                    && a.bounds
                        .iter()
                        .all(|bound| matches!(bound, Bound::Eq | Bound::Ord | Bound::Show))
            })
    }

    fn params(&self, left: &[Param], right: &[Param]) -> bool {
        left.len() == right.len()
            && left
                .iter()
                .zip(right.iter())
                .all(|(a, b)| match (&a.kind, &b.kind) {
                    (
                        ParamKind::SelfParam { mutable: am },
                        ParamKind::SelfParam { mutable: bm },
                    ) => am == bm,
                    (
                        ParamKind::Normal {
                            name: an,
                            mutable: am,
                            ty: at,
                        },
                        ParamKind::Normal {
                            name: bn,
                            mutable: bm,
                            ty: bt,
                        },
                    ) => an == bn && am == bm && self.ty(at, bt),
                    _ => false,
                })
    }

    fn block(&self, left: &Block, right: &Block) -> bool {
        left.stmts.len() == right.stmts.len()
            && left
                .stmts
                .iter()
                .zip(right.stmts.iter())
                .all(|(a, b)| self.stmt(a, b))
    }

    fn opt_block(&self, left: Option<&Block>, right: Option<&Block>) -> bool {
        match (left, right) {
            (Some(a), Some(b)) => self.block(a, b),
            (None, None) => true,
            _ => false,
        }
    }

    fn stmt(&self, left: &Stmt, right: &Stmt) -> bool {
        match (&left.kind, &right.kind) {
            (
                StmtKind::Let {
                    mutable: am,
                    pattern: ap,
                    ty: at,
                    value: av,
                },
                StmtKind::Let {
                    mutable: bm,
                    pattern: bp,
                    ty: bt,
                    value: bv,
                },
            ) => {
                am == bm
                    && self.pattern(ap, bp)
                    && self.opt_ty(at.as_ref(), bt.as_ref())
                    && self.expr(av, bv)
            }
            (StmtKind::While { cond: ac, body: ab }, StmtKind::While { cond: bc, body: bb }) => {
                self.expr(ac, bc) && self.block(ab, bb)
            }
            (
                StmtKind::For {
                    pattern: ap,
                    iter: ai,
                    body: ab,
                },
                StmtKind::For {
                    pattern: bp,
                    iter: bi,
                    body: bb,
                },
            ) => self.pattern(ap, bp) && self.expr(ai, bi) && self.block(ab, bb),
            (StmtKind::Return(a), StmtKind::Return(b)) => self.opt_expr(a.as_ref(), b.as_ref()),
            (StmtKind::Break, StmtKind::Break) | (StmtKind::Continue, StmtKind::Continue) => true,
            (
                StmtKind::Expr {
                    expr: ae,
                    assign: aa,
                },
                StmtKind::Expr {
                    expr: be,
                    assign: ba,
                },
            ) => {
                self.expr(ae, be)
                    && match (aa, ba) {
                        (None, None) => true,
                        (Some((ao, av)), Some((bo, bv))) => ao == bo && self.expr(av, bv),
                        _ => false,
                    }
            }
            _ => false,
        }
    }

    fn opt_expr(&self, left: Option<&Expr>, right: Option<&Expr>) -> bool {
        match (left, right) {
            (Some(a), Some(b)) => self.expr(a, b),
            (None, None) => true,
            _ => false,
        }
    }

    fn expr(&self, left: &Expr, right: &Expr) -> bool {
        if self.expr_parens(self.left, left) != self.expr_parens(self.right, right) {
            return false;
        }
        match (&left.kind, &right.kind) {
            (ExprKind::Int(a), ExprKind::Int(b)) => a == b,
            (ExprKind::Float(a), ExprKind::Float(b)) => a == b,
            (ExprKind::Bool(a), ExprKind::Bool(b)) => a == b,
            (ExprKind::Str(a), ExprKind::Str(b)) => {
                a.len() == b.len()
                    && a.iter().zip(b.iter()).all(|(a, b)| match (a, b) {
                        (StrExprPart::Text(a), StrExprPart::Text(b)) => a == b,
                        (StrExprPart::Expr(a), StrExprPart::Expr(b)) => self.expr(a, b),
                        _ => false,
                    })
            }
            (ExprKind::Unit, ExprKind::Unit) => true,
            (ExprKind::Var(a), ExprKind::Var(b)) => a == b,
            (ExprKind::Tuple(a), ExprKind::Tuple(b)) | (ExprKind::List(a), ExprKind::List(b)) => {
                self.exprs(a, b)
            }
            (ExprKind::Map(a), ExprKind::Map(b)) => {
                a.len() == b.len()
                    && a.iter()
                        .zip(b.iter())
                        .all(|((ak, av), (bk, bv))| self.expr(ak, bk) && self.expr(av, bv))
            }
            (
                ExprKind::StructLit {
                    name: an,
                    fields: af,
                    base: ab,
                },
                ExprKind::StructLit {
                    name: bn,
                    fields: bf,
                    base: bb,
                },
            ) => {
                an == bn && self.field_inits(af, bf) && self.opt_expr(ab.as_deref(), bb.as_deref())
            }
            (ExprKind::Unary { op: ao, expr: ae }, ExprKind::Unary { op: bo, expr: be }) => {
                ao == bo && self.expr(ae, be)
            }
            (
                ExprKind::Binary {
                    op: ao,
                    lhs: al,
                    rhs: ar,
                },
                ExprKind::Binary {
                    op: bo,
                    lhs: bl,
                    rhs: br,
                },
            ) => ao == bo && self.expr(al, bl) && self.expr(ar, br),
            (
                ExprKind::Range {
                    start: as_,
                    end: ae,
                    inclusive: ai,
                },
                ExprKind::Range {
                    start: bs,
                    end: be,
                    inclusive: bi,
                },
            ) => ai == bi && self.expr(as_, bs) && self.expr(ae, be),
            (
                ExprKind::If {
                    cond: ac,
                    then_branch: at,
                    else_branch: ae,
                },
                ExprKind::If {
                    cond: bc,
                    then_branch: bt,
                    else_branch: be,
                },
            ) => {
                self.expr(ac, bc)
                    && self.block(at, bt)
                    && self.opt_expr(ae.as_deref(), be.as_deref())
            }
            (
                ExprKind::Match {
                    scrutinee: as_,
                    arms: aa,
                },
                ExprKind::Match {
                    scrutinee: bs,
                    arms: ba,
                },
            ) => {
                self.expr(as_, bs)
                    && aa.len() == ba.len()
                    && aa.iter().zip(ba.iter()).all(|(a, b)| self.match_arm(a, b))
            }
            (
                ExprKind::Lambda {
                    params: ap,
                    ret: ar,
                    body: ab,
                },
                ExprKind::Lambda {
                    params: bp,
                    ret: br,
                    body: bb,
                },
            ) => {
                ap.len() == bp.len()
                    && ap
                        .iter()
                        .zip(bp.iter())
                        .all(|(a, b)| self.lambda_param(a, b))
                    && self.opt_ty(ar.as_ref(), br.as_ref())
                    && match (ab, bb) {
                        (LambdaBody::Block(a), LambdaBody::Block(b)) => self.block(a, b),
                        (LambdaBody::Expr(a), LambdaBody::Expr(b)) => self.expr(a, b),
                        _ => false,
                    }
            }
            (ExprKind::Block(a), ExprKind::Block(b)) => self.block(a, b),
            (ExprKind::Field { base: a, name: an }, ExprKind::Field { base: b, name: bn }) => {
                self.expr(a, b) && field_name_eq(an, bn)
            }
            (
                ExprKind::Call {
                    callee: ac,
                    args: aa,
                },
                ExprKind::Call {
                    callee: bc,
                    args: ba,
                },
            ) => {
                self.expr(ac, bc)
                    && aa.len() == ba.len()
                    && aa
                        .iter()
                        .zip(ba.iter())
                        .all(|(a, b)| a.mutable == b.mutable && self.expr(&a.expr, &b.expr))
            }
            (
                ExprKind::Index {
                    base: ab,
                    index: ai,
                },
                ExprKind::Index {
                    base: bb,
                    index: bi,
                },
            ) => self.expr(ab, bb) && self.expr(ai, bi),
            (ExprKind::Try(a), ExprKind::Try(b)) => self.expr(a, b),
            (ExprKind::Convert { to: at, expr: ae }, ExprKind::Convert { to: bt, expr: be }) => {
                at == bt && self.expr(ae, be)
            }
            _ => false,
        }
    }

    fn exprs(&self, left: &[Expr], right: &[Expr]) -> bool {
        left.len() == right.len() && left.iter().zip(right.iter()).all(|(a, b)| self.expr(a, b))
    }

    fn field_inits(&self, left: &[FieldInit], right: &[FieldInit]) -> bool {
        left.len() == right.len()
            && left.iter().zip(right.iter()).all(|(a, b)| {
                a.name == b.name
                    && match (&a.value, &b.value) {
                        (Some(a), Some(b)) => self.expr(a, b),
                        (None, None) => true,
                        _ => false,
                    }
            })
    }

    fn match_arm(&self, left: &MatchArm, right: &MatchArm) -> bool {
        self.pattern(&left.pattern, &right.pattern)
            && self.opt_expr(left.guard.as_ref(), right.guard.as_ref())
            && match (&left.body, &right.body) {
                (MatchBody::Block(a), MatchBody::Block(b)) => self.block(a, b),
                (MatchBody::Expr(a), MatchBody::Expr(b)) => self.expr(a, b),
                _ => false,
            }
    }

    fn lambda_param(&self, left: &LambdaParam, right: &LambdaParam) -> bool {
        self.pattern(&left.pattern, &right.pattern)
            && self.opt_ty(left.ty.as_ref(), right.ty.as_ref())
    }

    fn pattern(&self, left: &Pattern, right: &Pattern) -> bool {
        if self.pattern_parens(self.left, left) != self.pattern_parens(self.right, right) {
            return false;
        }
        match (&left.kind, &right.kind) {
            (PatKind::Wild, PatKind::Wild) => true,
            (PatKind::Bind(a), PatKind::Bind(b)) => a == b,
            (PatKind::Int(a), PatKind::Int(b)) => a == b,
            (PatKind::Bool(a), PatKind::Bool(b)) => a == b,
            (PatKind::Str(a), PatKind::Str(b)) => a == b,
            (PatKind::Tuple(a), PatKind::Tuple(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| self.pattern(a, b))
            }
            (
                PatKind::Variant {
                    qualifier: aq,
                    name: an,
                    args: aa,
                },
                PatKind::Variant {
                    qualifier: bq,
                    name: bn,
                    args: ba,
                },
            ) => {
                aq == bq
                    && an == bn
                    && aa.len() == ba.len()
                    && aa.iter().zip(ba.iter()).all(|(a, b)| self.pattern(a, b))
            }
            (
                PatKind::Struct {
                    name: an,
                    fields: af,
                    rest: ar,
                },
                PatKind::Struct {
                    name: bn,
                    fields: bf,
                    rest: br,
                },
            ) => {
                an == bn
                    && ar == br
                    && af.len() == bf.len()
                    && af
                        .iter()
                        .zip(bf.iter())
                        .all(|(a, b)| a.name == b.name && self.pattern(&a.pattern, &b.pattern))
            }
            (PatKind::Or(a), PatKind::Or(b)) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| self.pattern(a, b))
            }
            _ => false,
        }
    }

    fn ty(&self, left: &TyExpr, right: &TyExpr) -> bool {
        if self.type_parens(self.left, left) != self.type_parens(self.right, right) {
            return false;
        }
        match (&left.kind, &right.kind) {
            (TyKind::Prim(a), TyKind::Prim(b)) => a == b,
            (TyKind::Unit, TyKind::Unit) => true,
            (TyKind::Named { name: an, args: aa }, TyKind::Named { name: bn, args: ba }) => {
                an == bn && self.tys(aa, ba)
            }
            (TyKind::Optional(a), TyKind::Optional(b)) => self.ty(a, b),
            (TyKind::Tuple(a), TyKind::Tuple(b)) => self.tys(a, b),
            (
                TyKind::Fn {
                    params: ap,
                    ret: ar,
                },
                TyKind::Fn {
                    params: bp,
                    ret: br,
                },
            ) => self.tys(ap, bp) && self.opt_ty(ar.as_deref(), br.as_deref()),
            _ => false,
        }
    }

    fn tys(&self, left: &[TyExpr], right: &[TyExpr]) -> bool {
        left.len() == right.len() && left.iter().zip(right.iter()).all(|(a, b)| self.ty(a, b))
    }

    fn opt_ty(&self, left: Option<&TyExpr>, right: Option<&TyExpr>) -> bool {
        match (left, right) {
            (Some(a), Some(b)) => self.ty(a, b),
            (None, None) => true,
            _ => false,
        }
    }

    fn expr_parens(&self, program: &Program, expr: &Expr) -> usize {
        program.expression_parens.get(&expr.id).map_or(0, Vec::len)
    }

    fn type_parens(&self, program: &Program, ty: &TyExpr) -> usize {
        program.type_parens.get(&ty.id).map_or(0, Vec::len)
    }

    fn pattern_parens(&self, program: &Program, pattern: &Pattern) -> usize {
        program.pattern_parens.get(&pattern.id).map_or(0, Vec::len)
    }
}

fn field_name_eq(left: &FieldName, right: &FieldName) -> bool {
    match (left, right) {
        (FieldName::Ident(a), FieldName::Ident(b)) => a == b,
        (FieldName::Index(a), FieldName::Index(b)) => a == b,
        _ => false,
    }
}

// Keep the imports used by the comparison exhaustive helpers.
#[allow(dead_code)]
fn _types(_: AssignOp, _: BinaryOp, _: UnaryOp, _: PrimTy, _: Bound, _: Arg) {}

#[cfg(test)]
mod tests {
    use super::*;
    use martite_base::LoadLimits;
    use martite_syntax::parse_source;

    fn equal(left: &str, right: &str) -> bool {
        let left = parse_source(left, LoadLimits::default()).program;
        let right = parse_source(right, LoadLimits::default()).program;
        structurally_equal(&left, &right)
    }

    #[test]
    fn ignores_spans_comments_and_import_order() {
        assert!(equal(
            "use z\nuse a\nfn main(){let x=0xff}",
            "use a\nuse z\n\nfn main() {\n    let x = 0xFF\n}"
        ));
    }

    #[test]
    fn detects_parenthesis_and_ast_changes() {
        assert!(!equal("fn main(){let x=(1)}", "fn main(){let x=1}"));
        assert!(!equal("fn main(){let x=1+2}", "fn main(){let x=1-2}"));
    }
}
