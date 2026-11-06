//! Rules that need the checked program: resolved, typed, patterns, and flow.

use std::collections::HashSet;

use martite_base::{Applicability, Code, Diagnostic, Fix, FixEdit, Span};
use martite_check::{
    BindingId, BuiltinFn, BuiltinReceiver, CheckedProgram, DefId, DefKind, EntryKind, FnKind,
    FnSig, ModuleId, ParamSig, PreludeVariant, Resolution, Ty, TypeDefKind, exhaustive,
};
use martite_syntax::{
    AssignOp, BinaryOp, Block, Expr, ExprKind, FieldName, Item, MatchBody, PatKind, Stmt, StmtKind,
    TyExpr, TyKind, UnaryOp,
};

use crate::analysis::{self, Analysis, ModuleRef, same_expr, same_place, stable};
use crate::registry;
use crate::settings::ResolvedSettings;
use crate::trivia::find_name_span;

pub fn run(
    program: &CheckedProgram,
    analysis: &Analysis<'_>,
    module: ModuleRef<'_>,
    settings: &ResolvedSettings,
) -> Vec<Diagnostic> {
    let handler_reachable = handler_reachable(program, analysis);
    let mut checked = Checked {
        program,
        analysis,
        settings,
        module,
        out: Vec::new(),
        handler_reachable,
        summaries: true,
    };
    checked.run();
    checked.out
}

struct Checked<'a> {
    program: &'a CheckedProgram,
    analysis: &'a Analysis<'a>,
    settings: &'a ResolvedSettings,
    module: ModuleRef<'a>,
    out: Vec<Diagnostic>,
    handler_reachable: HashSet<DefId>,
    summaries: bool,
}

#[derive(Clone)]
struct Ctx<'a> {
    def: Option<DefId>,
    in_test: bool,
    loop_depth: u32,
    guards: Vec<Guard<'a>>,
}

impl<'a> Ctx<'a> {
    fn new() -> Self {
        Self {
            def: None,
            in_test: false,
            loop_depth: 0,
            guards: Vec::new(),
        }
    }
}

#[derive(Clone, Copy)]
struct Guard<'a> {
    expr: &'a Expr,
    order: u32,
}

#[derive(Clone, Copy, Default)]
struct Flow {
    mutated: Option<Span>,
    bad: Option<Span>,
    err: bool,
}

impl<'a> Checked<'a> {
    fn run(&mut self) {
        let bodies: Vec<usize> = self
            .analysis
            .bodies
            .iter()
            .enumerate()
            .filter(|(_, body)| body.module == self.module.id)
            .map(|(index, _)| index)
            .collect();
        for index in bodies {
            let body = &self.analysis.bodies[index];
            let ctx = Ctx {
                def: Some(body.def),
                in_test: body.sig.kind == FnKind::Test,
                loop_depth: 0,
                guards: Vec::new(),
            };
            self.check_block(body.block, ctx);
            self.check_state_commit(body);
        }
        self.check_consts();
        self.check_bindings();
        self.check_params();
        self.check_imports();
        self.check_private_items();
        self.check_state_type();
        self.check_tests();
    }

    fn emit(
        &mut self,
        code: Code,
        span: Span,
        message: impl Into<String>,
        help: Option<String>,
        fix: Option<Fix>,
    ) {
        let Some(severity) = self.settings.level(code).severity() else {
            return;
        };
        let Some(meta) = registry::rule(code) else {
            return;
        };
        let mut diagnostic =
            Diagnostic::lint(code, self.module.source, span, message).with_severity(severity);
        diagnostic.group = Some(meta.group);
        if let Some(help) = help {
            diagnostic.help = Some(help);
        }
        diagnostic.fix = fix;
        self.out.push(diagnostic);
    }

    fn slice(&self, span: Span) -> &'a str {
        let start = span.start.min(self.module.text.len() as u32) as usize;
        let end = span.end.min(self.module.text.len() as u32) as usize;
        if start <= end {
            &self.module.text[start..end]
        } else {
            ""
        }
    }

    fn check_consts(&mut self) {
        for item in &self.module.ast.items {
            if let Item::Const(constant) = item {
                let ctx = Ctx::new();
                self.check_expr(&constant.value, &ctx);
            }
        }
    }

    // ----- blocks and statements ---------------------------------------

    fn check_block(&mut self, block: &'a Block, ctx: Ctx<'a>) {
        let mut diverged = false;
        for stmt in &block.stmts {
            if diverged {
                let fix = Fix {
                    applicability: Applicability::Maybe,
                    edits: vec![FixEdit::new(
                        line_removal_span(self.module.text, stmt.span),
                        "",
                    )],
                };
                self.emit(
                    Code::L004,
                    stmt.span,
                    "unreachable statement",
                    Some("remove it or change the preceding control flow".into()),
                    Some(fix),
                );
                break;
            }
            self.check_stmt(stmt, &ctx);
            if self.stmt_diverges(stmt) {
                diverged = true;
            }
        }
    }

    fn check_stmt(&mut self, stmt: &'a Stmt, ctx: &Ctx<'a>) {
        match &stmt.kind {
            StmtKind::Let { pattern, value, .. } => {
                if !ctx.in_test
                    && matches!(pattern.kind, PatKind::Wild)
                    && analysis::expr_is_result(self.program, value)
                {
                    self.emit(
                        Code::L009,
                        pattern.span,
                        "a `Result` is discarded",
                        Some("use `?`, a `match`, or an explicit `is_err` check".into()),
                        None,
                    );
                }
                self.check_expr(value, ctx);
            }
            StmtKind::While { cond, body } => {
                if self.program.entry_kind == EntryKind::Plugin
                    && ctx
                        .def
                        .is_some_and(|def| self.handler_reachable.contains(&def))
                    && matches!(cond.kind, ExprKind::Bool(true))
                    && !loop_has_exit(body)
                {
                    let span = find_name_span(self.module.text, stmt.span, "while");
                    self.emit(
                        Code::L019,
                        span,
                        "`while true` has no exit",
                        Some("add a `break`, `return`, or `?`, or bound the loop".into()),
                        None,
                    );
                }
                self.check_expr(cond, ctx);
                self.check_block(
                    body,
                    Ctx {
                        loop_depth: ctx.loop_depth + 1,
                        ..ctx.clone()
                    },
                );
            }
            StmtKind::For { iter, body, .. } => {
                self.check_expr(iter, ctx);
                self.check_block(
                    body,
                    Ctx {
                        loop_depth: ctx.loop_depth + 1,
                        ..ctx.clone()
                    },
                );
            }
            StmtKind::Return(value) => {
                if let Some(value) = value {
                    self.check_expr(value, ctx);
                }
            }
            StmtKind::Break | StmtKind::Continue => {}
            StmtKind::Expr { expr, assign } => {
                if let Some((op, rhs)) = assign {
                    self.check_assignment(stmt, expr, *op, rhs, ctx);
                    self.check_expr(expr, ctx);
                    self.check_expr(rhs, ctx);
                } else {
                    self.check_expr(expr, ctx);
                }
            }
        }
    }

    fn check_assignment(
        &mut self,
        stmt: &'a Stmt,
        lhs: &'a Expr,
        op: AssignOp,
        rhs: &'a Expr,
        ctx: &Ctx<'a>,
    ) {
        let operator = operator_span(
            self.module.text,
            lhs.span.end,
            rhs.span.start,
            assign_op_str(op),
        );
        if op == AssignOp::Assign && same_place(self.program, lhs, rhs) {
            let help = if analysis::ty_has_float(self.program, lhs) {
                Some("for floats, use `x.is_nan()` to test for NaN".into())
            } else {
                None
            };
            self.emit(
                Code::L006,
                operator,
                "assigning a place to itself",
                help,
                None,
            );
        }
        if matches!(op, AssignOp::Div | AssignOp::Rem)
            && matches!(self.program.type_of(lhs.id), Some(Ty::Int))
            && is_zero_literal(rhs)
        {
            self.emit(
                Code::L010,
                rhs.span,
                "integer division by zero always panics",
                Some("use a non-zero divisor or handle the zero case".into()),
                None,
            );
        }
        if self.in_handler(ctx)
            && matches!(op, AssignOp::Add | AssignOp::Sub | AssignOp::Mul)
            && matches!(self.program.type_of(lhs.id), Some(Ty::Int))
        {
            self.emit(
                Code::L026,
                operator,
                "integer arithmetic can overflow",
                Some("validate the operands or use a wider representation".into()),
                None,
            );
        }
        if self.in_handler(ctx)
            && matches!(op, AssignOp::Div | AssignOp::Rem)
            && matches!(self.program.type_of(lhs.id), Some(Ty::Int))
            && !is_nonzero_literal(rhs)
        {
            self.emit(
                Code::L026,
                operator,
                "integer division can panic on a zero divisor",
                Some("check the divisor first".into()),
                None,
            );
        }
        if ctx.loop_depth > 0 && analysis::ty_has_float(self.program, lhs) {
            // handled by the string rule below; float concatenation is not valid
        }
        if ctx.loop_depth > 0 && matches!(self.program.type_of(lhs.id), Some(Ty::Str)) {
            let repeated = match op {
                AssignOp::Add => true,
                AssignOp::Assign => match &rhs.kind {
                    ExprKind::Binary {
                        op: BinaryOp::Add, ..
                    } => add_chain_contains(self.program, rhs, lhs),
                    _ => false,
                },
                _ => false,
            };
            if repeated {
                self.emit(
                    Code::L025,
                    operator,
                    "repeated string concatenation inside a loop",
                    Some("collect into a `list[str]` and call `join`".into()),
                    None,
                );
            }
        }
        if op == AssignOp::Assign {
            if let ExprKind::Binary {
                op: binary_op,
                lhs: binary_lhs,
                rhs: binary_rhs,
            } = &rhs.kind
            {
                if matches!(
                    binary_op,
                    BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem
                ) && same_place(self.program, lhs, binary_lhs)
                    && analysis::side_effect_free(self.program, lhs)
                    && !mutates_root(self.program, binary_rhs, lhs)
                {
                    let mut edits =
                        vec![FixEdit::new(operator, format!("{}=", binary_op.as_str()))];
                    edits.push(FixEdit::new(
                        Span::new(rhs.span.start, binary_rhs.span.start),
                        "",
                    ));
                    let _ = stmt;
                    self.emit(
                        Code::L012,
                        operator,
                        "compound assignment is shorter",
                        Some(format!(
                            "replace with `{} {}= ...`",
                            self.slice(lhs.span),
                            binary_op.as_str()
                        )),
                        Some(Fix {
                            applicability: Applicability::Machine,
                            edits,
                        }),
                    );
                }
            }
        }
    }

    fn stmt_diverges(&self, stmt: &Stmt) -> bool {
        match &stmt.kind {
            StmtKind::Return(_) | StmtKind::Break | StmtKind::Continue => true,
            StmtKind::While { cond, body } => {
                matches!(cond.kind, ExprKind::Bool(true)) && !loop_has_break(body)
            }
            StmtKind::Expr { expr, assign: None } => {
                self.is_panic_call(expr) || matches!(self.program.type_of(expr.id), Some(Ty::Never))
            }
            _ => false,
        }
    }

    fn is_panic_call(&self, expr: &Expr) -> bool {
        let ExprKind::Call { callee, .. } = &expr.kind else {
            return false;
        };
        matches!(
            self.program.resolution(callee.id),
            Some(Resolution::BuiltinFn(BuiltinFn::Panic))
        )
    }

    // ----- expressions --------------------------------------------------

    fn check_expr(&mut self, expr: &'a Expr, ctx: &Ctx<'a>) {
        match &expr.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Unit
            | ExprKind::Var(_) => {}
            ExprKind::Str(parts) => {
                for part in parts {
                    if let martite_syntax::StrExprPart::Expr(expr) = part {
                        self.check_expr(expr, ctx);
                    }
                }
            }
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                for item in items {
                    self.check_expr(item, ctx);
                }
            }
            ExprKind::Map(entries) => {
                for (key, value) in entries {
                    self.check_expr(key, ctx);
                    self.check_expr(value, ctx);
                }
            }
            ExprKind::StructLit { fields, base, .. } => {
                for field in fields {
                    if let Some(value) = &field.value {
                        self.check_expr(value, ctx);
                    }
                }
                if let Some(base) = base {
                    self.check_expr(base, ctx);
                }
            }
            ExprKind::Unary { op, expr: inner } => {
                if *op == UnaryOp::Neg
                    && matches!(self.program.type_of(expr.id), Some(Ty::Int))
                    && self.in_handler(ctx)
                {
                    self.emit(
                        Code::L026,
                        expr.span,
                        "integer negation can overflow",
                        Some("use `checked` arithmetic or validate the value first".into()),
                        None,
                    );
                }
                self.check_expr(inner, ctx);
            }
            ExprKind::Binary { op, lhs, rhs } => {
                self.check_binary(expr, *op, lhs, rhs, ctx);
                self.check_expr(lhs, ctx);
                if *op == BinaryOp::And {
                    let mut guards = ctx.guards.clone();
                    if let Some((base, order)) = guard_test(self.program, lhs) {
                        guards.push(Guard { expr: base, order });
                    }
                    let inner = Ctx {
                        guards,
                        ..ctx.clone()
                    };
                    self.check_expr(rhs, &inner);
                } else {
                    self.check_expr(rhs, ctx);
                }
            }
            ExprKind::Range { start, end, .. } => {
                self.check_expr(start, ctx);
                self.check_expr(end, ctx);
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.check_expr(cond, ctx);
                let mut guards = ctx.guards.clone();
                collect_guards(self.program, cond, &mut guards);
                let then_ctx = Ctx {
                    guards,
                    ..ctx.clone()
                };
                self.check_block(then_branch, then_ctx);
                if let Some(else_branch) = else_branch {
                    self.check_expr(else_branch, ctx);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.check_match(expr.span, scrutinee, arms);
                self.check_expr(scrutinee, ctx);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.check_expr(guard, ctx);
                    }
                    match &arm.body {
                        MatchBody::Block(block) => self.check_block(block, ctx.clone()),
                        MatchBody::Expr(expr) => self.check_expr(expr, ctx),
                    }
                }
            }
            ExprKind::Lambda { params, body, .. } => {
                self.check_eta_lambda(expr, params, body);
                match body {
                    martite_syntax::LambdaBody::Block(block) => {
                        self.check_block(block, ctx.clone())
                    }
                    martite_syntax::LambdaBody::Expr(expr) => self.check_expr(expr, ctx),
                }
            }
            ExprKind::Block(block) => self.check_block(block, ctx.clone()),
            ExprKind::Field { base, .. } => self.check_expr(base, ctx),
            ExprKind::Call { callee, args } => {
                self.check_call(expr, callee, args, ctx);
            }
            ExprKind::Index { base, index } => {
                if self.in_handler(ctx) {
                    self.emit(
                        Code::L026,
                        expr.span,
                        "indexing can panic",
                        Some("use `.get` and handle the missing value".into()),
                        None,
                    );
                }
                self.check_expr(base, ctx);
                self.check_expr(index, ctx);
            }
            ExprKind::Try(inner) => self.check_expr(inner, ctx),
            ExprKind::Convert { to, expr: inner } => {
                if *to == martite_syntax::PrimTy::Int
                    && matches!(self.program.type_of(inner.id), Some(Ty::Float))
                    && self.in_handler(ctx)
                {
                    self.emit(
                        Code::L026,
                        expr.span,
                        "`int(float)` can panic",
                        Some("check that the value is finite and in range".into()),
                        None,
                    );
                }
                self.check_expr(inner, ctx);
            }
        }
    }

    fn check_binary(
        &mut self,
        expr: &'a Expr,
        op: BinaryOp,
        lhs: &'a Expr,
        rhs: &'a Expr,
        ctx: &Ctx<'a>,
    ) {
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne) {
            let operator =
                operator_span(self.module.text, lhs.span.end, rhs.span.start, op.as_str());
            if self_comparison(self.program, lhs, rhs) {
                let help = if analysis::ty_has_float(self.program, lhs) && op == BinaryOp::Ne {
                    Some("for floats, `x.is_nan()` is the intended NaN test".into())
                } else {
                    None
                };
                self.emit(Code::L006, operator, "self-comparison", help, None);
            }
            let float = analysis::ty_has_float(self.program, lhs)
                || analysis::ty_has_float(self.program, rhs);
            if float {
                let zero =
                    |expr: &Expr| matches!(expr.kind, ExprKind::Float(value) if value == 0.0);
                if self.settings.options.float_eq_zero_ok && (zero(lhs) || zero(rhs)) {
                    // allowed
                } else {
                    self.emit(
                        Code::L007,
                        operator,
                        "equality comparison on floats is exact",
                        Some("compare with a tolerance instead".into()),
                        None,
                    );
                }
            }
        }
        if matches!(op, BinaryOp::Div | BinaryOp::Rem)
            && matches!(self.program.type_of(expr.id), Some(Ty::Int))
            && is_zero_literal(rhs)
        {
            self.emit(
                Code::L010,
                rhs.span,
                "integer division by zero always panics",
                Some("use a non-zero divisor or handle the zero case".into()),
                None,
            );
        }
        if self.in_handler(ctx) && matches!(op, BinaryOp::Div | BinaryOp::Rem) {
            if matches!(self.program.type_of(expr.id), Some(Ty::Int)) && !is_nonzero_literal(rhs) {
                self.emit(
                    Code::L026,
                    expr.span,
                    "integer division can panic on a zero divisor",
                    Some("check the divisor first".into()),
                    None,
                );
            }
        }
        if self.in_handler(ctx)
            && matches!(op, BinaryOp::Add | BinaryOp::Sub | BinaryOp::Mul)
            && matches!(self.program.type_of(expr.id), Some(Ty::Int))
        {
            self.emit(
                Code::L026,
                expr.span,
                "integer arithmetic can overflow",
                Some("validate the operands or use a wider representation".into()),
                None,
            );
        }
    }

    fn check_match(
        &mut self,
        whole: Span,
        scrutinee: &'a Expr,
        arms: &'a [martite_syntax::MatchArm],
    ) {
        let Some(ty) = self.program.type_of(scrutinee.id) else {
            return;
        };
        let useful = exhaustive::useful_arms(arms, ty, &self.program.types);
        for (index, (arm, useful)) in arms.iter().zip(useful).enumerate() {
            if !useful {
                let removal = if index + 1 < arms.len() {
                    Span::new(arm.span.start, arms[index + 1].span.start)
                } else if index > 0 {
                    Span::new(arms[index - 1].span.end, arm.span.end)
                } else {
                    arm.span
                };
                let fix = Fix {
                    applicability: Applicability::Maybe,
                    edits: vec![FixEdit::new(removal, "")],
                };
                self.emit(
                    Code::L003,
                    arm.pattern.span,
                    "this match arm is redundant or unreachable",
                    Some("remove the arm, or make an earlier arm less broad".into()),
                    Some(fix),
                );
            }
        }
        if matches!(ty, Ty::Bool) && arms.len() == 2 && arms.iter().all(|arm| arm.guard.is_none()) {
            let mut saw_true = false;
            let mut saw_false_or_wild = false;
            for arm in arms {
                match &arm.pattern.kind {
                    PatKind::Bool(true) => saw_true = true,
                    PatKind::Bool(false) | PatKind::Wild => saw_false_or_wild = true,
                    _ => {}
                }
            }
            if saw_true && saw_false_or_wild {
                let true_arm = arms
                    .iter()
                    .position(|arm| matches!(arm.pattern.kind, PatKind::Bool(true)));
                let other_arm = arms
                    .iter()
                    .position(|arm| !matches!(arm.pattern.kind, PatKind::Bool(true)));
                let fix = match (true_arm, other_arm) {
                    (Some(true_index), Some(other_index)) if true_index < other_index => {
                        let replacement = format!(
                            "if ({}) {} else {}",
                            self.slice(scrutinee.span),
                            arm_body_text(self, &arms[true_index].body),
                            arm_body_text(self, &arms[other_index].body)
                        );
                        Some(Fix {
                            applicability: Applicability::Maybe,
                            edits: vec![FixEdit::new(whole, replacement)],
                        })
                    }
                    _ => None,
                };
                self.emit(
                    Code::L014,
                    scrutinee.span,
                    "`match` on a `bool` can be an `if`",
                    Some("use `if flag { ... } else { ... }`".into()),
                    fix,
                );
            }
        }
    }

    fn check_call(
        &mut self,
        expr: &'a Expr,
        callee: &'a Expr,
        args: &'a [martite_syntax::Arg],
        ctx: &Ctx<'a>,
    ) {
        if let ExprKind::Field { base, name } = &callee.kind {
            let field = match name {
                FieldName::Ident(name) => Some(name.as_str()),
                FieldName::Index(_) => None,
            };
            if field == Some("expect")
                && matches!(
                    self.program.resolution(callee.id),
                    Some(Resolution::BuiltinMethod {
                        receiver: BuiltinReceiver::Option | BuiltinReceiver::Result,
                        ..
                    })
                )
                && self.program.entry_kind == EntryKind::Plugin
                && !ctx.in_test
                && !guarded(self, base, expr.span.end, ctx)
            {
                self.emit(
                    Code::L008,
                    callee.span,
                    "unguarded `expect` in plugin code",
                    Some("a trap aborts the hook; return `Err` or guard the call".into()),
                    None,
                );
            }
            if field == Some("unwrap_or") && args.len() == 1 {
                let option = matches!(
                    self.program.resolution(callee.id),
                    Some(Resolution::BuiltinMethod {
                        receiver: BuiltinReceiver::Option,
                        ..
                    })
                );
                if option && !simple_default(&args[0].expr) {
                    let replacement = format!(
                        "({}) ?? ({})",
                        self.slice(base.span),
                        self.slice(args[0].expr.span)
                    );
                    self.emit(
                        Code::L023,
                        expr.span,
                        "`unwrap_or` evaluates its default eagerly",
                        Some("use `??` for a lazy default".into()),
                        Some(Fix {
                            applicability: Applicability::Maybe,
                            edits: vec![FixEdit::new(expr.span, replacement)],
                        }),
                    );
                }
            }
            if self.in_handler(ctx) {
                self.check_panicking_method(callee, args, expr);
            }
        }
        if self.in_handler(ctx)
            && matches!(
                self.program.resolution(callee.id),
                Some(Resolution::BuiltinFn(BuiltinFn::Panic))
            )
        {
            self.emit(
                Code::L026,
                callee.span,
                "`panic` traps the whole hook",
                Some("return `Err` instead of panicking".into()),
                None,
            );
        }
        if self.program.entry_kind == EntryKind::Plugin
            && !ctx.in_test
            && matches!(
                self.program.resolution(callee.id),
                Some(Resolution::BuiltinFn(BuiltinFn::Print))
            )
        {
            self.emit(
                Code::L031,
                callee.span,
                "`print` in plugin code",
                Some("the host log sink may be a no-op; remove the debug call".into()),
                None,
            );
        }
        self.check_expr(callee, ctx);
        for arg in args {
            self.check_expr(&arg.expr, ctx);
        }
    }

    fn check_eta_lambda(
        &mut self,
        expr: &'a Expr,
        params: &'a [martite_syntax::LambdaParam],
        body: &'a martite_syntax::LambdaBody,
    ) {
        let body = match body {
            martite_syntax::LambdaBody::Expr(expr) => expr.as_ref(),
            martite_syntax::LambdaBody::Block(block) => {
                let [stmt] = block.stmts.as_slice() else {
                    return;
                };
                let StmtKind::Expr { expr, assign: None } = &stmt.kind else {
                    return;
                };
                expr
            }
        };
        let ExprKind::Call { callee, args } = &body.kind else {
            return;
        };
        if args.len() != params.len() || args.iter().any(|arg| arg.mutable) {
            return;
        }
        for (param, arg) in params.iter().zip(args) {
            let PatKind::Bind(name) = &param.pattern.kind else {
                return;
            };
            let ExprKind::Var(arg_name) = &arg.expr.kind else {
                return;
            };
            if name != arg_name {
                return;
            }
        }
        let def = match self.program.resolution(callee.id) {
            Some(Resolution::Fn(def)) | Some(Resolution::ModuleItem(def)) => *def,
            _ => return,
        };
        if !self
            .program
            .fn_sig(def)
            .is_some_and(|sig| sig.kind == FnKind::Free)
        {
            return;
        }
        let replacement = self.slice(callee.span).to_string();
        self.emit(
            Code::L029,
            expr.span,
            "lambda only forwards its arguments to a free function",
            Some("use the function itself".into()),
            Some(Fix {
                applicability: Applicability::Maybe,
                edits: vec![FixEdit::new(expr.span, replacement)],
            }),
        );
    }

    fn check_state_commit(&mut self, body: &'a analysis::FunctionBody<'a>) {
        if self.program.entry_kind != EntryKind::Plugin {
            return;
        }
        if !matches!(body.sig.ret, Ty::Result(_, _)) {
            return;
        }
        let mut state_bindings = Vec::new();
        if let Some(param) = body.sig.self_param() {
            if param.mutable
                && self
                    .program
                    .state_type
                    .as_ref()
                    .is_some_and(|state| *state == param.ty)
            {
                state_bindings.push(param.binding);
            }
        }
        for param in body.sig.call_params() {
            if param.mutable
                && self
                    .program
                    .state_type
                    .as_ref()
                    .is_some_and(|state| *state == param.ty)
            {
                state_bindings.push(param.binding);
            }
        }
        if state_bindings.is_empty() {
            return;
        }
        let mut flow = Flow::default();
        self.flow_block(body.block, &state_bindings, &mut flow);
        if let Some(stmt) = body.block.stmts.last() {
            if let StmtKind::Expr { expr, assign: None } = &stmt.kind {
                if is_err_expr(self.program, expr) {
                    flow_error(&mut flow);
                }
            }
        }
        if let Some(span) = flow.bad {
            self.emit(
                Code::L027,
                span,
                "state is mutated before a fallible return",
                Some("validate first and mutate last, so `Err` leaves state unchanged".into()),
                None,
            );
        }
    }

    fn call_summary(&self, expr: &Expr) -> Option<(bool, bool)> {
        let ExprKind::Call { callee, .. } = &expr.kind else {
            return None;
        };
        let def = match self.program.resolution(callee.id) {
            Some(Resolution::Fn(def)) | Some(Resolution::ModuleItem(def)) => *def,
            _ => return None,
        };
        self.helper_summary(def)
    }

    fn helper_summary(&self, def: DefId) -> Option<(bool, bool)> {
        let body = self.analysis.body(def)?;
        if body.sig.kind != FnKind::Free || !matches!(body.sig.ret, Ty::Result(_, _)) {
            return None;
        }
        let mut state = Vec::new();
        if let Some(param) = body.sig.self_param() {
            if param.mutable {
                state.push(param.binding);
            }
        }
        for param in body.sig.call_params() {
            if param.mutable
                && self
                    .program
                    .state_type
                    .as_ref()
                    .is_some_and(|state_ty| *state_ty == param.ty)
            {
                state.push(param.binding);
            }
        }
        if state.is_empty() {
            return None;
        }
        let summary = Checked {
            program: self.program,
            analysis: self.analysis,
            settings: self.settings,
            module: self.module,
            out: Vec::new(),
            handler_reachable: HashSet::new(),
            summaries: false,
        };
        let mut flow = Flow::default();
        summary.flow_block(body.block, &state, &mut flow);
        if let Some(stmt) = body.block.stmts.last() {
            if let StmtKind::Expr { expr, assign: None } = &stmt.kind {
                if is_err_expr(self.program, expr) {
                    flow_error(&mut flow);
                }
            }
        }
        Some((flow.mutated.is_some(), flow.err))
    }

    fn flow_block(&self, block: &Block, state: &[BindingId], flow: &mut Flow) {
        for stmt in &block.stmts {
            self.flow_stmt(stmt, state, flow);
            if self.stmt_diverges(stmt) {
                break;
            }
        }
    }

    fn flow_stmt(&self, stmt: &Stmt, state: &[BindingId], flow: &mut Flow) {
        match &stmt.kind {
            StmtKind::Let { value, .. } => self.flow_expr(value, state, flow),
            StmtKind::While { cond, body } => {
                self.flow_expr(cond, state, flow);
                let mut inner = *flow;
                self.flow_block(body, state, &mut inner);
                merge_flow(flow, &inner);
            }
            StmtKind::For { iter, body, .. } => {
                self.flow_expr(iter, state, flow);
                let mut inner = *flow;
                self.flow_block(body, state, &mut inner);
                merge_flow(flow, &inner);
            }
            StmtKind::Return(value) => {
                if let Some(value) = value {
                    self.flow_expr(value, state, flow);
                    if is_err_expr(self.program, value) {
                        flow_error(flow);
                    }
                }
            }
            StmtKind::Break | StmtKind::Continue => {}
            StmtKind::Expr { expr, assign } => {
                if let Some((_, rhs)) = assign {
                    if analysis::place_root(self.program, expr)
                        .is_some_and(|binding| state.contains(&binding))
                    {
                        mark_mutation(flow, stmt.span);
                    }
                    self.flow_expr(expr, state, flow);
                    self.flow_expr(rhs, state, flow);
                } else {
                    self.flow_expr(expr, state, flow);
                }
            }
        }
    }

    fn flow_expr(&self, expr: &Expr, state: &[BindingId], flow: &mut Flow) {
        match &expr.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Unit
            | ExprKind::Var(_) => {}
            ExprKind::Str(parts) => {
                for part in parts {
                    if let martite_syntax::StrExprPart::Expr(expr) = part {
                        self.flow_expr(expr, state, flow);
                    }
                }
            }
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                for item in items {
                    self.flow_expr(item, state, flow);
                }
            }
            ExprKind::Map(entries) => {
                for (key, value) in entries {
                    self.flow_expr(key, state, flow);
                    self.flow_expr(value, state, flow);
                }
            }
            ExprKind::StructLit { fields, base, .. } => {
                for field in fields {
                    if let Some(value) = &field.value {
                        self.flow_expr(value, state, flow);
                    }
                }
                if let Some(base) = base {
                    self.flow_expr(base, state, flow);
                }
            }
            ExprKind::Unary { expr, .. } => self.flow_expr(expr, state, flow),
            ExprKind::Binary { lhs, rhs, .. } => {
                self.flow_expr(lhs, state, flow);
                self.flow_expr(rhs, state, flow);
            }
            ExprKind::Range { start, end, .. } => {
                self.flow_expr(start, state, flow);
                self.flow_expr(end, state, flow);
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.flow_expr(cond, state, flow);
                let mut then_flow = *flow;
                self.flow_block(then_branch, state, &mut then_flow);
                let mut else_flow = *flow;
                if let Some(else_branch) = else_branch {
                    self.flow_expr(else_branch, state, &mut else_flow);
                }
                merge_branch(flow, &then_flow, &else_flow);
            }
            ExprKind::Match { scrutinee, arms } => {
                self.flow_expr(scrutinee, state, flow);
                let mut merged = *flow;
                for arm in arms {
                    let mut arm_flow = *flow;
                    if let Some(guard) = &arm.guard {
                        self.flow_expr(guard, state, &mut arm_flow);
                    }
                    match &arm.body {
                        MatchBody::Block(block) => self.flow_block(block, state, &mut arm_flow),
                        MatchBody::Expr(expr) => self.flow_expr(expr, state, &mut arm_flow),
                    }
                    merge_flow(&mut merged, &arm_flow);
                }
                *flow = merged;
            }
            ExprKind::Lambda { .. } => {}
            ExprKind::Block(block) => self.flow_block(block, state, flow),
            ExprKind::Field { base, .. } => self.flow_expr(base, state, flow),
            ExprKind::Call { callee, args } => {
                match self.program.resolution(callee.id) {
                    Some(Resolution::Method(def))
                        if self
                            .program
                            .fn_sig(*def)
                            .is_some_and(|sig| sig.self_mutable()) =>
                    {
                        if let ExprKind::Field { base, .. } = &callee.kind {
                            if analysis::place_root(self.program, base)
                                .is_some_and(|binding| state.contains(&binding))
                            {
                                mark_mutation(flow, callee.span);
                            }
                        }
                    }
                    Some(Resolution::BuiltinMethod { receiver, name })
                        if analysis::builtin_mutates(*receiver, name) =>
                    {
                        if let ExprKind::Field { base, .. } = &callee.kind {
                            if analysis::place_root(self.program, base)
                                .is_some_and(|binding| state.contains(&binding))
                            {
                                mark_mutation(flow, base.span);
                            }
                        }
                    }
                    _ => {}
                }
                if self.summaries {
                    if let Some((mutates, can_err)) = self.call_summary(expr) {
                        if mutates {
                            mark_mutation(flow, expr.span);
                        }
                        if can_err {
                            flow_error(flow);
                        }
                    }
                }
                for arg in args {
                    if arg.mutable
                        && analysis::place_root(self.program, &arg.expr)
                            .is_some_and(|binding| state.contains(&binding))
                    {
                        mark_mutation(flow, arg.span);
                    }
                    self.flow_expr(&arg.expr, state, flow);
                }
                self.flow_expr(callee, state, flow);
            }
            ExprKind::Index { base, index } => {
                self.flow_expr(base, state, flow);
                self.flow_expr(index, state, flow);
            }
            ExprKind::Try(inner) => {
                self.flow_expr(inner, state, flow);
                flow_error(flow);
            }
            ExprKind::Convert { expr, .. } => self.flow_expr(expr, state, flow),
        }
    }

    fn check_tests(&mut self) {
        let tests: Vec<DefId> = self
            .analysis
            .bodies
            .iter()
            .filter(|body| body.module == self.module.id && body.sig.kind == FnKind::Test)
            .map(|body| body.def)
            .collect();
        for test in tests {
            let Some(body) = self.analysis.body(test) else {
                continue;
            };
            if test_asserts(
                self.program,
                self.analysis,
                test,
                body.block,
                &mut HashSet::new(),
            ) {
                continue;
            }
            let span = find_name_span(self.module.text, body.sig.span, &body.sig.name);
            self.emit(
                Code::L030,
                span,
                "test contains no assertions",
                Some("call `assert`, directly or through a helper".into()),
                None,
            );
        }
    }

    fn check_panicking_method(
        &mut self,
        callee: &'a Expr,
        args: &'a [martite_syntax::Arg],
        call: &'a Expr,
    ) {
        let Some(Resolution::BuiltinMethod { receiver, name }) = self.program.resolution(callee.id)
        else {
            return;
        };
        let name = *name;
        let (message, help): (&str, &str) = match (receiver, name) {
            (BuiltinReceiver::List, "insert") => (
                "`insert` can panic",
                "check the index before inserting",
            ),
            (BuiltinReceiver::List, "remove_at") => (
                "`remove_at` can panic",
                "check the index before removing",
            ),
            (BuiltinReceiver::Int, "abs") => {
                ("`abs` can overflow", "handle the minimum integer explicitly")
            }
            (BuiltinReceiver::Int, "pow") => {
                ("integer `pow` can overflow", "validate the exponent and result")
            }
            (BuiltinReceiver::Float, "round") => {
                if args.first().is_some_and(|arg| {
                    !matches!(arg.expr.kind, ExprKind::Int(value) if value >= 0)
                }) {
                    ("`round` can panic on a negative digit count", "use a non-negative digit count")
                } else {
                    return;
                }
            }
            (BuiltinReceiver::Str, "replace") => {
                if args.first().is_some_and(|arg| {
                    !matches!(&arg.expr.kind, ExprKind::Str(parts)
                        if parts.len() == 1 && matches!(&parts[0], martite_syntax::StrExprPart::Text(text) if !text.is_empty()))
                }) {
                    ("`replace` can panic on an empty `from`", "check that `from` is not empty")
                } else {
                    return;
                }
            }
            _ => return,
        };
        let _ = call;
        self.emit(Code::L026, callee.span, message, Some(help.into()), None);
    }

    fn in_handler(&self, ctx: &Ctx<'a>) -> bool {
        self.program.entry_kind == EntryKind::Plugin
            && ctx
                .def
                .is_some_and(|def| self.handler_reachable.contains(&def))
    }

    // ----- declarations -------------------------------------------------

    fn check_bindings(&mut self) {
        let decls: Vec<(BindingId, _)> = self
            .analysis
            .decls
            .iter()
            .filter(|(_, decl)| decl.source == self.module.source)
            .map(|(binding, decl)| (*binding, decl.clone()))
            .collect();
        for (binding, decl) in decls {
            if decl.kind == analysis::DeclKind::SelfParam {
                continue;
            }
            if decl.name.starts_with('_') {
                continue;
            }
            if !self.analysis.reads_of(binding).is_empty() {
                continue;
            }
            if self
                .analysis
                .shorthand_reads
                .get(&self.module.id)
                .is_some_and(|names| names.contains(&decl.name))
            {
                continue;
            }
            let span = find_name_span(self.module.text, decl.span, &decl.name);
            let fix = Fix {
                applicability: Applicability::Maybe,
                edits: vec![FixEdit::new(Span::empty(span.start), "_")],
            };
            self.emit(
                Code::L001,
                span,
                format!("`{}` is never used", decl.name),
                Some("remove it, or prefix the name with `_`".into()),
                Some(fix),
            );
        }

        for (binding, decl) in &self.analysis.decls {
            if decl.source != self.module.source {
                continue;
            }
            if decl.kind != analysis::DeclKind::Var || self.analysis.is_mutated(*binding) {
                continue;
            }
            let span = keyword_before(self.module.text, decl.span.start, "var");
            self.emit(
                Code::L002,
                span,
                format!("variable `{}` is never mutated", decl.name),
                Some("replace `var` with `let`".into()),
                Some(Fix::machine(vec![FixEdit::new(span, "let")])),
            );
        }

        for binding in self.analysis.decls.keys() {
            let Some(decl) = self.analysis.decls.get(binding) else {
                continue;
            };
            if decl.source != self.module.source {
                continue;
            }
            if !decl.name.starts_with('_') || decl.name == "_" {
                continue;
            }
            let reads = self.analysis.reads_of(*binding);
            let shorthand: Vec<Span> = self
                .analysis
                .shorthand_sites
                .get(&self.module.id)
                .map(|sites| {
                    sites
                        .iter()
                        .filter(|(name, _)| *name == decl.name)
                        .map(|(_, span)| *span)
                        .collect()
                })
                .unwrap_or_default();
            let first_span = reads
                .first()
                .map(|site| site.span)
                .or_else(|| shorthand.first().copied());
            let Some(first_span) = first_span else {
                continue;
            };
            let name = decl.name.trim_start_matches('_');
            let mut edits = vec![FixEdit::new(
                find_name_span(self.module.text, decl.span, &decl.name),
                name.to_string(),
            )];
            for site in reads.iter().chain(self.analysis.writes_of(*binding)) {
                edits.push(FixEdit::new(site.span, name.to_string()));
            }
            for span in shorthand {
                edits.push(FixEdit::new(span, name.to_string()));
            }
            self.emit(
                Code::L028,
                first_span,
                format!("`{}` is marked unused but is read here", decl.name),
                Some(format!("rename it to `{name}`")),
                Some(Fix {
                    applicability: Applicability::Maybe,
                    edits,
                }),
            );
        }
    }

    fn check_params(&mut self) {
        let sigs: Vec<_> = self
            .program
            .functions
            .values()
            .filter(|sig| sig.module == self.module.id)
            .collect();
        for sig in sigs {
            for param in &sig.params {
                if !param.mutable || self.analysis.is_mutated(param.binding) {
                    continue;
                }
                let span = find_name_span(self.module.text, param.span, "mut");
                let fix = self.mut_param_fix(sig, param, span);
                self.emit(
                    Code::L015,
                    span,
                    format!("`mut` parameter `{}` is never mutated", param.name),
                    Some("remove `mut` from the parameter".into()),
                    fix,
                );
            }
        }
    }

    fn mut_param_fix(&self, sig: &FnSig, param: &ParamSig, declaration: Span) -> Option<Fix> {
        if sig.is_pub || sig.kind == FnKind::Handler {
            return None;
        }
        if self.analysis.function_value_uses.contains(&sig.def) {
            return None;
        }
        let mut declaration = declaration;
        let bytes = self.module.text.as_bytes();
        while (declaration.end as usize) < bytes.len()
            && matches!(bytes[declaration.end as usize], b' ' | b'\t')
        {
            declaration.end += 1;
        }
        let mut edits = vec![FixEdit::new(declaration, "")];
        if !param.is_self {
            let index = sig
                .call_params()
                .iter()
                .position(|candidate| candidate.binding == param.binding)?;
            for call in self
                .analysis
                .calls
                .iter()
                .filter(|call| call.callee == sig.def)
            {
                let (arg_span, mutable) = *call.args.get(index)?;
                if !mutable {
                    return None;
                }
                let text = self.analysis.source_text(call.source)?;
                let span = find_mut_keyword(text, arg_span)?;
                edits.push(FixEdit::in_source(call.source, span, ""));
            }
        }
        Some(Fix {
            applicability: Applicability::Maybe,
            edits,
        })
    }

    fn check_imports(&mut self) {
        let Some(resolved_module) = self
            .program
            .resolved
            .modules
            .iter()
            .find(|module| module.id == self.module.id)
        else {
            return;
        };
        let def_uses = self.analysis.def_uses.get(&self.module.id);
        let module_uses = self.analysis.module_uses.get(&self.module.id);
        let module_names = self.analysis.module_name_uses.get(&self.module.id);
        let builtin_uses = self.analysis.builtin_uses.get(&self.module.id);
        for import in &resolved_module.uses {
            let local = import
                .alias
                .clone()
                .or_else(|| import.path.last().cloned())
                .unwrap_or_default();
            let used = match &import.target {
                martite_resolve::ModuleTargetUse::Module(target) => {
                    module_uses
                        .map(|uses| uses.contains(target))
                        .unwrap_or(false)
                        || module_names
                            .map(|names| names.contains(&local))
                            .unwrap_or(false)
                }
                martite_resolve::ModuleTargetUse::Item(def) => {
                    let direct = def_uses.map(|uses| uses.contains(def)).unwrap_or(false);
                    if direct {
                        true
                    } else {
                        self.analysis
                            .type_names_in(self.module.id)
                            .any(|name| *name == local)
                    }
                }
                martite_resolve::ModuleTargetUse::BuiltinItem(item) => {
                    builtin_uses
                        .map(|uses| uses.contains(item))
                        .unwrap_or(false)
                        || self
                            .analysis
                            .type_names_in(self.module.id)
                            .any(|name| *name == local)
                }
            };
            if used {
                continue;
            }
            let span = find_name_span(self.module.text, import.span, &local);
            let fix = Fix::machine(vec![FixEdit::new(
                line_removal_span(self.module.text, import.span),
                "",
            )]);
            self.emit(
                Code::L001,
                span,
                format!("import `{local}` is never used"),
                Some("remove the import".into()),
                Some(fix),
            );
        }
    }

    fn check_private_items(&mut self) {
        for def in &self.program.resolved.defs {
            if def.module != self.module.id || def.is_pub {
                continue;
            }
            match def.kind {
                DefKind::Fn | DefKind::Method => {
                    if self.analysis.is_reachable(def.id) {
                        continue;
                    }
                }
                DefKind::Const => {
                    if self
                        .analysis
                        .def_uses
                        .values()
                        .any(|uses| uses.contains(&def.id))
                    {
                        continue;
                    }
                }
                DefKind::Struct | DefKind::Enum => {
                    if self
                        .analysis
                        .type_names_in(self.module.id)
                        .any(|name| *name == def.name)
                    {
                        continue;
                    }
                }
                _ => continue,
            }
            let resolved_module = self.program.resolved.module(def.module);
            let span = if let Some(decl) = self.program.function_decl(def.id) {
                find_name_span(self.module.text, decl.span, &def.name)
            } else {
                let Some(item) = resolved_module.ast.items.get(def.item_index) else {
                    continue;
                };
                find_name_span(self.module.text, item.span(), &def.name)
            };
            self.emit(
                Code::L001,
                span,
                format!("private item `{}` is never used", def.name),
                Some("remove it, or make it `pub` if it is part of the API".into()),
                None,
            );
        }
    }

    // ----- L021 ---------------------------------------------------------

    fn check_state_type(&mut self) {
        if self.program.entry_kind != EntryKind::Plugin {
            return;
        }
        let Some(state_ty) = &self.program.state_type else {
            return;
        };
        let Some(init) = self.program.init else {
            return;
        };
        let info = self.program.def(init);
        if info.module != self.module.id {
            return;
        }
        let resolved_module = self.program.resolved.module(info.module);
        let Some(Item::On(decl)) = resolved_module.ast.items.get(info.item_index) else {
            return;
        };
        let Some(ret) = &decl.ret else {
            return;
        };
        if let Some(span) = self.find_handle(state_ty, ret, info.module, &mut HashSet::new()) {
            self.emit(
                Code::L021,
                span,
                "an opaque host handle is stored in the plugin state",
                Some("store an id and re-resolve the handle after a restore".into()),
                None,
            );
        }
    }

    fn find_handle(
        &self,
        ty: &Ty,
        tyexpr: &TyExpr,
        module: ModuleId,
        visited: &mut HashSet<u32>,
    ) -> Option<Span> {
        match ty {
            Ty::Named(id, args) => {
                let def = self.program.types.get(*id)?;
                if args
                    .iter()
                    .any(|arg| ty_reaches_opaque(arg, &self.program.types, &mut HashSet::new()))
                {
                    return Some(tyexpr.span);
                }
                if !visited.insert(id.0) {
                    return None;
                }
                if matches!(def.kind, TypeDefKind::Opaque) {
                    return Some(tyexpr.span);
                }
                if def.is_interface || def.is_builtin {
                    return if self.reaches_opaque(*id, &mut HashSet::new()) {
                        Some(tyexpr.span)
                    } else {
                        None
                    };
                }
                let Some(def_id) = def.def else {
                    return None;
                };
                let info = self.program.def(def_id);
                let module_ref = self.program.resolved.module(info.module);
                let Some(item) = module_ref.ast.items.get(info.item_index) else {
                    return None;
                };
                match (&def.kind, item) {
                    (TypeDefKind::Struct { fields }, Item::Struct(decl)) => {
                        for (field_ty, field_expr) in fields.iter().zip(&decl.fields) {
                            if let Some(span) =
                                self.find_handle(&field_ty.ty, &field_expr.ty, info.module, visited)
                            {
                                return Some(span);
                            }
                        }
                    }
                    (TypeDefKind::Enum { variants }, Item::Enum(decl)) => {
                        for variant in variants {
                            let Some(ast) =
                                decl.variants.iter().find(|ast| ast.name == variant.name)
                            else {
                                continue;
                            };
                            for (payload_ty, payload_expr) in
                                variant.payload.iter().zip(&ast.payload)
                            {
                                if let Some(span) =
                                    self.find_handle(payload_ty, payload_expr, info.module, visited)
                                {
                                    return Some(span);
                                }
                            }
                        }
                    }
                    _ => {}
                }
                let _ = (module, args);
                None
            }
            Ty::List(inner) => {
                let inner_expr = type_arg(tyexpr, "list", 0)?;
                self.find_handle(inner, inner_expr, module, visited)
            }
            Ty::Map(key, value) => {
                let key_expr = type_arg(tyexpr, "map", 0)?;
                let value_expr = type_arg(tyexpr, "map", 1)?;
                self.find_handle(key, key_expr, module, visited)
                    .or_else(|| self.find_handle(value, value_expr, module, visited))
            }
            Ty::Option(inner) => {
                let inner_expr = option_arg(tyexpr)?;
                self.find_handle(inner, inner_expr, module, visited)
            }
            Ty::Result(ok, err) => {
                let ok_expr = type_arg(tyexpr, "Result", 0)?;
                let err_expr = type_arg(tyexpr, "Result", 1)?;
                self.find_handle(ok, ok_expr, module, visited)
                    .or_else(|| self.find_handle(err, err_expr, module, visited))
            }
            Ty::Tuple(items) => {
                let TyKind::Tuple(exprs) = &tyexpr.kind else {
                    return None;
                };
                for (item, expr) in items.iter().zip(exprs) {
                    if let Some(span) = self.find_handle(item, expr, module, visited) {
                        return Some(span);
                    }
                }
                None
            }
            _ => None,
        }
    }

    fn reaches_opaque(&self, id: martite_check::TypeId, visited: &mut HashSet<u32>) -> bool {
        if !visited.insert(id.0) {
            return false;
        }
        let Some(def) = self.program.types.get(id) else {
            return false;
        };
        match &def.kind {
            TypeDefKind::Opaque => true,
            TypeDefKind::Struct { fields } => {
                let mut found = false;
                for field in fields {
                    if ty_reaches_opaque(&field.ty, &self.program.types, visited) {
                        found = true;
                        break;
                    }
                }
                found
            }
            TypeDefKind::Enum { variants } => {
                let mut found = false;
                for variant in variants {
                    for ty in &variant.payload {
                        if ty_reaches_opaque(ty, &self.program.types, visited) {
                            found = true;
                            break;
                        }
                    }
                    if found {
                        break;
                    }
                }
                found
            }
        }
    }
}

fn ty_reaches_opaque(
    ty: &Ty,
    types: &martite_check::TypeTable,
    visited: &mut HashSet<u32>,
) -> bool {
    match ty {
        Ty::Named(id, args) => {
            if !visited.insert(id.0) {
                return false;
            }
            let Some(def) = types.get(*id) else {
                return false;
            };
            let own = match &def.kind {
                TypeDefKind::Opaque => true,
                TypeDefKind::Struct { fields } => {
                    let mut found = false;
                    for field in fields {
                        if ty_reaches_opaque(&field.ty, types, visited) {
                            found = true;
                            break;
                        }
                    }
                    found
                }
                TypeDefKind::Enum { variants } => {
                    let mut found = false;
                    for variant in variants {
                        for ty in &variant.payload {
                            if ty_reaches_opaque(ty, types, visited) {
                                found = true;
                                break;
                            }
                        }
                        if found {
                            break;
                        }
                    }
                    found
                }
            };
            if own {
                return true;
            }
            for arg in args {
                if ty_reaches_opaque(arg, types, visited) {
                    return true;
                }
            }
            false
        }
        Ty::List(inner) | Ty::Option(inner) => ty_reaches_opaque(inner, types, visited),
        Ty::Map(key, value) | Ty::Result(key, value) => {
            ty_reaches_opaque(key, types, visited) || ty_reaches_opaque(value, types, visited)
        }
        Ty::Tuple(items) => items
            .iter()
            .any(|item| ty_reaches_opaque(item, types, visited)),
        _ => false,
    }
}

fn type_arg<'a>(tyexpr: &'a TyExpr, name: &str, index: usize) -> Option<&'a TyExpr> {
    match &tyexpr.kind {
        TyKind::Named { name: actual, args } if actual == name => args.get(index),
        _ => None,
    }
}

fn option_arg(tyexpr: &TyExpr) -> Option<&TyExpr> {
    match &tyexpr.kind {
        TyKind::Optional(inner) => Some(inner),
        TyKind::Named { name, args } if name == "Option" => args.first(),
        _ => None,
    }
}

// ----- helpers ----------------------------------------------------------

fn assign_op_str(op: AssignOp) -> &'static str {
    match op {
        AssignOp::Assign => "=",
        AssignOp::Add => "+=",
        AssignOp::Sub => "-=",
        AssignOp::Mul => "*=",
        AssignOp::Div => "/=",
        AssignOp::Rem => "%=",
    }
}

fn operator_span(text: &str, lhs_end: u32, rhs_start: u32, op: &str) -> Span {
    if op == "=" || op == "+=" || op == "-=" || op == "*=" || op == "/=" || op == "%=" {
        let start = lhs_end.min(text.len() as u32) as usize;
        let end = rhs_start.min(text.len() as u32) as usize;
        if start <= end {
            if let Some(offset) = text[start..end].find(op) {
                let at = start + offset;
                return Span::new(at as u32, (at + op.len()) as u32);
            }
        }
        return Span::empty(lhs_end);
    }
    crate::trivia::operator_span(text, lhs_end, rhs_start, op)
}

fn find_mut_keyword(text: &str, span: Span) -> Option<Span> {
    let start = span.start.min(text.len() as u32) as usize;
    let end = span.end.min(text.len() as u32) as usize;
    let offset = text[start..end].find("mut")?;
    let at = start + offset;
    let mut after = at + 3;
    // Remove the separator too, so the fix does not leave `fn f( x)` or
    // `f( x)` behind.
    while after < text.len() && matches!(text.as_bytes()[after], b' ' | b'\t') {
        after += 1;
    }
    Some(Span::new(at as u32, after as u32))
}

fn line_removal_span(text: &str, span: Span) -> Span {
    let start = span.start.min(text.len() as u32) as usize;
    let end = span.end.min(text.len() as u32) as usize;
    let line_start = text[..start]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let line_end = text[end..]
        .find('\n')
        .map(|offset| end + offset + 1)
        .unwrap_or(text.len());
    Span::new(line_start as u32, line_end as u32)
}

fn keyword_before(text: &str, at: u32, keyword: &str) -> Span {
    let start = at.min(text.len() as u32) as usize;
    let line_start = text[..start]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    if let Some(offset) = text[line_start..start].rfind(keyword) {
        let at = line_start + offset;
        return Span::new(at as u32, (at + keyword.len()) as u32);
    }
    Span::empty(at as u32)
}

fn self_comparison(program: &CheckedProgram, lhs: &Expr, rhs: &Expr) -> bool {
    if analysis::place_root(program, lhs).is_some()
        && analysis::place_root(program, rhs).is_some()
        && same_place(program, lhs, rhs)
    {
        return true;
    }
    analysis::side_effect_free(program, lhs)
        && analysis::side_effect_free(program, rhs)
        && same_expr(lhs, rhs)
}

fn add_chain_contains(program: &CheckedProgram, expr: &Expr, target: &Expr) -> bool {
    let ExprKind::Binary {
        op: BinaryOp::Add,
        lhs,
        rhs,
    } = &expr.kind
    else {
        return false;
    };
    same_place(program, lhs, target)
        || same_place(program, rhs, target)
        || add_chain_contains(program, lhs, target)
        || add_chain_contains(program, rhs, target)
}

fn is_zero_literal(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Int(0) => true,
        ExprKind::Unary {
            op: UnaryOp::Neg,
            expr,
        } => is_zero_literal(expr),
        _ => false,
    }
}

fn is_nonzero_literal(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Int(value) => *value != 0,
        _ => false,
    }
}

fn simple_default(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Unit
        | ExprKind::Var(_) => true,
        ExprKind::Field { .. } => true,
        _ => false,
    }
}

fn same_root(program: &CheckedProgram, left: &Expr, right: &Expr) -> bool {
    match (
        analysis::place_root(program, left),
        analysis::place_root(program, right),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => same_place(program, left, right),
    }
}

fn mutates_root(program: &CheckedProgram, expr: &Expr, root: &Expr) -> bool {
    // Conservative containment check: a mut argument or mut-self call rooted at
    // the same place as the assignment target.
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            if let ExprKind::Field { base, .. } = &callee.kind {
                if let Some(Resolution::Method(def)) = program.resolution(callee.id) {
                    if program.fn_sig(*def).is_some_and(|sig| sig.self_mutable())
                        && same_root(program, base, root)
                    {
                        return true;
                    }
                }
            }
            args.iter().any(|arg| {
                arg.mutable && same_root(program, &arg.expr, root)
                    || mutates_root(program, &arg.expr, root)
            }) || mutates_root(program, callee, root)
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            mutates_root(program, lhs, root) || mutates_root(program, rhs, root)
        }
        ExprKind::Unary { expr, .. } => mutates_root(program, expr, root),
        ExprKind::Tuple(items) | ExprKind::List(items) => {
            items.iter().any(|item| mutates_root(program, item, root))
        }
        ExprKind::Map(entries) => entries.iter().any(|(key, value)| {
            mutates_root(program, key, root) || mutates_root(program, value, root)
        }),
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            block_mutates_root(program, then_branch, root)
                || mutates_root(program, cond, root)
                || else_branch
                    .as_ref()
                    .is_some_and(|expr| mutates_root(program, expr, root))
        }
        _ => false,
    }
}

fn block_mutates_root(program: &CheckedProgram, block: &Block, root: &Expr) -> bool {
    block.stmts.iter().any(|stmt| match &stmt.kind {
        StmtKind::Let { value, .. } => mutates_root(program, value, root),
        StmtKind::While { cond, body } => {
            mutates_root(program, cond, root) || block_mutates_root(program, body, root)
        }
        StmtKind::For { iter, body, .. } => {
            mutates_root(program, iter, root) || block_mutates_root(program, body, root)
        }
        StmtKind::Return(Some(value)) => mutates_root(program, value, root),
        StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue => false,
        StmtKind::Expr { expr, assign } => {
            mutates_root(program, expr, root)
                || assign
                    .as_ref()
                    .is_some_and(|(_, rhs)| mutates_root(program, rhs, root))
        }
    })
}

fn loop_has_exit(block: &Block) -> bool {
    exit_in_block(block, 0)
}

fn loop_has_break(block: &Block) -> bool {
    break_in_block(block, 0)
}

fn break_in_block(block: &Block, loop_depth: u32) -> bool {
    block
        .stmts
        .iter()
        .any(|stmt| break_in_stmt(stmt, loop_depth))
}

fn break_in_stmt(stmt: &Stmt, loop_depth: u32) -> bool {
    match &stmt.kind {
        StmtKind::Break => loop_depth == 0,
        StmtKind::Continue | StmtKind::Return(_) => false,
        StmtKind::While { cond, body } => {
            break_in_expr(cond, loop_depth) || break_in_block(body, loop_depth + 1)
        }
        StmtKind::For { iter, body, .. } => {
            break_in_expr(iter, loop_depth) || break_in_block(body, loop_depth + 1)
        }
        StmtKind::Let { value, .. } => break_in_expr(value, loop_depth),
        StmtKind::Expr { expr, assign } => {
            break_in_expr(expr, loop_depth)
                || assign
                    .as_ref()
                    .is_some_and(|(_, rhs)| break_in_expr(rhs, loop_depth))
        }
    }
}

fn break_in_expr(expr: &Expr, loop_depth: u32) -> bool {
    match &expr.kind {
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Unit
        | ExprKind::Var(_) => false,
        ExprKind::Str(parts) => parts.iter().any(|part| match part {
            martite_syntax::StrExprPart::Text(_) => false,
            martite_syntax::StrExprPart::Expr(expr) => break_in_expr(expr, loop_depth),
        }),
        ExprKind::Tuple(items) | ExprKind::List(items) => {
            items.iter().any(|item| break_in_expr(item, loop_depth))
        }
        ExprKind::Map(entries) => entries
            .iter()
            .any(|(key, value)| break_in_expr(key, loop_depth) || break_in_expr(value, loop_depth)),
        ExprKind::StructLit { fields, base, .. } => {
            fields.iter().any(|field| {
                field
                    .value
                    .as_ref()
                    .is_some_and(|value| break_in_expr(value, loop_depth))
            }) || base
                .as_ref()
                .is_some_and(|base| break_in_expr(base, loop_depth))
        }
        ExprKind::Unary { expr, .. } => break_in_expr(expr, loop_depth),
        ExprKind::Binary { lhs, rhs, .. } => {
            break_in_expr(lhs, loop_depth) || break_in_expr(rhs, loop_depth)
        }
        ExprKind::Range { start, end, .. } => {
            break_in_expr(start, loop_depth) || break_in_expr(end, loop_depth)
        }
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            break_in_expr(cond, loop_depth)
                || break_in_block(then_branch, loop_depth)
                || else_branch
                    .as_ref()
                    .is_some_and(|branch| break_in_expr(branch, loop_depth))
        }
        ExprKind::Match { scrutinee, arms } => {
            break_in_expr(scrutinee, loop_depth)
                || arms.iter().any(|arm| {
                    arm.guard
                        .as_ref()
                        .is_some_and(|guard| break_in_expr(guard, loop_depth))
                        || match &arm.body {
                            MatchBody::Block(block) => break_in_block(block, loop_depth),
                            MatchBody::Expr(expr) => break_in_expr(expr, loop_depth),
                        }
                })
        }
        ExprKind::Lambda { .. } => false,
        ExprKind::Block(block) => break_in_block(block, loop_depth),
        ExprKind::Field { base, .. } => break_in_expr(base, loop_depth),
        ExprKind::Call { callee, args } => {
            break_in_expr(callee, loop_depth)
                || args.iter().any(|arg| break_in_expr(&arg.expr, loop_depth))
        }
        ExprKind::Index { base, index } => {
            break_in_expr(base, loop_depth) || break_in_expr(index, loop_depth)
        }
        ExprKind::Try(inner) => break_in_expr(inner, loop_depth),
        ExprKind::Convert { expr, .. } => break_in_expr(expr, loop_depth),
    }
}

fn exit_in_block(block: &Block, loop_depth: u32) -> bool {
    block
        .stmts
        .iter()
        .any(|stmt| exit_in_stmt(stmt, loop_depth))
}

fn exit_in_stmt(stmt: &Stmt, loop_depth: u32) -> bool {
    match &stmt.kind {
        StmtKind::Break => loop_depth == 0,
        StmtKind::Return(_) => true,
        StmtKind::While { cond, body } => {
            exit_in_expr(cond, loop_depth) || exit_in_block(body, loop_depth + 1)
        }
        StmtKind::For { iter, body, .. } => {
            exit_in_expr(iter, loop_depth) || exit_in_block(body, loop_depth + 1)
        }
        StmtKind::Let { value, .. } => exit_in_expr(value, loop_depth),
        StmtKind::Expr { expr, assign } => {
            exit_in_expr(expr, loop_depth)
                || assign
                    .as_ref()
                    .is_some_and(|(_, rhs)| exit_in_expr(rhs, loop_depth))
        }
        StmtKind::Continue => false,
    }
}

fn exit_in_expr(expr: &Expr, loop_depth: u32) -> bool {
    match &expr.kind {
        ExprKind::Try(_) => true,
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Unit
        | ExprKind::Var(_) => false,
        ExprKind::Str(parts) => parts.iter().any(|part| match part {
            martite_syntax::StrExprPart::Text(_) => false,
            martite_syntax::StrExprPart::Expr(expr) => exit_in_expr(expr, loop_depth),
        }),
        ExprKind::Tuple(items) | ExprKind::List(items) => {
            items.iter().any(|item| exit_in_expr(item, loop_depth))
        }
        ExprKind::Map(entries) => entries
            .iter()
            .any(|(key, value)| exit_in_expr(key, loop_depth) || exit_in_expr(value, loop_depth)),
        ExprKind::StructLit { fields, base, .. } => {
            fields.iter().any(|field| {
                field
                    .value
                    .as_ref()
                    .is_some_and(|value| exit_in_expr(value, loop_depth))
            }) || base
                .as_ref()
                .is_some_and(|base| exit_in_expr(base, loop_depth))
        }
        ExprKind::Unary { expr, .. } => exit_in_expr(expr, loop_depth),
        ExprKind::Binary { lhs, rhs, .. } => {
            exit_in_expr(lhs, loop_depth) || exit_in_expr(rhs, loop_depth)
        }
        ExprKind::Range { start, end, .. } => {
            exit_in_expr(start, loop_depth) || exit_in_expr(end, loop_depth)
        }
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            exit_in_expr(cond, loop_depth)
                || exit_in_block(then_branch, loop_depth)
                || else_branch
                    .as_ref()
                    .is_some_and(|branch| exit_in_expr(branch, loop_depth))
        }
        ExprKind::Match { scrutinee, arms } => {
            exit_in_expr(scrutinee, loop_depth)
                || arms.iter().any(|arm| {
                    arm.guard
                        .as_ref()
                        .is_some_and(|guard| exit_in_expr(guard, loop_depth))
                        || match &arm.body {
                            MatchBody::Block(block) => exit_in_block(block, loop_depth),
                            MatchBody::Expr(expr) => exit_in_expr(expr, loop_depth),
                        }
                })
        }
        ExprKind::Lambda { .. } => false,
        ExprKind::Block(block) => exit_in_block(block, loop_depth),
        ExprKind::Field { base, .. } => exit_in_expr(base, loop_depth),
        ExprKind::Call { callee, args } => {
            exit_in_expr(callee, loop_depth)
                || args.iter().any(|arg| exit_in_expr(&arg.expr, loop_depth))
        }
        ExprKind::Index { base, index } => {
            exit_in_expr(base, loop_depth) || exit_in_expr(index, loop_depth)
        }
        ExprKind::Convert { expr, .. } => exit_in_expr(expr, loop_depth),
    }
}

fn guard_test<'a>(program: &CheckedProgram, expr: &'a Expr) -> Option<(&'a Expr, u32)> {
    let ExprKind::Call { callee, args } = &expr.kind else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    let ExprKind::Field { base, name } = &callee.kind else {
        return None;
    };
    let name = match name {
        FieldName::Ident(name) => name.as_str(),
        FieldName::Index(_) => return None,
    };
    if name != "is_some" && name != "is_ok" {
        return None;
    }
    if !stable(program, base) {
        return None;
    }
    Some((base, expr.span.end))
}

fn collect_guards<'a>(program: &CheckedProgram, cond: &'a Expr, out: &mut Vec<Guard<'a>>) {
    match &cond.kind {
        ExprKind::Binary {
            op: BinaryOp::And,
            lhs,
            rhs,
        } => {
            collect_guards(program, lhs, out);
            collect_guards(program, rhs, out);
        }
        _ => {
            if let Some((base, order)) = guard_test(program, cond) {
                out.push(Guard { expr: base, order });
            }
        }
    }
}

fn guarded(checked: &Checked<'_>, base: &Expr, call_start: u32, ctx: &Ctx<'_>) -> bool {
    if !stable(checked.program, base) {
        return false;
    }
    let mut roots = Vec::new();
    collect_roots(checked.program, base, &mut roots);
    ctx.guards.iter().any(|guard| {
        same_expr(guard.expr, base)
            && !roots.iter().any(|binding| {
                checked.analysis.writes_of(*binding).iter().any(|site| {
                    site.source == checked.module.source
                        && site.span.start >= guard.order
                        && site.span.start < call_start
                })
            })
    })
}

fn collect_roots(program: &CheckedProgram, expr: &Expr, out: &mut Vec<BindingId>) {
    if let ExprKind::Var(_) = &expr.kind {
        if let Some(Resolution::Local(binding)) = program.resolution(expr.id) {
            out.push(*binding);
        }
        return;
    }
    match &expr.kind {
        ExprKind::Field { base, .. } => collect_roots(program, base, out),
        ExprKind::Index { base, index } => {
            collect_roots(program, base, out);
            collect_roots(program, index, out);
        }
        ExprKind::Call { callee, args } => {
            collect_roots(program, callee, out);
            for arg in args {
                collect_roots(program, &arg.expr, out);
            }
        }
        ExprKind::Unary { expr, .. } => collect_roots(program, expr, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_roots(program, lhs, out);
            collect_roots(program, rhs, out);
        }
        _ => {}
    }
}

fn mark_mutation(flow: &mut Flow, span: Span) {
    if flow.mutated.is_none() {
        flow.mutated = Some(span);
    }
}

fn flow_error(flow: &mut Flow) {
    flow.err = true;
    if flow.bad.is_none() {
        flow.bad = flow.mutated;
    }
}

fn merge_flow(target: &mut Flow, other: &Flow) {
    if target.mutated.is_none() {
        target.mutated = other.mutated;
    }
    if target.bad.is_none() {
        target.bad = other.bad;
    }
    target.err |= other.err;
}

fn merge_branch(target: &mut Flow, left: &Flow, right: &Flow) {
    target.mutated = left.mutated.or(right.mutated);
    target.bad = left.bad.or(right.bad);
    target.err = left.err || right.err;
}

fn arm_body_text(checked: &Checked<'_>, body: &MatchBody) -> String {
    match body {
        MatchBody::Block(block) => checked.slice(block.span).to_string(),
        MatchBody::Expr(expr) => format!("{{ {} }}", checked.slice(expr.span)),
    }
}

fn is_err_expr(program: &CheckedProgram, expr: &Expr) -> bool {
    let ExprKind::Call { callee, .. } = &expr.kind else {
        return false;
    };
    matches!(
        program.resolution(callee.id),
        Some(Resolution::PreludeVariant(PreludeVariant::Err))
    )
}

fn test_asserts(
    program: &CheckedProgram,
    analysis: &Analysis<'_>,
    def: DefId,
    block: &Block,
    visited: &mut HashSet<DefId>,
) -> bool {
    if block_asserts(program, block) {
        return true;
    }
    analysis
        .calls_from(def)
        .any(|call| free_fn_asserts(program, analysis, call.callee, visited))
}

fn free_fn_asserts(
    program: &CheckedProgram,
    analysis: &Analysis<'_>,
    def: DefId,
    visited: &mut HashSet<DefId>,
) -> bool {
    if !visited.insert(def) {
        return false;
    }
    let Some(body) = analysis.body(def) else {
        return false;
    };
    if body.sig.kind != FnKind::Free {
        return false;
    }
    if block_asserts(program, body.block) {
        return true;
    }
    analysis
        .calls_from(def)
        .any(|call| free_fn_asserts(program, analysis, call.callee, visited))
}

fn block_asserts(program: &CheckedProgram, block: &Block) -> bool {
    block.stmts.iter().any(|stmt| stmt_asserts(program, stmt))
}

fn stmt_asserts(program: &CheckedProgram, stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Let { value, .. } => expr_asserts(program, value),
        StmtKind::While { cond, body } => {
            expr_asserts(program, cond) || block_asserts(program, body)
        }
        StmtKind::For { iter, body, .. } => {
            expr_asserts(program, iter) || block_asserts(program, body)
        }
        StmtKind::Return(value) => value
            .as_ref()
            .is_some_and(|value| expr_asserts(program, value)),
        StmtKind::Break | StmtKind::Continue => false,
        StmtKind::Expr { expr, assign } => {
            expr_asserts(program, expr)
                || assign
                    .as_ref()
                    .is_some_and(|(_, rhs)| expr_asserts(program, rhs))
        }
    }
}

fn expr_asserts(program: &CheckedProgram, expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Call { callee, args } => {
            if matches!(
                program.resolution(callee.id),
                Some(Resolution::BuiltinFn(BuiltinFn::Assert))
            ) {
                return true;
            }
            expr_asserts(program, callee) || args.iter().any(|arg| expr_asserts(program, &arg.expr))
        }
        ExprKind::Str(parts) => parts.iter().any(|part| match part {
            martite_syntax::StrExprPart::Text(_) => false,
            martite_syntax::StrExprPart::Expr(expr) => expr_asserts(program, expr),
        }),
        ExprKind::Tuple(items) | ExprKind::List(items) => {
            items.iter().any(|item| expr_asserts(program, item))
        }
        ExprKind::Map(entries) => entries
            .iter()
            .any(|(key, value)| expr_asserts(program, key) || expr_asserts(program, value)),
        ExprKind::StructLit { fields, base, .. } => {
            fields.iter().any(|field| {
                field
                    .value
                    .as_ref()
                    .is_some_and(|value| expr_asserts(program, value))
            }) || base
                .as_ref()
                .is_some_and(|base| expr_asserts(program, base))
        }
        ExprKind::Unary { expr, .. } => expr_asserts(program, expr),
        ExprKind::Binary { lhs, rhs, .. } => {
            expr_asserts(program, lhs) || expr_asserts(program, rhs)
        }
        ExprKind::Range { start, end, .. } => {
            expr_asserts(program, start) || expr_asserts(program, end)
        }
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            expr_asserts(program, cond)
                || block_asserts(program, then_branch)
                || else_branch
                    .as_ref()
                    .is_some_and(|branch| expr_asserts(program, branch))
        }
        ExprKind::Match { scrutinee, arms } => {
            expr_asserts(program, scrutinee)
                || arms.iter().any(|arm| {
                    arm.guard
                        .as_ref()
                        .is_some_and(|guard| expr_asserts(program, guard))
                        || match &arm.body {
                            MatchBody::Block(block) => block_asserts(program, block),
                            MatchBody::Expr(expr) => expr_asserts(program, expr),
                        }
                })
        }
        ExprKind::Lambda { body, .. } => match body {
            martite_syntax::LambdaBody::Block(block) => block_asserts(program, block),
            martite_syntax::LambdaBody::Expr(expr) => expr_asserts(program, expr),
        },
        ExprKind::Block(block) => block_asserts(program, block),
        ExprKind::Field { base, .. } => expr_asserts(program, base),
        ExprKind::Index { base, index } => {
            expr_asserts(program, base) || expr_asserts(program, index)
        }
        ExprKind::Try(inner) => expr_asserts(program, inner),
        ExprKind::Convert { expr, .. } => expr_asserts(program, expr),
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Unit
        | ExprKind::Var(_) => false,
    }
}

fn handler_reachable(program: &CheckedProgram, analysis: &Analysis<'_>) -> HashSet<DefId> {
    let mut reachable = HashSet::new();
    let mut stack: Vec<DefId> = program.handlers.values().copied().collect();
    while let Some(def) = stack.pop() {
        if !reachable.insert(def) {
            continue;
        }
        for call in analysis.calls_from(def) {
            stack.push(call.callee);
        }
    }
    reachable
}
