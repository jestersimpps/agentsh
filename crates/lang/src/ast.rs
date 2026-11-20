use agentsh_spec::{Span, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let { name: String, value: Pipeline, span: Span },
    If { cond: Expr, then: Program, otherwise: Option<Program>, span: Span },
    For { var: String, iter: Expr, body: Program, span: Span },
    Try { body: Program, catch_var: Option<String>, handler: Option<Program>, span: Span },
    Pipeline(Pipeline),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pipeline {
    pub head: Stage,
    pub rest: Vec<Call>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    Call(Call),
    Expr(Expr, Span),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub name: String,
    pub args: Vec<Arg>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Positional(Expr),
    Named(String, Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Lit(Value),
    Var(String, Vec<Accessor>),
    Field(Vec<String>),
    Bare(String),
    List(Vec<Expr>),
    Record(Vec<(String, Expr)>),
    Interp(Vec<InterpPart>),
    Unary(UnOp, Box<Expr>),
    Binary(Box<Expr>, BinOp, Box<Expr>),
    Func(String, Vec<Expr>, Span),
    Sub(Box<Pipeline>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Accessor {
    Field(String),
    Index(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum InterpPart {
    Lit(String),
    Expr(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Not,
    Neg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Match,
    NotMatch,
    In,
    Add,
    Sub,
    Mul,
    Div,
    Rem,
}
