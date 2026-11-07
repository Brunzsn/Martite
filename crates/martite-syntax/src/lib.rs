//! Lexer, AST, and parser for Martite.

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod token;

pub use ast::*;
pub use lexer::{Lexed, lex, lex_fragment};
pub use parser::{
    NodeIdAllocator, ParenthesisTables, ParseOptions, ParseOutput, Program, parse_interface,
    parse_interface_normalized, parse_interface_normalized_with_options,
    parse_interface_normalized_with_tokens, parse_interface_normalized_with_tokens_with_options,
    parse_interface_with, parse_interface_with_allocator, parse_source, parse_source_normalized,
    parse_source_normalized_with_options, parse_source_normalized_with_tokens,
    parse_source_normalized_with_tokens_with_options, parse_source_with,
    parse_source_with_allocator,
};
pub use token::{Comment, IdentClass, StrLit, StrPart, Token, TokenKind, TypeName};
