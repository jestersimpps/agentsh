use crate::ParseError;
use crate::ast::{Accessor, Arg, BinOp, Call, Expr, InterpPart, Pipeline, Program, Stage, Stmt, UnOp};
use crate::token::{StrPart, Tok, Token};
use agentsh_spec::{Span, Value};

const MAX_DEPTH: usize = 256;

thread_local! {
    static DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

struct DepthGuard;

impl DepthGuard {
    fn enter(span: Span) -> Result<DepthGuard, ParseError> {
        let depth = DEPTH.with(|d| {
            d.set(d.get() + 1);
            d.get()
        });
        let guard = DepthGuard;
        if depth > MAX_DEPTH {
            return Err(ParseError::new(format!("nesting deeper than {MAX_DEPTH} levels"), span)
                .with_hint("split the program into smaller steps with let"));
        }
        Ok(guard)
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

const KEYWORDS: &[&str] = &["let", "if", "else", "for", "in", "try", "catch", "and", "or", "not"];

const VALUE_HINT: &str = "strings are quoted (\"text\"), variables start with $ ($name), row fields start with . (.name), functions use parentheses (glob(\"*.rs\")), and a command used as a value goes in parentheses: (ls \"src\")";

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    no_record: bool,
    in_args: bool,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0, no_record: false, in_args: false }
    }

    pub fn program(mut self) -> Result<Program, ParseError> {
        self.block_body(&Tok::Eof)
    }

    pub fn standalone_expr(mut self) -> Result<Expr, ParseError> {
        let expr = self.expr()?;
        self.skip_separators();
        if !self.at(&Tok::Eof) {
            return Err(self.unexpected("end of expression"));
        }
        Ok(expr)
    }

    fn token(&self) -> &Token {
        &self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn peek(&self) -> &Tok {
        &self.token().tok
    }

    fn span(&self) -> Span {
        self.token().span
    }

    fn bump(&mut self) -> Tok {
        let tok = self.peek().clone();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        tok
    }

    fn at(&self, tok: &Tok) -> bool {
        self.peek() == tok
    }

    fn at_ident(&self, word: &str) -> bool {
        matches!(self.peek(), Tok::Ident(w) if w == word)
    }

    fn expect(&mut self, tok: Tok, context: &str) -> Result<(), ParseError> {
        if self.at(&tok) {
            self.bump();
            Ok(())
        } else {
            Err(self.unexpected(context))
        }
    }

    fn unexpected(&self, expected: &str) -> ParseError {
        ParseError::new(format!("expected {expected}, found {}", self.peek().describe()), self.span())
    }

    fn skip_separators(&mut self) {
        while matches!(self.peek(), Tok::Newline | Tok::Semi) {
            self.bump();
        }
    }

    fn skip_newlines(&mut self) {
        while self.at(&Tok::Newline) {
            self.bump();
        }
    }

    fn peek_past_newlines_is(&self, word: &str) -> bool {
        self.tokens[self.pos..]
            .iter()
            .find(|t| t.tok != Tok::Newline)
            .is_some_and(|t| matches!(&t.tok, Tok::Ident(w) if w == word))
    }

    fn block_body(&mut self, end: &Tok) -> Result<Program, ParseError> {
        let mut stmts = Vec::new();
        loop {
            self.skip_separators();
            if self.at(end) {
                break;
            }
            if self.at(&Tok::Eof) {
                return Err(self.unexpected("`}` to close the block"));
            }
            stmts.push(self.stmt()?);
            if !(matches!(self.peek(), Tok::Newline | Tok::Semi) || self.at(end)) {
                return Err(self.unexpected("a new line or `;` before the next statement"));
            }
        }
        Ok(Program { stmts })
    }

    fn block(&mut self) -> Result<Program, ParseError> {
        let _guard = DepthGuard::enter(self.span())?;
        self.expect(Tok::LBrace, "`{` to start a block")?;
        let saved = std::mem::replace(&mut self.no_record, false);
        let body = self.block_body(&Tok::RBrace)?;
        self.no_record = saved;
        self.expect(Tok::RBrace, "`}`")?;
        Ok(body)
    }

    fn stmt(&mut self) -> Result<Stmt, ParseError> {
        match self.peek().clone() {
            Tok::Ident(w) if w == "let" => self.let_stmt(),
            Tok::Ident(w) if w == "if" => self.if_stmt(),
            Tok::Ident(w) if w == "for" => self.for_stmt(),
            Tok::Ident(w) if w == "try" => self.try_stmt(),
            Tok::Ident(w) if w == "else" || w == "catch" => {
                Err(ParseError::new(format!("`{w}` without a matching block before it"), self.span()))
            }
            _ => Ok(Stmt::Pipeline(self.pipeline()?)),
        }
    }

    fn binding_name(&mut self, context: &str) -> Result<String, ParseError> {
        match self.peek().clone() {
            Tok::Var(name) => {
                self.bump();
                Ok(name)
            }
            Tok::Ident(name) if !KEYWORDS.contains(&name.as_str()) => {
                self.bump();
                Ok(name)
            }
            _ => Err(self.unexpected(context)),
        }
    }

    fn let_stmt(&mut self) -> Result<Stmt, ParseError> {
        let span = self.span();
        self.bump();
        let name = self.binding_name("a variable name after `let`")?;
        if !self.at(&Tok::Assign) {
            return Err(self.unexpected("`=`").with_hint("let name = value"));
        }
        self.bump();
        let value = self.pipeline()?;
        Ok(Stmt::Let { name, value, span })
    }

    fn if_stmt(&mut self) -> Result<Stmt, ParseError> {
        let span = self.span();
        self.bump();
        let cond = self.header_expr()?;
        let then = self.block()?;
        let otherwise = if self.peek_past_newlines_is("else") {
            self.skip_newlines();
            self.bump();
            if self.at_ident("if") { Some(Program { stmts: vec![self.if_stmt()?] }) } else { Some(self.block()?) }
        } else {
            None
        };
        Ok(Stmt::If { cond, then, otherwise, span })
    }

    fn for_stmt(&mut self) -> Result<Stmt, ParseError> {
        let span = self.span();
        self.bump();
        let var = self.binding_name("a loop variable after `for`")?;
        if !self.at_ident("in") {
            return Err(self.unexpected("`in`").with_hint("for $item in [1, 2, 3] { ... }"));
        }
        self.bump();
        let iter = self.header_expr()?;
        let body = self.block()?;
        Ok(Stmt::For { var, iter, body, span })
    }

    fn try_stmt(&mut self) -> Result<Stmt, ParseError> {
        let span = self.span();
        self.bump();
        let body = self.block()?;
        if !self.peek_past_newlines_is("catch") {
            return Ok(Stmt::Try { body, catch_var: None, handler: None, span });
        }
        self.skip_newlines();
        self.bump();
        let catch_var = if self.at(&Tok::LBrace) { None } else { Some(self.binding_name("an error variable or `{`")?) };
        let handler = Some(self.block()?);
        Ok(Stmt::Try { body, catch_var, handler, span })
    }

    fn header_expr(&mut self) -> Result<Expr, ParseError> {
        let saved = std::mem::replace(&mut self.no_record, true);
        let expr = self.expr();
        self.no_record = saved;
        expr
    }

    fn pipeline(&mut self) -> Result<Pipeline, ParseError> {
        let head = match self.peek() {
            Tok::Ident(name) if !KEYWORDS.contains(&name.as_str()) => Stage::Call(self.call()?),
            Tok::Bare(word) if is_qualified(word) => Stage::Call(self.call()?),
            _ => {
                let span = self.span();
                Stage::Expr(self.expr()?, span)
            }
        };
        let mut rest = Vec::new();
        while self.at(&Tok::Pipe) {
            self.bump();
            self.skip_newlines();
            match self.peek() {
                Tok::Ident(name) if !KEYWORDS.contains(&name.as_str()) => rest.push(self.call()?),
                Tok::Bare(word) if is_qualified(word) => rest.push(self.call()?),
                Tok::Ident(word) if KEYWORDS.contains(&word.as_str()) => {
                    return Err(self.unexpected("a command after `|`").with_hint(format!(
                        "`{word}` can't follow a pipe; store the value first: let rows = (...) then {word} ... over $rows"
                    )));
                }
                _ => return Err(self.unexpected("a command after `|`")),
            }
        }
        Ok(Pipeline { head, rest })
    }

    fn at_call_end(&self) -> bool {
        matches!(
            self.peek(),
            Tok::Pipe | Tok::Newline | Tok::Semi | Tok::RBrace | Tok::RParen | Tok::RBracket | Tok::Comma | Tok::Eof
        ) || (self.no_record && self.at(&Tok::LBrace))
    }

    fn call(&mut self) -> Result<Call, ParseError> {
        let span = self.span();
        let name = match self.bump() {
            Tok::Ident(name) => name,
            Tok::Bare(word) if is_qualified(&word) => word,
            _ => return Err(ParseError::new("expected a command name", span)),
        };
        let mut args = Vec::new();
        let saved = std::mem::replace(&mut self.in_args, true);
        while !self.at_call_end() {
            if let Tok::Key(key) = self.peek().clone() {
                self.bump();
                if self.at_call_end() {
                    self.in_args = saved;
                    return Err(self.unexpected(&format!("a value after `{key}:`")));
                }
                args.push(Arg::Named(key, self.expr()?));
            } else if self.at(&Tok::Colon) {
                self.in_args = saved;
                return Err(ParseError::new("stray `:`", self.span())
                    .with_hint("write named arguments as key:value with no spaces"));
            } else {
                args.push(Arg::Positional(self.expr()?));
            }
        }
        self.in_args = saved;
        Ok(Call { name, args, span })
    }

    fn expr(&mut self) -> Result<Expr, ParseError> {
        let _guard = DepthGuard::enter(self.span())?;
        self.or_expr()
    }

    fn or_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.and_expr()?;
        while self.at_ident("or") {
            self.bump();
            self.skip_newlines();
            let right = self.and_expr()?;
            left = Expr::Binary(Box::new(left), BinOp::Or, Box::new(right));
        }
        Ok(left)
    }

    fn and_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.not_expr()?;
        while self.at_ident("and") {
            self.bump();
            self.skip_newlines();
            let right = self.not_expr()?;
            left = Expr::Binary(Box::new(left), BinOp::And, Box::new(right));
        }
        Ok(left)
    }

    fn not_expr(&mut self) -> Result<Expr, ParseError> {
        let _guard = DepthGuard::enter(self.span())?;
        if self.at_ident("not") {
            self.bump();
            return Ok(Expr::Unary(UnOp::Not, Box::new(self.not_expr()?)));
        }
        self.cmp_expr()
    }

    fn cmp_op(&self) -> Option<BinOp> {
        Some(match self.peek() {
            Tok::Eq => BinOp::Eq,
            Tok::Ne => BinOp::Ne,
            Tok::Lt => BinOp::Lt,
            Tok::Le => BinOp::Le,
            Tok::Gt => BinOp::Gt,
            Tok::Ge => BinOp::Ge,
            Tok::Match => BinOp::Match,
            Tok::NotMatch => BinOp::NotMatch,
            Tok::Ident(w) if w == "in" => BinOp::In,
            _ => return None,
        })
    }

    fn cmp_expr(&mut self) -> Result<Expr, ParseError> {
        let left = self.add_expr()?;
        let Some(op) = self.cmp_op() else { return Ok(left) };
        self.bump();
        let right = self.add_expr()?;
        if self.cmp_op().is_some() {
            return Err(ParseError::new("comparisons can't be chained", self.span())
                .with_hint("combine them with `and`: a < b and b < c"));
        }
        Ok(Expr::Binary(Box::new(left), op, Box::new(right)))
    }

    fn add_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.mul_expr()?;
        loop {
            let op = match self.peek() {
                Tok::Plus => BinOp::Add,
                Tok::Minus => BinOp::Sub,
                _ => return Ok(left),
            };
            self.bump();
            let right = self.mul_expr()?;
            left = Expr::Binary(Box::new(left), op, Box::new(right));
        }
    }

    fn mul_expr(&mut self) -> Result<Expr, ParseError> {
        let mut left = self.unary_expr()?;
        loop {
            let op = match self.peek() {
                Tok::Star => BinOp::Mul,
                Tok::Slash => BinOp::Div,
                Tok::Percent => BinOp::Rem,
                _ => return Ok(left),
            };
            self.bump();
            let right = self.unary_expr()?;
            left = Expr::Binary(Box::new(left), op, Box::new(right));
        }
    }

    fn unary_expr(&mut self) -> Result<Expr, ParseError> {
        let _guard = DepthGuard::enter(self.span())?;
        if self.at(&Tok::Minus) {
            self.bump();
            return Ok(Expr::Unary(UnOp::Neg, Box::new(self.unary_expr()?)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, ParseError> {
        let span = self.span();
        match self.peek().clone() {
            Tok::Int(i) => self.lit(Value::Int(i)),
            Tok::Float(f) => self.lit(Value::Float(f)),
            Tok::Bool(b) => self.lit(Value::Bool(b)),
            Tok::Null => self.lit(Value::Null),
            Tok::Str(parts) => {
                self.bump();
                string_expr(parts)
            }
            Tok::Var(name) => {
                self.bump();
                let accessors = self.accessors()?;
                Ok(Expr::Var(name, accessors))
            }
            Tok::Field(path) => {
                self.bump();
                Ok(Expr::Field(path.split('.').map(str::to_owned).collect()))
            }
            Tok::Func(name) => self.func_call(name, span),
            Tok::LParen => self.paren(),
            Tok::LBracket => self.list(),
            Tok::LBrace if !self.no_record => self.record(),
            Tok::Bare(word) => Err(bare_word_error(&word, span)),
            Tok::Ident(name) if self.in_args && !KEYWORDS.contains(&name.as_str()) => {
                self.bump();
                Ok(Expr::Bare(name))
            }
            Tok::Ident(name) => {
                Err(ParseError::new(format!("`{name}` is not a value here"), span).with_hint(VALUE_HINT))
            }
            Tok::Gt | Tok::Lt => Err(ParseError::new("redirection isn't supported", span)
                .with_hint("write files with: write \"path\" content; pipe a value into write: ... | write \"path\"")),
            _ => Err(self.unexpected("a value")),
        }
    }

    fn lit(&mut self, value: Value) -> Result<Expr, ParseError> {
        self.bump();
        Ok(Expr::Lit(value))
    }

    fn accessors(&mut self) -> Result<Vec<crate::ast::Accessor>, ParseError> {
        let mut out = Vec::new();
        loop {
            let token = self.token();
            if token.space_before {
                return Ok(out);
            }
            match token.tok.clone() {
                Tok::Field(path) => {
                    self.bump();
                    out.extend(path.split('.').map(|s| Accessor::Field(s.to_owned())));
                }
                Tok::LBracket => {
                    self.bump();
                    let index = self.expr()?;
                    self.expect(Tok::RBracket, "`]`")?;
                    out.push(Accessor::Index(Box::new(index)));
                }
                _ => return Ok(out),
            }
        }
    }

    fn func_call(&mut self, name: String, span: Span) -> Result<Expr, ParseError> {
        self.bump();
        self.expect(Tok::LParen, "`(`")?;
        let items = self.comma_list(Tok::RParen)?;
        Ok(Expr::Func(name, items, span))
    }

    fn comma_list(&mut self, close: Tok) -> Result<Vec<Expr>, ParseError> {
        let saved = std::mem::replace(&mut self.no_record, false);
        let mut items = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(&close) {
                break;
            }
            items.push(self.expr()?);
            self.skip_newlines();
            if self.at(&Tok::Comma) {
                self.bump();
            } else if !self.at(&close) {
                return Err(self.unexpected("`,` or a closing bracket"));
            }
        }
        self.bump();
        self.no_record = saved;
        Ok(items)
    }

    fn paren(&mut self) -> Result<Expr, ParseError> {
        self.bump();
        let saved_args = std::mem::replace(&mut self.in_args, false);
        let saved = std::mem::replace(&mut self.no_record, false);
        self.skip_newlines();
        let pipeline = self.pipeline()?;
        self.skip_newlines();
        self.no_record = saved;
        self.in_args = saved_args;
        self.expect(Tok::RParen, "`)`")?;
        match pipeline {
            Pipeline { head: Stage::Expr(expr, _), rest } if rest.is_empty() => Ok(expr),
            other => Ok(Expr::Sub(Box::new(other))),
        }
    }

    fn list(&mut self) -> Result<Expr, ParseError> {
        self.bump();
        Ok(Expr::List(self.comma_list(Tok::RBracket)?))
    }

    fn record(&mut self) -> Result<Expr, ParseError> {
        self.bump();
        let saved = std::mem::replace(&mut self.no_record, false);
        let mut entries = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(&Tok::RBrace) {
                break;
            }
            let key = self.record_key()?;
            entries.push((key, self.expr()?));
            self.skip_newlines();
            if self.at(&Tok::Comma) {
                self.bump();
            } else if !self.at(&Tok::RBrace) {
                return Err(self.unexpected("`,` or `}`"));
            }
        }
        self.bump();
        self.no_record = saved;
        Ok(Expr::Record(entries))
    }

    fn record_key(&mut self) -> Result<String, ParseError> {
        match self.bump() {
            Tok::Key(k) => Ok(k),
            Tok::Ident(k) => {
                self.expect(Tok::Colon, "`:` after the record key")?;
                Ok(k)
            }
            Tok::Str(parts) => {
                let key = match parts.as_slice() {
                    [StrPart::Lit(s)] => s.clone(),
                    _ => return Err(ParseError::new("record keys can't use interpolation", self.span())),
                };
                self.expect(Tok::Colon, "`:` after the record key")?;
                Ok(key)
            }
            _ => Err(ParseError::new("expected a record key", self.span()).with_hint("{name: \"x\", size: 1}")),
        }
    }
}

fn string_expr(parts: Vec<StrPart>) -> Result<Expr, ParseError> {
    if let [StrPart::Lit(s)] = parts.as_slice() {
        return Ok(Expr::Lit(Value::Str(s.clone())));
    }
    let mut out = Vec::with_capacity(parts.len());
    for part in parts {
        out.push(match part {
            StrPart::Lit(s) => InterpPart::Lit(s),
            StrPart::Var(name) => InterpPart::Expr(Expr::Var(name, Vec::new())),
            StrPart::Expr(src, span) => {
                InterpPart::Expr(crate::parse_expr(&implicit_var(&src)).map_err(|e| ParseError { span, ..e })?)
            }
        });
    }
    Ok(Expr::Interp(out))
}

fn implicit_var(src: &str) -> String {
    let trimmed = src.trim_start();
    let ident_len = trimmed.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(trimmed.len());
    let starts_with_ident = trimmed.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_');
    let is_call = trimmed[ident_len..].starts_with('(');
    let is_keyword = matches!(&trimmed[..ident_len], "true" | "false" | "null" | "not");
    if starts_with_ident && !is_call && !is_keyword { format!("${trimmed}") } else { src.to_string() }
}

/// A package command name: `package.command`, both plain identifiers.
fn is_qualified(word: &str) -> bool {
    let ident = |s: &str| {
        let mut chars = s.chars();
        chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    matches!(word.split_once('.'), Some((package, command)) if ident(package) && ident(command))
}

fn bare_word_error(word: &str, span: Span) -> ParseError {
    let err = ParseError::new(format!("unquoted word `{word}`"), span);
    if word.starts_with('-') {
        return err.with_hint(
            "commands take key:value arguments, not flags (see help). For exec, quote each argument: exec \"git\" \"log\" \"--oneline\"",
        );
    }
    if word.chars().any(|c| c.is_ascii_digit()) && word.contains(['-', '/', '*']) {
        return err.with_hint(format!("put spaces around operators, or quote it as a string: \"{word}\""));
    }
    err.with_hint(format!("quote it: \"{word}\""))
}

#[cfg(test)]
mod tests {
    use crate::{Arg, Expr, Stage, Stmt, parse};

    #[test]
    fn pipeline_with_where() {
        let program = parse("find \".\" name:\"*.log\" | where .size > 1mb | take 5").unwrap();
        let Stmt::Pipeline(p) = &program.stmts[0] else { panic!() };
        assert_eq!(p.rest.len(), 2);
        let Stage::Call(head) = &p.head else { panic!() };
        assert_eq!(head.name, "find");
        assert!(matches!(&head.args[1], Arg::Named(k, _) if k == "name"));
    }

    #[test]
    fn control_flow() {
        let src = "let n = 0\nfor $f in glob(\"*.rs\") {\n  if $f =~ \"main\" { print $f } else { print \"no\" }\n}\ntry { rm \"x\" } catch $e { print $e.code }";
        let program = parse(src).unwrap();
        assert_eq!(program.stmts.len(), 3);
    }

    #[test]
    fn package_commands_are_qualified_names() {
        let program = parse("js.scripts | where .name == \"test\" | demo.pick_one").unwrap();
        let Stmt::Pipeline(p) = &program.stmts[0] else { panic!() };
        let Stage::Call(head) = &p.head else { panic!() };
        assert_eq!(head.name, "js.scripts");
        assert_eq!(p.rest[1].name, "demo.pick_one");
        assert!(parse("read main.rs").is_err(), "a dotted word in argument position still needs quotes");
    }

    #[test]
    fn bare_word_hint() {
        let err = parse("read src/main.rs").unwrap_err();
        assert_eq!(err.hint.as_deref(), Some("quote it: \"src/main.rs\""));
    }

    #[test]
    fn var_accessors_need_adjacency() {
        let program = parse("print $r.path $x").unwrap();
        let Stmt::Pipeline(p) = &program.stmts[0] else { panic!() };
        let Stage::Call(call) = &p.head else { panic!() };
        assert_eq!(call.args.len(), 2);
        assert!(matches!(&call.args[0], Arg::Positional(Expr::Var(_, acc)) if acc.len() == 1));
    }
}

#[cfg(test)]
mod fuzz {
    use crate::{parse, parse_expr};

    const PIECES: &[&str] = &[
        "read",
        "edit",
        "let",
        "if",
        "else",
        "for",
        "in",
        "try",
        "catch",
        "and",
        "or",
        "not",
        " ",
        " ",
        "\n",
        ";",
        "|",
        "(",
        ")",
        "[",
        "]",
        "{",
        "}",
        ",",
        ":",
        "=",
        "==",
        "!=",
        "<",
        ">",
        "=~",
        "+",
        "-",
        "*",
        "/",
        "%",
        "$x",
        "$r.a",
        ".f",
        "x:",
        "f(",
        "\"",
        "\"a $b ${c}\"",
        "'raw'",
        "r#\"q\"#",
        "r#\"",
        "1mb",
        "30s",
        "-5",
        "1.5",
        "true",
        "null",
        "src/a.rs",
        "--flag",
        "#c",
        "\\",
        "${",
        "$",
        "é",
        "\u{0}",
        "`",
        "&&",
        "||",
        "!",
        "~",
        "r",
        "\"\\d\"",
    ];

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    #[test]
    fn deep_nesting_is_an_error_not_a_crash() {
        let deep = format!("{}1{}", "(".repeat(20_000), ")".repeat(20_000));
        assert!(parse(&deep).unwrap_err().message.contains("nesting deeper"));
        assert!(parse(&"[".repeat(20_000)).is_err());
        assert!(parse(&"- ".repeat(20_000)).is_err());
        assert!(parse(&"\"${".repeat(5_000)).is_err());
        assert!(parse("((((1))))").is_ok());
    }

    #[test]
    fn random_programs_never_panic() {
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        for _ in 0..20_000 {
            let len = (rng.next() % 24) as usize;
            let program: String = (0..len).map(|_| PIECES[(rng.next() % PIECES.len() as u64) as usize]).collect();
            let _ = parse(&program);
            let _ = parse_expr(&program);
        }
    }
}
