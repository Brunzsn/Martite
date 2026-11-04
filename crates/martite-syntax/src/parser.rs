use std::mem::discriminant;
use std::sync::atomic::{AtomicU32, Ordering};

use martite_base::{Code, Diagnostic, LoadLimits, Span};

use crate::ast::*;
use crate::lexer::{lex, lex_fragment, Lexed};
use crate::token::{IdentClass, StrPart, Token, TokenKind, TypeName};

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub items: Vec<Item>,
    pub is_interface: bool,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct ParseOutput {
    pub program: Program,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn parse_source(source: &str, limits: LoadLimits) -> ParseOutput {
    Parser::new(source, limits, false).parse_program()
}

pub fn parse_interface(source: &str, limits: LoadLimits) -> ParseOutput {
    Parser::new(source, limits, true).parse_program()
}

type PResult<T> = Result<T, ()>;

struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    idx: usize,
    limits: LoadLimits,
    diagnostics: Vec<Diagnostic>,
    ignore_newlines: bool,
    allow_struct_lit: bool,
    interface: bool,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn new(source: &'a str, limits: LoadLimits, interface: bool) -> Self {
        let lexed = if interface {
            lex(source, limits)
        } else {
            lex(source, limits)
        };
        Self::with_lexed(source, lexed, limits, interface)
    }

    fn with_lexed(
        source: &'a str,
        lexed: Lexed,
        limits: LoadLimits,
        interface: bool,
    ) -> Self {
        Self {
            source,
            tokens: lexed.tokens,
            idx: 0,
            limits,
            diagnostics: lexed.diagnostics,
            ignore_newlines: !interface,
            allow_struct_lit: true,
            interface,
            depth: 0,
        }
    }

    fn parse_program(mut self) -> ParseOutput {
        let start = self.span();
        let mut items = Vec::new();
        loop {
            self.skip_all_newlines();
            if self.at(&TokenKind::Eof) {
                break;
            }
            match self.parse_item() {
                Ok(item) => {
                    items.push(item);
                    self.statement_end();
                }
                Err(()) => self.recover_item(),
            }
        }
        let end = self.span();
        ParseOutput {
            program: Program {
                items,
                is_interface: self.interface,
                span: start.merge(end),
            },
            diagnostics: self.diagnostics,
        }
    }

    // ----- token helpers -------------------------------------------------

    fn peek_token(&mut self) -> &Token {
        if self.ignore_newlines {
            while matches!(self.tokens[self.idx].kind, TokenKind::Newline) {
                self.idx += 1;
            }
        }
        &self.tokens[self.idx]
    }

    fn peek_kind(&mut self) -> &TokenKind {
        &self.peek_token().kind
    }

    fn raw_kind(&self) -> &TokenKind {
        &self.tokens[self.idx].kind
    }

    fn advance(&mut self) -> Token {
        self.peek_token();
        let token = self.tokens[self.idx].clone();
        if self.idx + 1 < self.tokens.len() {
            self.idx += 1;
        }
        token
    }

    fn span(&self) -> Span {
        self.tokens
            .get(self.idx)
            .map(|t| t.span)
            .unwrap_or_else(|| Span::empty(self.source.len() as u32))
    }

    fn at(&mut self, kind: &TokenKind) -> bool {
        discriminant(self.peek_kind()) == discriminant(kind)
    }

    fn eat(&mut self, kind: &TokenKind) -> bool {
        if self.at(kind) {
            self.advance();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, kind: &TokenKind) -> PResult<Token> {
        if self.at(kind) {
            Ok(self.advance())
        } else {
            let found = self.peek_kind().to_string();
            Err(self.error_at_current(format!("expected {kind}, found {found}")))
        }
    }

    fn expect_ident(&mut self) -> PResult<(String, IdentClass, Span)> {
        let token = self.peek_token().clone();
        match token.kind {
            TokenKind::Ident(name, class) => {
                self.advance();
                Ok((name, class, token.span))
            }
            _ => Err(self.error_at_current(format!(
                "expected an identifier, found {}",
                token.kind
            ))),
        }
    }

    fn expect_snake(&mut self) -> PResult<(String, Span)> {
        let (name, class, span) = self.expect_ident()?;
        if class != IdentClass::Snake {
            self.error(Code::E104, span, format!("`{name}` must be snake_case"));
        }
        Ok((name, span))
    }

    fn expect_pascal(&mut self) -> PResult<(String, Span)> {
        let (name, class, span) = self.expect_ident()?;
        if class != IdentClass::Pascal {
            self.error(Code::E104, span, format!("`{name}` must be PascalCase"));
        }
        Ok((name, span))
    }

    fn expect_screaming(&mut self) -> PResult<(String, Span)> {
        let (name, class, span) = self.expect_ident()?;
        if class != IdentClass::Screaming {
            self.error(
                Code::E104,
                span,
                format!("`{name}` must be SCREAMING_CASE"),
            );
        }
        Ok((name, span))
    }

    fn error(&mut self, code: Code, span: Span, message: impl Into<String>) {
        let mut d = Diagnostic::error(code, message);
        d.span = Some(span);
        self.diagnostics.push(d);
    }

    fn error_at_current(&mut self, message: impl Into<String>) -> () {
        let span = self.span();
        self.error(Code::E003, span, message);
    }

    fn id(&mut self) -> NodeId {
        static NEXT_NODE_ID: AtomicU32 = AtomicU32::new(1);
        NodeId(NEXT_NODE_ID.fetch_add(1, Ordering::Relaxed))
    }

    fn enter_depth(&mut self) -> PResult<()> {
        if self.depth >= self.limits.nesting_depth {
            let span = self.span();
            self.error(
                Code::E002,
                span,
                format!(
                    "syntactic nesting exceeds the limit of {}",
                    self.limits.nesting_depth
                ),
            );
            return Err(());
        }
        self.depth += 1;
        Ok(())
    }

    fn skip_all_newlines(&mut self) {
        while matches!(self.raw_kind(), TokenKind::Newline) {
            self.idx += 1;
        }
    }

    fn skip_continuation_newlines(&mut self) {
        if self.ignore_newlines {
            self.skip_all_newlines();
            return;
        }
        loop {
            if !matches!(self.raw_kind(), TokenKind::Newline) {
                return;
            }
            let prev = self.prev_significant();
            let next = self.next_significant(self.idx);
            let prev_ok = prev.as_ref().is_some_and(is_continuation_prev);
            let next_ok = next.as_ref().is_some_and(is_continuation_next);
            if prev_ok || next_ok {
                self.skip_all_newlines();
            } else {
                return;
            }
        }
    }

    fn prev_significant(&self) -> Option<TokenKind> {
        self.tokens[..self.idx]
            .iter()
            .rev()
            .find(|t| !matches!(t.kind, TokenKind::Newline))
            .map(|t| t.kind.clone())
    }

    fn next_significant(&self, from: usize) -> Option<TokenKind> {
        self.tokens[from..]
            .iter()
            .find(|t| !matches!(t.kind, TokenKind::Newline))
            .map(|t| t.kind.clone())
    }

    fn lookahead_after_newlines(&self, kind: &TokenKind) -> bool {
        self.tokens[self.idx..]
            .iter()
            .find(|t| !matches!(t.kind, TokenKind::Newline))
            .is_some_and(|t| discriminant(&t.kind) == discriminant(kind))
    }

    fn statement_end(&mut self) {
        if self.at(&TokenKind::RBrace) || self.at(&TokenKind::Eof) {
            return;
        }
        if self.ignore_newlines {
            return;
        }
        if matches!(self.raw_kind(), TokenKind::Newline) {
            self.skip_all_newlines();
            return;
        }
        let span = self.span();
        self.error(
            Code::E003,
            span,
            "expected a newline or end of block after this statement",
        );
        self.recover_item();
    }

    fn recover_item(&mut self) {
        let mut depth = 0i32;
        while !matches!(self.raw_kind(), TokenKind::Eof) {
            match self.raw_kind() {
                TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket => depth += 1,
                TokenKind::RBrace | TokenKind::RParen | TokenKind::RBracket => {
                    if depth > 0 {
                        depth -= 1;
                    }
                }
                TokenKind::Newline if depth <= 0 => {
                    self.idx += 1;
                    self.skip_all_newlines();
                    return;
                }
                _ => {}
            }
            self.idx += 1;
        }
    }

    fn with_newlines<T>(
        &mut self,
        ignore: bool,
        f: impl FnOnce(&mut Self) -> PResult<T>,
    ) -> PResult<T> {
        let saved = self.ignore_newlines;
        self.ignore_newlines = ignore;
        let result = f(self);
        self.ignore_newlines = saved;
        result
    }

    fn with_struct_lit<T>(&mut self, allow: bool, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        let saved = self.allow_struct_lit;
        self.allow_struct_lit = allow;
        let result = f(self);
        self.allow_struct_lit = saved;
        result
    }

    // ----- items ---------------------------------------------------------

    fn parse_item(&mut self) -> PResult<Item> {
        self.skip_all_newlines();
        let start = self.span();
        if self.at(&TokenKind::Use) {
            return self.parse_use().map(Item::Use);
        }
        if self.at(&TokenKind::Test) {
            if self.interface {
                return Err(self.error_at_current("test blocks are not allowed in interface files"));
            }
            return self.parse_test().map(Item::Test);
        }
        if self.at(&TokenKind::On) {
            return self.parse_on().map(Item::On);
        }
        if self.at(&TokenKind::Impl) {
            if self.interface {
                return Err(self.error_at_current("impl blocks are not allowed in interface files"));
            }
            return self.parse_impl().map(Item::Impl);
        }
        let is_pub = self.eat(&TokenKind::Pub);
        if self.at(&TokenKind::Fn) {
            return self.parse_fn(is_pub, false).map(Item::Fn);
        }
        if self.at(&TokenKind::Struct) {
            return self.parse_struct(is_pub).map(Item::Struct);
        }
        if self.at(&TokenKind::Enum) {
            return self.parse_enum(is_pub).map(Item::Enum);
        }
        if self.at(&TokenKind::Const) {
            return self.parse_const(is_pub).map(Item::Const);
        }
        let span = start.merge(self.span());
        self.error(Code::E003, span, "expected an item");
        Err(())
    }

    fn parse_use(&mut self) -> PResult<UseDecl> {
        let start = self.expect(&TokenKind::Use)?.span;
        let mut path = vec![self.expect_snake()?.0];
        while self.eat(&TokenKind::Dot) {
            let (name, _, _) = self.expect_ident()?;
            path.push(name);
        }
        let alias = if self.eat(&TokenKind::As) {
            let (name, _, span) = self.expect_ident()?;
            let _ = span;
            Some(name)
        } else {
            None
        };
        let span = start.merge(self.span_before());
        Ok(UseDecl {
            id: self.id(),
            span,
            path,
            alias,
        })
    }

    fn parse_test(&mut self) -> PResult<TestDecl> {
        let start = self.expect(&TokenKind::Test)?.span;
        let token = self.peek_token().clone();
        let name = match token.kind {
            TokenKind::Str(lit) => {
                self.advance();
                let mut text = String::new();
                for part in &lit.parts {
                    match part {
                        StrPart::Text(t) => text.push_str(t),
                        StrPart::Expr(_) => {
                            return Err(self.error_at(
                                Code::E003,
                                token.span,
                                "test names may not contain interpolation",
                            ))
                        }
                    }
                }
                text
            }
            _ => {
                return Err(self.error_at_current("expected a test name string"));
            }
        };
        let body = self.parse_block()?;
        let span = start.merge(body.span);
        Ok(TestDecl {
            id: self.id(),
            span,
            name,
            body,
        })
    }

    fn parse_on(&mut self) -> PResult<OnDecl> {
        let start = self.expect(&TokenKind::On)?.span;
        let (name, _) = self.expect_snake()?;
        let params = self.parse_params(true)?;
        let ret = self.parse_optional_ret()?;
        let body = if self.interface {
            self.end_interface_signature()?;
            Block {
                id: self.id(),
                span: self.span_before(),
                stmts: Vec::new(),
            }
        } else {
            self.parse_block()?
        };
        let span = start.merge(body.span);
        Ok(OnDecl {
            id: self.id(),
            span,
            name,
            params,
            ret,
            body,
        })
    }

    fn parse_impl(&mut self) -> PResult<ImplDecl> {
        let start = self.expect(&TokenKind::Impl)?.span;
        let generics = self.parse_generics()?;
        let (type_name, _) = self.expect_pascal()?;
        let type_args = self.parse_optional_type_args()?;
        let mut methods = Vec::new();
        self.expect(&TokenKind::LBrace)?;
        let body = self.with_newlines(true, |p| {
            while !p.at(&TokenKind::RBrace) && !p.at(&TokenKind::Eof) {
                let is_pub = p.eat(&TokenKind::Pub);
                let method = p.parse_fn(is_pub, true)?;
                methods.push(method);
            }
            Ok(())
        });
        if let Err(()) = body {
            return Err(());
        }
        self.expect(&TokenKind::RBrace)?;
        let span = start.merge(self.span_before());
        Ok(ImplDecl {
            id: self.id(),
            span,
            generics,
            type_name,
            type_args,
            methods,
        })
    }

    fn parse_fn(&mut self, is_pub: bool, allow_self: bool) -> PResult<FnDecl> {
        let start = self.expect(&TokenKind::Fn)?.span;
        let (name, _) = self.expect_snake()?;
        let generics = self.parse_generics()?;
        let params = self.parse_params(allow_self)?;
        let ret = self.parse_optional_ret()?;
        let body = if self.interface {
            self.end_interface_signature()?;
            None
        } else {
            Some(self.parse_block()?)
        };
        let end = body.as_ref().map(|b| b.span).unwrap_or_else(|| self.span_before());
        let span = start.merge(end);
        Ok(FnDecl {
            id: self.id(),
            span,
            is_pub,
            name,
            generics,
            params,
            ret,
            body,
        })
    }

    fn parse_params(&mut self, allow_self: bool) -> PResult<Vec<Param>> {
        self.expect(&TokenKind::LParen)?;
        let mut params = Vec::new();
        self.with_newlines(true, |p| {
            let mut first = true;
            while !p.at(&TokenKind::RParen) && !p.at(&TokenKind::Eof) {
                let start = p.span();
                let mutable = p.eat(&TokenKind::Mut);
                if p.eat(&TokenKind::SelfKw) {
                    if !allow_self || !first || p.interface {
                        return Err(p.error_at(
                            Code::E003,
                            start.merge(p.span_before()),
                            "`self` may only be the first parameter of a method or handler",
                        ));
                    }
                    params.push(Param {
                        id: p.id(),
                        span: start.merge(p.span_before()),
                        kind: ParamKind::SelfParam { mutable },
                    });
                } else {
                    let (name, name_span) = p.expect_snake()?;
                    p.expect(&TokenKind::Colon)?;
                    let ty = p.parse_type()?;
                    params.push(Param {
                        id: p.id(),
                        span: start.merge(ty.span),
                        kind: ParamKind::Normal { name, mutable, ty },
                    });
                    let _ = name_span;
                }
                first = false;
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
            }
            Ok(())
        })?;
        self.expect(&TokenKind::RParen)?;
        Ok(params)
    }

    fn parse_optional_ret(&mut self) -> PResult<Option<TyExpr>> {
        if self.eat(&TokenKind::Arrow) {
            Ok(Some(self.parse_type()?))
        } else {
            Ok(None)
        }
    }

    fn parse_struct(&mut self, is_pub: bool) -> PResult<StructDecl> {
        let start = self.expect(&TokenKind::Struct)?.span;
        let (name, _) = self.expect_pascal()?;
        let generics = self.parse_generics()?;
        let mut fields = Vec::new();
        let mut opaque = false;
        if self.at(&TokenKind::LBrace) {
            self.expect(&TokenKind::LBrace)?;
            self.with_newlines(true, |p| {
                while !p.at(&TokenKind::RBrace) && !p.at(&TokenKind::Eof) {
                    let field_start = p.span();
                    let (field_name, _) = p.expect_snake()?;
                    p.expect(&TokenKind::Colon)?;
                    let ty = p.parse_type()?;
                    fields.push(Field {
                        id: p.id(),
                        span: field_start.merge(ty.span),
                        name: field_name,
                        ty,
                    });
                    if !p.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                Ok(())
            })?;
            self.expect(&TokenKind::RBrace)?;
        } else if self.interface {
            opaque = true;
            self.end_interface_signature()?;
        } else {
            return Err(self.error_at_current("expected `{` after struct name"));
        }
        let span = start.merge(self.span_before());
        Ok(StructDecl {
            id: self.id(),
            span,
            is_pub,
            name,
            generics,
            fields,
            opaque,
        })
    }

    fn parse_enum(&mut self, is_pub: bool) -> PResult<EnumDecl> {
        let start = self.expect(&TokenKind::Enum)?.span;
        let (name, _) = self.expect_pascal()?;
        let generics = self.parse_generics()?;
        self.expect(&TokenKind::LBrace)?;
        let mut variants = Vec::new();
        self.with_newlines(true, |p| {
            while !p.at(&TokenKind::RBrace) && !p.at(&TokenKind::Eof) {
                let (variant_name, variant_span) = p.expect_pascal()?;
                let mut payload = Vec::new();
                if p.eat(&TokenKind::LParen) {
                    p.with_newlines(true, |p| {
                        while !p.at(&TokenKind::RParen) && !p.at(&TokenKind::Eof) {
                            payload.push(p.parse_type()?);
                            if !p.eat(&TokenKind::Comma) {
                                break;
                            }
                        }
                        Ok(())
                    })?;
                    p.expect(&TokenKind::RParen)?;
                }
                variants.push(Variant {
                    id: p.id(),
                    span: variant_span.merge(p.span_before()),
                    name: variant_name,
                    payload,
                });
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
            }
            Ok(())
        })?;
        self.expect(&TokenKind::RBrace)?;
        let span = start.merge(self.span_before());
        Ok(EnumDecl {
            id: self.id(),
            span,
            is_pub,
            name,
            generics,
            variants,
        })
    }

    fn parse_const(&mut self, is_pub: bool) -> PResult<ConstDecl> {
        let start = self.expect(&TokenKind::Const)?.span;
        let (name, _) = self.expect_screaming()?;
        self.expect(&TokenKind::Colon)?;
        let ty = self.parse_type()?;
        self.expect(&TokenKind::Eq)?;
        let value = self.parse_expr()?;
        let span = start.merge(value.span);
        Ok(ConstDecl {
            id: self.id(),
            span,
            is_pub,
            name,
            ty,
            value,
        })
    }

    fn parse_generics(&mut self) -> PResult<Vec<GenericParam>> {
        if !self.eat(&TokenKind::LBracket) {
            return Ok(Vec::new());
        }
        let mut params = Vec::new();
        self.with_newlines(true, |p| {
            while !p.at(&TokenKind::RBracket) && !p.at(&TokenKind::Eof) {
                let (name, span) = p.expect_pascal()?;
                let mut bounds = Vec::new();
                if p.eat(&TokenKind::Colon) {
                    loop {
                        let token = p.peek_token().clone();
                        let bound = match token.kind {
                            TokenKind::Ident(ref s, IdentClass::Pascal) if s == "Eq" => Bound::Eq,
                            TokenKind::Ident(ref s, IdentClass::Pascal) if s == "Ord" => Bound::Ord,
                            TokenKind::Ident(ref s, IdentClass::Pascal) if s == "Show" => Bound::Show,
                            _ => {
                                return Err(p.error_at_current(
                                    "expected a bound: Eq, Ord, or Show",
                                ))
                            }
                        };
                        p.advance();
                        bounds.push(bound);
                        if !p.eat(&TokenKind::Plus) {
                            break;
                        }
                    }
                }
                params.push(GenericParam {
                    id: p.id(),
                    span: span.merge(p.span_before()),
                    name,
                    bounds,
                });
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
            }
            Ok(())
        })?;
        self.expect(&TokenKind::RBracket)?;
        Ok(params)
    }

    fn parse_optional_type_args(&mut self) -> PResult<Vec<TyExpr>> {
        if !self.eat(&TokenKind::LBracket) {
            return Ok(Vec::new());
        }
        let mut args = Vec::new();
        self.with_newlines(true, |p| {
            while !p.at(&TokenKind::RBracket) && !p.at(&TokenKind::Eof) {
                args.push(p.parse_type()?);
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
            }
            Ok(())
        })?;
        self.expect(&TokenKind::RBracket)?;
        Ok(args)
    }

    fn end_interface_signature(&mut self) -> PResult<()> {
        if matches!(self.raw_kind(), TokenKind::Newline | TokenKind::Eof) {
            Ok(())
        } else {
            Err(self.error_at_current("expected a newline after interface signature"))
        }
    }

    // ----- types ---------------------------------------------------------

    fn parse_type(&mut self) -> PResult<TyExpr> {
        self.enter_depth()?;
        let result = stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || {
            self.parse_type_inner()
        });
        self.depth -= 1;
        result
    }

    fn parse_type_inner(&mut self) -> PResult<TyExpr> {
        let start = self.span();
        let mut ty = self.parse_base_type()?;
        if self.eat(&TokenKind::Question) {
            if self.at(&TokenKind::Question) {
                return Err(self.error_at_current("write Option[T?] instead of T??"));
            }
            let span = start.merge(self.span_before());
            ty = TyExpr {
                id: self.id(),
                span,
                kind: TyKind::Optional(Box::new(ty)),
            };
        }
        Ok(ty)
    }

    fn parse_base_type(&mut self) -> PResult<TyExpr> {
        let start = self.span();
        let token = self.peek_token().clone();
        match token.kind {
            TokenKind::TypeName(t) => {
                self.advance();
                let prim = match t {
                    TypeName::Int => PrimTy::Int,
                    TypeName::Float => PrimTy::Float,
                    TypeName::Bool => PrimTy::Bool,
                    TypeName::Str => PrimTy::Str,
                    TypeName::Range => PrimTy::Range,
                    TypeName::List | TypeName::Map => {
                        let args = self.parse_optional_type_args()?;
                        return Ok(TyExpr {
                            id: self.id(),
                            span: start.merge(self.span_before()),
                            kind: TyKind::Named {
                                name: t.as_str().to_string(),
                                args,
                            },
                        });
                    }
                };
                Ok(TyExpr {
                    id: self.id(),
                    span: token.span,
                    kind: TyKind::Prim(prim),
                })
            }
            TokenKind::Ident(_, IdentClass::Pascal) => {
                let (name, span) = self.expect_pascal()?;
                let args = self.parse_optional_type_args()?;
                Ok(TyExpr {
                    id: self.id(),
                    span: span.merge(self.span_before()),
                    kind: TyKind::Named { name, args },
                })
            }
            TokenKind::LParen => {
                self.advance();
                self.with_newlines(true, |p| {
                    if p.eat(&TokenKind::RParen) {
                        return Ok(TyExpr {
                            id: p.id(),
                            span: start.merge(p.span_before()),
                            kind: TyKind::Unit,
                        });
                    }
                    let first = p.parse_type()?;
                    if p.eat(&TokenKind::Comma) {
                        let mut items = vec![first];
                        while !p.at(&TokenKind::RParen) && !p.at(&TokenKind::Eof) {
                            items.push(p.parse_type()?);
                            if !p.eat(&TokenKind::Comma) {
                                break;
                            }
                        }
                        p.expect(&TokenKind::RParen)?;
                        Ok(TyExpr {
                            id: p.id(),
                            span: start.merge(p.span_before()),
                            kind: TyKind::Tuple(items),
                        })
                    } else {
                        p.expect(&TokenKind::RParen)?;
                        Ok(first)
                    }
                })
            }
            TokenKind::Fn => {
                self.advance();
                self.expect(&TokenKind::LParen)?;
                let mut params = Vec::new();
                self.with_newlines(true, |p| {
                    while !p.at(&TokenKind::RParen) && !p.at(&TokenKind::Eof) {
                        params.push(p.parse_type()?);
                        if !p.eat(&TokenKind::Comma) {
                            break;
                        }
                    }
                    Ok(())
                })?;
                self.expect(&TokenKind::RParen)?;
                let ret = if self.eat(&TokenKind::Arrow) {
                    Some(Box::new(self.parse_type()?))
                } else {
                    None
                };
                Ok(TyExpr {
                    id: self.id(),
                    span: start.merge(self.span_before()),
                    kind: TyKind::Fn { params, ret },
                })
            }
            _ => Err(self.error_at_current(format!(
                "expected a type, found {}",
                token.kind
            ))),
        }
    }

    // ----- statements and blocks ----------------------------------------

    fn parse_block(&mut self) -> PResult<Block> {
        let start = self.expect(&TokenKind::LBrace)?.span;
        let mut stmts = Vec::new();
        self.with_newlines(false, |p| {
            p.skip_all_newlines();
            while !p.at(&TokenKind::RBrace) && !p.at(&TokenKind::Eof) {
                let stmt = p.parse_stmt()?;
                stmts.push(stmt);
                p.statement_end();
                p.skip_all_newlines();
            }
            Ok(())
        })?;
        let end = self.expect(&TokenKind::RBrace)?.span;
        Ok(Block {
            id: self.id(),
            span: start.merge(end),
            stmts,
        })
    }

    fn parse_stmt(&mut self) -> PResult<Stmt> {
        let start = self.span();
        let is_let = self.eat(&TokenKind::Let);
        let is_var = if is_let { false } else { self.eat(&TokenKind::Var) };
        if is_let || is_var {
            let mutable = is_var;
            let pattern = self.parse_pattern()?;
            let ty = if self.eat(&TokenKind::Colon) {
                Some(self.parse_type()?)
            } else {
                None
            };
            self.expect(&TokenKind::Eq)?;
            let value = self.parse_expr()?;
            let span = start.merge(value.span);
            return Ok(Stmt {
                id: self.id(),
                span,
                kind: StmtKind::Let {
                    mutable,
                    pattern,
                    ty,
                    value,
                },
            });
        }
        if self.eat(&TokenKind::While) {
            self.skip_continuation_newlines();
            let cond = self.with_struct_lit(false, |p| p.parse_expr())?;
            self.skip_all_newlines();
            let body = self.parse_block()?;
            let span = start.merge(body.span);
            return Ok(Stmt {
                id: self.id(),
                span,
                kind: StmtKind::While { cond, body },
            });
        }
        if self.eat(&TokenKind::For) {
            let pattern = self.parse_pattern()?;
            self.expect(&TokenKind::In)?;
            let iter = self.with_struct_lit(false, |p| p.parse_expr())?;
            self.skip_all_newlines();
            let body = self.parse_block()?;
            let span = start.merge(body.span);
            return Ok(Stmt {
                id: self.id(),
                span,
                kind: StmtKind::For {
                    pattern,
                    iter,
                    body,
                },
            });
        }
        if self.eat(&TokenKind::Return) {
            self.skip_continuation_newlines();
            let value = if self.starts_expr() {
                Some(self.parse_expr()?)
            } else {
                None
            };
            let span = start.merge(self.span_before());
            return Ok(Stmt {
                id: self.id(),
                span,
                kind: StmtKind::Return(value),
            });
        }
        if self.eat(&TokenKind::Break) {
            let span = start.merge(self.span_before());
            return Ok(Stmt {
                id: self.id(),
                span,
                kind: StmtKind::Break,
            });
        }
        if self.eat(&TokenKind::Continue) {
            let span = start.merge(self.span_before());
            return Ok(Stmt {
                id: self.id(),
                span,
                kind: StmtKind::Continue,
            });
        }

        let expr = self.parse_expr()?;
        let assign = if let Some(op) = self.assign_op() {
            self.advance();
            self.skip_continuation_newlines();
            let rhs = self.parse_expr()?;
            Some((op, rhs))
        } else {
            None
        };
        let span = start.merge(self.span_before());
        Ok(Stmt {
            id: self.id(),
            span,
            kind: StmtKind::Expr { expr, assign },
        })
    }

    fn assign_op(&mut self) -> Option<AssignOp> {
        Some(match self.peek_kind() {
            TokenKind::Eq => AssignOp::Assign,
            TokenKind::PlusEq => AssignOp::Add,
            TokenKind::MinusEq => AssignOp::Sub,
            TokenKind::StarEq => AssignOp::Mul,
            TokenKind::SlashEq => AssignOp::Div,
            TokenKind::PercentEq => AssignOp::Rem,
            _ => return None,
        })
    }

    fn starts_expr(&mut self) -> bool {
        matches!(
            self.peek_kind(),
            TokenKind::Int(_)
                | TokenKind::Float(_)
                | TokenKind::Str(_)
                | TokenKind::True
                | TokenKind::False
                | TokenKind::SelfKw
                | TokenKind::Ident(..)
                | TokenKind::TypeName(_)
                | TokenKind::LParen
                | TokenKind::LBracket
                | TokenKind::LBrace
                | TokenKind::If
                | TokenKind::Match
                | TokenKind::Fn
                | TokenKind::Not
                | TokenKind::Minus
        )
    }

    // ----- expressions ---------------------------------------------------

    fn parse_expr(&mut self) -> PResult<Expr> {
        self.enter_depth()?;
        let result = stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || {
            self.parse_expr_inner()
        });
        self.depth -= 1;
        result
    }

    fn parse_expr_inner(&mut self) -> PResult<Expr> {
        self.skip_continuation_newlines();
        let lhs = self.parse_or()?;
        if self.at(&TokenKind::QuestionQuestion) {
            self.advance();
            self.skip_continuation_newlines();
            let rhs = self.parse_expr()?;
            let span = lhs.span.merge(rhs.span);
            return Ok(Expr {
                id: self.id(),
                span,
                kind: ExprKind::Binary {
                    op: BinaryOp::Coalesce,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            });
        }
        Ok(lhs)
    }

    fn parse_or(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_and()?;
        while self.at(&TokenKind::Or) {
            self.advance();
            self.skip_continuation_newlines();
            let rhs = self.parse_and()?;
            let span = lhs.span.merge(rhs.span);
            lhs = Expr {
                id: self.id(),
                span,
                kind: ExprKind::Binary {
                    op: BinaryOp::Or,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_not()?;
        while self.at(&TokenKind::And) {
            self.advance();
            self.skip_continuation_newlines();
            let rhs = self.parse_not()?;
            let span = lhs.span.merge(rhs.span);
            lhs = Expr {
                id: self.id(),
                span,
                kind: ExprKind::Binary {
                    op: BinaryOp::And,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    fn parse_not(&mut self) -> PResult<Expr> {
        if self.eat(&TokenKind::Not) {
            self.skip_continuation_newlines();
            let expr = self.parse_not()?;
            let span = self.span_before().merge(expr.span);
            return Ok(Expr {
                id: self.id(),
                span,
                kind: ExprKind::Unary {
                    op: UnaryOp::Not,
                    expr: Box::new(expr),
                },
            });
        }
        self.parse_cmp()
    }

    fn parse_cmp(&mut self) -> PResult<Expr> {
        let lhs = self.parse_range()?;
        let op = match self.peek_kind() {
            TokenKind::EqEq => BinaryOp::Eq,
            TokenKind::NotEq => BinaryOp::Ne,
            TokenKind::Lt => BinaryOp::Lt,
            TokenKind::Le => BinaryOp::Le,
            TokenKind::Gt => BinaryOp::Gt,
            TokenKind::Ge => BinaryOp::Ge,
            _ => return Ok(lhs),
        };
        self.advance();
        self.skip_continuation_newlines();
        let rhs = self.parse_range()?;
        if matches!(
            self.peek_kind(),
            TokenKind::EqEq
                | TokenKind::NotEq
                | TokenKind::Lt
                | TokenKind::Le
                | TokenKind::Gt
                | TokenKind::Ge
        ) {
            return Err(self.error_at_current(
                "comparison operators are non-associative; add parentheses",
            ));
        }
        let span = lhs.span.merge(rhs.span);
        Ok(Expr {
            id: self.id(),
            span,
            kind: ExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
        })
    }

    fn parse_range(&mut self) -> PResult<Expr> {
        let start = self.parse_add()?;
        let inclusive = match self.peek_kind() {
            TokenKind::DotDot => false,
            TokenKind::DotDotEq => true,
            _ => return Ok(start),
        };
        self.advance();
        self.skip_continuation_newlines();
        let end = self.parse_add()?;
        if matches!(self.peek_kind(), TokenKind::DotDot | TokenKind::DotDotEq) {
            return Err(self.error_at_current(
                "range operators are non-associative; add parentheses",
            ));
        }
        let span = start.span.merge(end.span);
        Ok(Expr {
            id: self.id(),
            span,
            kind: ExprKind::Range {
                start: Box::new(start),
                end: Box::new(end),
                inclusive,
            },
        })
    }

    fn parse_add(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_mul()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Plus => BinaryOp::Add,
                TokenKind::Minus => BinaryOp::Sub,
                _ => break,
            };
            self.advance();
            self.skip_continuation_newlines();
            let rhs = self.parse_mul()?;
            let span = lhs.span.merge(rhs.span);
            lhs = Expr {
                id: self.id(),
                span,
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    fn parse_mul(&mut self) -> PResult<Expr> {
        let mut lhs = self.parse_unary()?;
        loop {
            let op = match self.peek_kind() {
                TokenKind::Star => BinaryOp::Mul,
                TokenKind::Slash => BinaryOp::Div,
                TokenKind::Percent => BinaryOp::Rem,
                _ => break,
            };
            self.advance();
            self.skip_continuation_newlines();
            let rhs = self.parse_unary()?;
            let span = lhs.span.merge(rhs.span);
            lhs = Expr {
                id: self.id(),
                span,
                kind: ExprKind::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                },
            };
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> PResult<Expr> {
        if self.at(&TokenKind::Minus) {
            let minus = self.advance();
            self.skip_continuation_newlines();
            if let TokenKind::Int(v) = self.peek_kind().clone() {
                if v == (i64::MAX as i128) + 1 {
                    let tok = self.advance();
                    return Ok(Expr {
                        id: self.id(),
                        span: minus.span.merge(tok.span),
                        kind: ExprKind::Int(i64::MIN),
                    });
                }
            }
            let expr = self.parse_unary()?;
            let span = minus.span.merge(expr.span);
            return Ok(Expr {
                id: self.id(),
                span,
                kind: ExprKind::Unary {
                    op: UnaryOp::Neg,
                    expr: Box::new(expr),
                },
            });
        }
        self.parse_postfix()
    }

    fn parse_postfix(&mut self) -> PResult<Expr> {
        let mut expr = self.parse_primary()?;
        loop {
            if self.ignore_newlines {
                self.skip_all_newlines();
            }
            match self.peek_kind() {
                TokenKind::LParen => {
                    let args = self.parse_args()?;
                    let span = expr.span.merge(self.span_before());
                    expr = Expr {
                        id: self.id(),
                        span,
                        kind: ExprKind::Call {
                            callee: Box::new(expr),
                            args,
                        },
                    };
                }
                TokenKind::LBracket => {
                    self.advance();
                    let index = self.with_newlines(true, |p| p.parse_expr())?;
                    self.expect(&TokenKind::RBracket)?;
                    let span = expr.span.merge(self.span_before());
                    expr = Expr {
                        id: self.id(),
                        span,
                        kind: ExprKind::Index {
                            base: Box::new(expr),
                            index: Box::new(index),
                        },
                    };
                }
                TokenKind::Dot => {
                    self.advance();
                    let token = self.peek_token().clone();
                    let name = match token.kind {
                        TokenKind::Ident(name, _) => {
                            self.advance();
                            FieldName::Ident(name)
                        }
                        TokenKind::TypeName(ty) => {
                            self.advance();
                            FieldName::Ident(ty.as_str().to_string())
                        }
                        TokenKind::Int(v) if v >= 0 && v <= u32::MAX as i128 => {
                            self.advance();
                            FieldName::Index(v as u32)
                        }
                        _ => {
                            return Err(self.error_at_current(
                                "expected a field name or tuple index after `.`",
                            ))
                        }
                    };
                    let span = expr.span.merge(self.span_before());
                    expr = Expr {
                        id: self.id(),
                        span,
                        kind: ExprKind::Field {
                            base: Box::new(expr),
                            name,
                        },
                    };
                }
                TokenKind::Question => {
                    self.advance();
                    let span = expr.span.merge(self.span_before());
                    expr = Expr {
                        id: self.id(),
                        span,
                        kind: ExprKind::Try(Box::new(expr)),
                    };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    fn parse_args(&mut self) -> PResult<Vec<Arg>> {
        self.expect(&TokenKind::LParen)?;
        let mut args = Vec::new();
        self.with_newlines(true, |p| {
            while !p.at(&TokenKind::RParen) && !p.at(&TokenKind::Eof) {
                let start = p.span();
                let mutable = p.eat(&TokenKind::Mut);
                let expr = p.parse_expr()?;
                args.push(Arg {
                    id: p.id(),
                    span: start.merge(expr.span),
                    mutable,
                    expr,
                });
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
            }
            Ok(())
        })?;
        self.expect(&TokenKind::RParen)?;
        Ok(args)
    }

    fn parse_primary(&mut self) -> PResult<Expr> {
        let start = self.span();
        let token = self.peek_token().clone();
        match token.kind {
            TokenKind::Int(v) => {
                self.advance();
                let value = if v > i64::MAX as i128 {
                    self.error(
                        Code::E001,
                        token.span,
                        "integer literal is out of range",
                    );
                    i64::MAX
                } else {
                    v as i64
                };
                Ok(Expr {
                    id: self.id(),
                    span: token.span,
                    kind: ExprKind::Int(value),
                })
            }
            TokenKind::Float(v) => {
                self.advance();
                Ok(Expr {
                    id: self.id(),
                    span: token.span,
                    kind: ExprKind::Float(v),
                })
            }
            TokenKind::Str(lit) => {
                self.advance();
                let parts = self.parse_string_parts(&lit, token.span)?;
                Ok(Expr {
                    id: self.id(),
                    span: token.span,
                    kind: ExprKind::Str(parts),
                })
            }
            TokenKind::True | TokenKind::False => {
                self.advance();
                Ok(Expr {
                    id: self.id(),
                    span: token.span,
                    kind: ExprKind::Bool(matches!(token.kind, TokenKind::True)),
                })
            }
            TokenKind::SelfKw => {
                self.advance();
                Ok(Expr {
                    id: self.id(),
                    span: token.span,
                    kind: ExprKind::Var("self".to_string()),
                })
            }
            TokenKind::Ident(name, class) => {
                if class == IdentClass::Invalid {
                    return Err(self.error_at_current(format!("invalid identifier `{name}`")));
                }
                if class == IdentClass::Pascal && self.lookahead_is(&TokenKind::LBrace) {
                    if !self.allow_struct_lit {
                        return Err(self.error_at_current(
                            "struct literals are not allowed here; add parentheses",
                        ));
                    }
                    return self.parse_struct_literal(name, token.span);
                }
                self.advance();
                Ok(Expr {
                    id: self.id(),
                    span: token.span,
                    kind: ExprKind::Var(name),
                })
            }
            TokenKind::TypeName(t) => {
                self.advance();
                match t {
                    TypeName::Int | TypeName::Float | TypeName::Str => {
                        if !self.eat(&TokenKind::LParen) {
                            return Err(self.error_at_current(format!(
                                "`{}` is only valid as a conversion here",
                                t.as_str()
                            )));
                        }
                        let expr = self.with_newlines(true, |p| p.parse_expr())?;
                        self.expect(&TokenKind::RParen)?;
                        let prim = match t {
                            TypeName::Int => PrimTy::Int,
                            TypeName::Float => PrimTy::Float,
                            TypeName::Str => PrimTy::Str,
                            _ => unreachable!(),
                        };
                        Ok(Expr {
                            id: self.id(),
                            span: start.merge(self.span_before()),
                            kind: ExprKind::Convert {
                                to: prim,
                                expr: Box::new(expr),
                            },
                        })
                    }
                    _ => Err(self.error_at(
                        Code::E003,
                        token.span,
                        format!("`{}` cannot be used as an expression", t.as_str()),
                    )),
                }
            }
            TokenKind::LParen => self.parse_paren_expr(),
            TokenKind::LBracket => self.parse_list_expr(),
            TokenKind::LBrace => self.parse_map_expr(),
            TokenKind::If => self.parse_if_expr(),
            TokenKind::Match => self.parse_match_expr(),
            TokenKind::Fn => self.parse_lambda(),
            TokenKind::Underscore => Err(self.error_at_current(
                "`_` is a wildcard and cannot be used as an expression",
            )),
            _ => Err(self.error_at_current(format!(
                "expected an expression, found {}",
                token.kind
            ))),
        }
    }

    fn lookahead_is(&mut self, kind: &TokenKind) -> bool {
        if self.ignore_newlines {
            self.peek_token();
        }
        let mut i = self.idx + 1;
        while i < self.tokens.len() {
            let t = &self.tokens[i];
            if matches!(t.kind, TokenKind::Newline) {
                if !self.ignore_newlines {
                    return false;
                }
                i += 1;
                continue;
            }
            return discriminant(&t.kind) == discriminant(kind);
        }
        false
    }

    fn parse_string_parts(
        &mut self,
        lit: &crate::token::StrLit,
        span: Span,
    ) -> PResult<Vec<StrExprPart>> {
        let mut parts = Vec::new();
        for part in &lit.parts {
            match part {
                StrPart::Text(t) => parts.push(StrExprPart::Text(t.clone())),
                StrPart::Expr(expr_span) => {
                    let lexed = lex_fragment(self.source, *expr_span, self.limits);
                    let mut sub = Parser::with_lexed(
                        self.source,
                        lexed,
                        self.limits,
                        false,
                    );
                    let expr = sub.parse_expr().map_err(|_| {
                        self.error(
                            Code::E003,
                            *expr_span,
                            "malformed expression inside interpolation",
                        )
                    })?;
                    sub.peek_token();
                    if !sub.at(&TokenKind::Eof) {
                        let err_span = sub.span();
                        sub.error(
                            Code::E003,
                            err_span,
                            "unexpected token inside interpolation",
                        );
                    }
                    self.diagnostics.extend(sub.diagnostics);
                    parts.push(StrExprPart::Expr(expr));
                }
            }
        }
        let _ = span;
        Ok(parts)
    }

    fn parse_paren_expr(&mut self) -> PResult<Expr> {
        let start = self.expect(&TokenKind::LParen)?.span;
        self.with_newlines(true, |p| {
            if p.eat(&TokenKind::RParen) {
                return Ok(Expr {
                    id: p.id(),
                    span: start.merge(p.span_before()),
                    kind: ExprKind::Unit,
                });
            }
            let first = p.with_struct_lit(true, |p| p.parse_expr())?;
            if p.eat(&TokenKind::Comma) {
                let mut items = vec![first];
                while !p.at(&TokenKind::RParen) && !p.at(&TokenKind::Eof) {
                    items.push(p.with_struct_lit(true, |p| p.parse_expr())?);
                    if !p.eat(&TokenKind::Comma) {
                        break;
                    }
                }
                p.expect(&TokenKind::RParen)?;
                Ok(Expr {
                    id: p.id(),
                    span: start.merge(p.span_before()),
                    kind: ExprKind::Tuple(items),
                })
            } else {
                p.expect(&TokenKind::RParen)?;
                Ok(first)
            }
        })
    }

    fn parse_list_expr(&mut self) -> PResult<Expr> {
        let start = self.expect(&TokenKind::LBracket)?.span;
        let mut items = Vec::new();
        self.with_newlines(true, |p| {
            while !p.at(&TokenKind::RBracket) && !p.at(&TokenKind::Eof) {
                items.push(p.with_struct_lit(true, |p| p.parse_expr())?);
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
            }
            Ok(())
        })?;
        self.expect(&TokenKind::RBracket)?;
        Ok(Expr {
            id: self.id(),
            span: start.merge(self.span_before()),
            kind: ExprKind::List(items),
        })
    }

    fn parse_map_expr(&mut self) -> PResult<Expr> {
        let start = self.expect(&TokenKind::LBrace)?.span;
        let mut entries = Vec::new();
        self.with_newlines(true, |p| {
            while !p.at(&TokenKind::RBrace) && !p.at(&TokenKind::Eof) {
                let key = p.with_struct_lit(true, |p| p.parse_expr())?;
                p.expect(&TokenKind::Colon)?;
                let value = p.with_struct_lit(true, |p| p.parse_expr())?;
                entries.push((key, value));
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
            }
            Ok(())
        })?;
        self.expect(&TokenKind::RBrace)?;
        Ok(Expr {
            id: self.id(),
            span: start.merge(self.span_before()),
            kind: ExprKind::Map(entries),
        })
    }

    fn parse_struct_literal(&mut self, name: String, start: Span) -> PResult<Expr> {
        self.advance();
        let mut fields = Vec::new();
        let mut base = None;
        self.expect(&TokenKind::LBrace)?;
        self.with_newlines(true, |p| {
            if p.eat(&TokenKind::DotDot) {
                base = Some(Box::new(p.with_struct_lit(true, |p| p.parse_expr())?));
                if p.eat(&TokenKind::Comma) {
                    // continue with fields
                } else {
                    return Ok(());
                }
            }
            while !p.at(&TokenKind::RBrace) && !p.at(&TokenKind::Eof) {
                let field_start = p.span();
                let (field_name, _) = p.expect_snake()?;
                let value = if p.eat(&TokenKind::Colon) {
                    Some(p.with_struct_lit(true, |p| p.parse_expr())?)
                } else {
                    Some(Expr {
                        id: p.id(),
                        span: field_start,
                        kind: ExprKind::Var(field_name.clone()),
                    })
                };
                fields.push(FieldInit {
                    id: p.id(),
                    span: field_start.merge(p.span_before()),
                    name: field_name,
                    value,
                });
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
            }
            Ok(())
        })?;
        self.expect(&TokenKind::RBrace)?;
        Ok(Expr {
            id: self.id(),
            span: start.merge(self.span_before()),
            kind: ExprKind::StructLit {
                name,
                fields,
                base,
            },
        })
    }

    fn parse_if_expr(&mut self) -> PResult<Expr> {
        let start = self.expect(&TokenKind::If)?.span;
        self.skip_continuation_newlines();
        let cond = self.with_struct_lit(false, |p| p.parse_expr())?;
        self.skip_all_newlines();
        let then_branch = self.parse_block()?;
        let mut end = then_branch.span;
        let else_branch = if self.lookahead_after_newlines(&TokenKind::Else) {
            self.skip_all_newlines();
            self.expect(&TokenKind::Else)?;
            self.skip_all_newlines();
            let branch = if self.at(&TokenKind::If) {
                self.parse_if_expr()?
            } else {
                let block = self.parse_block()?;
                Expr {
                    id: self.id(),
                    span: block.span,
                    kind: ExprKind::Block(block),
                }
            };
            end = branch.span;
            Some(Box::new(branch))
        } else {
            None
        };
        Ok(Expr {
            id: self.id(),
            span: start.merge(end),
            kind: ExprKind::If {
                cond: Box::new(cond),
                then_branch,
                else_branch,
            },
        })
    }

    fn parse_match_expr(&mut self) -> PResult<Expr> {
        let start = self.expect(&TokenKind::Match)?.span;
        self.skip_continuation_newlines();
        let scrutinee = self.with_struct_lit(false, |p| p.parse_expr())?;
        self.skip_all_newlines();
        self.expect(&TokenKind::LBrace)?;
        let mut arms = Vec::new();
        self.with_newlines(true, |p| {
            while !p.at(&TokenKind::RBrace) && !p.at(&TokenKind::Eof) {
                let arm_start = p.span();
                let pattern = p.parse_pattern()?;
                let guard = if p.eat(&TokenKind::If) {
                    Some(p.with_struct_lit(false, |p| p.parse_expr())?)
                } else {
                    None
                };
                p.expect(&TokenKind::FatArrow)?;
                let body = if p.at(&TokenKind::LBrace) {
                    MatchBody::Block(p.parse_block()?)
                } else {
                    MatchBody::Expr(p.with_struct_lit(true, |p| p.parse_expr())?)
                };
                let span = arm_start.merge(p.span_before());
                arms.push(MatchArm {
                    id: p.id(),
                    span,
                    pattern,
                    guard,
                    body,
                });
                if p.eat(&TokenKind::Comma) {
                    continue;
                }
                if !p.at(&TokenKind::RBrace) {
                    return Err(p.error_at_current(
                        "expected a comma between match arms",
                    ));
                }
            }
            Ok(())
        })?;
        self.expect(&TokenKind::RBrace)?;
        Ok(Expr {
            id: self.id(),
            span: start.merge(self.span_before()),
            kind: ExprKind::Match {
                scrutinee: Box::new(scrutinee),
                arms,
            },
        })
    }

    fn parse_lambda(&mut self) -> PResult<Expr> {
        let start = self.expect(&TokenKind::Fn)?.span;
        self.expect(&TokenKind::LParen)?;
        let mut params = Vec::new();
        self.with_newlines(true, |p| {
            while !p.at(&TokenKind::RParen) && !p.at(&TokenKind::Eof) {
                let param_start = p.span();
                let pattern = p.parse_pattern()?;
                let ty = if p.eat(&TokenKind::Colon) {
                    Some(p.parse_type()?)
                } else {
                    None
                };
                params.push(LambdaParam {
                    id: p.id(),
                    span: param_start.merge(p.span_before()),
                    pattern,
                    ty,
                });
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
            }
            Ok(())
        })?;
        self.expect(&TokenKind::RParen)?;
        let ret = if self.eat(&TokenKind::Arrow) {
            Some(self.parse_type()?)
        } else {
            None
        };
        let body = if self.eat(&TokenKind::FatArrow) {
            LambdaBody::Expr(Box::new(self.parse_expr()?))
        } else {
            LambdaBody::Block(self.parse_block()?)
        };
        let end = match &body {
            LambdaBody::Block(b) => b.span,
            LambdaBody::Expr(e) => e.span,
        };
        Ok(Expr {
            id: self.id(),
            span: start.merge(end),
            kind: ExprKind::Lambda { params, ret, body },
        })
    }

    // ----- patterns ------------------------------------------------------

    fn parse_pattern(&mut self) -> PResult<Pattern> {
        self.enter_depth()?;
        let result = stacker::maybe_grow(64 * 1024, 16 * 1024 * 1024, || {
            self.parse_pattern_inner()
        });
        self.depth -= 1;
        result
    }

    fn parse_pattern_inner(&mut self) -> PResult<Pattern> {
        let first = self.parse_pat()?;
        if !self.at(&TokenKind::Pipe) {
            return Ok(first);
        }
        let mut alternatives = vec![first];
        while self.eat(&TokenKind::Pipe) {
            alternatives.push(self.parse_pat()?);
        }
        let span = alternatives
            .first()
            .map(|p| p.span)
            .unwrap_or_else(|| self.span())
            .merge(
                alternatives
                    .last()
                    .map(|p| p.span)
                    .unwrap_or_else(|| self.span()),
            );
        Ok(Pattern {
            id: self.id(),
            span,
            kind: PatKind::Or(alternatives),
        })
    }

    fn parse_pat(&mut self) -> PResult<Pattern> {
        let start = self.span();
        let token = self.peek_token().clone();
        match token.kind {
            TokenKind::Underscore => {
                self.advance();
                Ok(Pattern {
                    id: self.id(),
                    span: token.span,
                    kind: PatKind::Wild,
                })
            }
            TokenKind::Int(v) => {
                self.advance();
                if v > i64::MAX as i128 {
                    self.error(Code::E001, token.span, "integer pattern is out of range");
                }
                Ok(Pattern {
                    id: self.id(),
                    span: token.span,
                    kind: PatKind::Int(v as i64),
                })
            }
            TokenKind::Minus => {
                self.advance();
                let token = self.peek_token().clone();
                if let TokenKind::Int(v) = token.kind {
                    self.advance();
                    let value = -v;
                    if value < i64::MIN as i128 {
                        self.error(
                            Code::E001,
                            start.merge(token.span),
                            "integer pattern is out of range",
                        );
                    }
                    Ok(Pattern {
                        id: self.id(),
                        span: start.merge(token.span),
                        kind: PatKind::Int(value as i64),
                    })
                } else {
                    Err(self.error_at_current("expected an integer after `-` in a pattern"))
                }
            }
            TokenKind::True | TokenKind::False => {
                self.advance();
                Ok(Pattern {
                    id: self.id(),
                    span: token.span,
                    kind: PatKind::Bool(matches!(token.kind, TokenKind::True)),
                })
            }
            TokenKind::Str(lit) => {
                self.advance();
                let mut text = String::new();
                for part in &lit.parts {
                    match part {
                        StrPart::Text(t) => text.push_str(t),
                        StrPart::Expr(_) => {
                            return Err(self.error_at(
                                Code::E003,
                                token.span,
                                "string patterns may not contain interpolation",
                            ))
                        }
                    }
                }
                Ok(Pattern {
                    id: self.id(),
                    span: token.span,
                    kind: PatKind::Str(text),
                })
            }
            TokenKind::Ident(name, IdentClass::Snake) => {
                self.advance();
                Ok(Pattern {
                    id: self.id(),
                    span: token.span,
                    kind: PatKind::Bind(name),
                })
            }
            TokenKind::Ident(name, IdentClass::Pascal) => {
                self.advance();
                if self.eat(&TokenKind::Dot) {
                    let (variant, _variant_span) = self.expect_pascal()?;
                    let args = self.parse_pat_payload()?;
                    Ok(Pattern {
                        id: self.id(),
                        span: token.span.merge(self.span_before()),
                        kind: PatKind::Variant {
                            qualifier: Some(name),
                            name: variant,
                            args,
                        },
                    })
                } else if self.at(&TokenKind::LParen) {
                    let args = self.parse_pat_payload()?;
                    Ok(Pattern {
                        id: self.id(),
                        span: token.span.merge(self.span_before()),
                        kind: PatKind::Variant {
                            qualifier: None,
                            name,
                            args,
                        },
                    })
                } else if self.at(&TokenKind::LBrace) {
                    self.parse_struct_pattern(name, token.span)
                } else {
                    Ok(Pattern {
                        id: self.id(),
                        span: token.span,
                        kind: PatKind::Variant {
                            qualifier: None,
                            name,
                            args: Vec::new(),
                        },
                    })
                }
            }
            TokenKind::LParen => {
                self.advance();
                self.with_newlines(true, |p| {
                    if p.eat(&TokenKind::RParen) {
                        return Ok(Pattern {
                            id: p.id(),
                            span: start.merge(p.span_before()),
                            kind: PatKind::Tuple(Vec::new()),
                        });
                    }
                    let first = p.parse_pattern()?;
                    if p.eat(&TokenKind::Comma) {
                        let mut items = vec![first];
                        while !p.at(&TokenKind::RParen) && !p.at(&TokenKind::Eof) {
                            items.push(p.parse_pattern()?);
                            if !p.eat(&TokenKind::Comma) {
                                break;
                            }
                        }
                        p.expect(&TokenKind::RParen)?;
                        Ok(Pattern {
                            id: p.id(),
                            span: start.merge(p.span_before()),
                            kind: PatKind::Tuple(items),
                        })
                    } else {
                        p.expect(&TokenKind::RParen)?;
                        Ok(first)
                    }
                })
            }
            TokenKind::Ident(_, IdentClass::Screaming) => Err(self.error_at_current(
                "consts cannot be used as patterns",
            )),
            _ => Err(self.error_at_current(format!(
                "expected a pattern, found {}",
                token.kind
            ))),
        }
    }

    fn parse_pat_payload(&mut self) -> PResult<Vec<Pattern>> {
        self.expect(&TokenKind::LParen)?;
        let mut args = Vec::new();
        self.with_newlines(true, |p| {
            while !p.at(&TokenKind::RParen) && !p.at(&TokenKind::Eof) {
                args.push(p.parse_pattern()?);
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
            }
            Ok(())
        })?;
        self.expect(&TokenKind::RParen)?;
        Ok(args)
    }

    fn parse_struct_pattern(&mut self, name: String, start: Span) -> PResult<Pattern> {
        self.expect(&TokenKind::LBrace)?;
        let mut fields = Vec::new();
        let mut rest = false;
        self.with_newlines(true, |p| {
            while !p.at(&TokenKind::RBrace) && !p.at(&TokenKind::Eof) {
                if p.eat(&TokenKind::DotDot) {
                    rest = true;
                    if !p.at(&TokenKind::RBrace) {
                        return Err(p.error_at_current("`..` must be the last field pattern"));
                    }
                    break;
                }
                let field_start = p.span();
                let (field_name, _) = p.expect_snake()?;
                let pattern = if p.eat(&TokenKind::Colon) {
                    p.parse_pattern()?
                } else {
                    Pattern {
                        id: p.id(),
                        span: field_start,
                        kind: PatKind::Bind(field_name.clone()),
                    }
                };
                fields.push(FieldPat {
                    id: p.id(),
                    span: field_start.merge(pattern.span),
                    name: field_name,
                    pattern,
                });
                if !p.eat(&TokenKind::Comma) {
                    break;
                }
            }
            Ok(())
        })?;
        self.expect(&TokenKind::RBrace)?;
        Ok(Pattern {
            id: self.id(),
            span: start.merge(self.span_before()),
            kind: PatKind::Struct { name, fields, rest },
        })
    }

    fn span_before(&self) -> Span {
        self.tokens[..self.idx]
            .iter()
            .rev()
            .find(|t| !matches!(t.kind, TokenKind::Newline))
            .map(|t| t.span)
            .unwrap_or_else(|| Span::empty(0))
    }

    fn error_at(&mut self, code: Code, span: Span, message: impl Into<String>) -> () {
        self.error(code, span, message);
    }
}

fn is_continuation_prev(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Plus
            | TokenKind::Minus
            | TokenKind::Star
            | TokenKind::Slash
            | TokenKind::Percent
            | TokenKind::Eq
            | TokenKind::PlusEq
            | TokenKind::MinusEq
            | TokenKind::StarEq
            | TokenKind::SlashEq
            | TokenKind::PercentEq
            | TokenKind::EqEq
            | TokenKind::NotEq
            | TokenKind::Lt
            | TokenKind::Le
            | TokenKind::Gt
            | TokenKind::Ge
            | TokenKind::And
            | TokenKind::Or
            | TokenKind::QuestionQuestion
            | TokenKind::DotDot
            | TokenKind::DotDotEq
            | TokenKind::Comma
            | TokenKind::Colon
            | TokenKind::FatArrow
            | TokenKind::Arrow
            | TokenKind::LParen
            | TokenKind::LBracket
            | TokenKind::LBrace
    )
}

fn is_continuation_next(kind: &TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Dot
            | TokenKind::Else
            | TokenKind::And
            | TokenKind::Or
            | TokenKind::QuestionQuestion
    )
}
