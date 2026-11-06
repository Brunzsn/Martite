//! AST-driven formatter implementation.

use martite_base::Span;
use martite_config::{EndOfLine, FmtConfig};
use martite_syntax::{
    Arg, AssignOp, BinaryOp, Block, Comment, ConstDecl, EnumDecl, Expr, ExprKind, Field, FieldInit,
    FieldName, FieldPat, FnDecl, GenericParam, ImplDecl, Item, LambdaBody, LambdaParam, MatchArm,
    MatchBody, OnDecl, Param, ParamKind, ParenthesisTables, PatKind, Pattern, Program, Stmt,
    StmtKind, StructDecl, TestDecl, Token, TokenKind, TyExpr, TyKind, UnaryOp, UseDecl, Variant,
};

use crate::doc::{Doc, Renderer};
use crate::normalize;

pub(crate) fn format_program(
    source: &str,
    program: &Program,
    parentheses: &ParenthesisTables,
    tokens: Vec<Token>,
    config: &FmtConfig,
) -> String {
    let mut formatter = Formatter::new(source, program, parentheses, tokens, config);
    let doc = formatter.program_doc();
    let eol = match config.end_of_line {
        EndOfLine::Lf => "\n",
        EndOfLine::Crlf => "\r\n",
    };
    Renderer::new(config.max_width, config.indent, eol).render(&doc)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitKind {
    Use,
    ConstOneLine,
    Other,
}

struct Unit {
    doc: Doc,
    kind: UnitKind,
    start: u32,
    end: u32,
    key: Vec<String>,
    run: usize,
}

enum StructPatEntry<'a> {
    Field(&'a FieldPat),
    Rest(Span),
}

enum StructLitEntry<'a> {
    Base(&'a Expr),
    Field(&'a FieldInit),
}

enum Link<'a> {
    Field(&'a FieldName, Span),
    Call(&'a [Arg], Span),
    Index(&'a Expr, Span),
    Try(Span),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpClass {
    AddSub,
    MulDivRem,
    Compare,
    And,
    Or,
    Coalesce,
}

fn op_class(op: BinaryOp) -> OpClass {
    match op {
        BinaryOp::Add | BinaryOp::Sub => OpClass::AddSub,
        BinaryOp::Mul | BinaryOp::Div | BinaryOp::Rem => OpClass::MulDivRem,
        BinaryOp::Eq | BinaryOp::Ne | BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge => {
            OpClass::Compare
        }
        BinaryOp::And => OpClass::And,
        BinaryOp::Or => OpClass::Or,
        BinaryOp::Coalesce => OpClass::Coalesce,
    }
}

struct Formatter<'a> {
    source: &'a str,
    program: &'a Program,
    parentheses: &'a ParenthesisTables,
    tokens: Vec<Token>,
    config: &'a FmtConfig,
    used: Vec<bool>,
    cursor: usize,
}

impl<'a> Formatter<'a> {
    fn new(
        source: &'a str,
        program: &'a Program,
        parentheses: &'a ParenthesisTables,
        tokens: Vec<Token>,
        config: &'a FmtConfig,
    ) -> Self {
        Self {
            source,
            program,
            parentheses,
            tokens,
            config,
            used: vec![false; program.comments.len()],
            cursor: 0,
        }
    }

    // ----- token and comment helpers -------------------------------------

    fn text(&self, span: Span) -> &str {
        let start = span.start as usize;
        let end = span.end as usize;
        if start <= end && end <= self.source.len() {
            &self.source[start..end]
        } else {
            ""
        }
    }

    fn first_token_where(
        &self,
        start: u32,
        end: u32,
        predicate: impl Fn(&TokenKind) -> bool,
    ) -> Option<Span> {
        let low = self
            .tokens
            .partition_point(|token| token.span.start < start);
        for token in &self.tokens[low..] {
            if token.span.start >= end {
                break;
            }
            if matches!(token.kind, TokenKind::Newline | TokenKind::Eof) {
                continue;
            }
            if token.span.end <= end && predicate(&token.kind) {
                return Some(token.span);
            }
        }
        None
    }

    fn last_token_where(
        &self,
        start: u32,
        end: u32,
        predicate: impl Fn(&TokenKind) -> bool,
    ) -> Option<Span> {
        let high = self.tokens.partition_point(|token| token.span.start <= end);
        for token in self.tokens[..high].iter().rev() {
            if token.span.end <= start {
                break;
            }
            if matches!(token.kind, TokenKind::Newline | TokenKind::Eof) {
                continue;
            }
            if token.span.start >= start && token.span.end <= end && predicate(&token.kind) {
                return Some(token.span);
            }
        }
        None
    }

    fn token_between(&self, start: u32, end: u32, text: &str) -> Option<Span> {
        self.first_token_where(start, end, |kind| normalize::token_text(kind) == Some(text))
    }

    fn matching_close(&self, open: Span, open_text: &str, close_text: &str) -> Option<Span> {
        let mut depth = 0usize;
        let low = self
            .tokens
            .partition_point(|token| token.span.start < open.start);
        for token in &self.tokens[low..] {
            if matches!(token.kind, TokenKind::Newline | TokenKind::Eof) {
                continue;
            }
            let text = normalize::token_text(&token.kind);
            if text == Some(open_text) {
                depth += 1;
            } else if text == Some(close_text) {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(token.span);
                }
            }
        }
        None
    }

    fn open_paren_after(&self, start: u32, end: u32) -> Option<Span> {
        self.token_between(start, end, "(")
    }

    fn next_significant_start(&self, pos: u32) -> Option<u32> {
        let low = self.tokens.partition_point(|token| token.span.start < pos);
        self.tokens[low..]
            .iter()
            .find(|token| !matches!(token.kind, TokenKind::Newline | TokenKind::Eof))
            .map(|token| token.span.start)
    }

    fn is_trailing(&self, comment: &Comment) -> bool {
        let start = comment.span.start as usize;
        let line_start = self.source[..start.min(self.source.len())]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        self.source[line_start..start]
            .chars()
            .any(|ch| !ch.is_whitespace())
    }

    fn pending_index_before(&mut self, pos: u32) -> Option<usize> {
        loop {
            while self.cursor < self.used.len() && self.used[self.cursor] {
                self.cursor += 1;
            }
            let comment = self.program.comments.get(self.cursor)?;
            if comment.span.start < pos {
                return Some(self.cursor);
            }
            return None;
        }
    }

    fn has_pending_before(&mut self, pos: u32) -> bool {
        self.pending_index_before(pos).is_some()
    }

    fn take_pending(&mut self, index: usize) -> Comment {
        self.used[index] = true;
        self.cursor = index + 1;
        self.program.comments[index].clone()
    }

    fn leading_before(&mut self, pos: u32) -> Doc {
        self.leading_before_mode(pos, true)
    }

    fn leading_before_closing(&mut self, pos: u32) -> Doc {
        self.leading_before_mode(pos, false)
    }

    fn leading_before_mode(&mut self, pos: u32, allow_trailing_blank: bool) -> Doc {
        let mut docs = Vec::new();
        let mut previous: Option<Comment> = None;
        while let Some(index) = self.pending_index_before(pos) {
            let comment = self.take_pending(index);
            if let Some(previous) = &previous {
                let glued = previous.doc || is_allow(previous) || comment.doc || is_allow(&comment);
                if !glued && self.has_blank_line(previous.span.end, comment.span.start) {
                    docs.push(Doc::BlankLine);
                } else {
                    docs.push(Doc::HardLine);
                }
            }
            docs.push(Doc::text(normalize::comment(&comment)));
            previous = Some(comment);
        }
        if let Some(previous) = previous {
            let glued = previous.doc || is_allow(&previous);
            if allow_trailing_blank && !glued && self.has_blank_line(previous.span.end, pos) {
                docs.push(Doc::BlankLine);
            } else {
                docs.push(Doc::HardLine);
            }
        }
        Doc::concat(docs)
    }

    fn content_start_before(&mut self, pos: u32) -> u32 {
        self.pending_index_before(pos)
            .map(|index| self.program.comments[index].span.start)
            .unwrap_or(pos)
    }

    /// Consume comments that trail the token ending at `pos`.
    fn take_trailing(&mut self, pos: u32) -> (Doc, bool) {
        let next = self
            .next_significant_start(pos)
            .unwrap_or_else(|| self.source.len() as u32);
        let mut docs = Vec::new();
        let mut any = false;
        loop {
            while self.cursor < self.used.len() && self.used[self.cursor] {
                self.cursor += 1;
            }
            let Some(comment) = self.program.comments.get(self.cursor) else {
                break;
            };
            if comment.span.start < pos || comment.span.start >= next {
                break;
            }
            if !self.is_trailing(comment) {
                break;
            }
            let comment = self.take_pending(self.cursor);
            docs.push(Doc::LineSuffix(normalize::comment(&comment)));
            docs.push(Doc::BreakParent);
            any = true;
        }
        (Doc::concat(docs), any)
    }

    fn token(&mut self, text: &str, span: Option<Span>) -> Doc {
        let mut doc = Doc::text(text);
        if let Some(span) = span {
            let (suffix, _) = self.take_trailing(span.end);
            doc = doc.append(suffix);
        }
        doc
    }

    fn open_with_leading(&mut self, text: &str, open: Span, next: u32) -> Doc {
        let (suffix, had_trailing) = self.take_trailing(open.end);
        let has_leading = self.has_pending_before(next);
        let leading = self.leading_before(next);
        let mut docs = vec![Doc::text(text), suffix];
        if had_trailing || has_leading {
            docs.push(Doc::HardLine);
        }
        docs.push(leading);
        Doc::concat(docs)
    }

    fn has_blank_line(&self, start: u32, end: u32) -> bool {
        let start = start.min(self.source.len() as u32) as usize;
        let end = end.min(self.source.len() as u32) as usize;
        if start >= end {
            return false;
        }
        let bytes = self.source.as_bytes();
        let mut index = start;
        while index + 1 < end {
            if bytes[index] == b'\n' {
                let mut next = index + 1;
                while next < end && matches!(bytes[next], b' ' | b'\t' | b'\r') {
                    next += 1;
                }
                if next < end && bytes[next] == b'\n' {
                    return true;
                }
            }
            index += 1;
        }
        false
    }

    // ----- program and items ---------------------------------------------

    fn program_doc(&mut self) -> Doc {
        let mut units: Vec<Unit> = Vec::new();
        let mut index = 0usize;
        let mut use_run = 0usize;
        let items = &self.program.items;
        while index < items.len() {
            if matches!(items[index], Item::Use(_)) {
                let start = index;
                let mut previous_end = items[index].span().end;
                index += 1;
                while index < items.len() && matches!(items[index], Item::Use(_)) {
                    if self.has_blank_line(previous_end, items[index].span().start) {
                        break;
                    }
                    previous_end = items[index].span().end;
                    index += 1;
                }
                let run = &items[start..index];
                let mut units_in_run = Vec::new();
                for item in run {
                    let Item::Use(use_decl) = item else {
                        unreachable!()
                    };
                    let lead = self.leading_before(item.span().start);
                    let body = self.use_decl(use_decl);
                    let (suffix, _) = self.take_trailing(item.span().end);
                    units_in_run.push(Unit {
                        doc: lead.append(body).append(suffix),
                        kind: UnitKind::Use,
                        start: item.span().start,
                        end: item.span().end,
                        key: use_decl.path.clone(),
                        run: use_run,
                    });
                }
                units_in_run.sort_by(|left, right| left.key.cmp(&right.key));
                use_run += 1;
                units.extend(units_in_run);
                continue;
            }

            let item = &items[index];
            let content_start = self.content_start_before(item.span().start);
            let lead = self.leading_before(item.span().start);
            let body = self.item_body(item);
            let (suffix, _) = self.take_trailing(item.span().end);
            let one_line = matches!(item, Item::Const(_))
                && body
                    .flat_width()
                    .is_some_and(|width| width <= self.config.max_width);
            units.push(Unit {
                doc: lead.append(body).append(suffix),
                kind: if one_line {
                    UnitKind::ConstOneLine
                } else {
                    UnitKind::Other
                },
                start: content_start,
                end: item.span().end,
                key: Vec::new(),
                run: usize::MAX,
            });
            index += 1;
        }

        let mut docs = Vec::new();
        let mut previous: Option<usize> = None;
        for (index, unit) in units.iter().enumerate() {
            if let Some(previous_index) = previous {
                let previous_unit = &units[previous_index];
                let keep_blank = previous_unit.kind == UnitKind::ConstOneLine
                    && unit.kind == UnitKind::ConstOneLine
                    && self.has_blank_line(previous_unit.end, unit.start);
                let previous_use = previous_unit.kind == UnitKind::Use;
                let current_use = unit.kind == UnitKind::Use;
                if previous_use && current_use {
                    if previous_unit.run != unit.run {
                        docs.push(Doc::BlankLine);
                    } else {
                        docs.push(Doc::HardLine);
                    }
                } else if keep_blank {
                    docs.push(Doc::BlankLine);
                } else if previous_unit.kind == UnitKind::ConstOneLine
                    && unit.kind == UnitKind::ConstOneLine
                {
                    docs.push(Doc::HardLine);
                } else {
                    docs.push(Doc::BlankLine);
                }
            }
            docs.push(unit.doc.clone());
            previous = Some(index);
        }

        if self.has_pending_before(u32::MAX) {
            if !docs.is_empty() {
                docs.push(Doc::HardLine);
            }
            docs.push(self.leading_before_closing(u32::MAX));
        }
        Doc::concat(docs)
    }

    fn item_body(&mut self, item: &Item) -> Doc {
        match item {
            Item::Use(decl) => self.use_decl(decl),
            Item::Test(decl) => self.test_decl(decl),
            Item::On(decl) => self.on_decl(decl),
            Item::Impl(decl) => self.impl_decl(decl),
            Item::Fn(decl) => self.fn_decl(decl),
            Item::Struct(decl) => self.struct_decl(decl),
            Item::Enum(decl) => self.enum_decl(decl),
            Item::Const(decl) => self.const_decl(decl),
        }
    }

    fn use_decl(&mut self, decl: &UseDecl) -> Doc {
        let mut text = String::from("use ");
        text.push_str(&decl.path.join("."));
        if let Some(alias) = &decl.alias {
            text.push_str(" as ");
            text.push_str(alias);
        }
        Doc::text(text)
    }

    fn test_decl(&mut self, decl: &TestDecl) -> Doc {
        let string_span = self.first_token_where(decl.span.start, decl.body.span.start, |kind| {
            matches!(kind, TokenKind::Str(_))
        });
        let name = string_span
            .map(|span| self.text(span).to_string())
            .unwrap_or_else(|| format!("\"{}\"", escape_string(&decl.name)));
        let body = self.block(&decl.body);
        Doc::concat([
            self.token("test ", None),
            Doc::text(name),
            if let Some(span) = string_span {
                self.take_trailing(span.end).0
            } else {
                Doc::Nil
            },
            Doc::text(" "),
            body,
        ])
    }

    fn on_decl(&mut self, decl: &OnDecl) -> Doc {
        let mut docs = vec![Doc::text("on "), Doc::text(decl.name.clone())];
        docs.push(self.param_list(&decl.params, decl.span.start));
        if let Some(ret) = &decl.ret {
            docs.push(self.return_type(ret, decl.body.span.start));
        }
        if !self.program.is_interface {
            docs.push(Doc::text(" "));
            docs.push(self.block(&decl.body));
        }
        Doc::concat(docs)
    }

    fn impl_decl(&mut self, decl: &ImplDecl) -> Doc {
        let mut docs = vec![Doc::text("impl")];
        if !decl.generics.is_empty() {
            docs.push(self.generic_params(&decl.generics));
        }
        docs.push(Doc::text(" "));
        docs.push(Doc::text(decl.type_name.clone()));
        if !decl.type_args.is_empty() {
            docs.push(self.type_args(&decl.type_args));
        }
        let open = self.token_between(decl.span.start, decl.span.end, "{");
        let Some(open) = open else {
            return Doc::concat(docs);
        };
        let close = self
            .last_token_where(open.end, decl.span.end, |kind| {
                matches!(kind, TokenKind::RBrace)
            })
            .unwrap_or_else(|| Span::empty(decl.span.end));
        docs.push(Doc::text(" "));
        let (open_suffix, open_had_trailing) = self.take_trailing(open.end);
        let open_doc = Doc::text("{").append(open_suffix);
        docs.push(open_doc);
        if decl.methods.is_empty() {
            if !open_had_trailing && !self.has_pending_before(close.start) {
                docs.push(self.token("}", Some(close)));
                return Doc::concat(docs);
            }
            if self.has_pending_before(close.start) {
                docs.push(
                    Doc::concat([Doc::HardLine, self.leading_before_closing(close.start)]).indent(),
                );
            }
            docs.push(Doc::HardLine);
            docs.push(self.token("}", Some(close)));
            return Doc::concat(docs);
        }

        let mut method_docs = Vec::new();
        let mut previous: Option<(u32, bool)> = None;
        for method in &decl.methods {
            let content_start = self.content_start_before(method.span.start);
            let lead = self.leading_before(method.span.start);
            let body = self.fn_decl(method);
            let (suffix, _) = self.take_trailing(method.span.end);
            let one_line = body
                .flat_width()
                .is_some_and(|width| width + self.config.indent <= self.config.max_width);
            let doc = lead.append(body).append(suffix);
            if let Some((previous_end, previous_one_line)) = previous {
                let separator = if previous_one_line
                    && one_line
                    && !self.has_blank_line(previous_end, content_start)
                {
                    Doc::HardLine
                } else {
                    Doc::BlankLine
                };
                method_docs.push(separator);
            }
            method_docs.push(doc);
            previous = Some((method.span.end, one_line));
        }
        let body = Doc::concat(method_docs);
        docs.push(Doc::concat([Doc::HardLine, body]).indent());
        if self.has_pending_before(close.start) {
            docs.push(
                Doc::concat([Doc::HardLine, self.leading_before_closing(close.start)]).indent(),
            );
        }
        docs.push(Doc::HardLine);
        docs.push(self.token("}", Some(close)));
        Doc::concat(docs)
    }

    fn fn_decl(&mut self, decl: &FnDecl) -> Doc {
        let mut docs = Vec::new();
        if decl.is_pub {
            docs.push(Doc::text("pub "));
        }
        docs.push(Doc::text("fn "));
        docs.push(Doc::text(decl.name.clone()));
        if !decl.generics.is_empty() {
            docs.push(self.generic_params(&decl.generics));
        }
        docs.push(self.param_list(&decl.params, decl.span.start));
        let body_start = decl
            .body
            .as_ref()
            .map(|block| block.span.start)
            .unwrap_or(decl.span.end);
        if let Some(ret) = &decl.ret {
            docs.push(self.return_type(ret, body_start));
        }
        if let Some(body) = &decl.body {
            docs.push(Doc::text(" "));
            docs.push(self.block(body));
        }
        Doc::concat(docs)
    }

    fn struct_decl(&mut self, decl: &StructDecl) -> Doc {
        let mut docs = vec![Doc::text(if decl.is_pub {
            "pub struct "
        } else {
            "struct "
        })];
        docs.push(Doc::text(decl.name.clone()));
        if !decl.generics.is_empty() {
            docs.push(self.generic_params(&decl.generics));
        }
        if decl.opaque {
            return Doc::concat(docs);
        }
        let open = self.token_between(decl.span.start, decl.span.end, "{");
        let Some(open) = open else {
            return Doc::concat(docs);
        };
        let close = self
            .last_token_where(open.end, decl.span.end, |kind| {
                matches!(kind, TokenKind::RBrace)
            })
            .unwrap_or_else(|| Span::empty(decl.span.end));
        let fields: Vec<(Span, &Field)> = decl
            .fields
            .iter()
            .map(|field| (field.span, field))
            .collect();
        docs.push(Doc::text(" "));
        docs.push(self.comma_list(
            "{",
            "}",
            Some(open),
            Some(close),
            &fields,
            |formatter, field| formatter.field(field),
            true,
            false,
            true,
        ));
        Doc::concat(docs)
    }

    fn enum_decl(&mut self, decl: &EnumDecl) -> Doc {
        let mut docs = vec![Doc::text(if decl.is_pub { "pub enum " } else { "enum " })];
        docs.push(Doc::text(decl.name.clone()));
        if !decl.generics.is_empty() {
            docs.push(self.generic_params(&decl.generics));
        }
        let open = self.token_between(decl.span.start, decl.span.end, "{");
        let Some(open) = open else {
            return Doc::concat(docs);
        };
        let close = self
            .last_token_where(open.end, decl.span.end, |kind| {
                matches!(kind, TokenKind::RBrace)
            })
            .unwrap_or_else(|| Span::empty(decl.span.end));
        let variants: Vec<(Span, &Variant)> = decl
            .variants
            .iter()
            .map(|variant| (variant.span, variant))
            .collect();
        docs.push(Doc::text(" "));
        docs.push(self.comma_list(
            "{",
            "}",
            Some(open),
            Some(close),
            &variants,
            |formatter, variant| formatter.variant(variant),
            true,
            false,
            true,
        ));
        Doc::concat(docs)
    }

    fn const_decl(&mut self, decl: &ConstDecl) -> Doc {
        let mut docs = vec![Doc::text(if decl.is_pub { "pub const " } else { "const " })];
        docs.push(Doc::text(decl.name.clone()));
        let colon = self.find_before(decl.ty.span.start, ":");
        docs.push(Doc::Group(Box::new(Doc::concat([
            self.token(":", colon),
            Doc::concat([Doc::Line, self.ty(&decl.ty)]).indent(),
        ]))));
        let eq = self.token_between(decl.ty.span.end, decl.value.span.start, "=");
        docs.push(Doc::text(" "));
        docs.push(self.token("=", eq));
        docs.push(self.assignment_value(&decl.value, decl.ty.span.end, eq));
        Doc::concat(docs)
    }

    fn field(&mut self, field: &Field) -> Doc {
        let colon = self.token_between(
            field.span.start + field.name.len() as u32,
            field.ty.span.start,
            ":",
        );
        Doc::concat([
            Doc::text(field.name.clone()),
            Doc::Group(Box::new(Doc::concat([
                self.token(":", colon),
                Doc::concat([Doc::Line, self.ty(&field.ty)]).indent(),
            ]))),
        ])
    }

    fn variant(&mut self, variant: &Variant) -> Doc {
        let mut docs = vec![Doc::text(variant.name.clone())];
        if !variant.payload.is_empty() || self.variant_has_parens(variant) {
            let open = self.token_between(variant.span.start, variant.span.end, "(");
            let close = open.and_then(|open| self.matching_close(open, "(", ")"));
            let payload: Vec<(Span, &TyExpr)> =
                variant.payload.iter().map(|ty| (ty.span, ty)).collect();
            if let Some(open) = open {
                docs.push(self.comma_list(
                    "(",
                    ")",
                    Some(open),
                    close,
                    &payload,
                    |formatter, ty| formatter.ty(ty),
                    false,
                    false,
                    true,
                ));
            }
        }
        Doc::concat(docs)
    }

    fn variant_has_parens(&self, variant: &Variant) -> bool {
        self.token_between(variant.span.start, variant.span.end, "(")
            .is_some()
    }

    fn generic_params(&mut self, params: &[GenericParam]) -> Doc {
        let items: Vec<(Span, &GenericParam)> =
            params.iter().map(|param| (param.span, param)).collect();
        let open = params
            .first()
            .and_then(|param| self.find_before(param.span.start, "["));
        let close = open.and_then(|open| self.matching_close(open, "[", "]"));
        self.comma_list(
            "[",
            "]",
            open,
            close,
            &items,
            |formatter, param| formatter.generic_param(param),
            false,
            false,
            true,
        )
    }

    fn generic_param(&mut self, param: &GenericParam) -> Doc {
        let mut docs = vec![Doc::text(param.name.clone())];
        if !param.bounds.is_empty() {
            let colon = self.token_between(param.span.start, param.span.end, ":");
            let pluses: Vec<Span> = self
                .tokens
                .iter()
                .filter(|token| {
                    token.span.start >= param.span.start
                        && token.span.end <= param.span.end
                        && normalize::token_text(&token.kind) == Some("+")
                })
                .map(|token| token.span)
                .collect();
            let mut bounds = Vec::new();
            for (index, bound) in param.bounds.iter().enumerate() {
                if index > 0 {
                    let plus = pluses.get(index - 1).copied();
                    let (suffix, had_comment) = if let Some(span) = plus {
                        self.take_trailing(span.end)
                    } else {
                        (Doc::Nil, false)
                    };
                    bounds.push(Doc::text(" "));
                    bounds.push(Doc::text("+").append(suffix));
                    if had_comment {
                        bounds.push(Doc::HardLine);
                    } else {
                        bounds.push(Doc::text(" "));
                    }
                }
                bounds.push(Doc::text(bound.as_str()));
            }
            docs.push(Doc::Group(Box::new(Doc::concat([
                self.token(":", colon),
                Doc::concat([Doc::Line, Doc::concat(bounds)]).indent(),
            ]))));
        }
        Doc::concat(docs)
    }

    fn find_before(&self, pos: u32, text: &str) -> Option<Span> {
        self.tokens
            .iter()
            .filter(|token| !matches!(token.kind, TokenKind::Newline | TokenKind::Eof))
            .filter(|token| token.span.end <= pos)
            .filter(|token| normalize::token_text(&token.kind) == Some(text))
            .next_back()
            .map(|token| token.span)
    }

    // ----- parameters and types ------------------------------------------

    fn param_list(&mut self, params: &[Param], search_start: u32) -> Doc {
        let open = self
            .first_token_where(search_start, self.source.len() as u32, |kind| {
                matches!(kind, TokenKind::LParen)
            })
            .or_else(|| self.token_between(search_start, self.source.len() as u32, "("));
        let Some(open) = open else {
            return Doc::text("()");
        };
        let close = self.matching_close(open, "(", ")");
        let items: Vec<(Span, &Param)> = params.iter().map(|param| (param.span, param)).collect();
        self.comma_list(
            "(",
            ")",
            Some(open),
            close,
            &items,
            |formatter, param| formatter.param(param),
            false,
            false,
            true,
        )
    }

    fn param(&mut self, param: &Param) -> Doc {
        match &param.kind {
            ParamKind::SelfParam { mutable } => {
                Doc::text(if *mutable { "mut self" } else { "self" })
            }
            ParamKind::Normal { name, mutable, ty } => {
                let mut text = String::new();
                if *mutable {
                    text.push_str("mut ");
                }
                text.push_str(name);
                let colon =
                    self.token_between(param.span.start + text.len() as u32, ty.span.start, ":");
                Doc::concat([
                    Doc::text(text),
                    Doc::Group(Box::new(Doc::concat([
                        self.token(":", colon),
                        Doc::concat([Doc::Line, self.ty(ty)]).indent(),
                    ]))),
                ])
            }
        }
    }

    fn return_type(&mut self, ret: &TyExpr, _search_end: u32) -> Doc {
        let arrow = self.find_before(ret.span.start, "->");
        Doc::concat([Doc::text(" "), self.arrow_type(arrow, ret)])
    }

    fn arrow_type(&mut self, arrow: Option<Span>, ret: &TyExpr) -> Doc {
        let (suffix, _) = if let Some(arrow) = arrow {
            self.take_trailing(arrow.end)
        } else {
            (Doc::Nil, false)
        };
        let ret_doc = self.ty(ret);
        Doc::Group(Box::new(Doc::concat([
            Doc::text("->"),
            suffix,
            Doc::concat([Doc::Line, ret_doc]).indent(),
        ])))
    }

    fn type_args(&mut self, args: &[TyExpr]) -> Doc {
        let open = args
            .first()
            .and_then(|arg| self.find_before(arg.span.start, "["));
        let close = open.and_then(|open| self.matching_close(open, "[", "]"));
        let items: Vec<(Span, &TyExpr)> = args.iter().map(|arg| (arg.span, arg)).collect();
        self.comma_list(
            "[",
            "]",
            open,
            close,
            &items,
            |formatter, ty| formatter.ty(ty),
            false,
            false,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn comma_list<T>(
        &mut self,
        open_text: &str,
        close_text: &str,
        open_span: Option<Span>,
        close_span: Option<Span>,
        items: &[(Span, T)],
        mut format_item: impl FnMut(&mut Self, &T) -> Doc,
        flat_space: bool,
        single_flat_comma: bool,
        broken_comma: bool,
    ) -> Doc {
        let open = self.token(open_text, open_span);
        let close_start = close_span
            .map(|span| span.start)
            .unwrap_or_else(|| items.last().map(|(span, _)| span.end).unwrap_or(0));

        let mut content = Vec::new();
        for (index, (span, item)) in items.iter().enumerate() {
            if index > 0 {
                let previous_end = items[index - 1].0.end;
                let comma = self.token_between(previous_end, span.start, ",");
                content.push(self.token(",", comma));
                content.push(Doc::Line);
            }
            content.push(self.leading_before(span.start));
            content.push(format_item(self, item));
            if index + 1 == items.len() {
                let trailing_span = self.token_between(span.end, close_start, ",");
                let trailing = if single_flat_comma && items.len() == 1 {
                    Doc::text(",")
                } else if broken_comma {
                    Doc::IfBreak {
                        broken: Box::new(Doc::text(",")),
                        flat: Box::new(Doc::Nil),
                    }
                } else {
                    Doc::Nil
                };
                content.push(trailing);
                if let Some(span) = trailing_span {
                    content.push(self.take_trailing(span.end).0);
                }
                if self.has_pending_before(close_start) {
                    content.push(Doc::HardLine);
                }
                content.push(self.leading_before_closing(close_start));
            }
        }
        if items.is_empty() {
            if self.has_pending_before(close_start) {
                content.push(Doc::HardLine);
            }
            content.push(self.leading_before_closing(close_start));
        }

        let inner_line = if items.is_empty() || !flat_space {
            Doc::SoftLine
        } else {
            Doc::Line
        };
        let close_line = if items.is_empty() || !flat_space {
            Doc::SoftLine
        } else {
            Doc::Line
        };
        Doc::Group(Box::new(Doc::concat([
            open,
            Doc::concat([inner_line, Doc::concat(content)]).indent(),
            close_line,
            self.token(close_text, close_span),
        ])))
    }

    fn ty(&mut self, ty: &TyExpr) -> Doc {
        let pairs = self.parentheses.type_parens(ty.id);
        let start = pairs
            .last()
            .map(|pair| pair.open.start)
            .unwrap_or(ty.span.start);
        let lead = self.leading_before(start);
        let mut index = pairs.len();
        let core = self.wrap_type_parens(pairs, &mut index, ty);
        lead.append(core)
    }

    fn wrap_type_parens(
        &mut self,
        pairs: &[martite_syntax::ParenPair],
        index: &mut usize,
        ty: &TyExpr,
    ) -> Doc {
        if *index == 0 {
            return self.ty_core(ty);
        }
        *index -= 1;
        let pair = pairs[*index];
        let next = if *index == 0 {
            ty.span.start
        } else {
            pairs[*index - 1].open.start
        };
        let open = self.open_with_leading("(", pair.open, next);
        let inner = self.wrap_type_parens(pairs, index, ty);
        let has_close = self.has_pending_before(pair.close.start);
        let mut docs = vec![open, inner.indent()];
        if has_close {
            docs.push(Doc::HardLine);
        }
        docs.push(self.leading_before_closing(pair.close.start));
        docs.push(self.token(")", Some(pair.close)));
        Doc::concat(docs)
    }

    fn ty_core(&mut self, ty: &TyExpr) -> Doc {
        match &ty.kind {
            TyKind::Prim(prim) => Doc::text(prim.as_str()),
            TyKind::Unit => Doc::text("()"),
            TyKind::Named { name, args } => {
                let mut docs = vec![Doc::text(name.clone())];
                if !args.is_empty() || self.type_has_brackets(ty) {
                    docs.push(self.type_args(args));
                }
                Doc::concat(docs)
            }
            TyKind::Optional(inner) => {
                let question = self.token_between(inner.span.end, ty.span.end, "?");
                Doc::concat([self.ty(inner), self.token("?", question)])
            }
            TyKind::Tuple(items) => {
                let open = self.token_between(ty.span.start, ty.span.end, "(");
                let close = open.and_then(|open| self.matching_close(open, "(", ")"));
                let items: Vec<(Span, &TyExpr)> =
                    items.iter().map(|item| (item.span, item)).collect();
                self.comma_list(
                    "(",
                    ")",
                    open,
                    close,
                    &items,
                    |formatter, item| formatter.ty(item),
                    false,
                    items.len() == 1,
                    true,
                )
            }
            TyKind::Fn { params, ret } => {
                let open = self.token_between(ty.span.start, ty.span.end, "(");
                let close = open.and_then(|open| self.matching_close(open, "(", ")"));
                let items: Vec<(Span, &TyExpr)> =
                    params.iter().map(|param| (param.span, param)).collect();
                let mut docs = vec![Doc::text("fn")];
                if let Some(open) = open {
                    docs.push(self.comma_list(
                        "(",
                        ")",
                        Some(open),
                        close,
                        &items,
                        |formatter, item| formatter.ty(item),
                        false,
                        false,
                        true,
                    ));
                } else {
                    docs.push(Doc::text("()"));
                }
                if let Some(ret) = ret {
                    let arrow = self.token_between(ty.span.start, ret.span.start, "->");
                    docs.push(Doc::text(" "));
                    docs.push(self.arrow_type(arrow, ret));
                }
                Doc::concat(docs)
            }
        }
    }

    fn type_has_brackets(&self, ty: &TyExpr) -> bool {
        self.token_between(ty.span.start, ty.span.end, "[")
            .is_some()
    }

    // ----- patterns -------------------------------------------------------

    fn pattern(&mut self, pattern: &Pattern) -> Doc {
        let pairs = self.parentheses.pattern_parens(pattern.id);
        let start = pairs
            .last()
            .map(|pair| pair.open.start)
            .unwrap_or(pattern.span.start);
        let lead = self.leading_before(start);
        let mut index = pairs.len();
        let core = self.wrap_pattern_parens(pairs, &mut index, pattern);
        lead.append(core)
    }

    fn wrap_pattern_parens(
        &mut self,
        pairs: &[martite_syntax::ParenPair],
        index: &mut usize,
        pattern: &Pattern,
    ) -> Doc {
        if *index == 0 {
            return self.pattern_core(pattern);
        }
        *index -= 1;
        let pair = pairs[*index];
        let next = if *index == 0 {
            pattern.span.start
        } else {
            pairs[*index - 1].open.start
        };
        let open = self.open_with_leading("(", pair.open, next);
        let inner = self.wrap_pattern_parens(pairs, index, pattern);
        let has_close = self.has_pending_before(pair.close.start);
        let mut docs = vec![open, inner.indent()];
        if has_close {
            docs.push(Doc::HardLine);
        }
        docs.push(self.leading_before_closing(pair.close.start));
        docs.push(self.token(")", Some(pair.close)));
        Doc::concat(docs)
    }

    fn pattern_core(&mut self, pattern: &Pattern) -> Doc {
        match &pattern.kind {
            PatKind::Wild => Doc::text("_"),
            PatKind::Bind(name) => Doc::text(name.clone()),
            PatKind::Int(value) => {
                let raw = normalize::literal_text(self.source, pattern.span, &value.to_string());
                Doc::text(normalize::number(&raw))
            }
            PatKind::Bool(value) => Doc::text(if *value { "true" } else { "false" }),
            PatKind::Str(_) => Doc::text(self.text(pattern.span).to_string()),
            PatKind::Tuple(items) => {
                let open = self.token_between(pattern.span.start, pattern.span.end, "(");
                let close = open.and_then(|open| self.matching_close(open, "(", ")"));
                let items: Vec<(Span, &Pattern)> =
                    items.iter().map(|item| (item.span, item)).collect();
                self.comma_list(
                    "(",
                    ")",
                    open,
                    close,
                    &items,
                    |formatter, item| formatter.pattern(item),
                    false,
                    items.len() == 1,
                    true,
                )
            }
            PatKind::Variant {
                qualifier,
                name,
                args,
            } => {
                let mut docs = Vec::new();
                if let Some(qualifier) = qualifier {
                    docs.push(Doc::text(qualifier.clone()));
                    docs.push(Doc::text("."));
                }
                docs.push(Doc::text(name.clone()));
                let open = self.token_between(pattern.span.start, pattern.span.end, "(");
                if let Some(open) = open {
                    let close = self.matching_close(open, "(", ")");
                    let items: Vec<(Span, &Pattern)> =
                        args.iter().map(|arg| (arg.span, arg)).collect();
                    docs.push(self.comma_list(
                        "(",
                        ")",
                        Some(open),
                        close,
                        &items,
                        |formatter, item| formatter.pattern(item),
                        false,
                        false,
                        true,
                    ));
                }
                Doc::concat(docs)
            }
            PatKind::Struct { name, fields, rest } => {
                let open = self.token_between(pattern.span.start, pattern.span.end, "{");
                let close = open.and_then(|open| self.matching_close(open, "{", "}"));
                let mut docs = vec![Doc::text(name.clone()), Doc::text(" ")];
                if let Some(open) = open {
                    let close = close.unwrap_or_else(|| Span::empty(pattern.span.end));
                    let mut items: Vec<(Span, StructPatEntry<'_>)> = fields
                        .iter()
                        .map(|field| (field.span, StructPatEntry::Field(field)))
                        .collect();
                    if *rest {
                        let span = self
                            .token_between(pattern.span.start, close.start, "..")
                            .unwrap_or_else(|| Span::empty(close.start));
                        items.push((span, StructPatEntry::Rest(span)));
                    }
                    docs.push(self.comma_list(
                        "{",
                        "}",
                        Some(open),
                        Some(close),
                        &items,
                        |formatter, entry| match entry {
                            StructPatEntry::Field(field) => formatter.field_pat(field),
                            StructPatEntry::Rest(span) => formatter.token("..", Some(*span)),
                        },
                        true,
                        false,
                        !*rest,
                    ));
                }
                Doc::concat(docs)
            }
            PatKind::Or(alternatives) => {
                let mut docs = Vec::new();
                for (index, alternative) in alternatives.iter().enumerate() {
                    if index > 0 {
                        let previous = &alternatives[index - 1];
                        let pipe =
                            self.token_between(previous.span.end, alternative.span.start, "|");
                        docs.push(Doc::text(" "));
                        docs.push(self.token("|", pipe));
                        docs.push(Doc::Line);
                    }
                    docs.push(self.pattern(alternative));
                }
                Doc::Group(Box::new(Doc::concat([
                    docs.remove(0),
                    Doc::concat(docs).indent(),
                ])))
            }
        }
    }

    fn field_pat(&mut self, field: &FieldPat) -> Doc {
        let explicit = self
            .text(field.span)
            .trim_start()
            .strip_prefix(&field.name)
            .is_some_and(|rest| rest.trim_start().starts_with(':'));
        let mut docs = vec![Doc::text(field.name.clone())];
        if explicit {
            let colon = self.token_between(
                field.span.start + field.name.len() as u32,
                field.pattern.span.start,
                ":",
            );
            docs.push(Doc::Group(Box::new(Doc::concat([
                self.token(":", colon),
                Doc::concat([Doc::Line, self.pattern(&field.pattern)]).indent(),
            ]))));
        }
        Doc::concat(docs)
    }

    // ----- expressions ---------------------------------------------------

    fn expr(&mut self, expr: &Expr) -> Doc {
        let pairs = self.parentheses.expression_parens(expr.id);
        let start = pairs
            .last()
            .map(|pair| pair.open.start)
            .unwrap_or(expr.span.start);
        let lead = self.leading_before(start);
        let mut index = pairs.len();
        let core = self.wrap_expr_parens(pairs, &mut index, expr);
        lead.append(core)
    }

    fn wrap_expr_parens(
        &mut self,
        pairs: &[martite_syntax::ParenPair],
        index: &mut usize,
        expr: &Expr,
    ) -> Doc {
        if *index == 0 {
            return self.expr_core(expr);
        }
        *index -= 1;
        let pair = pairs[*index];
        let next = if *index == 0 {
            expr.span.start
        } else {
            pairs[*index - 1].open.start
        };
        let open = self.open_with_leading("(", pair.open, next);
        let inner = self.wrap_expr_parens(pairs, index, expr);
        let has_close = self.has_pending_before(pair.close.start);
        let mut docs = vec![open, inner.indent()];
        if has_close {
            docs.push(Doc::HardLine);
        }
        docs.push(self.leading_before_closing(pair.close.start));
        docs.push(self.token(")", Some(pair.close)));
        Doc::concat(docs)
    }

    fn expr_core(&mut self, expr: &Expr) -> Doc {
        if matches!(
            expr.kind,
            ExprKind::Field { .. }
                | ExprKind::Call { .. }
                | ExprKind::Index { .. }
                | ExprKind::Try(_)
        ) {
            return self.postfix_expr(expr);
        }
        let doc = match &expr.kind {
            ExprKind::Int(value) => {
                let raw = normalize::literal_text(self.source, expr.span, &value.to_string());
                Doc::text(normalize::number(&raw))
            }
            ExprKind::Float(value) => {
                let fallback = format!("{value:?}");
                let raw = normalize::literal_text(self.source, expr.span, &fallback);
                Doc::text(normalize::number(&raw))
            }
            ExprKind::Bool(value) => Doc::text(if *value { "true" } else { "false" }),
            ExprKind::Str(_) => Doc::text(self.text(expr.span).to_string()),
            ExprKind::Unit => Doc::text("()"),
            ExprKind::Var(name) => Doc::text(name.clone()),
            ExprKind::Tuple(items) => {
                let open = self.token_between(expr.span.start, expr.span.end, "(");
                let close = open.and_then(|open| self.matching_close(open, "(", ")"));
                let items: Vec<(Span, &Expr)> =
                    items.iter().map(|item| (item.span, item)).collect();
                self.comma_list(
                    "(",
                    ")",
                    open,
                    close,
                    &items,
                    |formatter, item| formatter.expr(item),
                    false,
                    items.len() == 1,
                    true,
                )
            }
            ExprKind::List(items) => {
                let open = self.token_between(expr.span.start, expr.span.end, "[");
                let close = open.and_then(|open| self.matching_close(open, "[", "]"));
                let items: Vec<(Span, &Expr)> =
                    items.iter().map(|item| (item.span, item)).collect();
                self.comma_list(
                    "[",
                    "]",
                    open,
                    close,
                    &items,
                    |formatter, item| formatter.expr(item),
                    false,
                    false,
                    true,
                )
            }
            ExprKind::Map(entries) => {
                let open = self.token_between(expr.span.start, expr.span.end, "{");
                let close = open.and_then(|open| self.matching_close(open, "{", "}"));
                let entries: Vec<(Span, &(Expr, Expr))> = entries
                    .iter()
                    .map(|entry| (entry.0.span.merge(entry.1.span), entry))
                    .collect();
                self.comma_list(
                    "{",
                    "}",
                    open,
                    close,
                    &entries,
                    |formatter, (key, value)| formatter.map_entry(key, value),
                    false,
                    false,
                    true,
                )
            }
            ExprKind::StructLit { name, fields, base } => {
                self.struct_literal(expr, name, fields, base.as_deref())
            }
            ExprKind::Unary { op, expr: inner } => {
                let op_text = match op {
                    UnaryOp::Neg => "-",
                    UnaryOp::Not => "not ",
                };
                let op_span = self.token_between(expr.span.start, inner.span.start, op_text.trim());
                Doc::concat([self.token(op_text, op_span), self.expr(inner)])
            }
            ExprKind::Binary { op, lhs, rhs } => self.binary_doc(expr, *op, lhs, rhs),
            ExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                let op_text = if *inclusive { "..=" } else { ".." };
                let op_span = self.token_between(start.span.end, end.span.start, op_text);
                Doc::concat([
                    self.expr(start),
                    self.token(op_text, op_span),
                    self.expr(end),
                ])
            }
            ExprKind::If { .. } => self.if_doc(expr),
            ExprKind::Match { scrutinee, arms } => self.match_doc(expr, scrutinee, arms),
            ExprKind::Lambda { params, ret, body } => {
                self.lambda_doc(expr, params, ret.as_ref(), body)
            }
            ExprKind::Block(block) => self.block(block),
            ExprKind::Convert { to, expr: inner } => {
                let open = self.open_paren_after(expr.span.start, expr.span.end);
                let close = open.and_then(|open| self.matching_close(open, "(", ")"));
                let mut docs = vec![Doc::text(to.as_str())];
                if let Some(open) = open {
                    docs.push(self.open_with_leading("(", open, inner.span.start));
                    docs.push(self.expr(inner).indent());
                    if let Some(close) = close {
                        if self.has_pending_before(close.start) {
                            docs.push(
                                Doc::concat([
                                    Doc::HardLine,
                                    self.leading_before_closing(close.start),
                                ])
                                .indent(),
                            );
                        }
                        docs.push(self.token(")", Some(close)));
                    }
                }
                Doc::concat(docs)
            }
            ExprKind::Field { .. }
            | ExprKind::Call { .. }
            | ExprKind::Index { .. }
            | ExprKind::Try(_) => unreachable!(),
        };
        let (suffix, _) = self.take_trailing(expr.span.end);
        doc.append(suffix)
    }

    fn map_entry(&mut self, key: &Expr, value: &Expr) -> Doc {
        let colon = self.token_between(key.span.end, value.span.start, ":");
        Doc::Group(Box::new(Doc::concat([
            self.expr(key),
            self.token(":", colon),
            Doc::concat([Doc::Line, self.expr(value)]).indent(),
        ])))
    }

    fn struct_literal(
        &mut self,
        expr: &Expr,
        name: &str,
        fields: &[FieldInit],
        base: Option<&Expr>,
    ) -> Doc {
        let open = self.token_between(expr.span.start, expr.span.end, "{");
        let close = open.and_then(|open| self.matching_close(open, "{", "}"));
        let Some(open) = open else {
            return Doc::text(name.to_string());
        };
        let mut entries: Vec<(Span, StructLitEntry<'_>)> = Vec::new();
        if let Some(base) = base {
            let span = self
                .token_between(expr.span.start, base.span.start, "..")
                .unwrap_or(base.span);
            entries.push((span.merge(base.span), StructLitEntry::Base(base)));
        }
        for field in fields {
            entries.push((field.span, StructLitEntry::Field(field)));
        }
        let close = close.unwrap_or_else(|| Span::empty(expr.span.end));
        let content = self.comma_list(
            "{",
            "}",
            Some(open),
            Some(close),
            &entries,
            |formatter, entry| match entry {
                StructLitEntry::Base(base) => {
                    let dot = formatter.find_before(base.span.start, "..");
                    let dot = dot.or_else(|| {
                        formatter.token_between(expr.span.start, base.span.start, "..")
                    });
                    Doc::concat([formatter.token("..", dot), formatter.expr(base)])
                }
                StructLitEntry::Field(field) => formatter.field_init(field),
            },
            true,
            false,
            true,
        );
        Doc::concat([Doc::text(name.to_string()), Doc::text(" "), content])
    }

    fn field_init(&mut self, field: &FieldInit) -> Doc {
        let explicit = self
            .text(field.span)
            .trim_start()
            .strip_prefix(&field.name)
            .is_some_and(|rest| rest.trim_start().starts_with(':'));
        let mut docs = vec![Doc::text(field.name.clone())];
        if explicit {
            if let Some(value) = &field.value {
                let colon = self.token_between(
                    field.span.start + field.name.len() as u32,
                    value.span.start,
                    ":",
                );
                docs.push(self.token(":", colon));
                docs.push(Doc::concat([Doc::Line, self.expr(value)]).indent());
            }
        }
        Doc::Group(Box::new(Doc::concat(docs)))
    }

    fn binary_doc(&mut self, expr: &Expr, op: BinaryOp, lhs: &Expr, rhs: &Expr) -> Doc {
        let class = op_class(op);
        let mut operands: Vec<&Expr> = Vec::new();
        let mut ops: Vec<BinaryOp> = Vec::new();
        self.flatten_binary(expr, class, &mut operands, &mut ops);
        if operands.is_empty() {
            operands.push(lhs);
            operands.push(rhs);
            ops.push(op);
        }

        let first = self.expr(operands[0]);
        let mut tail = Vec::new();
        for (index, op) in ops.iter().enumerate() {
            let previous = operands[index];
            let next = operands[index + 1];
            let op_span = self.token_between(previous.span.end, next.span.start, op.as_str());
            let (suffix, had_comment) = if let Some(span) = op_span {
                let text = Doc::text(op.as_str());
                let (suffix, had) = self.take_trailing(span.end);
                (text.append(suffix), had)
            } else {
                (Doc::text(op.as_str()), false)
            };
            match class {
                OpClass::And | OpClass::Or | OpClass::Coalesce => {
                    let lead = op_span
                        .map(|span| self.leading_before(span.start))
                        .unwrap_or(Doc::Nil);
                    tail.push(Doc::Line);
                    tail.push(lead);
                    tail.push(suffix);
                    if had_comment {
                        tail.push(Doc::HardLine);
                    } else {
                        tail.push(Doc::text(" "));
                    }
                    tail.push(self.expr(next));
                }
                _ => {
                    tail.push(Doc::text(" "));
                    tail.push(suffix);
                    tail.push(Doc::Line);
                    tail.push(self.expr(next));
                }
            }
        }
        Doc::Group(Box::new(Doc::concat([first, Doc::concat(tail).indent()])))
    }

    fn flatten_binary<'b>(
        &self,
        expr: &'b Expr,
        class: OpClass,
        operands: &mut Vec<&'b Expr>,
        ops: &mut Vec<BinaryOp>,
    ) {
        self.flatten_binary_inner(expr, class, operands, ops, true);
    }

    fn flatten_binary_inner<'b>(
        &self,
        expr: &'b Expr,
        class: OpClass,
        operands: &mut Vec<&'b Expr>,
        ops: &mut Vec<BinaryOp>,
        root: bool,
    ) {
        let parenthesized = !root && !self.parentheses.expression_parens(expr.id).is_empty();
        let ExprKind::Binary { op, lhs, rhs } = &expr.kind else {
            operands.push(expr);
            return;
        };
        if parenthesized || op_class(*op) != class {
            operands.push(expr);
            return;
        }
        self.flatten_binary_inner(lhs, class, operands, ops, false);
        ops.push(*op);
        self.flatten_binary_inner(rhs, class, operands, ops, false);
    }

    fn postfix_expr(&mut self, expr: &Expr) -> Doc {
        let mut links = Vec::new();
        let root = self.decompose_postfix(expr, &mut links);
        let first_call = links.iter().position(|link| matches!(link, Link::Call(..)));
        let Some(first_call) = first_call else {
            return self.sequential_postfix(root, &links);
        };
        if first_call + 1 == links.len() {
            return self.sequential_postfix(root, &links);
        }

        let mut head = self.expr(root);
        let mut previous_end = root.span.end;
        for link in &links[..=first_call] {
            let (doc, end) = self.link_doc(link, previous_end, false);
            head = head.append(doc);
            previous_end = end;
        }
        let mut segments: Vec<Doc> = Vec::new();
        let mut current: Option<Doc> = None;
        for link in &links[first_call + 1..] {
            let starts_line = matches!(link, Link::Field(..));
            let (doc, end) = self.link_doc(link, previous_end, starts_line);
            previous_end = end;
            if starts_line {
                if let Some(segment) = current.take() {
                    segments.push(segment);
                }
                current = Some(doc);
            } else if let Some(segment) = current.as_mut() {
                *segment = std::mem::replace(segment, Doc::Nil).append(doc);
            } else {
                head = head.append(doc);
            }
        }
        if let Some(segment) = current {
            segments.push(segment);
        }
        let mut tail = Vec::new();
        for segment in segments {
            tail.push(Doc::SoftLine);
            tail.push(segment);
        }
        let (suffix, _) = self.take_trailing(previous_end);
        Doc::Group(Box::new(Doc::concat([
            head,
            Doc::concat(tail).indent(),
            suffix,
        ])))
    }

    fn sequential_postfix(&mut self, root: &Expr, links: &[Link<'_>]) -> Doc {
        let mut doc = self.expr(root);
        let mut previous_end = root.span.end;
        for link in links {
            let (link_doc, end) = self.link_doc(link, previous_end, false);
            doc = doc.append(link_doc);
            previous_end = end;
        }
        let (suffix, _) = self.take_trailing(previous_end);
        doc.append(suffix)
    }

    fn decompose_postfix<'b>(&self, expr: &'b Expr, out: &mut Vec<Link<'b>>) -> &'b Expr {
        match &expr.kind {
            ExprKind::Field { base, name } => {
                let root = self.decompose_postfix(base, out);
                out.push(Link::Field(name, expr.span));
                root
            }
            ExprKind::Call { callee, args } => {
                let root = self.decompose_postfix(callee, out);
                out.push(Link::Call(args, expr.span));
                root
            }
            ExprKind::Index { base, index } => {
                let root = self.decompose_postfix(base, out);
                out.push(Link::Index(index, expr.span));
                root
            }
            ExprKind::Try(inner) => {
                let root = self.decompose_postfix(inner, out);
                out.push(Link::Try(expr.span));
                root
            }
            _ => expr,
        }
    }

    fn link_doc(&mut self, link: &Link<'_>, previous_end: u32, leading_dot: bool) -> (Doc, u32) {
        match link {
            Link::Field(name, span) => {
                let dot = if leading_dot {
                    self.token_between(previous_end, span.end, ".")
                } else {
                    None
                };
                let dot_span = dot.or_else(|| self.token_between(previous_end, span.end, "."));
                let mut docs = Vec::new();
                if let Some(dot) = dot_span {
                    if self.has_pending_before(dot.start) {
                        let lead = self.leading_before(dot.start);
                        let body = Doc::concat([Doc::HardLine, lead, self.token(".", Some(dot))]);
                        docs.push(if leading_dot { body } else { body.indent() });
                    } else {
                        docs.push(self.token(".", Some(dot)));
                    }
                } else if !leading_dot {
                    docs.push(Doc::text("."));
                }
                docs.push(Doc::text(match name {
                    FieldName::Ident(name) => name.clone(),
                    FieldName::Index(index) => index.to_string(),
                }));
                let (suffix, _) = self.take_trailing(span.end);
                docs.push(suffix);
                (Doc::concat(docs), span.end)
            }
            Link::Call(args, span) => {
                let open = self.token_between(previous_end, span.end, "(");
                let close = open.and_then(|open| self.matching_close(open, "(", ")"));
                let Some(open) = open else {
                    return (Doc::Nil, span.end);
                };
                let _ = leading_dot;
                (self.args_doc(args, open, close), span.end)
            }
            Link::Index(index, span) => {
                let open = self.token_between(previous_end, span.end, "[");
                let close = open.and_then(|open| self.matching_close(open, "[", "]"));
                let mut docs = Vec::new();
                if let Some(open) = open {
                    docs.push(self.open_with_leading("[", open, index.span.start));
                    docs.push(self.expr(index).indent());
                    if let Some(close) = close {
                        if self.has_pending_before(close.start) {
                            docs.push(
                                Doc::concat([
                                    Doc::HardLine,
                                    self.leading_before_closing(close.start),
                                ])
                                .indent(),
                            );
                        }
                        docs.push(self.token("]", Some(close)));
                    }
                }
                (Doc::Group(Box::new(Doc::concat(docs))), span.end)
            }
            Link::Try(span) => {
                let question = self.token_between(previous_end, span.end, "?");
                (self.token("?", question), span.end)
            }
        }
    }

    fn args_doc(&mut self, args: &[Arg], open: Span, close: Option<Span>) -> Doc {
        let items: Vec<(Span, &Arg)> = args.iter().map(|arg| (arg.span, arg)).collect();
        let normal = self.comma_list(
            "(",
            ")",
            Some(open),
            close,
            &items,
            |formatter, arg| formatter.arg_doc(arg),
            false,
            false,
            true,
        );
        let Some((last, earlier)) = args.split_last() else {
            return normal;
        };
        let ExprKind::Lambda {
            params,
            ret,
            body: LambdaBody::Block(block),
        } = &last.expr.kind
        else {
            return normal;
        };
        if earlier
            .iter()
            .any(|arg| matches!(arg.expr.kind, ExprKind::Lambda { .. }))
            || self.arguments_have_comments(open, close, args)
        {
            return normal;
        }
        let Some(close) = close else {
            return normal;
        };
        let lambda_expr = &last.expr;
        let header = self.lambda_header_doc(lambda_expr, params, ret.as_ref(), block.span.start);
        let header_width = header.flat_width();
        let mut hug = vec![self.token("(", Some(open))];
        for arg in earlier {
            hug.push(Doc::Flat(Box::new(self.arg_doc(arg))));
            hug.push(Doc::text(", "));
        }
        let mut lambda = vec![header];
        lambda.push(Doc::text(" "));
        lambda.push(self.block_expanded(block));
        hug.push(Doc::concat(lambda));
        hug.push(self.token(")", Some(close)));
        let hug = Doc::concat(hug);
        let width = earlier
            .iter()
            .try_fold(0usize, |total, arg| {
                Some(total + self.arg_doc(arg).flat_width()? + 2)
            })
            .and_then(|width| Some(width + header_width?));
        match width {
            Some(width) if width <= self.config.max_width => Doc::Choice {
                flat: Box::new(normal),
                broken: Box::new(hug),
            },
            _ => normal,
        }
    }

    fn arguments_have_comments(&self, open: Span, close: Option<Span>, args: &[Arg]) -> bool {
        let end = close
            .map(|span| span.end)
            .or_else(|| args.last().map(|arg| arg.span.end))
            .unwrap_or(open.end);
        self.program
            .comments
            .iter()
            .any(|comment| comment.span.start >= open.end && comment.span.start < end)
    }

    fn arg_doc(&mut self, arg: &Arg) -> Doc {
        let mut docs = Vec::new();
        if arg.mutable {
            docs.push(Doc::text("mut "));
        }
        docs.push(self.expr(&arg.expr));
        Doc::concat(docs)
    }

    fn lambda_header_doc(
        &mut self,
        expr: &Expr,
        params: &[LambdaParam],
        ret: Option<&TyExpr>,
        body_start: u32,
    ) -> Doc {
        let open = self.token_between(expr.span.start, body_start, "(");
        let close = open.and_then(|open| self.matching_close(open, "(", ")"));
        let items: Vec<(Span, &LambdaParam)> =
            params.iter().map(|param| (param.span, param)).collect();
        let mut docs = vec![Doc::text("fn")];
        if let Some(open) = open {
            docs.push(self.comma_list(
                "(",
                ")",
                Some(open),
                close,
                &items,
                |formatter, param| formatter.lambda_param(param),
                false,
                false,
                true,
            ));
        } else {
            docs.push(Doc::text("()"));
        }
        if let Some(ret) = ret {
            let arrow = self.token_between(
                close.map(|close| close.end).unwrap_or(expr.span.start),
                ret.span.start,
                "->",
            );
            docs.push(Doc::text(" "));
            docs.push(self.arrow_type(arrow, ret));
        }
        Doc::concat(docs)
    }

    fn lambda_doc(
        &mut self,
        expr: &Expr,
        params: &[LambdaParam],
        ret: Option<&TyExpr>,
        body: &LambdaBody,
    ) -> Doc {
        let body_start = match body {
            LambdaBody::Block(block) => block.span.start,
            LambdaBody::Expr(expr) => expr.span.start,
        };
        let open = self.token_between(expr.span.start, body_start, "(");
        let close = open.and_then(|open| self.matching_close(open, "(", ")"));
        let items: Vec<(Span, &LambdaParam)> =
            params.iter().map(|param| (param.span, param)).collect();
        let mut docs = vec![Doc::text("fn")];
        if let Some(open) = open {
            docs.push(self.comma_list(
                "(",
                ")",
                Some(open),
                close,
                &items,
                |formatter, param| formatter.lambda_param(param),
                false,
                false,
                true,
            ));
        } else {
            docs.push(Doc::text("()"));
        }
        if let Some(ret) = ret {
            let arrow = self.token_between(
                close.map(|close| close.end).unwrap_or(expr.span.start),
                ret.span.start,
                "->",
            );
            docs.push(Doc::text(" "));
            docs.push(self.arrow_type(arrow, ret));
        }
        match body {
            LambdaBody::Block(block) => {
                docs.push(Doc::text(" "));
                docs.push(self.block(block));
            }
            LambdaBody::Expr(expr) => {
                let arrow = self.token_between(
                    ret.map(|ret| ret.span.end)
                        .unwrap_or_else(|| close.map(|close| close.end).unwrap_or(expr.span.start)),
                    expr.span.start,
                    "=>",
                );
                docs.push(Doc::text(" "));
                docs.push(self.token("=>", arrow));
                docs.push(Doc::Group(Box::new(
                    Doc::concat([Doc::Line, self.expr(expr)]).indent(),
                )));
            }
        }
        Doc::concat(docs)
    }

    fn lambda_param(&mut self, param: &LambdaParam) -> Doc {
        let mut docs = vec![self.pattern(&param.pattern)];
        if let Some(ty) = &param.ty {
            let colon = self.token_between(param.pattern.span.end, ty.span.start, ":");
            docs.push(Doc::Group(Box::new(Doc::concat([
                self.token(":", colon),
                Doc::concat([Doc::Line, self.ty(ty)]).indent(),
            ]))));
        }
        Doc::concat(docs)
    }

    fn if_doc(&mut self, expr: &Expr) -> Doc {
        let ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } = &expr.kind
        else {
            unreachable!()
        };
        let can_flat = self.if_can_flat(expr);
        if !can_flat {
            return self.if_broken(cond, then_branch, else_branch.as_deref());
        }
        let flat = self.if_flat(cond, then_branch, else_branch.as_deref());
        let broken = self.if_broken(cond, then_branch, else_branch.as_deref());
        Doc::Choice {
            flat: Box::new(flat),
            broken: Box::new(broken),
        }
    }

    fn if_can_flat(&self, expr: &Expr) -> bool {
        if !self
            .program
            .comments
            .iter()
            .any(|comment| expr.span.contains(comment.span))
        {
            let ExprKind::If {
                then_branch,
                else_branch,
                ..
            } = &expr.kind
            else {
                return false;
            };
            if !self.block_can_one_line(then_branch) {
                return false;
            }
            match else_branch.as_deref() {
                None => true,
                Some(Expr {
                    kind: ExprKind::Block(block),
                    ..
                }) => self.block_can_one_line(block),
                Some(next) if matches!(next.kind, ExprKind::If { .. }) => self.if_can_flat(next),
                _ => false,
            }
        } else {
            false
        }
    }

    fn if_flat(&mut self, cond: &Expr, then_branch: &Block, else_branch: Option<&Expr>) -> Doc {
        let mut docs = vec![
            Doc::text("if "),
            self.expr(cond),
            Doc::text(" "),
            self.flat_branch(then_branch),
        ];
        if let Some(else_branch) = else_branch {
            docs.push(Doc::text(" else "));
            match &else_branch.kind {
                ExprKind::Block(block) => docs.push(self.flat_branch(block)),
                ExprKind::If { .. } => docs.push(self.if_flat_from_expr(else_branch)),
                _ => {}
            }
        }
        Doc::concat(docs)
    }

    fn flat_branch(&mut self, block: &Block) -> Doc {
        if block.stmts.is_empty() {
            self.block(block)
        } else {
            self.block_one_line(block)
        }
    }

    fn if_flat_from_expr(&mut self, expr: &Expr) -> Doc {
        let ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } = &expr.kind
        else {
            return Doc::Nil;
        };
        self.if_flat(cond, then_branch, else_branch.as_deref())
    }

    fn if_broken(&mut self, cond: &Expr, then_branch: &Block, else_branch: Option<&Expr>) -> Doc {
        let mut docs = vec![
            Doc::text("if "),
            self.expr(cond),
            Doc::text(" "),
            self.block_expanded(then_branch),
        ];
        if let Some(else_branch) = else_branch {
            let has_comment = self.program.comments.iter().any(|comment| {
                comment.span.start >= then_branch.span.end
                    && comment.span.start < else_branch.span.start
            });
            if has_comment {
                docs.push(Doc::HardLine);
                if self.has_pending_before(else_branch.span.start) {
                    docs.push(self.leading_before(else_branch.span.start));
                }
            } else {
                docs.push(Doc::text(" "));
            }
            docs.push(Doc::text("else "));
            match &else_branch.kind {
                ExprKind::Block(block) => docs.push(self.block_expanded(block)),
                ExprKind::If { .. } => docs.push(self.if_broken_from_expr(else_branch)),
                _ => {}
            }
        }
        Doc::concat(docs)
    }

    fn if_broken_from_expr(&mut self, expr: &Expr) -> Doc {
        let ExprKind::If {
            cond,
            then_branch,
            else_branch,
        } = &expr.kind
        else {
            return Doc::Nil;
        };
        self.if_broken(cond, then_branch, else_branch.as_deref())
    }

    fn match_doc(&mut self, expr: &Expr, scrutinee: &Expr, arms: &[MatchArm]) -> Doc {
        let open = self.token_between(scrutinee.span.end, expr.span.end, "{");
        let close = open.and_then(|open| self.matching_close(open, "{", "}"));
        let Some(open) = open else {
            return Doc::concat([Doc::text("match "), self.expr(scrutinee)]);
        };
        let close = close.unwrap_or_else(|| Span::empty(expr.span.end));
        let open_doc = self.token("{", Some(open));
        let mut docs = vec![
            Doc::text("match "),
            self.expr(scrutinee),
            Doc::text(" "),
            open_doc,
        ];
        let mut arm_docs = Vec::new();
        for (index, arm) in arms.iter().enumerate() {
            if !arm_docs.is_empty() {
                arm_docs.push(Doc::HardLine);
            }
            let limit = arms
                .get(index + 1)
                .map(|next| next.span.start)
                .unwrap_or(close.start);
            arm_docs.push(self.match_arm(arm, limit));
        }
        if self.has_pending_before(close.start) {
            if !arm_docs.is_empty() {
                arm_docs.push(Doc::HardLine);
            }
            arm_docs.push(self.leading_before_closing(close.start));
        }
        docs.push(Doc::concat([Doc::HardLine, Doc::concat(arm_docs)]).indent());
        docs.push(Doc::HardLine);
        docs.push(self.token("}", Some(close)));
        Doc::concat(docs)
    }

    fn match_arm(&mut self, arm: &MatchArm, end_limit: u32) -> Doc {
        let lead = self.leading_before(arm.span.start);
        let pattern = self.pattern(&arm.pattern);
        let mut head = vec![pattern];
        if let Some(guard) = &arm.guard {
            let if_span = self.token_between(arm.pattern.span.end, guard.span.start, "if");
            head.push(Doc::text(" "));
            head.push(self.token("if", if_span));
            head.push(Doc::text(" "));
            head.push(self.expr(guard));
        }
        let body_start = match &arm.body {
            MatchBody::Block(block) => block.span.start,
            MatchBody::Expr(expr) => expr.span.start,
        };
        let head_end = arm
            .guard
            .as_ref()
            .map(|guard| guard.span.end)
            .unwrap_or(arm.pattern.span.end);
        let arrow = self.token_between(head_end, body_start, "=>");
        match &arm.body {
            MatchBody::Block(block) => {
                head.push(Doc::text(" "));
                head.push(self.token("=>", arrow));
                head.push(Doc::text(" "));
                head.push(self.block(block));
                let comma = self.token_between(block.span.end, end_limit, ",");
                if let Some(comma) = comma {
                    head.push(self.take_trailing(comma.end).0);
                }
                lead.append(Doc::concat(head))
            }
            MatchBody::Expr(body) => {
                head.push(Doc::text(" "));
                head.push(self.token("=>", arrow));
                head.push(Doc::concat([Doc::Line, self.expr(body)]).indent());
                let group = Doc::Group(Box::new(Doc::concat(head)));
                let comma = self.token_between(body.span.end, end_limit, ",");
                let out = match comma {
                    Some(span) => {
                        let suffix = self.take_trailing(span.end).0;
                        Doc::concat([group, Doc::text(","), suffix])
                    }
                    None => Doc::concat([group, Doc::text(",")]),
                };
                lead.append(out)
            }
        }
    }

    fn block_expanded(&mut self, block: &Block) -> Doc {
        let open = self
            .token_between(block.span.start, block.span.end, "{")
            .unwrap_or_else(|| Span::empty(block.span.start));
        let close = self
            .last_token_where(open.end, block.span.end, |kind| {
                matches!(kind, TokenKind::RBrace)
            })
            .unwrap_or_else(|| Span::empty(block.span.end));
        if block.stmts.is_empty() {
            return self.block(block);
        }
        let open_doc = self.token("{", Some(open));
        let mut item_docs = Vec::new();
        let mut previous: Option<u32> = None;
        for stmt in &block.stmts {
            let content_start = self.content_start_before(stmt.span.start);
            let lead = self.leading_before(stmt.span.start);
            let body = self.stmt(stmt);
            let (suffix, _) = self.take_trailing(stmt.span.end);
            if let Some(previous_end) = previous {
                item_docs.push(if self.has_blank_line(previous_end, content_start) {
                    Doc::BlankLine
                } else {
                    Doc::HardLine
                });
            }
            item_docs.push(lead.append(body).append(suffix));
            previous = Some(stmt.span.end);
        }
        let mut docs = vec![open_doc];
        docs.push(Doc::concat([Doc::HardLine, Doc::concat(item_docs)]).indent());
        if self.has_pending_before(close.start) {
            docs.push(
                Doc::concat([Doc::HardLine, self.leading_before_closing(close.start)]).indent(),
            );
        }
        docs.push(Doc::HardLine);
        docs.push(self.token("}", Some(close)));
        Doc::concat(docs)
    }

    // ----- blocks and statements -----------------------------------------

    fn block(&mut self, block: &Block) -> Doc {
        let open = self
            .token_between(block.span.start, block.span.end, "{")
            .unwrap_or_else(|| Span::empty(block.span.start));
        let close = self
            .last_token_where(open.end, block.span.end, |kind| {
                matches!(kind, TokenKind::RBrace)
            })
            .unwrap_or_else(|| Span::empty(block.span.end));
        if block.stmts.is_empty() {
            let (suffix, had_trailing) = self.take_trailing(open.end);
            let open_doc = Doc::text("{").append(suffix);
            if !had_trailing && !self.has_pending_before(close.start) {
                return Doc::concat([open_doc, self.token("}", Some(close))]);
            }
            let leading = self.leading_before_closing(close.start);
            return Doc::concat([
                open_doc,
                Doc::concat([Doc::HardLine, leading]).indent(),
                Doc::HardLine,
                self.token("}", Some(close)),
            ]);
        }

        let can_one_line = self.block_can_one_line(block);
        if can_one_line {
            return self.block_one_line(block);
        }
        let open_doc = self.token("{", Some(open));
        let mut item_docs = Vec::new();
        let mut previous: Option<(u32, bool)> = None;
        for stmt in &block.stmts {
            let content_start = self.content_start_before(stmt.span.start);
            let lead = self.leading_before(stmt.span.start);
            let body = self.stmt(stmt);
            let (suffix, _) = self.take_trailing(stmt.span.end);
            if let Some((previous_end, _)) = previous {
                item_docs.push(if self.has_blank_line(previous_end, content_start) {
                    Doc::BlankLine
                } else {
                    Doc::HardLine
                });
            }
            item_docs.push(lead.append(body).append(suffix));
            previous = Some((stmt.span.end, false));
        }
        let mut docs = vec![open_doc];
        docs.push(Doc::concat([Doc::HardLine, Doc::concat(item_docs)]).indent());
        if self.has_pending_before(close.start) {
            docs.push(
                Doc::concat([Doc::HardLine, self.leading_before_closing(close.start)]).indent(),
            );
        }
        docs.push(Doc::HardLine);
        docs.push(self.token("}", Some(close)));
        let expanded = Doc::concat(docs);
        expanded
    }

    fn block_one_line(&mut self, block: &Block) -> Doc {
        let open = self
            .token_between(block.span.start, block.span.end, "{")
            .unwrap_or_else(|| Span::empty(block.span.start));
        let close = self
            .last_token_where(open.end, block.span.end, |kind| {
                matches!(kind, TokenKind::RBrace)
            })
            .unwrap_or_else(|| Span::empty(block.span.end));
        let stmt = &block.stmts[0];
        let lead = self.leading_before(stmt.span.start);
        let body = self.stmt(stmt);
        let (suffix, _) = self.take_trailing(stmt.span.end);
        let (close_suffix, _) = self.take_trailing(close.end);
        let block = Doc::Group(Box::new(Doc::concat([
            self.token("{", Some(open)),
            Doc::concat([Doc::Line, lead.append(body).append(suffix)]).indent(),
            Doc::Line,
            Doc::text("}"),
        ])));
        block.append(close_suffix)
    }

    fn block_can_one_line(&self, block: &Block) -> bool {
        if block.stmts.is_empty() {
            return !self
                .program
                .comments
                .iter()
                .any(|comment| block.span.contains(comment.span));
        }
        if block.stmts.len() != 1 {
            return false;
        }
        let stmt = &block.stmts[0];
        if !matches!(
            stmt.kind,
            StmtKind::Expr { .. } | StmtKind::Return(_) | StmtKind::Break | StmtKind::Continue
        ) {
            return false;
        }
        if self
            .program
            .comments
            .iter()
            .any(|comment| block.span.contains(comment.span))
        {
            return false;
        }
        !stmt_contains_nested_block(stmt)
    }

    fn stmt(&mut self, stmt: &Stmt) -> Doc {
        match &stmt.kind {
            StmtKind::Let {
                mutable,
                pattern,
                ty,
                value,
            } => {
                let mut docs = vec![Doc::text(if *mutable { "var " } else { "let " })];
                docs.push(self.pattern(pattern));
                if let Some(ty) = ty {
                    let colon = self.token_between(pattern.span.end, ty.span.start, ":");
                    docs.push(Doc::Group(Box::new(Doc::concat([
                        self.token(":", colon),
                        Doc::concat([Doc::Line, self.ty(ty)]).indent(),
                    ]))));
                }
                let eq_start = ty
                    .as_ref()
                    .map(|ty| ty.span.end)
                    .unwrap_or(pattern.span.end);
                let eq = self.token_between(eq_start, value.span.start, "=");
                docs.push(Doc::text(" "));
                docs.push(self.token("=", eq));
                docs.push(self.assignment_value(value, eq_start, eq));
                Doc::concat(docs)
            }
            StmtKind::While { cond, body } => {
                let mut docs = vec![Doc::text("while "), self.expr(cond), Doc::text(" ")];
                docs.push(self.block_expanded(body));
                Doc::concat(docs)
            }
            StmtKind::For {
                pattern,
                iter,
                body,
            } => {
                let mut docs = vec![Doc::text("for "), self.pattern(pattern), Doc::text(" in ")];
                docs.push(self.expr(iter));
                docs.push(Doc::text(" "));
                docs.push(self.block_expanded(body));
                Doc::concat(docs)
            }
            StmtKind::Return(value) => {
                let mut docs = vec![Doc::text("return")];
                if let Some(value) = value {
                    docs.push(Doc::text(" "));
                    docs.push(self.expr(value));
                }
                Doc::concat(docs)
            }
            StmtKind::Break => Doc::text("break"),
            StmtKind::Continue => Doc::text("continue"),
            StmtKind::Expr { expr, assign } => {
                let mut docs = vec![self.expr(expr)];
                if let Some((op, value)) = assign {
                    let op_text = assign_text(*op);
                    let op_span = self.token_between(expr.span.end, value.span.start, op_text);
                    docs.push(Doc::text(" "));
                    docs.push(self.token(op_text, op_span));
                    docs.push(self.assignment_value(value, expr.span.end, op_span));
                }
                Doc::concat(docs)
            }
        }
    }

    fn assignment_value(&mut self, value: &Expr, _search_start: u32, _eq: Option<Span>) -> Doc {
        let bracketed = self.is_bracketed(value);
        if bracketed {
            Doc::concat([Doc::text(" "), self.expr(value)])
        } else {
            Doc::Group(Box::new(Doc::concat([Doc::Indent(Box::new(Doc::concat(
                [Doc::Line, self.expr(value)],
            )))])))
        }
    }

    fn is_bracketed(&self, expr: &Expr) -> bool {
        if !self.parentheses.expression_parens(expr.id).is_empty() {
            return true;
        }
        matches!(
            expr.kind,
            ExprKind::Tuple(_)
                | ExprKind::List(_)
                | ExprKind::Map(_)
                | ExprKind::StructLit { .. }
                | ExprKind::Call { .. }
                | ExprKind::Index { .. }
                | ExprKind::Convert { .. }
                | ExprKind::Block(_)
                | ExprKind::If { .. }
                | ExprKind::Match { .. }
                | ExprKind::Lambda { .. }
        )
    }
}

fn escape_string(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

fn is_allow(comment: &Comment) -> bool {
    !comment.doc && comment.text.trim_start().starts_with("allow")
}

fn assign_text(op: AssignOp) -> &'static str {
    match op {
        AssignOp::Assign => "=",
        AssignOp::Add => "+=",
        AssignOp::Sub => "-=",
        AssignOp::Mul => "*=",
        AssignOp::Div => "/=",
        AssignOp::Rem => "%=",
    }
}

fn stmt_contains_nested_block(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Let { value, .. } => expr_contains_nested(value),
        StmtKind::Expr { expr, assign } => {
            expr_contains_nested(expr)
                || assign
                    .as_ref()
                    .is_some_and(|(_, value)| expr_contains_nested(value))
        }
        StmtKind::Return(Some(expr)) => expr_contains_nested(expr),
        StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue => false,
        StmtKind::While { .. } | StmtKind::For { .. } => true,
    }
}

fn expr_contains_nested(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Block(_) | ExprKind::Match { .. } => true,
        ExprKind::If { .. } => true,
        ExprKind::Tuple(items) | ExprKind::List(items) => items.iter().any(expr_contains_nested),
        ExprKind::Map(entries) => entries
            .iter()
            .any(|(key, value)| expr_contains_nested(key) || expr_contains_nested(value)),
        ExprKind::StructLit { fields, base, .. } => {
            fields
                .iter()
                .any(|field| field.value.as_ref().is_some_and(expr_contains_nested))
                || base.as_deref().is_some_and(expr_contains_nested)
        }
        ExprKind::Unary { expr, .. } | ExprKind::Try(expr) | ExprKind::Convert { expr, .. } => {
            expr_contains_nested(expr)
        }
        ExprKind::Binary { lhs, rhs, .. } => expr_contains_nested(lhs) || expr_contains_nested(rhs),
        ExprKind::Range { start, end, .. } => {
            expr_contains_nested(start) || expr_contains_nested(end)
        }
        ExprKind::Lambda { body, .. } => match body {
            LambdaBody::Block(_) => true,
            LambdaBody::Expr(expr) => expr_contains_nested(expr),
        },
        ExprKind::Field { base, .. } => expr_contains_nested(base),
        ExprKind::Call { callee, args } => {
            expr_contains_nested(callee) || args.iter().any(|arg| expr_contains_nested(&arg.expr))
        }
        ExprKind::Index { base, index } => {
            expr_contains_nested(base) || expr_contains_nested(index)
        }
        _ => false,
    }
}
