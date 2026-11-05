//! Syntax-tier rules. These need only the AST, spans, comments, and literal
//! values, so they also run under `--syntax-only`.

use martite_base::{Applicability, Code, Diagnostic, Fix, FixEdit, SourceId, Span};
use martite_syntax::{
    BinaryOp, Block, Comment, EnumDecl, Expr, ExprKind, FnDecl, Item, MatchBody, OnDecl, Program,
    Stmt, StmtKind, StructDecl, TestDecl, UnaryOp,
};

use crate::registry;
use crate::settings::ResolvedSettings;
use crate::trivia::{self, doc_comments, find_name_span, operator_span};

pub struct SyntaxCtx<'a> {
    source: SourceId,
    text: &'a str,
    program: &'a Program,
    settings: &'a ResolvedSettings,
    out: Vec<Diagnostic>,
}

pub fn run(
    source: SourceId,
    text: &str,
    program: &Program,
    settings: &ResolvedSettings,
) -> Vec<Diagnostic> {
    let mut ctx = SyntaxCtx {
        source,
        text,
        program,
        settings,
        out: Vec::new(),
    };
    ctx.run_program();
    ctx.out
}

struct DocElement {
    start: u32,
    span: Span,
    kind: &'static str,
    name: String,
    is_pub: bool,
    is_main: bool,
}

#[derive(Clone, Copy)]
struct ExprWalk {
    loop_depth: u32,
    suppress_l005: bool,
    suppress_l024: bool,
    statement: bool,
}

impl ExprWalk {
    fn new() -> Self {
        Self {
            loop_depth: 0,
            suppress_l005: false,
            suppress_l024: false,
            statement: false,
        }
    }
}

impl<'a> SyntaxCtx<'a> {
    fn run_program(&mut self) {
        let mut docs = Vec::new();
        for item in &self.program.items {
            self.walk_item(item, &mut docs);
        }
        self.check_docs(&docs);
        self.check_stray_docs(&docs);
    }

    fn emit(
        &mut self,
        code: Code,
        span: Span,
        message: impl Into<String>,
        help: Option<String>,
        fix: Option<Fix>,
    ) {
        let level = self.settings.level(code);
        let Some(severity) = level.severity() else {
            return;
        };
        let Some(meta) = registry::rule(code) else {
            return;
        };
        let mut diagnostic =
            Diagnostic::lint(code, self.source, span, message).with_severity(severity);
        diagnostic.group = Some(meta.group);
        if let Some(help) = help {
            diagnostic.help = Some(help);
        }
        diagnostic.fix = fix;
        self.out.push(diagnostic);
    }

    fn slice(&self, span: Span) -> &'a str {
        let start = span.start.min(self.text.len() as u32) as usize;
        let end = span.end.min(self.text.len() as u32) as usize;
        if start <= end {
            &self.text[start..end]
        } else {
            ""
        }
    }

    // ----- item and doc traversal ---------------------------------------

    fn walk_item(&mut self, item: &Item, docs: &mut Vec<DocElement>) {
        match item {
            Item::Use(_) => {
                docs.push(DocElement {
                    start: item.span().start,
                    span: item.span(),
                    kind: "import",
                    name: String::new(),
                    is_pub: false,
                    is_main: false,
                });
            }
            Item::Const(constant) => {
                docs.push(DocElement {
                    start: constant.span.start,
                    span: constant.span,
                    kind: "constant",
                    name: constant.name.clone(),
                    is_pub: constant.is_pub,
                    is_main: false,
                });
                self.walk_expr(&constant.value, ExprWalk::new());
            }
            Item::Struct(decl) => self.walk_struct(decl, docs),
            Item::Enum(decl) => self.walk_enum(decl, docs),
            Item::Impl(decl) => {
                docs.push(DocElement {
                    start: decl.span.start,
                    span: decl.span,
                    kind: "impl",
                    name: decl.type_name.clone(),
                    is_pub: false,
                    is_main: false,
                });
                for method in &decl.methods {
                    self.walk_fn_like(
                        "method",
                        &method.name,
                        method.span,
                        &method.params,
                        method.body.as_ref(),
                        method.is_pub,
                        false,
                        docs,
                    );
                }
            }
            Item::Fn(decl) => self.walk_fn(decl, docs),
            Item::On(decl) => self.walk_on(decl, docs),
            Item::Test(decl) => self.walk_test(decl, docs),
        }
    }

    fn walk_fn(&mut self, decl: &FnDecl, docs: &mut Vec<DocElement>) {
        self.walk_fn_like(
            "function",
            &decl.name,
            decl.span,
            &decl.params,
            decl.body.as_ref(),
            decl.is_pub,
            false,
            docs,
        );
    }

    fn walk_fn_like(
        &mut self,
        kind: &'static str,
        name: &str,
        span: Span,
        params: &[martite_syntax::Param],
        body: Option<&Block>,
        is_pub: bool,
        is_test: bool,
        docs: &mut Vec<DocElement>,
    ) {
        docs.push(DocElement {
            start: span.start,
            span,
            kind,
            name: name.to_string(),
            is_pub,
            is_main: name == "main" && kind == "function",
        });
        let Some(body) = body else {
            return;
        };
        self.check_final_return(body);
        let param_count = params
            .iter()
            .filter(|param| !matches!(param.kind, martite_syntax::ParamKind::SelfParam { .. }))
            .count();
        let metrics = measure(body, param_count);
        let name_span = find_name_span(self.text, span, name);
        self.report_metrics(
            kind,
            name,
            name_span,
            span,
            body,
            param_count,
            &metrics,
            is_test,
        );
        self.walk_block(body, ExprWalk::new());
    }

    fn walk_on(&mut self, decl: &OnDecl, docs: &mut Vec<DocElement>) {
        docs.push(DocElement {
            start: decl.span.start,
            span: decl.span,
            kind: "handler",
            name: decl.name.clone(),
            is_pub: false,
            is_main: false,
        });
        self.check_final_return(&decl.body);
        let param_count = decl
            .params
            .iter()
            .filter(|param| !matches!(param.kind, martite_syntax::ParamKind::SelfParam { .. }))
            .count();
        let metrics = measure(&decl.body, param_count);
        let name_span = find_name_span(self.text, decl.span, &decl.name);
        self.report_metrics(
            "handler",
            &decl.name,
            name_span,
            decl.body.span,
            &decl.body,
            param_count,
            &metrics,
            false,
        );
        self.walk_block(&decl.body, ExprWalk::new());
    }

    fn walk_test(&mut self, decl: &TestDecl, docs: &mut Vec<DocElement>) {
        docs.push(DocElement {
            start: decl.span.start,
            span: decl.span,
            kind: "test",
            name: decl.name.clone(),
            is_pub: false,
            is_main: false,
        });
        let metrics = measure(&decl.body, 0);
        let name_span = find_name_span(self.text, decl.span, &decl.name);
        self.report_metrics(
            "test",
            &decl.name,
            name_span,
            decl.body.span,
            &decl.body,
            0,
            &metrics,
            true,
        );
        self.walk_block(&decl.body, ExprWalk::new());
    }

    fn walk_struct(&mut self, decl: &StructDecl, docs: &mut Vec<DocElement>) {
        docs.push(DocElement {
            start: decl.span.start,
            span: decl.span,
            kind: "struct",
            name: decl.name.clone(),
            is_pub: decl.is_pub,
            is_main: false,
        });
        for field in &decl.fields {
            docs.push(DocElement {
                start: field.span.start,
                span: field.span,
                kind: "field",
                name: field.name.clone(),
                is_pub: false,
                is_main: false,
            });
        }
    }

    fn walk_enum(&mut self, decl: &EnumDecl, docs: &mut Vec<DocElement>) {
        docs.push(DocElement {
            start: decl.span.start,
            span: decl.span,
            kind: "enum",
            name: decl.name.clone(),
            is_pub: decl.is_pub,
            is_main: false,
        });
        for variant in &decl.variants {
            docs.push(DocElement {
                start: variant.span.start,
                span: variant.span,
                kind: "variant",
                name: variant.name.clone(),
                is_pub: false,
                is_main: false,
            });
        }
    }

    fn check_docs(&mut self, docs: &[DocElement]) {
        if self.program.is_interface {
            return;
        }
        for element in docs {
            if !element.is_pub || element.is_main {
                continue;
            }
            if !matches!(
                element.kind,
                "function" | "method" | "struct" | "enum" | "constant"
            ) {
                continue;
            }
            if !doc_comments(self.program, self.text, element.start).is_empty() {
                continue;
            }
            let span = find_name_span(self.text, element.span, &element.name);
            let message = format!(
                "missing documentation for public {} `{}`",
                element.kind, element.name
            );
            self.emit(
                Code::L018,
                span,
                message,
                Some("add a `///` comment describing the contract".into()),
                None,
            );
        }
    }

    fn check_stray_docs(&mut self, docs: &[DocElement]) {
        if self.program.is_interface {
            return;
        }
        let mut attached = std::collections::HashSet::new();
        for element in docs {
            for comment in doc_comments(self.program, self.text, element.start) {
                attached.insert(comment.span.start);
            }
        }
        let stray: Vec<&Comment> = self
            .program
            .comments
            .iter()
            .filter(|comment| comment.doc && !attached.contains(&comment.span.start))
            .collect();
        for comment in stray {
            let mut slash_end = comment.span.start;
            let bytes = self.text.as_bytes();
            while (slash_end as usize) < bytes.len() && bytes[slash_end as usize] == b'/' {
                slash_end += 1;
            }
            let fix = Fix {
                applicability: Applicability::Machine,
                edits: vec![FixEdit::new(
                    Span::new(comment.span.start, slash_end),
                    "//".to_string(),
                )],
            };
            self.emit(
                Code::L032,
                comment.span,
                "`///` here is an ordinary comment, not documentation",
                Some("use `//` for an ordinary comment".into()),
                Some(fix),
            );
        }
    }

    // ----- statements and expressions ----------------------------------

    fn walk_block(&mut self, block: &Block, walk: ExprWalk) {
        if block.stmts.is_empty() && trivia::comments_within(self.program, block.span).is_empty() {
            self.emit(
                Code::L016,
                Span::new(block.span.start, block.span.start.saturating_add(1)),
                "empty block",
                Some("write `()` explicitly when doing nothing is intentional".into()),
                None,
            );
        }
        for stmt in &block.stmts {
            self.walk_stmt(stmt, walk);
        }
    }

    fn walk_stmt(&mut self, stmt: &Stmt, walk: ExprWalk) {
        match &stmt.kind {
            StmtKind::Let { value, .. } => self.walk_expr(value, walk),
            StmtKind::While { cond, body } => {
                let emitted = self.check_condition(cond, true, stmt.span, Some("()".to_string()));
                self.walk_expr(
                    cond,
                    ExprWalk {
                        suppress_l005: emitted,
                        ..walk
                    },
                );
                self.walk_block(
                    body,
                    ExprWalk {
                        loop_depth: walk.loop_depth + 1,
                        ..walk
                    },
                );
            }
            StmtKind::For { iter, body, .. } => {
                self.walk_expr(iter, walk);
                self.walk_block(
                    body,
                    ExprWalk {
                        loop_depth: walk.loop_depth + 1,
                        ..walk
                    },
                );
            }
            StmtKind::Return(value) => {
                if let Some(value) = value {
                    self.walk_expr(value, walk);
                }
            }
            StmtKind::Break | StmtKind::Continue => {}
            StmtKind::Expr { expr, assign } => {
                if assign.is_some() {
                    self.walk_expr(expr, walk);
                    if let Some((_, rhs)) = assign {
                        self.walk_expr(rhs, walk);
                    }
                } else {
                    if let ExprKind::If {
                        cond,
                        then_branch,
                        else_branch: Some(else_branch),
                    } = &expr.kind
                    {
                        if block_ends_with_return(then_branch) {
                            let span =
                                Span::new(expr.span.start, expr.span.start.saturating_add(2));
                            let else_text = match &else_branch.kind {
                                ExprKind::Block(block) => {
                                    let inner = Span::new(
                                        block.span.start.saturating_add(1),
                                        block.span.end.saturating_sub(1),
                                    );
                                    self.slice(inner).trim().to_string()
                                }
                                _ => self.slice(else_branch.span).to_string(),
                            };
                            let replacement = format!(
                                "if {} {}\n{}",
                                self.slice(cond.span),
                                self.slice(then_branch.span),
                                else_text
                            );
                            let fix = Fix {
                                applicability: Applicability::Maybe,
                                edits: vec![FixEdit::new(expr.span, replacement)],
                            };
                            self.emit(
                                Code::L013,
                                span,
                                "unnecessary `else` after a branch that returns",
                                Some("move the `else` body after the `if`".into()),
                                Some(fix),
                            );
                        }
                    }
                    self.walk_expr(
                        expr,
                        ExprWalk {
                            statement: true,
                            ..walk
                        },
                    );
                }
            }
        }
    }

    fn walk_expr(&mut self, expr: &Expr, walk: ExprWalk) {
        match &expr.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Unit
            | ExprKind::Var(_) => {}
            ExprKind::Str(parts) => {
                for part in parts {
                    if let martite_syntax::StrExprPart::Expr(expr) = part {
                        self.walk_expr(expr, walk);
                    }
                }
            }
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                for item in items {
                    self.walk_expr(item, walk);
                }
            }
            ExprKind::Map(entries) => {
                for (key, value) in entries {
                    self.walk_expr(key, walk);
                    self.walk_expr(value, walk);
                }
            }
            ExprKind::StructLit { fields, base, .. } => {
                for field in fields {
                    if let Some(value) = &field.value {
                        self.walk_expr(value, walk);
                    }
                }
                if let Some(base) = base {
                    self.walk_expr(base, walk);
                }
            }
            ExprKind::Unary { op, expr: inner } => {
                let mut emitted_l005 = false;
                if *op == UnaryOp::Not {
                    emitted_l005 = self.check_boolean_constant(expr, walk.suppress_l005);
                    self.check_not_comparison(expr, inner);
                }
                self.walk_expr(
                    inner,
                    ExprWalk {
                        suppress_l005: walk.suppress_l005 || emitted_l005,
                        ..walk
                    },
                );
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let mut emitted_l005 = false;
                let mut emitted_l024 = false;
                if matches!(op, BinaryOp::And | BinaryOp::Or) {
                    emitted_l005 = self.check_boolean_constant(expr, walk.suppress_l005);
                }
                if matches!(op, BinaryOp::Eq | BinaryOp::Ne) {
                    self.check_bool_comparison(expr, *op, lhs, rhs);
                }
                if matches!(op, BinaryOp::And | BinaryOp::Or) {
                    emitted_l024 = self.check_mixed_boolean(*op, lhs, rhs, walk.suppress_l024);
                }
                self.walk_expr(
                    lhs,
                    ExprWalk {
                        suppress_l005: walk.suppress_l005 || emitted_l005,
                        suppress_l024: walk.suppress_l024 || emitted_l024,
                        ..walk
                    },
                );
                self.walk_expr(
                    rhs,
                    ExprWalk {
                        suppress_l005: walk.suppress_l005 || emitted_l005,
                        suppress_l024: walk.suppress_l024 || emitted_l024,
                        ..walk
                    },
                );
            }
            ExprKind::Range { start, end, .. } => {
                self.walk_expr(start, walk);
                self.walk_expr(end, walk);
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let replacement = self.if_constant_replacement(
                    cond,
                    then_branch,
                    else_branch.as_deref(),
                    walk.statement,
                );
                let emitted = self.check_condition(cond, false, expr.span, replacement);
                self.check_if_simplification(expr, cond, then_branch, else_branch.as_deref());
                self.walk_expr(
                    cond,
                    ExprWalk {
                        suppress_l005: emitted,
                        ..walk
                    },
                );
                self.walk_block(then_branch, walk);
                if let Some(else_branch) = else_branch {
                    self.walk_expr(else_branch, walk);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.walk_expr(scrutinee, walk);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.walk_expr(guard, walk);
                    }
                    match &arm.body {
                        MatchBody::Block(block) => self.walk_block(block, walk),
                        MatchBody::Expr(expr) => self.walk_expr(expr, walk),
                    }
                }
            }
            ExprKind::Lambda { body, .. } => match body {
                martite_syntax::LambdaBody::Block(block) => {
                    self.check_final_return(block);
                    self.walk_block(block, walk);
                }
                martite_syntax::LambdaBody::Expr(expr) => self.walk_expr(expr, walk),
            },
            ExprKind::Block(block) => self.walk_block(block, walk),
            ExprKind::Field { base, .. } => self.walk_expr(base, walk),
            ExprKind::Call { callee, args } => {
                self.walk_expr(callee, walk);
                for arg in args {
                    self.walk_expr(&arg.expr, walk);
                }
            }
            ExprKind::Index { base, index } => {
                self.walk_expr(base, walk);
                self.walk_expr(index, walk);
            }
            ExprKind::Try(inner) => self.walk_expr(inner, walk),
            ExprKind::Convert { expr, .. } => self.walk_expr(expr, walk),
        }
    }

    // ----- L005 ---------------------------------------------------------

    fn check_condition(
        &mut self,
        cond: &Expr,
        is_while: bool,
        whole: Span,
        replacement: Option<String>,
    ) -> bool {
        if is_while && matches!(cond.kind, ExprKind::Bool(true)) {
            return false;
        }
        if is_screaming_switch(cond) {
            return false;
        }
        let Some((value, span)) = const_bool(self.text, cond) else {
            return false;
        };
        let fix = replacement.map(|replacement| Fix {
            applicability: Applicability::Maybe,
            edits: vec![FixEdit::new(whole, replacement)],
        });
        self.emit(
            Code::L005,
            span,
            format!("this condition is always `{value}`"),
            Some("remove the constant condition or replace it with the intended test".into()),
            fix,
        );
        true
    }

    fn if_constant_replacement(
        &self,
        cond: &Expr,
        then_branch: &Block,
        else_branch: Option<&Expr>,
        statement: bool,
    ) -> Option<String> {
        let (value, _) = const_bool(self.text, cond)?;
        if value {
            if statement {
                Some(self.block_inner(then_branch))
            } else {
                self.block_value(then_branch)
            }
        } else {
            match else_branch {
                Some(expr) => match &expr.kind {
                    ExprKind::Block(block) => {
                        if statement {
                            Some(self.block_inner(block))
                        } else {
                            self.block_value(block)
                        }
                    }
                    _ => Some(self.slice(expr.span).to_string()),
                },
                None => Some("()".to_string()),
            }
        }
    }

    fn block_value(&self, block: &Block) -> Option<String> {
        let [stmt] = block.stmts.as_slice() else {
            return None;
        };
        let StmtKind::Expr { expr, assign: None } = &stmt.kind else {
            return None;
        };
        Some(self.slice(expr.span).to_string())
    }

    fn block_inner(&self, block: &Block) -> String {
        let inner = Span::new(
            block.span.start.saturating_add(1),
            block.span.end.saturating_sub(1),
        );
        self.slice(inner).trim().to_string()
    }

    fn check_boolean_constant(&mut self, expr: &Expr, suppressed: bool) -> bool {
        if suppressed {
            return false;
        }
        let Some((value, span)) = const_bool(self.text, expr) else {
            return false;
        };
        self.emit(
            Code::L005,
            span,
            format!("this boolean expression is always `{value}`"),
            Some("simplify the constant boolean expression".into()),
            None,
        );
        true
    }

    // ----- L011 ---------------------------------------------------------

    fn check_bool_comparison(&mut self, expr: &Expr, op: BinaryOp, lhs: &Expr, rhs: &Expr) {
        let (literal, other, literal_on_left) = if let ExprKind::Bool(value) = rhs.kind {
            (value, lhs, false)
        } else if let ExprKind::Bool(value) = lhs.kind {
            (value, rhs, true)
        } else {
            return;
        };
        let replacement = match (op, literal) {
            (BinaryOp::Eq, true) | (BinaryOp::Ne, false) => self.slice(other.span).to_string(),
            (BinaryOp::Eq, false) | (BinaryOp::Ne, true) => {
                format!("not ({})", self.slice(other.span))
            }
            _ => return,
        };
        let _ = literal_on_left;
        let fix = Fix::machine(vec![FixEdit::new(expr.span, replacement)]);
        self.emit(
            Code::L011,
            expr.span,
            "comparison with a boolean literal is redundant",
            Some("use the condition directly, or negate it".into()),
            Some(fix),
        );
    }

    fn check_not_comparison(&mut self, expr: &Expr, inner: &Expr) {
        let ExprKind::Binary { op, lhs, rhs } = &inner.kind else {
            return;
        };
        let inverse = match op {
            BinaryOp::Eq => "!=",
            BinaryOp::Ne => "==",
            _ => return,
        };
        let replacement = format!(
            "{} {} {}",
            self.slice(lhs.span),
            inverse,
            self.slice(rhs.span)
        );
        let fix = Fix::machine(vec![FixEdit::new(expr.span, replacement)]);
        self.emit(
            Code::L011,
            expr.span,
            format!(
                "`not` around `{}` can be written as `{inverse}`",
                op.as_str()
            ),
            Some(format!("replace the negation with `{inverse}`")),
            Some(fix),
        );
    }

    fn check_if_simplification(
        &mut self,
        expr: &Expr,
        cond: &Expr,
        then_branch: &Block,
        else_branch: Option<&Expr>,
    ) {
        let Some(else_branch) = else_branch else {
            return;
        };
        let Some(then_value) = block_bool_literal(then_branch) else {
            return;
        };
        let Some(else_value) = expr_bool_literal(else_branch) else {
            return;
        };
        let replacement = match (then_value, else_value) {
            (true, false) => self.slice(cond.span).to_string(),
            (false, true) => format!("not ({})", self.slice(cond.span)),
            _ => return,
        };
        let fix = Fix::machine(vec![FixEdit::new(expr.span, replacement)]);
        self.emit(
            Code::L011,
            expr.span,
            "`if` returning `true`/`false` can be written as its condition",
            Some("return the condition directly".into()),
            Some(fix),
        );
    }

    // ----- L013 ---------------------------------------------------------

    fn check_final_return(&mut self, body: &Block) {
        let Some(last) = body.stmts.last() else {
            return;
        };
        let StmtKind::Return(value) = &last.kind else {
            return;
        };
        let keyword = keyword_span(self.text, last.span, "return");
        match value {
            Some(value) => {
                let replacement = self.slice(value.span).to_string();
                let fix = Fix::machine(vec![FixEdit::new(last.span, replacement)]);
                self.emit(
                    Code::L013,
                    keyword,
                    "unnecessary `return` at the end of the body",
                    Some("use the final expression instead".into()),
                    Some(fix),
                );
            }
            None => {
                let fix = Fix::machine(vec![FixEdit::new(last.span, "")]);
                self.emit(
                    Code::L013,
                    keyword,
                    "unnecessary `return` at the end of the body",
                    Some("remove the final `return`".into()),
                    Some(fix),
                );
            }
        }
    }

    // ----- L024 ---------------------------------------------------------

    fn check_mixed_boolean(
        &mut self,
        op: BinaryOp,
        lhs: &Expr,
        rhs: &Expr,
        suppressed: bool,
    ) -> bool {
        if suppressed {
            return false;
        }
        let opposite = match op {
            BinaryOp::And => BinaryOp::Or,
            BinaryOp::Or => BinaryOp::And,
            _ => return false,
        };
        let mut mixed = Vec::new();
        for child in [lhs, rhs] {
            if let ExprKind::Binary { op: child_op, .. } = &child.kind {
                if *child_op == opposite && !self.program.parenthesized.contains(&child.id) {
                    mixed.push(child);
                }
            }
        }
        if mixed.is_empty() {
            return false;
        }
        let span = operator_span(self.text, lhs.span.end, rhs.span.start, op.as_str());
        let mut edits = Vec::new();
        for child in &mixed {
            edits.push(FixEdit::new(Span::empty(child.span.start), "(".to_string()));
            edits.push(FixEdit::new(Span::empty(child.span.end), ")".to_string()));
        }
        self.emit(
            Code::L024,
            span,
            "mixed `and` and `or` without parentheses",
            Some("add parentheses to make the grouping explicit".into()),
            Some(Fix::machine(edits)),
        );
        true
    }

    // ----- L017 ---------------------------------------------------------

    fn report_metrics(
        &mut self,
        kind: &str,
        name: &str,
        name_span: Span,
        item_span: Span,
        body: &Block,
        param_count: usize,
        metrics: &Metrics,
        is_test: bool,
    ) {
        let thresholds = self.settings.thresholds;
        if !is_test {
            let lines = count_lines(self.text, item_span);
            if lines as i64 > thresholds.max_fn_lines {
                self.emit(
                    Code::L017,
                    name_span,
                    format!(
                        "{kind} `{name}` has {lines} lines (maximum {})",
                        thresholds.max_fn_lines
                    ),
                    Some("split it into smaller functions".into()),
                    None,
                );
            }
        }
        if param_count as i64 > thresholds.max_params {
            self.emit(
                Code::L017,
                name_span,
                format!(
                    "{kind} `{name}` has {param_count} parameters (maximum {})",
                    thresholds.max_params
                ),
                Some("group related parameters into a struct".into()),
                None,
            );
        }
        let _ = body;
        if metrics.max_nesting as i64 > thresholds.max_nesting {
            self.emit(
                Code::L017,
                name_span,
                format!(
                    "{kind} `{name}` has nesting depth {} (maximum {})",
                    metrics.max_nesting, thresholds.max_nesting
                ),
                Some("extract nested logic into a helper".into()),
                None,
            );
        }
        if metrics.branches as i64 > thresholds.max_branches {
            self.emit(
                Code::L017,
                name_span,
                format!(
                    "{kind} `{name}` has {} branches (maximum {})",
                    metrics.branches, thresholds.max_branches
                ),
                Some("split the decision logic into smaller pieces".into()),
                None,
            );
        }
    }
}

// ----- helpers ----------------------------------------------------------

fn keyword_span(text: &str, span: Span, keyword: &str) -> Span {
    find_name_span(text, span, keyword)
}

fn is_screaming_switch(expr: &Expr) -> bool {
    let ExprKind::Var(name) = &expr.kind else {
        return false;
    };
    name.chars().any(|ch| ch.is_ascii_uppercase())
        && name
            .chars()
            .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

fn const_bool(text: &str, expr: &Expr) -> Option<(bool, Span)> {
    match &expr.kind {
        ExprKind::Bool(value) => Some((*value, expr.span)),
        ExprKind::Unary {
            op: UnaryOp::Not,
            expr: inner,
        } => {
            let (value, _) = const_bool(text, inner)?;
            Some((!value, expr.span))
        }
        ExprKind::Binary { op, lhs, rhs } => {
            let left = const_bool(text, lhs);
            let right = const_bool(text, rhs);
            let span = operator_span(text, lhs.span.end, rhs.span.start, op.as_str());
            match op {
                BinaryOp::And => match (left, right) {
                    (Some((false, _)), _) | (_, Some((false, _))) => Some((false, span)),
                    (Some((true, _)), Some((true, _))) => Some((true, span)),
                    _ => None,
                },
                BinaryOp::Or => match (left, right) {
                    (Some((true, _)), _) | (_, Some((true, _))) => Some((true, span)),
                    (Some((false, _)), Some((false, _))) => Some((false, span)),
                    _ => None,
                },
                _ => None,
            }
        }
        _ => None,
    }
}

fn block_bool_literal(block: &Block) -> Option<bool> {
    if block.stmts.len() != 1 {
        return None;
    }
    match &block.stmts[0].kind {
        StmtKind::Expr { expr, assign: None } => expr_bool_literal(expr),
        _ => None,
    }
}

fn expr_bool_literal(expr: &Expr) -> Option<bool> {
    match &expr.kind {
        ExprKind::Bool(value) => Some(*value),
        ExprKind::Block(block) => block_bool_literal(block),
        _ => None,
    }
}

fn block_ends_with_return(block: &Block) -> bool {
    matches!(
        block.stmts.last().map(|stmt| &stmt.kind),
        Some(StmtKind::Return(_))
    )
}

fn count_lines(text: &str, span: Span) -> usize {
    let start = span.start.min(text.len() as u32) as usize;
    let end = span.end.min(text.len() as u32) as usize;
    if start >= end {
        return 0;
    }
    let mut line_start = text[..start]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    let mut count = 0usize;
    while line_start < end {
        let line_end = text[line_start..]
            .find('\n')
            .map(|index| line_start + index)
            .unwrap_or(text.len());
        let line = &text[line_start..line_end];
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with("//") {
            count += 1;
        }
        if line_end >= text.len() {
            break;
        }
        line_start = line_end + 1;
    }
    count
}

// ----- L017 metrics -----------------------------------------------------

struct Metrics {
    params: usize,
    max_nesting: u32,
    branches: u32,
}

fn measure(body: &Block, params: usize) -> Metrics {
    let mut metrics = Metrics {
        params,
        max_nesting: 1,
        branches: 1,
    };
    metrics.walk_block(body, 1);
    metrics
}

impl Metrics {
    fn touch(&mut self, depth: u32) {
        self.max_nesting = self.max_nesting.max(depth);
    }

    fn walk_block(&mut self, block: &Block, depth: u32) {
        self.touch(depth);
        for stmt in &block.stmts {
            self.walk_stmt(stmt, depth);
        }
    }

    fn walk_stmt(&mut self, stmt: &Stmt, depth: u32) {
        match &stmt.kind {
            StmtKind::Let { value, .. } => self.walk_expr(value, depth),
            StmtKind::While { cond, body } => {
                self.branches += 1;
                self.walk_expr(cond, depth);
                self.walk_block(body, depth + 1);
            }
            StmtKind::For { iter, body, .. } => {
                self.branches += 1;
                self.walk_expr(iter, depth);
                self.walk_block(body, depth + 1);
            }
            StmtKind::Return(value) => {
                if let Some(value) = value {
                    self.walk_expr(value, depth);
                }
            }
            StmtKind::Break | StmtKind::Continue => {}
            StmtKind::Expr { expr, assign } => {
                self.walk_expr(expr, depth);
                if let Some((_, rhs)) = assign {
                    self.walk_expr(rhs, depth);
                }
            }
        }
    }

    fn walk_expr(&mut self, expr: &Expr, depth: u32) {
        match &expr.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Unit
            | ExprKind::Var(_) => {}
            ExprKind::Str(parts) => {
                for part in parts {
                    if let martite_syntax::StrExprPart::Expr(expr) = part {
                        self.walk_expr(expr, depth);
                    }
                }
            }
            ExprKind::Tuple(items) | ExprKind::List(items) => {
                for item in items {
                    self.walk_expr(item, depth);
                }
            }
            ExprKind::Map(entries) => {
                for (key, value) in entries {
                    self.walk_expr(key, depth);
                    self.walk_expr(value, depth);
                }
            }
            ExprKind::StructLit { fields, base, .. } => {
                for field in fields {
                    if let Some(value) = &field.value {
                        self.walk_expr(value, depth);
                    }
                }
                if let Some(base) = base {
                    self.walk_expr(base, depth);
                }
            }
            ExprKind::Unary { expr, .. } => self.walk_expr(expr, depth),
            ExprKind::Binary { op, lhs, rhs } => {
                if matches!(op, BinaryOp::And | BinaryOp::Or | BinaryOp::Coalesce) {
                    self.branches += 1;
                }
                self.walk_expr(lhs, depth);
                self.walk_expr(rhs, depth);
            }
            ExprKind::Range { start, end, .. } => {
                self.walk_expr(start, depth);
                self.walk_expr(end, depth);
            }
            ExprKind::If {
                cond,
                then_branch,
                else_branch,
            } => {
                self.branches += 1;
                let inner = depth + 1;
                self.touch(inner);
                self.walk_expr(cond, depth);
                self.walk_block(then_branch, inner);
                if let Some(else_branch) = else_branch {
                    self.walk_expr(else_branch, inner);
                }
            }
            ExprKind::Match { scrutinee, arms } => {
                self.branches += arms.len().saturating_sub(1) as u32;
                let inner = depth + 1;
                self.touch(inner);
                self.walk_expr(scrutinee, depth);
                for arm in arms {
                    if let Some(guard) = &arm.guard {
                        self.walk_expr(guard, inner);
                    }
                    match &arm.body {
                        MatchBody::Block(block) => self.walk_block(block, inner),
                        MatchBody::Expr(expr) => self.walk_expr(expr, inner),
                    }
                }
            }
            ExprKind::Lambda { params, body, .. } => {
                self.params += params.len();
                let inner = depth + 1;
                self.touch(inner);
                match body {
                    martite_syntax::LambdaBody::Block(block) => self.walk_block(block, inner),
                    martite_syntax::LambdaBody::Expr(expr) => self.walk_expr(expr, inner),
                }
            }
            ExprKind::Block(block) => self.walk_block(block, depth + 1),
            ExprKind::Field { base, .. } => self.walk_expr(base, depth),
            ExprKind::Call { callee, args } => {
                self.walk_expr(callee, depth);
                for arg in args {
                    self.walk_expr(&arg.expr, depth);
                }
            }
            ExprKind::Index { base, index } => {
                self.walk_expr(base, depth);
                self.walk_expr(index, depth);
            }
            ExprKind::Try(inner) => self.walk_expr(inner, depth),
            ExprKind::Convert { expr, .. } => self.walk_expr(expr, depth),
        }
    }
}
