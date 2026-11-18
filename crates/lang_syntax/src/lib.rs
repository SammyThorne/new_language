//! Lexing and parsing. Turns source text into an [`ast::Module`].
//!
//! The lexer converts indentation into `Indent`, `Dedent` and `Newline` tokens (see
//! docs/03-compiler-architecture.md, "Layout algorithm"), so the parser is an ordinary
//! recursive-descent parser that never looks at whitespace.

pub mod ast;
mod lexer;
mod parser;
pub mod token;

pub use lexer::{lex, lex_fragment};
pub use parser::parse;
