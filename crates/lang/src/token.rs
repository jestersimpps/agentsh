use agentsh_spec::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum StrPart {
    Lit(String),
    Var(String),
    Expr(String, Span),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(String),
    Key(String),
    Func(String),
    Var(String),
    Field(String),
    Bare(String),
    Str(Vec<StrPart>),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    LParen,
    RParen,
    Comma,
    Colon,
    Pipe,
    Semi,
    Newline,
    Assign,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Match,
    NotMatch,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
    pub space_before: bool,
}

impl Tok {
    pub fn describe(&self) -> String {
        match self {
            Tok::Ident(s) => format!("`{s}`"),
            Tok::Key(s) => format!("`{s}:`"),
            Tok::Func(s) => format!("`{s}(`"),
            Tok::Var(s) => format!("`${s}`"),
            Tok::Field(s) => format!("`.{s}`"),
            Tok::Bare(s) => format!("`{s}`"),
            Tok::Str(_) => "a string".into(),
            Tok::Int(i) => i.to_string(),
            Tok::Float(f) => f.to_string(),
            Tok::Bool(b) => b.to_string(),
            Tok::Null => "null".into(),
            Tok::Newline => "end of line".into(),
            Tok::Eof => "end of input".into(),
            other => format!("`{}`", symbol(other)),
        }
    }
}

fn symbol(tok: &Tok) -> &'static str {
    match tok {
        Tok::LBrace => "{",
        Tok::RBrace => "}",
        Tok::LBracket => "[",
        Tok::RBracket => "]",
        Tok::LParen => "(",
        Tok::RParen => ")",
        Tok::Comma => ",",
        Tok::Colon => ":",
        Tok::Pipe => "|",
        Tok::Semi => ";",
        Tok::Assign => "=",
        Tok::Eq => "==",
        Tok::Ne => "!=",
        Tok::Lt => "<",
        Tok::Le => "<=",
        Tok::Gt => ">",
        Tok::Ge => ">=",
        Tok::Match => "=~",
        Tok::NotMatch => "!~",
        Tok::Plus => "+",
        Tok::Minus => "-",
        Tok::Star => "*",
        Tok::Slash => "/",
        Tok::Percent => "%",
        _ => "?",
    }
}
