use crate::text::expand_files;
use crate::util::read_text;
use agentsh_core::{Args, Builtin, CommandSpec, Ctx, ParamSpec as P, ParamType as T, Result, ShellError};
use agentsh_platform::Capability::Read;
use agentsh_spec::{ErrorCode, Value};
use regex::Regex;
use std::sync::LazyLock;

const KEYWORDS: &str = r"fn|func|function|def|class|struct|enum|union|trait|impl|interface|protocol|extension|actor|type|mod|module|namespace|object|fun|record|macro_rules!";
const MODIFIERS: &str = r"(?:(?:pub(?:\([^)]*\))?|export|default|async|unsafe|const|static|extern|public|private|protected|internal|fileprivate|open|final|abstract|override|sealed|data|inline|suspend|mutating|nonisolated|virtual|partial|readonly|declare)\s+)*";
const MAX_TEXT: usize = 100;

static DECLARATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^(\s*){MODIFIERS}(?:{KEYWORDS})\s+([A-Za-z_$][\w$]*)")).unwrap());
static PREFIX_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*(///|//!|/\*\*|\*|#\[|@[A-Za-z]|#\s|"""|''')"#).unwrap());

pub const OUTLINE: Builtin = Builtin {
    spec: CommandSpec {
        name: "outline",
        summary: "declarations in a source file (functions, types, classes) with line numbers",
        params: &[
            P::pos("path", T::Path, "source file").required(),
            P::named("depth", T::Int, "maximum nesting level (0 = top level only)"),
        ],
        input: Some("path"),
        returns: "table of {line, depth, decl}",
        caps: &[Read],
        examples: &["outline \"src/parser.rs\"", "outline \"app/models.py\" depth:0"],
    },
    run: outline,
};

fn outline(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let raw = args.str("path")?;
    let path = ctx.existing_or_close(&raw)?;
    let text = read_text(ctx, &path)?;
    ctx.mark_seen(&path);
    let max_depth = args.opt_usize("depth")?;
    let lines: Vec<&str> = text.lines().collect();
    let unit = indent_unit(&lines);
    let rows = lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| DECLARATION.captures(line).map(|c| (i, line, c[1].len())))
        .map(|(i, line, indent)| (i, line, indent / unit))
        .filter(|(_, _, depth)| max_depth.is_none_or(|m| *depth <= m))
        .map(|(i, line, depth)| {
            Value::record([
                ("line", Value::Int(i as i64 + 1)),
                ("depth", Value::Int(depth as i64)),
                ("decl", Value::Str(clip(line.trim()))),
            ])
        })
        .collect();
    Ok(Value::List(rows))
}

pub const REFS: Builtin = Builtin {
    spec: CommandSpec {
        name: "refs",
        summary: "where a name is defined and used (whole-word, .gitignore-aware), grouped by file",
        params: &[
            P::pos("name", T::Str, "identifier to look up").required(),
            P::named("in", T::Paths, "files or folders; default: current folder"),
        ],
        input: None,
        returns: "table of {path, defs, uses} with line numbers",
        caps: &[Read],
        examples: &["refs \"parse_args\"", "refs \"UserService\" in:\"src\""],
    },
    run: refs,
};

fn refs(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let name = args.str("name")?;
    let pattern = Regex::new(&format!(r"(^|[^\w$]){}($|[^\w$])", regex::escape(&name)))
        .map_err(|e| ShellError::new(ErrorCode::InvalidValue, e.to_string()))?;
    let mut paths = args.strings("in")?;
    if paths.is_empty() {
        paths.push(".".into());
    }
    let mut rows: Vec<(usize, usize, Value)> = Vec::new();
    for file in expand_files(ctx, &paths)? {
        let Ok(bytes) = std::fs::read(&file) else { continue };
        if bytes[..bytes.len().min(8_000)].contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        if !text.contains(name.as_str()) {
            continue;
        }
        let (mut defs, mut uses) = (Vec::new(), Vec::new());
        for (i, line) in text.lines().enumerate().filter(|(_, l)| pattern.is_match(l)) {
            let is_def = DECLARATION.captures(line).is_some_and(|c| c[2] == *name);
            if is_def { defs.push((i + 1).to_string()) } else { uses.push((i + 1).to_string()) }
        }
        if defs.is_empty() && uses.is_empty() {
            continue;
        }
        let row = Value::record([
            ("path", Value::Str(ctx.display(&file))),
            ("defs", Value::Str(defs.join(","))),
            ("uses", Value::Str(uses.join(","))),
        ]);
        rows.push((defs.len(), uses.len(), row));
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    if rows.is_empty() {
        return Err(ShellError::new(ErrorCode::NoMatch, format!("`{name}` doesn't appear in the searched files"))
            .with_value(name.as_str()));
    }
    Ok(Value::List(rows.into_iter().map(|(_, _, r)| r).collect()))
}

pub fn symbol_range(text: &str, name: &str) -> Option<(usize, usize)> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|line| DECLARATION.captures(line).is_some_and(|c| &c[2] == name))?;
    let end = block_end(&lines, start);
    let mut first = start;
    while first > 0 && PREFIX_LINE.is_match(lines[first - 1]) {
        first -= 1;
    }
    Some((first, end))
}

pub fn symbol_names(text: &str) -> Vec<String> {
    text.lines().filter_map(|l| DECLARATION.captures(l).map(|c| c[2].to_string())).collect()
}

pub fn missing_symbol(raw: &str, name: &str, text: &str) -> ShellError {
    let names = symbol_names(text);
    let err =
        ShellError::new(ErrorCode::NotFound, format!("no declaration named `{name}` in `{raw}`")).with_value(name);
    match agentsh_core::closest(name, names.iter().map(String::as_str)) {
        Some(best) => err.with_hint(format!("did you mean \"{best}\"? (outline \"{raw}\" lists all)")),
        None => err.with_hint(format!("outline \"{raw}\" lists the declarations")),
    }
}

fn block_end(lines: &[&str], start: usize) -> usize {
    match brace_block_end(lines, start) {
        Some(end) => end,
        None => indent_block_end(lines, start),
    }
}

fn brace_block_end(lines: &[&str], start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut opened = false;
    let mut in_block_comment = false;
    for (offset, line) in lines[start..].iter().enumerate() {
        let mut chars = line.chars().peekable();
        let mut quote: Option<char> = None;
        while let Some(c) = chars.next() {
            if in_block_comment {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    in_block_comment = false;
                }
                continue;
            }
            if let Some(q) = quote {
                if c == '\\' {
                    chars.next();
                } else if c == q {
                    quote = None;
                }
                continue;
            }
            match c {
                '"' | '`' => quote = Some(c),
                '/' if chars.peek() == Some(&'/') => break,
                '/' if chars.peek() == Some(&'*') => {
                    chars.next();
                    in_block_comment = true;
                }
                '#' if !opened && offset > 0 => break,
                '{' => {
                    depth += 1;
                    opened = true;
                }
                '}' => {
                    depth -= 1;
                    if opened && depth == 0 {
                        return Some(start + offset);
                    }
                }
                ';' if !opened && depth == 0 => return Some(start + offset),
                ':' if !opened && offset == 0 && line.trim_end().ends_with(':') => return None,
                _ => {}
            }
        }
        if !opened && offset >= 3 {
            return None;
        }
    }
    opened.then_some(lines.len() - 1)
}

fn indent_block_end(lines: &[&str], start: usize) -> usize {
    let base = indentation(lines[start]);
    let mut end = start;
    for (offset, line) in lines[start + 1..].iter().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        if indentation(line) <= base {
            break;
        }
        end = start + 1 + offset;
    }
    end
}

fn indentation(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).map(|c| if c == '\t' { 4 } else { 1 }).sum()
}

fn indent_unit(lines: &[&str]) -> usize {
    lines.iter().map(|l| indentation(l)).filter(|&n| n > 0).min().unwrap_or(4).clamp(1, 8)
}

fn clip(line: &str) -> String {
    if line.chars().count() <= MAX_TEXT {
        return line.to_string();
    }
    let cut: String = line.chars().take(MAX_TEXT).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(text: &str, name: &str) -> String {
        let (s, e) = symbol_range(text, name).unwrap();
        text.lines().collect::<Vec<_>>()[s..=e].join("\n")
    }

    #[test]
    fn rust_function_with_docs_and_braces_in_strings() {
        let src = "use x;\n\n/// Adds.\n#[inline]\npub fn add(a: i32) -> i32 {\n    let s = \"}\";\n    if a > 0 { a } else { 0 }\n}\n\nfn other() {}\n";
        assert_eq!(
            block(src, "add"),
            "/// Adds.\n#[inline]\npub fn add(a: i32) -> i32 {\n    let s = \"}\";\n    if a > 0 { a } else { 0 }\n}"
        );
        assert_eq!(block(src, "other"), "fn other() {}");
    }

    #[test]
    fn python_by_indentation() {
        let src =
            "class A:\n    def f(self):\n        return 1\n\n    def g(self):\n        pass\n\ndef top():\n    pass\n";
        assert_eq!(block(src, "f"), "    def f(self):\n        return 1");
        assert_eq!(block(src, "A"), "class A:\n    def f(self):\n        return 1\n\n    def g(self):\n        pass");
    }

    #[test]
    fn typescript_and_swift() {
        let ts = "export async function load(id: string): Promise<void> {\n  await x(`}`);\n}\nexport interface User {\n  id: string;\n}\n";
        assert_eq!(block(ts, "load"), "export async function load(id: string): Promise<void> {\n  await x(`}`);\n}");
        assert_eq!(block(ts, "User"), "export interface User {\n  id: string;\n}");
        let swift = "struct Song {\n    let title: String\n    func play() {\n        print(\"{\")\n    }\n}\n";
        assert_eq!(block(swift, "play"), "    func play() {\n        print(\"{\")\n    }");
    }

    #[test]
    fn rust_type_alias_ends_at_semicolon() {
        assert_eq!(
            block("pub type Result<T> = std::result::Result<T, E>;\nfn x() {}\n", "Result"),
            "pub type Result<T> = std::result::Result<T, E>;"
        );
    }
}
