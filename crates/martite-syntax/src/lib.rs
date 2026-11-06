//! Lexer, AST, and parser for Martite.

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod token;

pub use ast::*;
pub use lexer::{Lexed, lex, lex_fragment};
pub use parser::{
    ParenthesisTables, ParseOptions, ParseOutput, Program, parse_interface,
    parse_interface_normalized, parse_interface_normalized_with_tokens, parse_interface_with,
    parse_source, parse_source_normalized, parse_source_normalized_with_tokens, parse_source_with,
};
pub use token::{Comment, IdentClass, StrLit, StrPart, Token, TokenKind, TypeName};
