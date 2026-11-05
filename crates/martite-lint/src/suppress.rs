//! Suppression comments and the post-tier L022 check.

use std::collections::HashMap;

use martite_base::{Applicability, Code, Diagnostic, Fix, FixEdit, SourceId, Span};
use martite_syntax::{Block, Expr, ExprKind, Item, MatchBody, Program, Stmt, StmtKind};

use crate::registry;
use crate::settings::ResolvedSettings;
use crate::trivia::{self, allow_comments};

#[derive(Debug, Clone)]
struct Element {
    span: Span,
    head: Span,
}

#[derive(Debug, Clone)]
struct SuppressionId {
    text: String,
    code: Option<Code>,
    span: Span,
    retired: bool,
    error_code: bool,
}

#[derive(Debug, Clone)]
struct Suppression {
    comment: Span,
    target: Option<usize>,
    ids: Vec<SuppressionId>,
}

/// Remove suppressed diagnostics and append L022 for ineffective comments.
pub fn apply(
    source: SourceId,
    text: &str,
    program: &Program,
    settings: &ResolvedSettings,
    mut diagnostics: Vec<Diagnostic>,
    include_l022: bool,
) -> Vec<Diagnostic> {
    let elements = collect_elements(program, text);
    let mut suppressions = collect_suppressions(program, text, &elements);

    for diagnostic in &mut diagnostics {
        if matches!(diagnostic.code, Code::L022 | Code::L032) {
            continue;
        }
        if diagnostic.source != Some(source) {
            continue;
        }
        let Some(span) = diagnostic.span else {
            continue;
        };
        for suppression in &mut suppressions {
            let Some(target) = suppression.target else {
                continue;
            };
            if !elements[target].head.contains(span) {
                continue;
            }
            if suppression
                .ids
                .iter()
                .any(|id| id.code == Some(diagnostic.code))
            {
                suppression
                    .ids
                    .iter_mut()
                    .filter(|id| id.code == Some(diagnostic.code))
                    .for_each(|id| id.text.push('\0')); // mark used, keep the text
            }
        }
    }

    // A diagnostic is suppressed when any matching comment names its rule.
    diagnostics.retain(|diagnostic| {
        if matches!(diagnostic.code, Code::L022 | Code::L032) {
            return true;
        }
        if diagnostic.source != Some(source) {
            return true;
        }
        let Some(span) = diagnostic.span else {
            return true;
        };
        !suppressions.iter().any(|suppression| {
            let Some(target) = suppression.target else {
                return false;
            };
            elements[target].head.contains(span)
                && suppression
                    .ids
                    .iter()
                    .any(|id| id.code == Some(diagnostic.code))
        })
    });

    if include_l022 && settings.enabled(Code::L022) {
        emit_l022(
            source,
            text,
            settings,
            &suppressions,
            &elements,
            &mut diagnostics,
        );
    }

    diagnostics
}

fn emit_l022(
    source: SourceId,
    text: &str,
    settings: &ResolvedSettings,
    suppressions: &[Suppression],
    _elements: &[Element],
    out: &mut Vec<Diagnostic>,
) {
    for suppression in suppressions {
        let mut offending = Vec::new();
        for id in &suppression.ids {
            let used = id.text.ends_with('\0');
            if id.error_code {
                offending.push((
                    id,
                    format!(
                        "`{}` is a compile error code",
                        id.text.trim_end_matches('\0')
                    ),
                ));
                continue;
            }
            let Some(code) = id.code else {
                offending.push((
                    id,
                    format!("unknown lint rule `{}`", id.text.trim_end_matches('\0')),
                ));
                continue;
            };
            if id.retired {
                offending.push((
                    id,
                    format!("retired lint rule `{}`", id.text.trim_end_matches('\0')),
                ));
                continue;
            }
            if !registry::is_known(code) {
                offending.push((
                    id,
                    format!("unknown lint rule `{}`", id.text.trim_end_matches('\0')),
                ));
                continue;
            }
            if !settings.selected(code) || !settings.enabled(code) {
                continue;
            }
            if !used {
                offending.push((
                    id,
                    format!(
                        "`{}` suppresses nothing here",
                        id.text.trim_end_matches('\0')
                    ),
                ));
            }
        }
        if offending.is_empty() {
            continue;
        }
        let message = offending
            .iter()
            .map(|(_, message)| message.clone())
            .collect::<Vec<_>>()
            .join("; ");
        let all = offending.len() == suppression.ids.len();
        let edits = if all {
            vec![FixEdit::new(suppression.comment, "")]
        } else {
            offending
                .iter()
                .map(|(id, _)| {
                    FixEdit::new(
                        suppression_id_removal_span(text, id.span, suppression.comment),
                        "",
                    )
                })
                .collect()
        };
        let mut diagnostic = Diagnostic::lint(
            Code::L022,
            source,
            suppression.comment,
            format!("suppression is ineffective: {message}"),
        )
        .with_severity(
            settings
                .level(Code::L022)
                .severity()
                .unwrap_or(martite_base::Severity::Warning),
        )
        .with_help("remove the stale ID or the whole comment")
        .with_fix(Fix {
            applicability: Applicability::Machine,
            edits,
        });
        diagnostic.group = Some(martite_base::LintGroup::Correct);
        out.push(diagnostic);
    }
}

fn collect_suppressions(program: &Program, text: &str, elements: &[Element]) -> Vec<Suppression> {
    let mut by_comment: HashMap<u32, Suppression> = HashMap::new();
    for (index, element) in elements.iter().enumerate() {
        for comment in allow_comments(program, text, element.span.start) {
            if let Some(ids) = parse_allow(comment.span, &comment.text) {
                by_comment.insert(
                    comment.span.start,
                    Suppression {
                        comment: comment.span,
                        target: Some(index),
                        ids,
                    },
                );
            }
        }
    }
    // Comments that are not attached to any element can never suppress.
    for comment in &program.comments {
        if comment.doc || !trivia::is_allow_comment(comment) {
            continue;
        }
        if by_comment.contains_key(&comment.span.start) {
            continue;
        }
        if let Some(ids) = parse_allow(comment.span, &comment.text) {
            by_comment.insert(
                comment.span.start,
                Suppression {
                    comment: comment.span,
                    target: None,
                    ids,
                },
            );
        }
    }
    let mut out: Vec<Suppression> = by_comment.into_values().collect();
    out.sort_by_key(|suppression| suppression.comment.start);
    out
}

fn suppression_id_removal_span(text: &str, id: Span, comment: Span) -> Span {
    let bytes = text.as_bytes();
    let mut end = id.end as usize;
    let mut after = end;
    while after < bytes.len() && bytes[after].is_ascii_whitespace() {
        after += 1;
    }
    if after < bytes.len() && bytes[after] == b',' {
        end = after + 1;
        while end < bytes.len() && bytes[end].is_ascii_whitespace() && bytes[end] != b'\n' {
            end += 1;
        }
        return Span::new(id.start, end as u32);
    }
    let mut start = id.start as usize;
    let mut before = start;
    while before > comment.start as usize
        && bytes[before - 1].is_ascii_whitespace()
        && bytes[before - 1] != b'\n'
    {
        before -= 1;
    }
    if before > comment.start as usize && bytes[before - 1] == b',' {
        start = before - 1;
    }
    Span::new(start as u32, id.end)
}

fn parse_allow(span: Span, text: &str) -> Option<Vec<SuppressionId>> {
    let trimmed = text.trim_start();
    let leading = text.len() - trimmed.len();
    let rest = trimmed.strip_prefix("allow")?;
    let rest_offset = leading + "allow".len();
    let rest = rest.trim_start();
    let open_offset = rest_offset + (trimmed.len() - "allow".len() - rest.len());
    if !rest.starts_with('(') {
        return None;
    }
    let close = rest.rfind(')')?;
    let content = &rest[1..close];
    let content_base = open_offset + 1;
    let mut out = Vec::new();
    let mut offset = 0usize;
    for part in content.split(',') {
        let raw = part;
        let leading_ws = raw.len() - raw.trim_start().len();
        let id_text = raw.trim();
        if !id_text.is_empty() {
            let start = span.start + 2 + (content_base + offset + leading_ws) as u32;
            let end = start + id_text.len() as u32;
            let code = Code::from_str(id_text);
            let retired = code.map(registry::is_retired).unwrap_or(false);
            let known = code.map(registry::is_known).unwrap_or(false);
            out.push(SuppressionId {
                text: id_text.to_string(),
                code: if known && !retired { code } else { None },
                span: Span::new(start, end),
                retired,
                error_code: id_text.starts_with('E'),
            });
        }
        offset += raw.len() + 1;
    }
    if out.is_empty() { None } else { Some(out) }
}

// ----- element collection ----------------------------------------------

fn collect_elements(program: &Program, text: &str) -> Vec<Element> {
    let mut out = Vec::new();
    for item in &program.items {
        collect_item(item, text, &mut out);
    }
    out.sort_by_key(|element| element.span.start);
    out
}

fn collect_item(item: &Item, text: &str, out: &mut Vec<Element>) {
    match item {
        Item::Use(_) | Item::Const(_) => push(out, item.span(), item.span()),
        Item::Struct(decl) => {
            push(out, decl.span, brace_head(text, decl.span));
            for field in &decl.fields {
                push(out, field.span, field.span);
            }
        }
        Item::Enum(decl) => {
            push(out, decl.span, brace_head(text, decl.span));
            for variant in &decl.variants {
                push(out, variant.span, variant.span);
            }
        }
        Item::Impl(decl) => {
            push(out, decl.span, brace_head(text, decl.span));
            for method in &decl.methods {
                let head = method
                    .body
                    .as_ref()
                    .map(|body| block_head(method.span, body))
                    .unwrap_or(method.span);
                push(out, method.span, head);
                if let Some(body) = &method.body {
                    collect_block(body, text, out);
                }
            }
        }
        Item::Fn(decl) => {
            let head = decl
                .body
                .as_ref()
                .map(|body| block_head(decl.span, body))
                .unwrap_or(decl.span);
            push(out, decl.span, head);
            if let Some(body) = &decl.body {
                collect_block(body, text, out);
            }
        }
        Item::On(decl) => {
            push(out, decl.span, block_head(decl.span, &decl.body));
            collect_block(&decl.body, text, out);
        }
        Item::Test(decl) => {
            push(out, decl.span, block_head(decl.span, &decl.body));
            collect_block(&decl.body, text, out);
        }
    }
}

fn collect_block(block: &Block, text: &str, out: &mut Vec<Element>) {
    for stmt in &block.stmts {
        collect_stmt(stmt, text, out);
    }
}

fn collect_stmt(stmt: &Stmt, text: &str, out: &mut Vec<Element>) {
    match &stmt.kind {
        StmtKind::Let { value, .. } => {
            push(out, stmt.span, stmt.span);
            collect_expr(value, text, out);
        }
        StmtKind::While { cond, body } => {
            push(out, stmt.span, block_head(stmt.span, body));
            collect_expr(cond, text, out);
            collect_block(body, text, out);
        }
        StmtKind::For { iter, body, .. } => {
            push(out, stmt.span, block_head(stmt.span, body));
            collect_expr(iter, text, out);
            collect_block(body, text, out);
        }
        StmtKind::Return(value) => {
            push(out, stmt.span, stmt.span);
            if let Some(value) = value {
                collect_expr(value, text, out);
            }
        }
        StmtKind::Break | StmtKind::Continue => push(out, stmt.span, stmt.span),
        StmtKind::Expr { expr, assign } => {
            if assign.is_some() {
                push(out, stmt.span, stmt.span);
            } else {
                let head = match &expr.kind {
                    ExprKind::If { then_branch, .. } => block_head(stmt.span, then_branch),
                    ExprKind::Match { arms, .. } => arms
                        .first()
                        .map(|arm| brace_head(text, Span::new(stmt.span.start, arm.span.start)))
                        .unwrap_or(stmt.span),
                    ExprKind::Lambda {
                        body: martite_syntax::LambdaBody::Block(body),
                        ..
                    } => block_head(stmt.span, body),
                    ExprKind::Block(block) => block_head(stmt.span, block),
                    _ => stmt.span,
                };
                push(out, stmt.span, head);
            }
            collect_expr(expr, text, out);
            if let Some((_, rhs)) = assign {
                collect_expr(rhs, text, out);
            }
        }
    }
}

fn collect_expr(expr: &Expr, text: &str, out: &mut Vec<Element>) {
    match &expr.kind {
        ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } => {
            collect_expr(cond, text, out);
            collect_block(then_branch, text, out);
            if let Some(else_branch) = else_branch {
                collect_expr(else_branch, text, out);
            }
        }
        ExprKind::Match { scrutinee, arms } => {
            collect_expr(scrutinee, text, out);
            for arm in arms {
                push(out, arm.span, arm.span);
                if let Some(guard) = &arm.guard {
                    collect_expr(guard, text, out);
                }
                match &arm.body {
                    MatchBody::Block(block) => collect_block(block, text, out),
                    MatchBody::Expr(expr) => collect_expr(expr, text, out),
                }
            }
        }
        ExprKind::Lambda { body, .. } => match body {
            martite_syntax::LambdaBody::Block(block) => collect_block(block, text, out),
            martite_syntax::LambdaBody::Expr(expr) => collect_expr(expr, text, out),
        },
        ExprKind::Block(block) => collect_block(block, text, out),
        ExprKind::Str(parts) => {
            for part in parts {
                if let martite_syntax::StrExprPart::Expr(expr) = part {
                    collect_expr(expr, text, out);
                }
            }
        }
        ExprKind::Tuple(items) | ExprKind::List(items) => {
            for item in items {
                collect_expr(item, text, out);
            }
        }
        ExprKind::Map(entries) => {
            for (key, value) in entries {
                collect_expr(key, text, out);
                collect_expr(value, text, out);
            }
        }
        ExprKind::StructLit { fields, base, .. } => {
            for field in fields {
                if let Some(value) = &field.value {
                    collect_expr(value, text, out);
                }
            }
            if let Some(base) = base {
                collect_expr(base, text, out);
            }
        }
        ExprKind::Unary { expr, .. } => collect_expr(expr, text, out),
        ExprKind::Binary { lhs, rhs, .. } => {
            collect_expr(lhs, text, out);
            collect_expr(rhs, text, out);
        }
        ExprKind::Range { start, end, .. } => {
            collect_expr(start, text, out);
            collect_expr(end, text, out);
        }
        ExprKind::Field { base, .. } => collect_expr(base, text, out),
        ExprKind::Call { callee, args } => {
            collect_expr(callee, text, out);
            for arg in args {
                collect_expr(&arg.expr, text, out);
            }
        }
        ExprKind::Index { base, index } => {
            collect_expr(base, text, out);
            collect_expr(index, text, out);
        }
        ExprKind::Try(inner) => collect_expr(inner, text, out),
        ExprKind::Convert { expr, .. } => collect_expr(expr, text, out),
        ExprKind::Int(_)
        | ExprKind::Float(_)
        | ExprKind::Bool(_)
        | ExprKind::Unit
        | ExprKind::Var(_) => {}
    }
}

fn push(out: &mut Vec<Element>, span: Span, head: Span) {
    out.push(Element { span, head });
}

fn block_head(span: Span, block: &Block) -> Span {
    Span::new(span.start, block.span.start.saturating_add(1))
}

fn brace_head(text: &str, span: Span) -> Span {
    let start = span.start.min(text.len() as u32) as usize;
    let end = span.end.min(text.len() as u32) as usize;
    if let Some(offset) = text[start..end].find('{') {
        let at = start + offset;
        Span::new(span.start, (at + 1) as u32)
    } else {
        span
    }
}
