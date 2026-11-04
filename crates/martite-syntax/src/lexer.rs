use std::rc::Rc;

use martite_base::{Code, Diagnostic, LoadLimits, Span};

use crate::token::{Comment, IdentClass, StrLit, StrPart, Token, TokenKind, TypeName};

#[derive(Debug, Clone, Default)]
pub struct Lexed {
    pub tokens: Vec<Token>,
    pub comments: Vec<Comment>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Lex a whole source file.
pub fn lex(source: &str, limits: LoadLimits) -> Lexed {
    Lexer::new(source, 0, source.len(), limits).run()
}

/// Lex a fragment (used for interpolation expressions). Token spans remain
/// relative to the original file.
pub fn lex_fragment(source: &str, span: Span, limits: LoadLimits) -> Lexed {
    let mut start = (span.start as usize).min(source.len());
    let mut end = (span.end as usize).min(source.len());
    while start > 0 && !source.is_char_boundary(start) {
        start -= 1;
    }
    while end > 0 && !source.is_char_boundary(end) {
        end -= 1;
    }
    if start > end {
        let mut d = Diagnostic::error(Code::E001, "invalid interpolation span");
        d.span = Some(Span::new(start as u32, end as u32));
        return Lexed {
            tokens: vec![Token {
                kind: TokenKind::Eof,
                span: Span::empty(end as u32),
            }],
            comments: Vec::new(),
            diagnostics: vec![d],
        };
    }
    Lexer::new(source, start, end, limits).run()
}

struct Lexer<'a> {
    src: &'a str,
    pos: usize,
    end: usize,
    limits: LoadLimits,
    out: Lexed,
    depth: usize,
    depth_reported: bool,
    last_sig: Option<TokenKind>,
    postfix_dot: bool,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, start: usize, end: usize, limits: LoadLimits) -> Self {
        let mut pos = start;
        if pos == 0 && src.starts_with('\u{feff}') {
            pos = '\u{feff}'.len_utf8();
        }
        Self {
            src,
            pos,
            end: end.min(src.len()),
            limits,
            out: Lexed::default(),
            depth: 0,
            depth_reported: false,
            last_sig: None,
            postfix_dot: false,
        }
    }

    fn run(mut self) -> Lexed {
        while self.pos < self.end {
            let was_postfix_dot = self.postfix_dot;
            self.postfix_dot = false;

            let Some(ch) = self.peek() else { break };
            match ch {
                ' ' | '\t' | '\r' => {
                    self.bump();
                }
                '\n' => {
                    let start = self.pos;
                    self.bump();
                    self.push(TokenKind::Newline, Span::new(start as u32, self.pos as u32));
                }
                '/' if self.peek_n(1) == Some(b'/') => self.comment(),
                '"' => {
                    let start = self.pos;
                    let kind = self.string();
                    self.push(kind, Span::new(start as u32, self.pos as u32));
                }
                '0'..='9' => {
                    let start = self.pos;
                    let kind = self.number(was_postfix_dot);
                    self.push(kind, Span::new(start as u32, self.pos as u32));
                }
                c if c.is_ascii_alphabetic() || c == '_' => self.identifier(),
                _ => self.punctuation(),
            }
        }
        let at = self.pos as u32;
        self.out.tokens.push(Token {
            kind: TokenKind::Eof,
            span: Span::empty(at),
        });
        self.out
    }

    fn peek(&self) -> Option<char> {
        self.src[self.pos..self.end].chars().next()
    }

    fn peek_n(&self, n: usize) -> Option<u8> {
        self.src.as_bytes().get(self.pos + n).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += ch.len_utf8();
        Some(ch)
    }

    fn push(&mut self, kind: TokenKind, span: Span) {
        let significant = !matches!(kind, TokenKind::Newline);
        self.out.tokens.push(Token {
            kind: kind.clone(),
            span,
        });
        if significant {
            self.last_sig = Some(kind);
        }
    }

    fn error(&mut self, code: Code, span: Span, message: impl Into<String>) {
        let mut d = Diagnostic::error(code, message);
        d.span = Some(span);
        self.out.diagnostics.push(d);
    }

    fn enter_bracket(&mut self, span: Span) {
        self.depth += 1;
        if self.depth > self.limits.nesting_depth && !self.depth_reported {
            self.depth_reported = true;
            self.error(
                Code::E002,
                span,
                format!(
                    "syntactic nesting exceeds the limit of {}",
                    self.limits.nesting_depth
                ),
            );
        }
    }

    fn leave_bracket(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn comment(&mut self) {
        let start = self.pos;
        self.bump();
        self.bump();
        let doc = self.peek() == Some('/');
        let content_start = self.pos;
        while let Some(ch) = self.peek() {
            if ch == '\n' {
                break;
            }
            self.bump();
        }
        let text = self.src[content_start..self.pos].to_string();
        self.out.comments.push(Comment {
            span: Span::new(start as u32, self.pos as u32),
            text,
            doc,
        });
    }

    fn identifier(&mut self) {
        let start = self.pos;
        while let Some(ch) = self.peek() {
            if ch.is_ascii_alphanumeric() || ch == '_' {
                self.bump();
            } else {
                break;
            }
        }
        let text = &self.src[start..self.pos];
        let span = Span::new(start as u32, self.pos as u32);

        if text == "_" {
            self.push(TokenKind::Underscore, span);
            return;
        }
        if let Some(ty) = TypeName::from_str(text) {
            self.push(TokenKind::TypeName(ty), span);
            return;
        }
        if let Some(kw) = keyword(text) {
            self.push(kw, span);
            return;
        }
        if !text.is_ascii() {
            self.error(Code::E001, span, "identifiers must be ASCII");
            self.push(
                TokenKind::Ident(text.to_string(), IdentClass::Invalid),
                span,
            );
            return;
        }
        let class = classify(text);
        if class == IdentClass::Invalid {
            self.error(
                Code::E104,
                span,
                format!("`{text}` is not snake_case, PascalCase, or SCREAMING_CASE"),
            );
        }
        self.push(TokenKind::Ident(text.to_string(), class), span);
    }

    fn number(&mut self, postfix_dot: bool) -> TokenKind {
        let start = self.pos;
        let mut span = Span::new(start as u32, self.pos as u32);
        if postfix_dot {
            // After a postfix `.` the lexer always reads a plain decimal integer.
            let digits_start = self.pos;
            while matches!(self.peek(), Some('0'..='9')) {
                self.bump();
            }
            let text = &self.src[digits_start..self.pos];
            span.end = self.pos as u32;
            return match text.parse::<i128>() {
                Ok(v) => TokenKind::Int(v),
                Err(_) => {
                    self.error(Code::E001, span, "tuple index is out of range");
                    TokenKind::Int(0)
                }
            };
        }

        let mut radix = 10;
        if self.peek() == Some('0') {
            match self.peek_n(1) {
                Some(b'x') | Some(b'X') => {
                    radix = 16;
                    self.bump();
                    self.bump();
                }
                Some(b'b') | Some(b'B') => {
                    radix = 2;
                    self.bump();
                    self.bump();
                }
                _ => {}
            }
        }

        if radix != 10 {
            let text = self.scan_digits(radix, span);
            if text.is_empty() {
                self.error(Code::E001, span, "expected digits after base prefix");
                return TokenKind::Int(0);
            }
            let cleaned: String = text.chars().filter(|c| *c != '_').collect();
            span.end = self.pos as u32;
            return match u128::from_str_radix(&cleaned, radix) {
                Ok(v) if v <= (i64::MAX as u128) + 1 => TokenKind::Int(v as i128),
                _ => {
                    self.error(Code::E001, span, "integer literal is out of range");
                    TokenKind::Int(0)
                }
            };
        }

        let int_text = self.scan_digits(10, span);
        let mut is_float = false;
        let mut float_text = int_text.clone();

        if self.peek() == Some('.')
            && matches!(self.peek_n(1), Some(b'0'..=b'9'))
            && self.peek_n(1) != Some(b'.')
        {
            is_float = true;
            self.bump();
            float_text.push('.');
            let frac = self.scan_digits(10, span);
            if frac.is_empty() {
                self.error(Code::E001, span, "expected digits after `.`");
            }
            float_text.push_str(&frac);
        }

        if matches!(self.peek(), Some('e') | Some('E')) {
            let save = self.pos;
            let mut exp = String::new();
            exp.push(self.bump().unwrap());
            if matches!(self.peek(), Some('+') | Some('-')) {
                exp.push(self.bump().unwrap());
            }
            let digits = self.scan_digits(10, span);
            if digits.is_empty() {
                self.pos = save;
                self.error(
                    Code::E001,
                    Span::new(start as u32, self.pos as u32),
                    "malformed exponent",
                );
            } else {
                is_float = true;
                exp.push_str(&digits);
                float_text.push_str(&exp);
            }
        }

        span.end = self.pos as u32;
        if is_float {
            let cleaned: String = float_text.chars().filter(|c| *c != '_').collect();
            return match cleaned.parse::<f64>() {
                Ok(v) if v.is_finite() => TokenKind::Float(v),
                _ => {
                    self.error(Code::E001, span, "float literal is out of range");
                    TokenKind::Float(0.0)
                }
            };
        }

        let cleaned: String = int_text.chars().filter(|c| *c != '_').collect();
        if cleaned.len() > 1 && cleaned.starts_with('0') {
            self.error(
                Code::E001,
                span,
                "leading zeros are not allowed (write 0 for zero)",
            );
        }
        match cleaned.parse::<u128>() {
            Ok(v) if v <= (i64::MAX as u128) + 1 => TokenKind::Int(v as i128),
            _ => {
                self.error(Code::E001, span, "integer literal is out of range");
                TokenKind::Int(0)
            }
        }
    }

    fn scan_digits(&mut self, radix: u32, span: Span) -> String {
        let mut out = String::new();
        let mut prev_digit = false;
        loop {
            let Some(ch) = self.peek() else { break };
            let is_digit = match radix {
                2 => matches!(ch, '0'..='1'),
                16 => ch.is_ascii_hexdigit(),
                _ => ch.is_ascii_digit(),
            };
            if is_digit {
                out.push(ch);
                prev_digit = true;
                self.bump();
            } else if ch == '_' {
                let next_ok = match radix {
                    2 => matches!(self.peek_n(1), Some(b'0'..=b'1')),
                    16 => matches!(
                        self.peek_n(1),
                        Some(b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F')
                    ),
                    _ => matches!(self.peek_n(1), Some(b'0'..=b'9')),
                };
                if !prev_digit || !next_ok {
                    self.error(
                        Code::E001,
                        span,
                        "underscores may only appear between digits",
                    );
                }
                prev_digit = false;
                self.bump();
            } else {
                break;
            }
        }
        out
    }

    fn string(&mut self) -> TokenKind {
        let open = self.pos;
        self.bump(); // opening quote
        let mut parts = Vec::new();
        let mut text = String::new();
        loop {
            let Some(ch) = self.peek() else {
                self.error(
                    Code::E001,
                    Span::new(open as u32, self.pos as u32),
                    "unterminated string",
                );
                break;
            };
            match ch {
                '"' => {
                    self.bump();
                    break;
                }
                '\n' | '\r' => {
                    self.error(
                        Code::E001,
                        Span::new(open as u32, self.pos as u32),
                        "strings may not contain raw newlines",
                    );
                    break;
                }
                '\\' => {
                    let escape_start = self.pos;
                    self.bump();
                    match self.peek() {
                        Some('n') => {
                            self.bump();
                            text.push('\n');
                        }
                        Some('r') => {
                            self.bump();
                            text.push('\r');
                        }
                        Some('t') => {
                            self.bump();
                            text.push('\t');
                        }
                        Some('0') => {
                            self.bump();
                            text.push('\0');
                        }
                        Some('\\') => {
                            self.bump();
                            text.push('\\');
                        }
                        Some('"') => {
                            self.bump();
                            text.push('"');
                        }
                        Some('{') => {
                            self.bump();
                            text.push('{');
                        }
                        Some('}') => {
                            self.bump();
                            text.push('}');
                        }
                        Some('u') => {
                            self.bump();
                            if self.peek() != Some('{') {
                                self.error(
                                    Code::E001,
                                    Span::new(escape_start as u32, self.pos as u32),
                                    "expected `{` after \\u",
                                );
                                continue;
                            }
                            self.bump();
                            let hex_start = self.pos;
                            while matches!(self.peek(), Some(c) if c.is_ascii_hexdigit()) {
                                self.bump();
                            }
                            let hex = &self.src[hex_start..self.pos];
                            if self.peek() != Some('}') || hex.is_empty() || hex.len() > 6 {
                                self.error(
                                    Code::E001,
                                    Span::new(escape_start as u32, self.pos as u32),
                                    "malformed \\u{...} escape",
                                );
                                while let Some(c) = self.peek() {
                                    if c == '}' {
                                        self.bump();
                                        break;
                                    }
                                    if c == '"' || c == '\n' {
                                        break;
                                    }
                                    self.bump();
                                }
                                continue;
                            }
                            self.bump();
                            let value = u32::from_str_radix(hex, 16).ok();
                            match value.and_then(char::from_u32) {
                                Some(c) => text.push(c),
                                None => self.error(
                                    Code::E001,
                                    Span::new(escape_start as u32, self.pos as u32),
                                    "\\u{...} is not a Unicode scalar value",
                                ),
                            }
                        }
                        _ => {
                            self.error(
                                Code::E001,
                                Span::new(escape_start as u32, self.pos as u32),
                                "unknown escape sequence",
                            );
                            if let Some(c) = self.bump() {
                                text.push(c);
                            }
                        }
                    }
                }
                '{' => {
                    let brace_start = self.pos;
                    self.bump();
                    if self.peek() == Some('}') {
                        self.error(
                            Code::E001,
                            Span::new(brace_start as u32, (self.pos + 1) as u32),
                            "empty interpolation is not allowed",
                        );
                        self.bump();
                        continue;
                    }
                    let expr_start = self.pos;
                    match match_brace(self.src, self.pos, self.end, self.limits.nesting_depth) {
                        Ok(close) => {
                            let expr_span = Span::new(expr_start as u32, close as u32);
                            if self.src[expr_start..close].trim().is_empty() {
                                self.error(
                                    Code::E001,
                                    Span::new(brace_start as u32, (close + 1) as u32),
                                    "empty interpolation is not allowed",
                                );
                            } else {
                                if !text.is_empty() {
                                    parts.push(StrPart::Text(std::mem::take(&mut text)));
                                }
                                parts.push(StrPart::Expr(expr_span));
                            }
                            self.pos = close + 1;
                        }
                        Err(BraceError::TooDeep) => {
                            self.error(
                                Code::E002,
                                Span::new(brace_start as u32, self.pos as u32),
                                format!(
                                    "interpolation nesting exceeds the limit of {}",
                                    self.limits.nesting_depth
                                ),
                            );
                            break;
                        }
                        Err(BraceError::Unterminated) => {
                            self.error(
                                Code::E001,
                                Span::new(brace_start as u32, self.pos as u32),
                                "unterminated interpolation",
                            );
                            break;
                        }
                    }
                }
                '}' => {
                    self.error(
                        Code::E001,
                        Span::new(self.pos as u32, (self.pos + 1) as u32),
                        "unescaped `}` in string; write `\\}`",
                    );
                    self.bump();
                }
                c => {
                    self.bump();
                    text.push(c);
                }
            }
        }
        if !text.is_empty() {
            parts.push(StrPart::Text(text));
        }
        TokenKind::Str(Rc::new(StrLit { parts }))
    }

    fn punctuation(&mut self) {
        let start = self.pos;
        let ch = self.bump().unwrap();
        let one = |lexer: &mut Self, kind: TokenKind| {
            lexer.push(kind, Span::new(start as u32, lexer.pos as u32));
        };
        let two = |lexer: &mut Self, expected: char, yes: TokenKind, no: TokenKind| {
            if lexer.peek() == Some(expected) {
                lexer.bump();
                lexer.push(yes, Span::new(start as u32, lexer.pos as u32));
            } else {
                lexer.push(no, Span::new(start as u32, lexer.pos as u32));
            }
        };
        match ch {
            '(' => {
                self.enter_bracket(Span::new(start as u32, self.pos as u32));
                one(self, TokenKind::LParen);
            }
            ')' => {
                self.leave_bracket();
                one(self, TokenKind::RParen);
            }
            '[' => {
                self.enter_bracket(Span::new(start as u32, self.pos as u32));
                one(self, TokenKind::LBracket);
            }
            ']' => {
                self.leave_bracket();
                one(self, TokenKind::RBracket);
            }
            '{' => {
                self.enter_bracket(Span::new(start as u32, self.pos as u32));
                one(self, TokenKind::LBrace);
            }
            '}' => {
                self.leave_bracket();
                one(self, TokenKind::RBrace);
            }
            ',' => one(self, TokenKind::Comma),
            ':' => one(self, TokenKind::Colon),
            '|' => one(self, TokenKind::Pipe),
            '+' => two(self, '=', TokenKind::PlusEq, TokenKind::Plus),
            '-' => two(self, '>', TokenKind::Arrow, TokenKind::Minus),
            '*' => two(self, '=', TokenKind::StarEq, TokenKind::Star),
            '/' => two(self, '=', TokenKind::SlashEq, TokenKind::Slash),
            '%' => two(self, '=', TokenKind::PercentEq, TokenKind::Percent),
            '=' => {
                if self.peek() == Some('=') {
                    self.bump();
                    self.push(TokenKind::EqEq, Span::new(start as u32, self.pos as u32));
                } else if self.peek() == Some('>') {
                    self.bump();
                    self.push(
                        TokenKind::FatArrow,
                        Span::new(start as u32, self.pos as u32),
                    );
                } else {
                    one(self, TokenKind::Eq);
                }
            }
            '!' => {
                if self.peek() == Some('=') {
                    self.bump();
                    self.push(TokenKind::NotEq, Span::new(start as u32, self.pos as u32));
                } else {
                    self.error(
                        Code::E001,
                        Span::new(start as u32, self.pos as u32),
                        "unexpected `!`; negation is `not`",
                    );
                }
            }
            '<' => two(self, '=', TokenKind::Le, TokenKind::Lt),
            '>' => two(self, '=', TokenKind::Ge, TokenKind::Gt),
            '?' => {
                if self.peek() == Some('?') {
                    self.bump();
                    self.push(
                        TokenKind::QuestionQuestion,
                        Span::new(start as u32, self.pos as u32),
                    );
                } else {
                    one(self, TokenKind::Question);
                }
            }
            '.' => {
                if self.peek() == Some('.') {
                    self.bump();
                    if self.peek() == Some('=') {
                        self.bump();
                        self.push(
                            TokenKind::DotDotEq,
                            Span::new(start as u32, self.pos as u32),
                        );
                    } else {
                        self.push(TokenKind::DotDot, Span::new(start as u32, self.pos as u32));
                    }
                } else {
                    self.postfix_dot = self
                        .last_sig
                        .as_ref()
                        .is_some_and(TokenKind::can_end_postfix);
                    one(self, TokenKind::Dot);
                }
            }
            '\u{feff}' if start == 0 => {}
            c if c.is_whitespace() => {}
            '\0' => self.error(
                Code::E001,
                Span::new(start as u32, self.pos as u32),
                "NUL byte is not allowed",
            ),
            c => {
                self.error(
                    Code::E001,
                    Span::new(start as u32, self.pos as u32),
                    format!("unexpected character `{c}`"),
                );
            }
        }
    }
}

fn keyword(text: &str) -> Option<TokenKind> {
    Some(match text {
        "fn" => TokenKind::Fn,
        "let" => TokenKind::Let,
        "var" => TokenKind::Var,
        "const" => TokenKind::Const,
        "struct" => TokenKind::Struct,
        "enum" => TokenKind::Enum,
        "impl" => TokenKind::Impl,
        "use" => TokenKind::Use,
        "as" => TokenKind::As,
        "pub" => TokenKind::Pub,
        "test" => TokenKind::Test,
        "on" => TokenKind::On,
        "if" => TokenKind::If,
        "else" => TokenKind::Else,
        "match" => TokenKind::Match,
        "while" => TokenKind::While,
        "for" => TokenKind::For,
        "in" => TokenKind::In,
        "return" => TokenKind::Return,
        "break" => TokenKind::Break,
        "continue" => TokenKind::Continue,
        "mut" => TokenKind::Mut,
        "self" => TokenKind::SelfKw,
        "and" => TokenKind::And,
        "or" => TokenKind::Or,
        "not" => TokenKind::Not,
        "true" => TokenKind::True,
        "false" => TokenKind::False,
        _ => return None,
    })
}

fn classify(text: &str) -> IdentClass {
    if is_snake(text) {
        return IdentClass::Snake;
    }
    if is_screaming(text) {
        return IdentClass::Screaming;
    }
    if is_pascal(text) {
        return IdentClass::Pascal;
    }
    IdentClass::Invalid
}

fn is_snake(text: &str) -> bool {
    let body = text.strip_prefix('_').unwrap_or(text);
    let mut chars = body.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn is_pascal(text: &str) -> bool {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    let rest: Vec<char> = chars.collect();
    if !rest.iter().all(|c| c.is_ascii_alphanumeric()) {
        return false;
    }
    text.len() == 1 || rest.iter().any(|c| c.is_ascii_lowercase())
}

fn is_screaming(text: &str) -> bool {
    if text.len() < 2 {
        return false;
    }
    let mut chars = text.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BraceError {
    Unterminated,
    TooDeep,
}

/// Find the `}` matching an interpolation that started before `start`.
/// Returns the byte index of the closing brace.
///
/// `remaining` bounds nested string interpolations. Braces inside an ordinary
/// expression are counted iteratively, so only a nested string literal
/// consumes stack depth.
fn match_brace(src: &str, start: usize, end: usize, remaining: usize) -> Result<usize, BraceError> {
    let bytes = src.as_bytes();
    let mut i = start;
    let mut brace_depth = 1usize;
    while i < end {
        match bytes[i] {
            b'{' => {
                brace_depth += 1;
                i += 1;
            }
            b'}' => {
                brace_depth -= 1;
                if brace_depth == 0 {
                    return Ok(i);
                }
                i += 1;
            }
            b'"' => {
                i = skip_string(src, i, end, remaining)?;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i += 2;
                while i < end && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'\n' | b'\r' => return Err(BraceError::Unterminated),
            _ => {
                i += src[i..end].chars().next().map(char::len_utf8).unwrap_or(1);
            }
        }
    }
    Err(BraceError::Unterminated)
}

fn skip_string(src: &str, start: usize, end: usize, remaining: usize) -> Result<usize, BraceError> {
    let bytes = src.as_bytes();
    let mut i = start + 1;
    while i < end {
        match bytes[i] {
            b'\\' => {
                i += 1;
                if i >= end {
                    return Err(BraceError::Unterminated);
                }
                let escaped = src[i..end].chars().next().ok_or(BraceError::Unterminated)?;
                i += escaped.len_utf8();
                // `\u{...}` may contain braces; skip the whole escape so the
                // scanner does not mistake it for an interpolation.
                if escaped == 'u' && bytes.get(i) == Some(&b'{') {
                    i += 1;
                    while i < end && bytes[i] != b'}' {
                        i += src[i..end].chars().next().map(char::len_utf8).unwrap_or(1);
                    }
                    if i < end {
                        i += 1;
                    }
                }
            }
            b'"' => return Ok(i + 1),
            b'{' => {
                if remaining == 0 {
                    return Err(BraceError::TooDeep);
                }
                let close = match_brace(src, i + 1, end, remaining - 1)?;
                i = close + 1;
            }
            b'\n' | b'\r' => return Err(BraceError::Unterminated),
            _ => {
                i += src[i..end].chars().next().map(char::len_utf8).unwrap_or(1);
            }
        }
    }
    Err(BraceError::Unterminated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(src, LoadLimits::default())
            .tokens
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn lex_basic() {
        let ks = kinds("let x = 1_000 + 0xFF");
        assert_eq!(
            ks,
            vec![
                TokenKind::Let,
                TokenKind::Ident("x".into(), IdentClass::Snake),
                TokenKind::Eq,
                TokenKind::Int(1000),
                TokenKind::Plus,
                TokenKind::Int(255),
                TokenKind::Eof
            ]
        );
    }

    #[test]
    fn range_vs_float_vs_fields() {
        assert_eq!(
            kinds("1..5")[..3],
            [TokenKind::Int(1), TokenKind::DotDot, TokenKind::Int(5)]
        );
        assert_eq!(kinds("1.5")[0], TokenKind::Float(1.5));
        assert_eq!(
            kinds("t.0.1")[..5],
            [
                TokenKind::Ident("t".into(), IdentClass::Snake),
                TokenKind::Dot,
                TokenKind::Int(0),
                TokenKind::Dot,
                TokenKind::Int(1)
            ]
        );
    }

    #[test]
    fn interpolation_parts() {
        let ks = kinds("\"a {x + 1} b\"");
        let TokenKind::Str(lit) = &ks[0] else {
            panic!("not a string");
        };
        assert_eq!(lit.parts.len(), 3);
        assert!(matches!(&lit.parts[0], StrPart::Text(t) if t == "a "));
        assert!(matches!(&lit.parts[1], StrPart::Expr(_)));
        assert!(matches!(&lit.parts[2], StrPart::Text(t) if t == " b"));
    }

    #[test]
    fn casing_errors() {
        let out = lex("let fooBar = 1", LoadLimits::default());
        assert_eq!(out.diagnostics[0].code, Code::E104);
    }

    #[test]
    fn bad_escape() {
        let out = lex("\"\\q\"", LoadLimits::default());
        assert_eq!(out.diagnostics[0].code, Code::E001);
    }

    #[test]
    fn escaped_non_ascii_inside_interpolation_does_not_panic() {
        let out = lex(r#""{"\é"}""#, LoadLimits::default());
        assert!(!out.tokens.is_empty());
    }

    #[test]
    fn invalid_fragment_span_is_a_diagnostic() {
        let out = lex_fragment("abc", Span::new(3, 1), LoadLimits::default());
        assert_eq!(out.diagnostics[0].code, Code::E001);
    }

    #[test]
    fn radix_aware_underscore_validation() {
        let out = lex("1_a", LoadLimits::default());
        assert!(out.diagnostics.iter().any(|d| d.code == Code::E001));
        let out = lex("0b1_a", LoadLimits::default());
        assert!(out.diagnostics.iter().any(|d| d.code == Code::E001));
    }
}
