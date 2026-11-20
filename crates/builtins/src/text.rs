use crate::util::{check_expect, is_binary_file, read_text};
use agentsh_core::{
    Args, Builtin, CommandSpec, Ctx, ParamSpec as P, ParamType as T, Result, ShellError, WalkOptions, walk,
};
use agentsh_platform::Capability::Read;
use agentsh_spec::{ErrorCode, Value};
use regex::Regex;
use std::path::PathBuf;

const DEFAULT_MAX_MATCHES: usize = 500;
const MAX_SEARCH_FILE_BYTES: u64 = 10 * 1024 * 1024;
const MATCH_TEXT_CLIP: usize = 300;

pub const GREP: Builtin = Builtin {
    spec: CommandSpec {
        name: "grep",
        summary: "search files (default: current folder, recursive, .gitignore-aware) or piped text with a regex",
        params: &[
            P::pos("pattern", T::Str, "regex (or literal text with fixed:true)").required(),
            P::named("in", T::Paths, "files or folders to search"),
            P::named("input", T::Any, "text, list of strings, or rows with .path to search"),
            P::named("ignore_case", T::Bool, "case-insensitive"),
            P::named("fixed", T::Bool, "treat pattern as literal text"),
            P::named("files", T::Bool, "return {path, matches} per file instead of lines"),
            P::named("max", T::Int, "stop after this many matches (default 500)"),
            P::named("before", T::Int, "lines of context before each match"),
            P::named("after", T::Int, "lines of context after each match (text then holds the whole snippet)"),
        ],
        input: Some("input"),
        returns: "table of {path, line, text}; for piped text {line, text}; for piped strings the matching strings",
        caps: &[Read],
        examples: &[
            "grep \"TODO\" in:\"src\"",
            "grep \"fn parse_args\" in:\"src\" after:20",
            "grep \"fn main\" fixed:true files:true",
            "find \".\" name:\"*.md\" | grep \"(?i)install\"",
        ],
    },
    run: grep,
};

#[derive(Clone, Copy)]
struct GrepOptions {
    max: usize,
    files_only: bool,
    before: usize,
    after: usize,
}

fn grep(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let re = build_regex(ctx, args)?;
    let opts = GrepOptions {
        max: args.opt_usize("max")?.unwrap_or(DEFAULT_MAX_MATCHES),
        files_only: args.flag("files")?,
        before: args.opt_usize("before")?.unwrap_or(0),
        after: args.opt_usize("after")?.unwrap_or(0),
    };
    match args.take("input") {
        Some(Value::Str(text)) => Ok(Value::List(grep_text(&re, &text, &opts))),
        Some(Value::List(items)) if items.iter().all(|v| matches!(v, Value::Str(_))) => Ok(Value::List(
            items.into_iter().filter(|v| matches!(v, Value::Str(s) if re.is_match(s))).take(opts.max).collect(),
        )),
        Some(Value::List(items)) => {
            let paths = items
                .iter()
                .map(|v| match v {
                    Value::Record(r) => {
                        r.get("path").and_then(|p| if let Value::Str(s) = p { Some(s.clone()) } else { None })
                    }
                    _ => None,
                })
                .collect::<Option<Vec<String>>>()
                .ok_or_else(|| {
                    ShellError::new(ErrorCode::TypeError, "piped rows need a .path field (e.g. from find)")
                })?;
            grep_files(ctx, &re, &paths, &opts)
        }
        Some(Value::Null) => Ok(Value::List(Vec::new())),
        Some(other) => Err(ShellError::type_error("grep input", "text, strings, or rows with .path", &other)),
        None => {
            let mut paths = args.strings("in")?;
            if paths.is_empty() {
                paths.push(".".into());
            }
            grep_files(ctx, &re, &paths, &opts)
        }
    }
}

fn build_regex(ctx: &mut Ctx, args: &Args) -> Result<Regex> {
    let pattern = args.str("pattern")?;
    let mut source = if args.flag("fixed")? { regex::escape(&pattern) } else { pattern };
    if args.flag("ignore_case")? {
        source = format!("(?i){source}");
    }
    ctx.regex(&source)
}

fn match_text(lines: &[&str], i: usize, opts: &GrepOptions) -> String {
    if opts.before == 0 && opts.after == 0 {
        return clip(lines[i]);
    }
    let start = i.saturating_sub(opts.before);
    let end = (i + opts.after + 1).min(lines.len());
    let mut snippet = lines[start..end].join("\n");
    snippet.push('\n');
    snippet
}

fn match_row(path: Option<&str>, lines: &[&str], i: usize, opts: &GrepOptions) -> Value {
    let mut record = Vec::with_capacity(4);
    if let Some(p) = path {
        record.push(("path", Value::str(p)));
    }
    record.push(("line", Value::Int(i as i64 + 1)));
    if opts.before > 0 {
        record.push(("from", Value::Int(i.saturating_sub(opts.before) as i64 + 1)));
    }
    record.push(("text", Value::Str(match_text(lines, i, opts))));
    Value::record(record)
}

fn grep_text(re: &Regex, text: &str, opts: &GrepOptions) -> Vec<Value> {
    let lines: Vec<&str> = text.lines().collect();
    (0..lines.len())
        .filter(|&i| re.is_match(lines[i]))
        .take(opts.max)
        .map(|i| match_row(None, &lines, i, opts))
        .collect()
}

fn grep_files(ctx: &mut Ctx, re: &Regex, raws: &[String], opts: &GrepOptions) -> Result<Value> {
    let (max, files_only) = (opts.max, opts.files_only);
    let files = expand_files(ctx, raws)?;
    let mut rows = Vec::new();
    let mut total = 0usize;
    'files: for file in files {
        let Ok(bytes) = std::fs::read(&file) else { continue };
        if bytes[..bytes.len().min(8_000)].contains(&0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        if !re.is_match(&text) {
            continue;
        }
        let shown = ctx.display(&file);
        let mut in_file = 0;
        let lines: Vec<&str> = text.lines().collect();
        for i in 0..lines.len() {
            if !re.is_match(lines[i]) {
                continue;
            }
            in_file += 1;
            total += 1;
            if !files_only {
                rows.push(match_row(Some(&shown), &lines, i, opts));
            }
            if total >= max {
                ctx.warn(format!("stopped after {max} matches; pass max: for more or narrow in:"));
                if files_only {
                    rows.push(Value::record([("path", Value::Str(shown)), ("matches", Value::Int(in_file))]));
                }
                break 'files;
            }
        }
        if files_only && in_file > 0 {
            rows.push(Value::record([("path", Value::Str(shown)), ("matches", Value::Int(in_file))]));
        }
    }
    Ok(Value::List(rows))
}

pub fn expand_files(ctx: &Ctx, raws: &[String]) -> Result<Vec<PathBuf>> {
    let options = WalkOptions { max_depth: None, hidden: false, respect_ignore: true };
    let mut files = Vec::new();
    for raw in &ctx.split_path_list(raws.to_vec()) {
        let path = ctx.existing(raw)?;
        if path.is_dir() {
            files.extend(
                walk(&path, &options)
                    .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
                    .filter(|e| e.metadata().is_ok_and(|m| m.len() <= MAX_SEARCH_FILE_BYTES))
                    .map(|e| e.into_path()),
            );
        } else {
            files.push(path);
        }
    }
    Ok(files)
}

fn clip(line: &str) -> String {
    if line.len() <= MATCH_TEXT_CLIP {
        return line.to_string();
    }
    let mut cut = MATCH_TEXT_CLIP;
    while !line.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}…", &line[..cut])
}

pub const REPLACE: Builtin = Builtin {
    spec: CommandSpec {
        name: "replace",
        summary: "replace text across files (in: or piped rows with .path), or in piped text",
        params: &[
            P::pos("pattern", T::Str, "text to find (a regex with regex:true)").required(),
            P::pos("with", T::Str, "replacement ($1 refers to groups with regex:true)").required(),
            P::named("in", T::Paths, "files or folders to change"),
            P::named("input", T::Any, "piped text, or rows with .path"),
            P::named("regex", T::Bool, "treat pattern as a regex"),
            P::named("ignore_case", T::Bool, "case-insensitive"),
            P::named("expect", T::Int, "fail (changing nothing) unless the total number of replacements matches"),
        ],
        input: Some("input"),
        returns: "table of {path, replacements}, or the replaced text",
        caps: &[Read],
        examples: &["replace \"old_name\" \"new_name\" in:\"src\" expect:4", "read \"a.txt\" | replace \"x\" \"y\""],
    },
    run: replace,
};

fn replace(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let pattern = args.str("pattern")?;
    let with = args.str("with")?;
    let mut source = if args.flag("regex")? { pattern } else { regex::escape(&pattern) };
    if args.flag("ignore_case")? {
        source = format!("(?i){source}");
    }
    let re = ctx.regex(&source)?;
    let replacement = if args.flag("regex")? { with } else { with.replace('$', "$$") };
    let expect = args.opt_usize("expect")?;
    let paths = match args.take("input") {
        Some(Value::Str(text)) => {
            check_expect(expect, re.find_iter(&text).count(), "replacements")?;
            return Ok(Value::Str(re.replace_all(&text, replacement.as_str()).into_owned()));
        }
        Some(Value::List(items)) => items
            .iter()
            .map(|v| match v {
                Value::Record(r) => {
                    r.get("path").and_then(|p| if let Value::Str(s) = p { Some(s.clone()) } else { None })
                }
                Value::Str(s) => Some(s.clone()),
                _ => None,
            })
            .collect::<Option<Vec<String>>>()
            .ok_or_else(|| ShellError::new(ErrorCode::TypeError, "piped rows need a .path field (e.g. from find)"))?,
        Some(other) => return Err(ShellError::type_error("replace input", "text or rows with .path", &other)),
        None => args.strings("in")?,
    };
    if paths.is_empty() {
        return Err(ShellError::new(
            ErrorCode::MissingArgument,
            "replace needs files: pass in:\"path\" or pipe rows from find",
        )
        .with_arg("in"));
    }
    ctx.require(agentsh_platform::Capability::Write)?;
    replace_in_files(ctx, &re, &replacement, &paths, expect)
}

fn replace_in_files(
    ctx: &mut Ctx,
    re: &Regex,
    replacement: &str,
    raws: &[String],
    expect: Option<usize>,
) -> Result<Value> {
    let mut planned = Vec::new();
    for file in expand_files(ctx, raws)? {
        if is_binary_file(&file) {
            continue;
        }
        let text = read_text(ctx, &file)?;
        let count = re.find_iter(&text).count();
        if count == 0 {
            continue;
        }
        let raw = ctx.display(&file);
        let path = ctx.resolve_for_write(&raw)?;
        ctx.check_fresh(&path)?;
        planned.push((path, re.replace_all(&text, replacement).into_owned(), count));
    }
    let total: usize = planned.iter().map(|(_, _, c)| c).sum();
    check_expect(expect, total, "replacements")?;
    let mut files = Vec::with_capacity(planned.len());
    for (path, updated, count) in planned {
        ctx.write_file(&path, updated.as_bytes(), false)?;
        files.push(Value::record([
            ("path", Value::Str(ctx.display(&path))),
            ("replacements", Value::Int(count as i64)),
        ]));
    }
    Ok(Value::List(files))
}

pub const LINES: Builtin = Builtin {
    spec: CommandSpec {
        name: "lines",
        summary: "split piped text into a list of lines",
        params: &[P::named("input", T::Str, "text").required(), P::named("blank", T::Bool, "keep blank lines")],
        input: Some("input"),
        returns: "list of strings",
        caps: &[],
        examples: &["read \"names.txt\" | lines | take 3"],
    },
    run: lines,
};

fn lines(_ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let text = args.str("input")?;
    let keep_blank = args.flag("blank")?;
    Ok(Value::List(text.lines().filter(|l| keep_blank || !l.trim().is_empty()).map(Value::str).collect()))
}

pub const COUNT: Builtin = Builtin {
    spec: CommandSpec {
        name: "count",
        summary: "number of items in a list, lines in text, or fields in a record",
        params: &[P::named("input", T::Any, "value to count").required()],
        input: Some("input"),
        returns: "int",
        caps: &[],
        examples: &["find \"src\" name:\"*.rs\" | count"],
    },
    run: count,
};

fn count(_ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    Ok(Value::Int(match args.take("input").unwrap_or_default() {
        Value::List(l) => l.len(),
        Value::Str(s) => s.lines().count(),
        Value::Record(r) => r.len(),
        Value::Null => 0,
        _ => 1,
    } as i64))
}
