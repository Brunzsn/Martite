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
    last_sig_can_end_postfix: bool,
    postfix_dot: bool,
}

impl<'a> Lexer<'a> {
    fn new(src: &'a str, start: usize, end: usize, limits: LoadLimits) -> Self {
        let mut pos = start;
        if pos == 0 && src.starts_with('\u{feff}') {
            pos = '\u{feff}'.len_utf8();
        }
        let end = end.min(src.len());
        let remaining = end.saturating_sub(pos);
        Self {
            src,
            pos,
            end,
            limits,
            out: Lexed {
                tokens: Vec::with_capacity(remaining / 4),
                comments: Vec::with_capacity(remaining / 32),
                diagnostics: Vec::new(),
            },
            depth: 0,
            depth_reported: false,
            last_sig_can_end_postfix: false,
            postfix_dot: false,
        }
    }

    fn run(mut self) -> Lexed {
        let src = self.src;
        let bytes = src.as_bytes();
        while self.pos < self.end {
            let was_postfix_dot = self.postfix_dot;
            self.postfix_dot = false;

            match bytes[self.pos] {
                b' ' | b'\t' | b'\r' => {
                    self.pos += 1;
                }
                b'\n' => {
                    let start = self.pos;
                    self.pos += 1;
                    self.push(TokenKind::Newline, Span::new(start as u32, self.pos as u32));
                }
                b'/' if self.peek_n(1) == Some(b'/') => self.comment(),
                b'"' => {
                    let start = self.pos;
                    let kind = self.string();
                    self.push(kind, Span::new(start as u32, self.pos as u32));
                }
                b'0'..=b'9' => {
                    let start = self.pos;
                    let kind = self.number(was_postfix_dot);
                    self.push(kind, Span::new(start as u32, self.pos as u32));
                }
                byte if byte.is_ascii_alphabetic() || byte == b'_' => self.identifier(),
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

    #[inline]
    fn peek_byte(&self) -> Option<u8> {
        if self.pos < self.end {
            self.src.as_bytes().get(self.pos).copied()
        } else {
            None
        }
    }

    #[inline]
    fn peek(&self) -> Option<char> {
        let byte = self.peek_byte()?;
        if byte.is_ascii() {
            Some(byte as char)
        } else {
            self.src[self.pos..self.end].chars().next()
        }
    }

    fn peek_n(&self, n: usize) -> Option<u8> {
        self.src.as_bytes().get(self.pos + n).copied()
    }

    #[inline]
    fn bump(&mut self) -> Option<char> {
        let byte = self.peek_byte()?;
        if byte.is_ascii() {
            self.pos += 1;
            Some(byte as char)
        } else {
            let ch = self.src[self.pos..self.end].chars().next()?;
            self.pos += ch.len_utf8();
            Some(ch)
        }
    }

    fn push(&mut self, kind: TokenKind, span: Span) {
        if !matches!(kind, TokenKind::Newline) {
            self.last_sig_can_end_postfix = kind.can_end_postfix();
        }
        self.out.tokens.push(Token { kind, span });
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
        while let Some(byte) = self.peek_byte() {
            if byte == b'\n' {
                break;
            }
            self.pos += 1;
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
        let bytes = self.src.as_bytes();
        while self.pos < self.end {
            let byte = bytes[self.pos];
            if byte.is_ascii_alphanumeric() || byte == b'_' {
                self.pos += 1;
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
            while matches!(self.peek_byte(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
            span.end = self.pos as u32;
            let digits = &self.src.as_bytes()[digits_start..self.pos];
            return match parse_digits(digits, 10) {
                Some(v) if v <= i128::MAX as u128 => TokenKind::Int(v as i128),
                _ => {
                    self.error(Code::E001, span, "tuple index is out of range");
                    TokenKind::Int(0)
                }
            };
        }

        let mut radix = 10;
        if self.peek_byte() == Some(b'0') {
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
            let digits_start = self.pos;
            let digits = self.scan_digits(radix, span);
            if digits == 0 {
                self.error(Code::E001, span, "expected digits after base prefix");
                return TokenKind::Int(0);
            }
            let bytes = &self.src.as_bytes()[digits_start..self.pos];
            span.end = self.pos as u32;
            return match parse_digits(bytes, radix) {
                Some(v) if v <= (i64::MAX as u128) + 1 => TokenKind::Int(v as i128),
                _ => {
                    self.error(Code::E001, span, "integer literal is out of range");
                    TokenKind::Int(0)
                }
            };
        }

        let int_start = self.pos;
        let int_digits = self.scan_digits(10, span);
        let mut is_float = false;

        if self.peek_byte() == Some(b'.') && matches!(self.peek_n(1), Some(b'0'..=b'9')) {
            is_float = true;
            self.bump();
            let digits = self.scan_digits(10, span);
            if digits == 0 {
                self.error(Code::E001, span, "expected digits after `.`");
            }
        }

        if matches!(self.peek_byte(), Some(b'e') | Some(b'E')) {
            let save = self.pos;
            self.bump();
            if matches!(self.peek_byte(), Some(b'+') | Some(b'-')) {
                self.bump();
            }
            let digits = self.scan_digits(10, span);
            if digits == 0 {
                self.pos = save;
                self.error(
                    Code::E001,
                    Span::new(start as u32, self.pos as u32),
                    "malformed exponent",
                );
            } else {
                is_float = true;
            }
        }

        span.end = self.pos as u32;
        if is_float {
            return match parse_float(&self.src[start..self.pos]) {
                Some(v) if v.is_finite() => TokenKind::Float(v),
                _ => {
                    self.error(Code::E001, span, "float literal is out of range");
                    TokenKind::Float(0.0)
                }
            };
        }

        let int_bytes = &self.src.as_bytes()[int_start..self.pos];
        if int_digits > 1 && int_bytes.first() == Some(&b'0') {
            self.error(
                Code::E001,
                span,
                "leading zeros are not allowed (write 0 for zero)",
            );
        }
        match parse_digits(int_bytes, 10) {
            Some(v) if v <= (i64::MAX as u128) + 1 => TokenKind::Int(v as i128),
            _ => {
                self.error(Code::E001, span, "integer literal is out of range");
                TokenKind::Int(0)
            }
        }
    }

    fn scan_digits(&mut self, radix: u32, span: Span) -> usize {
        let mut digits = 0;
        let mut prev_digit = false;
        while let Some(byte) = self.peek_byte() {
            if is_digit_for(byte, radix) {
                digits += 1;
                prev_digit = true;
                self.pos += 1;
            } else if byte == b'_' {
                let next_ok = match radix {
                    2 => matches!(self.peek_n(1), Some(b'0' | b'1')),
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
                self.pos += 1;
            } else {
                break;
            }
        }
        digits
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
                    self.postfix_dot = self.last_sig_can_end_postfix;
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

#[inline]
fn is_digit_for(byte: u8, radix: u32) -> bool {
    match radix {
        2 => matches!(byte, b'0'..=b'1'),
        16 => byte.is_ascii_hexdigit(),
        _ => byte.is_ascii_digit(),
    }
}

fn digit_value(byte: u8, radix: u32) -> Option<u32> {
    let value = match byte {
        b'0'..=b'9' => (byte - b'0') as u32,
        b'a'..=b'f' => (byte - b'a' + 10) as u32,
        b'A'..=b'F' => (byte - b'A' + 10) as u32,
        _ => return None,
    };
    (value < radix).then_some(value)
}

fn parse_digits(bytes: &[u8], radix: u32) -> Option<u128> {
    let mut value = 0u128;
    for &byte in bytes {
        if byte == b'_' {
            continue;
        }
        let digit = digit_value(byte, radix)?;
        value = value
            .checked_mul(radix as u128)?
            .checked_add(digit as u128)?;
    }
    Some(value)
}

/// Parse a numeric spelling, ignoring digit separators. Typical literals are
/// parsed straight from the source slice; only long literals with `_` need a
/// heap allocation.
fn parse_float(spelling: &str) -> Option<f64> {
    let bytes = spelling.as_bytes();
    if !bytes.contains(&b'_') {
        return spelling.parse::<f64>().ok();
    }
    const STACK: usize = 64;
    let mut scratch = [0u8; STACK];
    let mut len = 0;
    let mut fits = true;
    for &byte in bytes {
        if byte == b'_' {
            continue;
        }
        if len == STACK {
            fits = false;
            break;
        }
        scratch[len] = byte;
        len += 1;
    }
    if fits {
        std::str::from_utf8(&scratch[..len])
            .ok()?
            .parse::<f64>()
            .ok()
    } else {
        let cleaned: String = spelling.chars().filter(|c| *c != '_').collect();
        cleaned.parse::<f64>().ok()
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
    let mut has_lowercase = false;
    for c in chars {
        if !c.is_ascii_alphanumeric() {
            return false;
        }
        has_lowercase |= c.is_ascii_lowercase();
    }
    text.len() == 1 || has_lowercase
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
    fn numeric_literals_parse_directly() {
        assert_eq!(kinds("0")[0], TokenKind::Int(0));
        assert_eq!(kinds("0xFF")[0], TokenKind::Int(255));
        assert_eq!(kinds("0b1010_1010")[0], TokenKind::Int(170));
        assert_eq!(kinds("1_000_000")[0], TokenKind::Int(1_000_000));
        assert_eq!(
            kinds("9223372036854775807")[0],
            TokenKind::Int(i64::MAX as i128)
        );
        assert_eq!(
            kinds("9223372036854775808")[0],
            TokenKind::Int((i64::MAX as i128) + 1)
        );
        assert_eq!(
            kinds("0x8000000000000000")[0],
            TokenKind::Int((i64::MAX as i128) + 1)
        );
        assert_eq!(kinds("1.5")[0], TokenKind::Float(1.5));
        assert_eq!(kinds("1_0.5_0e1_0")[0], TokenKind::Float(105_000_000_000.0));
        assert_eq!(kinds("1E+5")[0], TokenKind::Float(100_000.0));
        assert_eq!(kinds("1e-2")[0], TokenKind::Float(0.01));
    }

    #[test]
    fn numeric_errors_keep_their_spans() {
        let cases = [
            ("00", Span::new(0, 2), "leading zeros"),
            ("0_1", Span::new(0, 3), "leading zeros"),
            (
                "9223372036854775809",
                Span::new(0, 19),
                "integer literal is out of range",
            ),
            (
                "0x8000000000000001",
                Span::new(0, 18),
                "integer literal is out of range",
            ),
            ("1e", Span::new(0, 1), "malformed exponent"),
            ("1e+", Span::new(0, 1), "malformed exponent"),
            ("1e999", Span::new(0, 5), "float literal is out of range"),
            ("0x", Span::new(0, 0), "expected digits after base prefix"),
            (
                "t.170141183460469231731687303715884105728",
                Span::new(2, 41),
                "tuple index is out of range",
            ),
        ];
        for (src, span, message) in cases {
            let out = lex(src, LoadLimits::default());
            let diagnostic = out
                .diagnostics
                .iter()
                .find(|diagnostic| diagnostic.message.contains(message))
                .unwrap_or_else(|| panic!("{src}: missing {message:?}: {:?}", out.diagnostics));
            assert_eq!(diagnostic.code, Code::E001, "{src}");
            assert_eq!(diagnostic.span, Some(span), "{src}: {}", diagnostic.message);
        }
    }

    #[test]
    fn numeric_underscore_errors_keep_zero_width_spans() {
        let out = lex("1__0", LoadLimits::default());
        let spans: Vec<_> = out.diagnostics.iter().map(|d| d.span).collect();
        assert_eq!(spans, vec![Some(Span::new(0, 0)), Some(Span::new(0, 0))]);
    }

    #[test]
    fn postfix_state_survives_newlines_and_comments() {
        for src in [
            "t.9223372036854775809",
            "t\n.9223372036854775809",
            "t // comment\n.9223372036854775809",
        ] {
            let out = lex(src, LoadLimits::default());
            assert!(out.diagnostics.is_empty(), "{src}: {:?}", out.diagnostics);
        }
    }

    #[test]
    fn non_ascii_comments_and_bom_are_handled() {
        let out = lex(
            "\u{feff}fn main() { let x = \"héllo\" } // ünïcode\n",
            LoadLimits::default(),
        );
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
    }

    #[test]
    fn non_ascii_outside_strings_is_an_error() {
        let out = lex("let café = 1", LoadLimits::default());
        assert!(out.diagnostics.iter().any(|d| d.code == Code::E001));
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
