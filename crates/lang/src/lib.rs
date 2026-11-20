mod ast;
mod lexer;
mod parse_error;
mod parser;
mod token;

pub use ast::{Accessor, Arg, BinOp, Call, Expr, InterpPart, Pipeline, Program, Stage, Stmt, UnOp};
pub use parse_error::ParseError;

pub fn parse(source: &str) -> Result<Program, ParseError> {
    let tokens = lexer::lex(source)?;
    parser::Parser::new(tokens).program()
}

pub fn parse_expr(source: &str) -> Result<Expr, ParseError> {
    let tokens = lexer::lex(source)?;
    parser::Parser::new(tokens).standalone_expr()
}
