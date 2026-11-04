use std::fmt;
use std::rc::Rc;

use martite_base::Span;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeName {
    Int,
    Float,
    Bool,
    Str,
    List,
    Map,
    Range,
}

impl TypeName {
    pub const fn as_str(self) -> &'static str {
        match self {
            TypeName::Int => "int",
            TypeName::Float => "float",
            TypeName::Bool => "bool",
            TypeName::Str => "str",
            TypeName::List => "list",
            TypeName::Map => "map",
            TypeName::Range => "range",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "int" => TypeName::Int,
            "float" => TypeName::Float,
            "bool" => TypeName::Bool,
            "str" => TypeName::Str,
            "list" => TypeName::List,
            "map" => TypeName::Map,
            "range" => TypeName::Range,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdentClass {
    Snake,
    Pascal,
    Screaming,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrLit {
    pub parts: Vec<StrPart>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrPart {
    Text(String),
    /// The span excludes the surrounding braces.
    Expr(Span),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Keywords.
    Fn,
    Let,
    Var,
    Const,
    Struct,
    Enum,
    Impl,
    Use,
    As,
    Pub,
    Test,
    On,
    If,
    Else,
    Match,
    While,
    For,
    In,
    Return,
    Break,
    Continue,
    Mut,
    SelfKw,
    And,
    Or,
    Not,
    True,
    False,

    TypeName(TypeName),
    Ident(String, IdentClass),
    Int(i128),
    Float(f64),
    Str(Rc<StrLit>),
    Newline,
    Eof,

    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Dot,
    DotDot,
    DotDotEq,
    Question,
    QuestionQuestion,
    Arrow,
    FatArrow,
    Pipe,
    Underscore,

    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eq,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
}

impl TokenKind {
    pub fn keyword_text(&self) -> Option<&'static str> {
        Some(match self {
            TokenKind::Fn => "fn",
            TokenKind::Let => "let",
            TokenKind::Var => "var",
            TokenKind::Const => "const",
            TokenKind::Struct => "struct",
            TokenKind::Enum => "enum",
            TokenKind::Impl => "impl",
            TokenKind::Use => "use",
            TokenKind::As => "as",
            TokenKind::Pub => "pub",
            TokenKind::Test => "test",
            TokenKind::On => "on",
            TokenKind::If => "if",
            TokenKind::Else => "else",
            TokenKind::Match => "match",
            TokenKind::While => "while",
            TokenKind::For => "for",
            TokenKind::In => "in",
            TokenKind::Return => "return",
            TokenKind::Break => "break",
            TokenKind::Continue => "continue",
            TokenKind::Mut => "mut",
            TokenKind::SelfKw => "self",
            TokenKind::And => "and",
            TokenKind::Or => "or",
            TokenKind::Not => "not",
            TokenKind::True => "true",
            TokenKind::False => "false",
            _ => return None,
        })
    }

    pub fn is_ident(&self) -> bool {
        matches!(self, TokenKind::Ident(..))
    }

    pub fn ident_text(&self) -> Option<&str> {
        match self {
            TokenKind::Ident(s, _) => Some(s),
            _ => None,
        }
    }

    /// Tokens that can end a postfix chain; the lexer uses this to tell
    /// `t.0.1` (two field accesses) from `0.1` (a float).
    pub fn can_end_postfix(&self) -> bool {
        matches!(
            self,
            TokenKind::Ident(..)
                | TokenKind::Int(_)
                | TokenKind::Float(_)
                | TokenKind::Str(_)
                | TokenKind::True
                | TokenKind::False
                | TokenKind::SelfKw
                | TokenKind::TypeName(_)
                | TokenKind::RParen
                | TokenKind::RBracket
                | TokenKind::RBrace
                | TokenKind::Question
        )
    }
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(kw) = self.keyword_text() {
            return f.write_str(kw);
        }
        let text = match self {
            TokenKind::TypeName(t) => return f.write_str(t.as_str()),
            TokenKind::Ident(s, _) => return f.write_str(s),
            TokenKind::Int(v) => return write!(f, "{v}"),
            TokenKind::Float(v) => return write!(f, "{v:?}"),
            TokenKind::Str(_) => "\"…\"",
            TokenKind::Newline => "newline",
            TokenKind::Eof => "end of file",
            TokenKind::LParen => "(",
            TokenKind::RParen => ")",
            TokenKind::LBracket => "[",
            TokenKind::RBracket => "]",
            TokenKind::LBrace => "{",
            TokenKind::RBrace => "}",
            TokenKind::Comma => ",",
            TokenKind::Colon => ":",
            TokenKind::Dot => ".",
            TokenKind::DotDot => "..",
            TokenKind::DotDotEq => "..=",
            TokenKind::Question => "?",
            TokenKind::QuestionQuestion => "??",
            TokenKind::Arrow => "->",
            TokenKind::FatArrow => "=>",
            TokenKind::Pipe => "|",
            TokenKind::Underscore => "_",
            TokenKind::Plus => "+",
            TokenKind::Minus => "-",
            TokenKind::Star => "*",
            TokenKind::Slash => "/",
            TokenKind::Percent => "%",
            TokenKind::Eq => "=",
            TokenKind::EqEq => "==",
            TokenKind::NotEq => "!=",
            TokenKind::Lt => "<",
            TokenKind::Le => "<=",
            TokenKind::Gt => ">",
            TokenKind::Ge => ">=",
            TokenKind::PlusEq => "+=",
            TokenKind::MinusEq => "-=",
            TokenKind::StarEq => "*=",
            TokenKind::SlashEq => "/=",
            TokenKind::PercentEq => "%=",
            _ => unreachable!("keywords are handled above"),
        };
        f.write_str(text)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub span: Span,
    pub text: String,
    pub doc: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display() {
        assert_eq!(TokenKind::DotDotEq.to_string(), "..=");
        assert_eq!(TokenKind::TypeName(TypeName::Int).to_string(), "int");
    }
}
