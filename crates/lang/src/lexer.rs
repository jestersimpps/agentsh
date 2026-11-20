use crate::ParseError;
use crate::token::{StrPart, Tok, Token};
use agentsh_spec::Span;

pub fn lex(source: &str) -> Result<Vec<Token>, ParseError> {
    Lexer::new(source).run()
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
    tokens: Vec<Token>,
    space_before: bool,
}

const SIZE_SUFFIXES: &[(&str, i64)] = &[
    ("b", 1),
    ("kb", 1_000),
    ("mb", 1_000_000),
    ("gb", 1_000_000_000),
    ("tb", 1_000_000_000_000),
    ("kib", 1 << 10),
    ("mib", 1 << 20),
    ("gib", 1 << 30),
    ("tib", 1 << 40),
];

const DURATION_SUFFIXES: &[(&str, i64)] = &[("s", 1), ("m", 60), ("h", 3_600), ("d", 86_400), ("w", 604_800)];

impl Lexer {
    fn new(source: &str) -> Self {
        Self { chars: source.chars().collect(), pos: 0, line: 1, col: 1, tokens: Vec::new(), space_before: true }
    }

    fn run(mut self) -> Result<Vec<Token>, ParseError> {
        while let Some(c) = self.peek() {
            let span = self.span();
            match c {
                ' ' | '\t' | '\r' => {
                    self.bump();
                    self.space_before = true;
                    continue;
                }
                '\\' if self.peek_at(1) == Some('\n') => {
                    self.bump();
                    self.bump();
                    self.space_before = true;
                    continue;
                }
                '#' => {
                    self.skip_comment();
                    continue;
                }
                '\n' => {
                    self.bump();
                    self.push(Tok::Newline, span);
                }
                '"' => {
                    let parts = self.lex_double_string(span)?;
                    self.push(Tok::Str(parts), span);
                }
                '\'' => {
                    let text = self.lex_single_string(span)?;
                    self.push(Tok::Str(vec![StrPart::Lit(text)]), span);
                }
                '$' => self.lex_var(span)?,
                'r' if self.raw_string_ahead() => {
                    let text = self.lex_raw_string(span)?;
                    self.push(Tok::Str(vec![StrPart::Lit(text)]), span);
                }
                _ if is_word_char(c) => self.lex_word(span)?,
                _ => self.lex_symbol(c, span)?,
            }
            self.space_before = false;
        }
        let span = self.span();
        self.push(Tok::Eof, span);
        Ok(self.tokens)
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied()?;
        self.pos += 1;
        if c == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(c)
    }

    fn span(&self) -> Span {
        Span { line: self.line, col: self.col }
    }

    fn push(&mut self, tok: Tok, span: Span) {
        self.tokens.push(Token { tok, span, space_before: self.space_before });
    }

    fn skip_comment(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            self.bump();
        }
    }

    fn lex_double_string(&mut self, start: Span) -> Result<Vec<StrPart>, ParseError> {
        self.bump();
        let mut parts = Vec::new();
        let mut lit = String::new();
        loop {
            let Some(c) = self.bump() else {
                return Err(ParseError::new("unterminated string", start).with_hint("close it with \""));
            };
            match c {
                '"' => break,
                '\\' => lit.push_str(&self.lex_escape(start)?),
                '$' => self.lex_interpolation(&mut parts, &mut lit, start)?,
                _ => lit.push(c),
            }
        }
        if !lit.is_empty() || parts.is_empty() {
            parts.push(StrPart::Lit(lit));
        }
        Ok(parts)
    }

    fn lex_escape(&mut self, start: Span) -> Result<String, ParseError> {
        match self.bump() {
            Some('n') => Ok("\n".into()),
            Some('t') => Ok("\t".into()),
            Some('r') => Ok("\r".into()),
            Some(c @ ('\\' | '"' | '$' | '\'')) => Ok(c.to_string()),
            Some(c) => Ok(format!("\\{c}")),
            None => Err(ParseError::new("unterminated string", start)),
        }
    }

    fn lex_interpolation(&mut self, parts: &mut Vec<StrPart>, lit: &mut String, start: Span) -> Result<(), ParseError> {
        match self.peek() {
            Some('{') => {
                self.bump();
                let expr_span = self.span();
                let mut depth = 1;
                let mut src = String::new();
                loop {
                    let Some(c) = self.bump() else {
                        return Err(ParseError::new("unterminated ${ in string", start).with_hint("close it with }"));
                    };
                    match c {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    src.push(c);
                }
                flush_lit(parts, lit);
                parts.push(StrPart::Expr(src, expr_span));
            }
            Some(c) if is_ident_start(c) => {
                let name = self.take_while(is_ident_char);
                flush_lit(parts, lit);
                parts.push(StrPart::Var(name));
            }
            _ => lit.push('$'),
        }
        Ok(())
    }

    fn lex_single_string(&mut self, start: Span) -> Result<String, ParseError> {
        self.bump();
        let mut text = String::new();
        loop {
            match self.bump() {
                Some('\'') => return Ok(text),
                Some(c) => text.push(c),
                None => {
                    return Err(ParseError::new("unterminated string", start).with_hint("close it with '"));
                }
            }
        }
    }

    fn raw_string_ahead(&self) -> bool {
        let mut i = 1;
        while self.peek_at(i) == Some('#') {
            i += 1;
        }
        self.peek_at(i) == Some('"')
    }

    fn lex_raw_string(&mut self, start: Span) -> Result<String, ParseError> {
        self.bump();
        let mut hashes = 0;
        while self.peek() == Some('#') {
            self.bump();
            hashes += 1;
        }
        self.bump();
        let closing: String = std::iter::once('"').chain(std::iter::repeat_n('#', hashes)).collect();
        let mut text = String::new();
        loop {
            let Some(c) = self.bump() else {
                return Err(
                    ParseError::new("unterminated raw string", start).with_hint(format!("close it with {closing}"))
                );
            };
            text.push(c);
            if text.ends_with(&closing) {
                text.truncate(text.len() - closing.len());
                return Ok(text);
            }
        }
    }

    fn lex_var(&mut self, span: Span) -> Result<(), ParseError> {
        self.bump();
        match self.peek() {
            Some(c) if is_ident_start(c) => {
                let name = self.take_while(is_ident_char);
                self.push(Tok::Var(name), span);
                Ok(())
            }
            Some('{') => Err(ParseError::new("${...} only works inside double-quoted strings", span)
                .with_hint("use $name outside strings")),
            _ => Err(ParseError::new("expected a variable name after $", span)),
        }
    }

    fn lex_word(&mut self, span: Span) -> Result<(), ParseError> {
        let word = self.take_while(is_word_char);
        if is_ident(&word) && self.peek() == Some(':') && self.peek_at(1) != Some(':') {
            self.bump();
            self.push(Tok::Key(word), span);
            return Ok(());
        }
        if is_ident(&word) && self.peek() == Some('(') {
            self.push(Tok::Func(word), span);
            return Ok(());
        }
        let tok = classify_word(&word, span)?;
        self.push(tok, span);
        Ok(())
    }

    fn lex_symbol(&mut self, c: char, span: Span) -> Result<(), ParseError> {
        self.bump();
        let next = self.peek();
        let (tok, consumed_next) = match (c, next) {
            ('{', _) => (Tok::LBrace, false),
            ('}', _) => (Tok::RBrace, false),
            ('[', _) => (Tok::LBracket, false),
            (']', _) => (Tok::RBracket, false),
            ('(', _) => (Tok::LParen, false),
            (')', _) => (Tok::RParen, false),
            (',', _) => (Tok::Comma, false),
            (':', _) => (Tok::Colon, false),
            (';', _) => (Tok::Semi, false),
            ('|', Some('|')) => {
                return Err(ParseError::new("`||` is not an operator", span)
                    .with_hint("use `or` for logic; use try { } catch { } to handle failures"));
            }
            ('|', _) => (Tok::Pipe, false),
            ('&', _) => {
                return Err(ParseError::new("`&` is not an operator", span)
                    .with_hint("use `and` for logic; put commands on separate lines (or use ;) to run them in order"));
            }
            ('=', Some('=')) => (Tok::Eq, true),
            ('=', Some('~')) => (Tok::Match, true),
            ('=', _) => (Tok::Assign, false),
            ('!', Some('=')) => (Tok::Ne, true),
            ('!', Some('~')) => (Tok::NotMatch, true),
            ('!', _) => {
                return Err(ParseError::new("`!` is not an operator", span).with_hint("use `not`"));
            }
            ('<', Some('=')) => (Tok::Le, true),
            ('<', _) => (Tok::Lt, false),
            ('>', Some('=')) => (Tok::Ge, true),
            ('>', _) => (Tok::Gt, false),
            ('+', _) => (Tok::Plus, false),
            ('%', _) => (Tok::Percent, false),
            ('`', _) => {
                return Err(ParseError::new("backticks are not supported", span)
                    .with_hint("use ( ... ) to run a command and use its value"));
            }
            _ => return Err(ParseError::new(format!("unexpected character `{c}`"), span)),
        };
        if consumed_next {
            self.bump();
        }
        self.push(tok, span);
        Ok(())
    }

    fn take_while(&mut self, pred: fn(char) -> bool) -> String {
        let mut out = String::new();
        while let Some(c) = self.peek() {
            if !pred(c) {
                break;
            }
            out.push(c);
            self.bump();
        }
        out
    }
}

fn flush_lit(parts: &mut Vec<StrPart>, lit: &mut String) {
    if !lit.is_empty() {
        parts.push(StrPart::Lit(std::mem::take(lit)));
    }
}

fn classify_word(word: &str, span: Span) -> Result<Tok, ParseError> {
    match word {
        "true" => return Ok(Tok::Bool(true)),
        "false" => return Ok(Tok::Bool(false)),
        "null" => return Ok(Tok::Null),
        "-" => return Ok(Tok::Minus),
        "*" => return Ok(Tok::Star),
        "/" => return Ok(Tok::Slash),
        _ => {}
    }
    if is_ident(word) {
        return Ok(Tok::Ident(word.to_string()));
    }
    if let Some(field) = word.strip_prefix('.')
        && !field.is_empty()
        && field.split('.').all(is_ident)
    {
        return Ok(Tok::Field(field.to_string()));
    }
    if let Some(tok) = parse_number(word, span)? {
        return Ok(tok);
    }
    Ok(Tok::Bare(word.to_string()))
}

fn parse_number(word: &str, span: Span) -> Result<Option<Tok>, ParseError> {
    let body = word.strip_prefix('-').unwrap_or(word);
    if !body.starts_with(|c: char| c.is_ascii_digit()) {
        return Ok(None);
    }
    let digits_end = body.find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '_')).unwrap_or(body.len());
    let (num, suffix) = body.split_at(digits_end);
    let num = num.replace('_', "");
    let negative = word.starts_with('-');
    let sign = if negative { -1.0 } else { 1.0 };
    let value: f64 = match num.parse() {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    if suffix.is_empty() {
        return Ok(Some(if num.contains('.') {
            Tok::Float(sign * value)
        } else {
            num.parse::<i64>().map(|i| Tok::Int(if negative { -i } else { i })).unwrap_or(Tok::Float(sign * value))
        }));
    }
    if !suffix.chars().all(|c| c.is_ascii_alphabetic()) {
        return Ok(None);
    }
    let lower = suffix.to_ascii_lowercase();
    if lower == "ms" {
        return Ok(Some(Tok::Float(sign * value / 1000.0)));
    }
    let multiplier = SIZE_SUFFIXES.iter().chain(DURATION_SUFFIXES.iter()).find(|(s, _)| *s == lower).map(|(_, m)| *m);
    match multiplier {
        Some(m) => Ok(Some(Tok::Int((sign * value * m as f64).round() as i64))),
        None => Err(ParseError::new(format!("unknown number suffix `{suffix}` in `{word}`"), span)
            .with_hint("sizes: b kb mb gb tb kib mib gib tib; durations: ms s m h d w; or quote it as a string")),
    }
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '/' | '*' | '?' | '~' | '-' | '@')
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(is_ident_start) && chars.all(is_ident_char)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(src: &str) -> Vec<Tok> {
        lex(src).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn sizes_and_durations() {
        assert_eq!(
            toks("1mb 2kib 30s 500ms")[..4],
            [Tok::Int(1_000_000), Tok::Int(2048), Tok::Int(30), Tok::Float(0.5)]
        );
    }

    #[test]
    fn key_func_field_bare() {
        assert_eq!(
            toks("find name:\"x\" glob(\"*\") .size src/main.rs"),
            vec![
                Tok::Ident("find".into()),
                Tok::Key("name".into()),
                Tok::Str(vec![StrPart::Lit("x".into())]),
                Tok::Func("glob".into()),
                Tok::LParen,
                Tok::Str(vec![StrPart::Lit("*".into())]),
                Tok::RParen,
                Tok::Field("size".into()),
                Tok::Bare("src/main.rs".into()),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn unknown_escapes_pass_through() {
        assert_eq!(toks(r#""\d+\.rs \(x)""#)[0], Tok::Str(vec![StrPart::Lit(r"\d+\.rs \(x)".into())]));
    }

    #[test]
    fn raw_strings() {
        assert_eq!(toks(r####"r"a\n $x""####)[0], Tok::Str(vec![StrPart::Lit(r"a\n $x".into())]));
        assert_eq!(
            toks(r####"r#"say "hi" and 'bye'"#"####)[0],
            Tok::Str(vec![StrPart::Lit(r#"say "hi" and 'bye'"#.into())])
        );
        assert_eq!(toks("read r")[1], Tok::Ident("r".into()));
    }

    #[test]
    fn interpolation() {
        assert_eq!(
            toks("\"$f has ${n + 1}\"")[0],
            Tok::Str(vec![
                StrPart::Var("f".into()),
                StrPart::Lit(" has ".into()),
                StrPart::Expr("n + 1".into(), Span { line: 1, col: 11 }),
            ])
        );
    }
}
