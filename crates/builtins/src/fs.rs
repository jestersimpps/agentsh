use crate::util::{check_expect, file_row, read_text};
use agentsh_core::ReadRecord;
use agentsh_core::{
    Args, Builtin, CommandSpec, Ctx, ParamSpec as P, ParamType as T, Result, ShellError, WalkOptions, walk,
};
use agentsh_platform::Capability::{Delete, Read, Write};
use agentsh_spec::{ErrorCode, Value};
use std::path::{Path, PathBuf};

const MAX_FIND_ROWS: usize = 50_000;

pub const LS: Builtin = Builtin {
    spec: CommandSpec {
        name: "ls",
        summary: "list a folder (or describe one file)",
        params: &[
            P::pos("path", T::Path, "folder to list; default: current folder"),
            P::named("all", T::Bool, "include hidden entries"),
        ],
        input: None,
        returns: "table of {path, type, size, modified}",
        caps: &[Read],
        examples: &["ls", "ls \"src\" all:true"],
    },
    run: ls,
};

fn ls(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let raw = args.opt_str("path")?.unwrap_or_else(|| ".".into());
    let path = ctx.existing(&raw)?;
    if !path.is_dir() {
        return Ok(Value::List(vec![file_row(ctx, &path)]));
    }
    let all = args.flag("all")?;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&path)
        .map_err(|e| ShellError::io(&e, &raw))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n != ".agentsh"))
        .filter(|p| all || !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .collect();
    entries.sort_by_key(|p| (!p.is_dir(), p.file_name().map(|n| n.to_os_string())));
    Ok(Value::List(entries.iter().map(|p| file_row(ctx, p)).collect()))
}

pub const READ: Builtin = Builtin {
    spec: CommandSpec {
        name: "read",
        summary: "read text files; one path returns its text, several return [{path, content}]",
        params: &[
            P::pos("paths", T::Paths, "file(s) to read").required().variadic(),
            P::named("from", T::Int, "first line to return (1-based)"),
            P::named("to", T::Int, "last line to return (inclusive)"),
            P::named("tail", T::Int, "return only the last N lines"),
            P::named("numbers", T::Bool, "prefix each line with its line number"),
            P::named("symbol", T::Str, "return just this function, type or class (see outline)"),
            P::named("full", T::Bool, "show the file even if it is unchanged since you last read it"),
        ],
        input: Some("paths"),
        returns: "string, or a list of {path, content} for several files",
        caps: &[Read],
        examples: &[
            "read \"README.md\"",
            "read \"src/main.rs\" from:40 to:80 numbers:true",
            "read \"Cargo.toml\" \"src/lib.rs\"",
            "read \"src/parser.rs\" symbol:\"parse_args\"",
        ],
    },
    run: read,
};

#[derive(Clone)]
struct LineRange {
    from: Option<usize>,
    to: Option<usize>,
    tail: Option<usize>,
    numbers: bool,
    full: bool,
    symbol: Option<String>,
}

impl LineRange {
    fn is_whole(&self) -> bool {
        self.from.is_none() && self.to.is_none() && self.tail.is_none() && !self.numbers && self.symbol.is_none()
    }
}

fn read(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let range = LineRange {
        from: args.opt_usize("from")?,
        to: args.opt_usize("to")?,
        tail: args.opt_usize("tail")?,
        numbers: args.flag("numbers")?,
        full: args.flag("full")?,
        symbol: args.opt_str("symbol")?,
    };
    if range.full {
        ctx.unshortened = true;
    }
    let raws = ctx.split_path_list(args.strings("paths")?);
    let total_budget = if range.full { usize::MAX } else { ctx.config.limits.max_string_bytes };
    if let [raw] = raws.as_slice() {
        return read_one(ctx, raw, &range, total_budget).map(Value::Str);
    }
    let budget = total_budget / raws.len().max(1);
    let mut files = Vec::with_capacity(raws.len());
    for raw in &raws {
        let content = read_one(ctx, raw, &range, budget)?;
        files.push(Value::record([("path", Value::Str(raw.clone())), ("content", Value::Str(content))]));
    }
    Ok(Value::List(files))
}

fn read_one(ctx: &mut Ctx, raw: &str, range: &LineRange, budget: usize) -> Result<String> {
    let path = ctx.existing_or_close(raw)?;
    if path.is_dir() {
        return Err(ShellError::new(ErrorCode::InvalidValue, format!("`{raw}` is a folder"))
            .with_value(raw)
            .with_hint(format!("list it with: ls \"{raw}\"")));
    }
    let text = read_text(ctx, &path)?;
    ctx.mark_seen(&path);
    let file_hash = agentsh_platform::hash_bytes(text.as_bytes());
    if range.is_whole() {
        let total = text.lines().count();
        let outline = (!range.full && total > OUTLINE_OVER_LINES).then(|| outline_instead(raw, &text, total)).flatten();
        let record = |hash: String, outline: Option<String>| ReadRecord {
            path: path.clone(),
            hash,
            file_hash: file_hash.clone(),
            start: 0,
            end: total,
            whole: true,
            numbers: false,
            full: range.full,
            outline,
            display: None,
            known: Vec::new(),
        };
        if outline.is_some() {
            ctx.note_read(record(file_hash.clone(), outline), &text);
        } else if text.len() <= budget {
            ctx.note_read(record(file_hash.clone(), None), &text);
        } else {
            ctx.warn(format!(
                "`{raw}` has {total} lines and is shown shortened; read a range with from:/to: (numbers:true helps)"
            ));
        }
        return Ok(text);
    }
    let (start, end) = slice_bounds(raw, &text, range)?;
    let out = slice_text(&text, start, end, range.numbers);
    if out.len() <= budget {
        let record = ReadRecord {
            path: path.clone(),
            hash: agentsh_platform::hash_bytes(out.as_bytes()),
            file_hash,
            start,
            end,
            whole: false,
            numbers: range.numbers,
            full: range.full,
            outline: None,
            display: None,
            known: Vec::new(),
        };
        ctx.note_read(record, &text);
    }
    Ok(out)
}

/// Whole-file reads of longer files show the outline instead (the value itself is the whole file).
const OUTLINE_OVER_LINES: usize = 400;

fn outline_instead(raw: &str, text: &str, total: usize) -> Option<String> {
    let rows = crate::code::outline_rows(text, None);
    if rows.len() < 3 {
        return None;
    }
    let listed: Vec<String> =
        rows.iter().map(|(line, depth, decl)| format!("{line}\t{}{decl}", "  ".repeat(*depth))).collect();
    Some(format!(
        "[`{raw}` has {total} lines, so here is its outline. read \"{raw}\" symbol:\"name\" shows one declaration, from:/to: a range, full:true the whole file]\n{}\n",
        listed.join("\n")
    ))
}

fn slice_bounds(label: &str, text: &str, range: &LineRange) -> Result<(usize, usize)> {
    let total = text.lines().count();
    let (start, end) = match (&range.symbol, range.tail) {
        (Some(name), _) => {
            let (s, e) =
                crate::code::symbol_range(text, name).ok_or_else(|| crate::code::missing_symbol(label, name, text))?;
            (s, e + 1)
        }
        (None, Some(n)) => (total.saturating_sub(n), total),
        (None, None) => (range.from.unwrap_or(1).max(1) - 1, range.to.unwrap_or(total).min(total)),
    };
    if start >= end && total > 0 {
        return Err(ShellError::new(
            ErrorCode::InvalidValue,
            format!("line range is empty; `{label}` has {total} lines"),
        ));
    }
    Ok((start, end))
}

fn slice_text(text: &str, start: usize, end: usize, numbers: bool) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let selected: Vec<String> = lines[start.min(total)..end.min(total)]
        .iter()
        .enumerate()
        .map(|(i, line)| if numbers { format!("{}\t{line}", start + i + 1) } else { line.to_string() })
        .collect();
    let mut out = selected.join("\n");
    if end < total || text.ends_with('\n') {
        out.push('\n');
    }
    out
}

pub const EXCERPT: Builtin = Builtin {
    spec: CommandSpec {
        name: "excerpt",
        summary: "lines of piped text, like read's from:/to:/tail:/symbol: but for any text",
        params: &[
            P::named("text", T::Str, "text to cut").required(),
            P::named("from", T::Int, "first line (1-based)"),
            P::named("to", T::Int, "last line (inclusive)"),
            P::named("tail", T::Int, "only the last N lines"),
            P::named("numbers", T::Bool, "prefix line numbers"),
            P::named("symbol", T::Str, "just this function, type or class"),
        ],
        input: Some("text"),
        returns: "string",
        caps: &[],
        examples: &[
            "exec \"git show main:src/a.rs\" parse:\"text\" | excerpt symbol:\"parse\"",
            "$log | excerpt tail:20",
        ],
    },
    run: excerpt,
};

fn excerpt(_ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let range = LineRange {
        from: args.opt_usize("from")?,
        to: args.opt_usize("to")?,
        tail: args.opt_usize("tail")?,
        numbers: args.flag("numbers")?,
        full: false,
        symbol: args.opt_str("symbol")?,
    };
    let text = args.str("text")?;
    if range.is_whole() {
        return Ok(Value::Str(text));
    }
    let (start, end) = slice_bounds("the text", &text, &range)?;
    Ok(Value::Str(slice_text(&text, start, end, range.numbers)))
}

pub const WRITE: Builtin = Builtin {
    spec: CommandSpec {
        name: "write",
        summary: "create or replace a file (non-strings are written as JSON)",
        params: &[
            P::pos("path", T::Path, "file to write").required(),
            P::pos("content", T::Any, "text to write").required(),
            P::named("append", T::Bool, "append instead of replacing"),
            P::named("overwrite", T::Bool, "replace an existing file you haven't read"),
        ],
        input: Some("content"),
        returns: "nothing; see effects",
        caps: &[Write],
        examples: &["write \"notes.md\" \"# Notes\\n\"", "find \".\" name:\"*.rs\" | write \"files.json\""],
    },
    run: write,
};

fn write(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let raw = args.str("path")?;
    let content = args.take("content").unwrap_or_default();
    let bytes = match content {
        Value::Str(s) => s.into_bytes(),
        other => {
            let mut json = serde_json::to_string_pretty(&other).unwrap_or_default();
            json.push('\n');
            json.into_bytes()
        }
    };
    let path = ctx.resolve_for_write(&raw)?;
    if path.is_dir() {
        return Err(ShellError::new(ErrorCode::InvalidValue, format!("`{raw}` is a folder")).with_value(raw.as_str()));
    }
    let append = args.flag("append")?;
    if path.exists() && !append {
        if ctx.state.seen.contains_key(&path) {
            ctx.check_fresh(&path)?;
        } else if !args.flag("overwrite")? {
            return Err(ShellError::new(
                ErrorCode::Stale,
                format!("`{raw}` already exists and hasn't been read in this session"),
            )
            .with_value(raw.as_str())
            .with_hint("read it first so nothing is lost, use edit for partial changes, or pass overwrite:true"));
        }
    }
    ctx.write_file(&path, &bytes, append)?;
    Ok(Value::Null)
}

pub const EDIT: Builtin = Builtin {
    spec: CommandSpec {
        name: "edit",
        summary: "replace exact text in a file; `old` must match exactly once unless all:true",
        params: &[
            P::pos("path", T::Path, "file to edit").required(),
            P::named("old", T::Str, "exact text to find (copy it from read)").required(),
            P::named("new", T::Str, "replacement text").required(),
            P::named("all", T::Bool, "replace every match"),
            P::named("expect", T::Int, "fail unless exactly this many matches exist"),
            P::named("diff", T::Bool, "return the unified diff instead of the changed line numbers"),
        ],
        input: Some("path"),
        returns: "the changed line numbers (or the diff with diff:true)",
        caps: &[Write],
        examples: &[
            "edit \"src/main.rs\" old:\"let x = 1;\" new:\"let x = 2;\"",
            "edit \"a.txt\" old:\"foo\" new:\"bar\" all:true expect:3",
        ],
    },
    run: edit,
};

fn edit(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let raw = args.str("path")?;
    let old = args.str("old")?;
    let new = args.str("new")?;
    if old.is_empty() {
        return Err(ShellError::new(ErrorCode::InvalidValue, "`old` can't be empty")
            .with_arg("old")
            .with_hint("to replace a whole file use write"));
    }
    let path = ctx.resolve_for_write(&raw)?;
    if !path.exists() {
        return Err(ctx.not_found(&raw, &path));
    }
    ctx.check_fresh(&path)?;
    let content = read_text(ctx, &path)?;
    let (old, new) = match_line_endings(&content, old, new);
    let count = content.matches(old.as_str()).count();
    let expect = args.opt_usize("expect")?;
    if count == 0 {
        return Err(no_match_error(&raw, &content, &old));
    }
    check_expect(expect, count, "matches of `old`")?;
    if count > 1 && !args.flag("all")? && expect.is_none() {
        return Err(ShellError::new(ErrorCode::AmbiguousMatch, format!("`old` matches {count} places in `{raw}`"))
            .with_arg("old")
            .with_value(count.to_string())
            .with_hint("include more surrounding text in old to pick one place, or pass all:true"));
    }
    let updated = content.replace(old.as_str(), &new);
    ctx.write_file(&path, updated.as_bytes(), false)?;
    if args.flag("diff")? {
        return Ok(Value::Str(
            similar::TextDiff::from_lines(&content, &updated).unified_diff().context_radius(1).to_string(),
        ));
    }
    Ok(Value::Str(changed_lines(&content, old.as_str(), new.lines().count().max(1))))
}

fn changed_lines(content: &str, old: &str, new_lines: usize) -> String {
    let starts: Vec<usize> = content.match_indices(old).map(|(i, _)| content[..i].matches('\n').count() + 1).collect();
    let mut shift = 0isize;
    let delta = new_lines as isize - old.lines().count().max(1) as isize;
    let ranges: Vec<String> = starts
        .iter()
        .map(|&line| {
            let first = (line as isize + shift) as usize;
            shift += delta;
            if new_lines > 1 { format!("{first}-{}", first + new_lines - 1) } else { first.to_string() }
        })
        .collect();
    let noun = if starts.len() == 1 && new_lines == 1 { "line" } else { "lines" };
    format!("changed {noun} {}", ranges.join(", "))
}

fn match_line_endings(content: &str, old: String, new: String) -> (String, String) {
    let crlf_file = content.contains("\r\n");
    let lf_snippet = old.contains('\n') && !old.contains("\r\n");
    if crlf_file && lf_snippet && !content.contains(old.as_str()) {
        (old.replace('\n', "\r\n"), new.replace("\r\n", "\n").replace('\n', "\r\n"))
    } else {
        (old, new)
    }
}

fn whitespace_variant(content: &str, old: &str) -> Option<String> {
    let wanted: Vec<&str> = old.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let first = *wanted.first()?;
    let lines: Vec<&str> = content.lines().collect();
    (0..lines.len()).find_map(|start| {
        if lines[start].trim() != first {
            return None;
        }
        let mut matched = Vec::new();
        let mut want = wanted.iter();
        let mut next = want.next();
        for line in &lines[start..] {
            let Some(target) = next else { break };
            if line.trim().is_empty() {
                matched.push(*line);
                continue;
            }
            if line.trim() != *target {
                return None;
            }
            matched.push(*line);
            next = want.next();
        }
        next.is_none().then(|| matched.join("\n"))
    })
}

fn no_match_error(raw: &str, content: &str, old: &str) -> ShellError {
    let err = ShellError::new(ErrorCode::NoMatch, format!("`old` was not found in `{raw}`")).with_arg("old");
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    if squash(content).contains(&squash(old)) {
        let err = err.with_hint("the text exists but whitespace or indentation differs; value shows the file's exact text to copy into old:");
        return match whitespace_variant(content, old) {
            Some(actual) => err.with_value(actual),
            None => err,
        };
    }
    let first = old.lines().find(|l| !l.trim().is_empty()).unwrap_or(old).trim();
    let best = content
        .lines()
        .enumerate()
        .map(|(i, line)| (i, line, strsim::normalized_levenshtein(first, line.trim())))
        .filter(|(_, _, score)| *score > 0.5)
        .max_by(|a, b| a.2.total_cmp(&b.2));
    match best {
        Some((i, line, _)) => err.with_hint(format!("closest line {}: {}", i + 1, line.trim())),
        None => err.with_hint(format!("read \"{raw}\" to see its current content")),
    }
}

pub const FIND: Builtin = Builtin {
    spec: CommandSpec {
        name: "find",
        summary: "recursively list files (respects .gitignore)",
        params: &[
            P::pos("path", T::Path, "folder to search; default: current folder"),
            P::named("name", T::Str, "glob on the file name, or on the relative path if it contains /"),
            P::named("type", T::Str, "\"file\" or \"dir\""),
            P::named("depth", T::Int, "maximum depth"),
            P::named("hidden", T::Bool, "include hidden files"),
            P::named("ignored", T::Bool, "include files ignored by .gitignore"),
        ],
        input: None,
        returns: "table of {path, type, size, modified}",
        caps: &[Read],
        examples: &["find \"src\" name:\"*.rs\"", "find \".\" type:\"dir\" depth:2"],
    },
    run: find,
};

fn find(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let raw = args.opt_str("path")?.unwrap_or_else(|| ".".into());
    let base = ctx.existing(&raw)?;
    let pattern = args.opt_str("name")?;
    let matcher = pattern.as_deref().map(|p| agentsh_core::compile_glob(p, false)).transpose()?;
    let match_path = pattern.as_deref().is_some_and(|p| p.contains('/'));
    let kind = args.opt_str("type")?;
    if let Some(k) = kind.as_deref().filter(|k| !matches!(*k, "file" | "dir")) {
        return Err(ShellError::new(ErrorCode::InvalidValue, format!("type must be \"file\" or \"dir\", got \"{k}\""))
            .with_arg("type"));
    }
    let options = WalkOptions {
        max_depth: args.opt_usize("depth")?,
        hidden: args.flag("hidden")?,
        respect_ignore: !args.flag("ignored")?,
    };
    let mut rows = Vec::new();
    for entry in walk(&base, &options) {
        let is_dir = entry.file_type().is_some_and(|t| t.is_dir());
        if kind.as_deref().is_some_and(|k| (k == "dir") != is_dir) {
            continue;
        }
        if let Some(m) = &matcher {
            let subject: &Path = if match_path {
                entry.path().strip_prefix(&base).unwrap_or(entry.path())
            } else {
                Path::new(entry.file_name())
            };
            if !m.is_match(subject) {
                continue;
            }
        }
        rows.push(file_row(ctx, entry.path()));
        if rows.len() >= MAX_FIND_ROWS {
            ctx.warn(format!("stopped after {MAX_FIND_ROWS} entries; narrow the path or name"));
            break;
        }
    }
    Ok(Value::List(rows))
}

pub const STAT: Builtin = Builtin {
    spec: CommandSpec {
        name: "stat",
        summary: "metadata for one path",
        params: &[P::pos("path", T::Path, "path to inspect").required()],
        input: Some("path"),
        returns: "{path, type, size, modified, mode, readonly, target?}",
        caps: &[Read],
        examples: &["stat \"Cargo.toml\""],
    },
    run: stat,
};

fn stat(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    use std::os::unix::fs::PermissionsExt;
    let raw = args.str("path")?;
    let path = ctx.existing(&raw)?;
    let meta = std::fs::symlink_metadata(&path).map_err(|e| ShellError::io(&e, &raw))?;
    let Value::Record(mut record) = file_row(ctx, &path) else { unreachable!() };
    record.insert("mode".into(), Value::Str(format!("{:o}", meta.permissions().mode() & 0o7777)));
    record.insert("readonly".into(), Value::Bool(meta.permissions().readonly()));
    if meta.is_symlink() {
        let target = std::fs::read_link(&path).map(|t| t.to_string_lossy().into_owned()).unwrap_or_default();
        record.insert("target".into(), Value::Str(target));
    }
    if meta.is_dir() {
        let count = std::fs::read_dir(&path).map(|rd| rd.count()).unwrap_or(0);
        record.insert("entries".into(), Value::Int(count as i64));
    }
    Ok(Value::Record(record))
}

pub const RM: Builtin = Builtin {
    spec: CommandSpec {
        name: "rm",
        summary: "remove files or folders (undoable)",
        params: &[
            P::pos("paths", T::Paths, "paths to remove").required().variadic(),
            P::named("recursive", T::Bool, "required to remove folders"),
            P::named("expect", T::Int, "fail unless exactly this many paths are targeted"),
        ],
        input: Some("paths"),
        returns: "nothing; see effects",
        caps: &[Delete],
        examples: &["rm \"old.log\"", "rm glob(\"*.tmp\") expect:3", "find \".\" name:\"*.bak\" | rm"],
    },
    run: rm,
};

fn rm(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let raws = ctx.split_path_list(args.strings("paths")?);
    let recursive = args.flag("recursive")?;
    check_expect(args.opt_usize("expect")?, raws.len(), "paths")?;
    let mut targets: Vec<PathBuf> = Vec::new();
    for raw in &raws {
        let path = ctx.resolve_for_write(raw)?;
        if path == ctx.root {
            return Err(
                ShellError::new(ErrorCode::ProtectedPath, "can't remove the workspace root").with_value(raw.as_str())
            );
        }
        if !path.exists() && !path.is_symlink() {
            return Err(ctx.not_found(raw, &path));
        }
        if path.is_dir() && !path.is_symlink() && !recursive {
            return Err(ShellError::new(ErrorCode::InvalidValue, format!("`{raw}` is a folder"))
                .with_value(raw.as_str())
                .with_hint(format!("rm \"{raw}\" recursive:true")));
        }
        targets.push(path);
    }
    targets.sort();
    targets.dedup();
    let nested: Vec<PathBuf> =
        targets.iter().filter(|t| targets.iter().any(|o| o != *t && t.starts_with(o))).cloned().collect();
    targets.retain(|t| !nested.contains(t));
    if targets.is_empty() {
        ctx.warn("nothing to remove: the path list was empty");
    }
    for path in &targets {
        ctx.remove_path(path)?;
    }
    Ok(Value::Null)
}

pub const MV: Builtin = Builtin {
    spec: CommandSpec {
        name: "mv",
        summary: "move or rename (into a folder if `to` is a folder); never overwrites",
        params: &[
            P::pos("from", T::Path, "existing path").required(),
            P::pos("to", T::Path, "new path or existing folder").required(),
        ],
        input: Some("from"),
        returns: "nothing; see effects",
        caps: &[Write],
        examples: &["mv \"draft.md\" \"docs/final.md\""],
    },
    run: mv,
};

fn mv(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let (from, to) = transfer_paths(ctx, args)?;
    ctx.move_path(&from, &to)?;
    Ok(Value::Null)
}

pub const CP: Builtin = Builtin {
    spec: CommandSpec {
        name: "cp",
        summary: "copy a file or folder (into a folder if `to` is a folder); never overwrites",
        params: &[
            P::pos("from", T::Path, "existing path").required(),
            P::pos("to", T::Path, "new path or existing folder").required(),
        ],
        input: Some("from"),
        returns: "nothing; see effects",
        caps: &[Write],
        examples: &["cp \"config.toml\" \"config.backup.toml\""],
    },
    run: cp,
};

fn cp(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let (from, to) = transfer_paths(ctx, args)?;
    ctx.copy_path(&from, &to)?;
    Ok(Value::Null)
}

fn transfer_paths(ctx: &Ctx, args: &Args) -> Result<(PathBuf, PathBuf)> {
    let raw_from = args.str("from")?;
    let raw_to = args.str("to")?;
    let from = ctx.resolve_for_write(&raw_from)?;
    if !from.exists() && !from.is_symlink() {
        return Err(ctx.not_found(&raw_from, &from));
    }
    let mut to = ctx.resolve_for_write(&raw_to)?;
    if to.is_dir() {
        to = to.join(from.file_name().unwrap_or_default());
    }
    if to.starts_with(&from) {
        return Err(ShellError::new(ErrorCode::InvalidValue, "can't move or copy a folder into itself")
            .with_value(raw_to.as_str()));
    }
    if to.exists() || to.is_symlink() {
        let shown = ctx.display(&to);
        return Err(ShellError::new(ErrorCode::AlreadyExists, format!("`{shown}` already exists"))
            .with_value(shown.clone())
            .with_hint(format!("remove it first (rm \"{shown}\") or choose another name")));
    }
    Ok((from, to))
}

pub const CHMOD: Builtin = Builtin {
    spec: CommandSpec {
        name: "chmod",
        summary: "change permissions (undoable): octal like \"755\", or \"+x\" / \"-x\"",
        params: &[
            P::pos("paths", T::Paths, "files or folders").required().variadic(),
            P::named("mode", T::Str, "\"755\", \"644\", \"+x\" or \"-x\"").required(),
        ],
        input: Some("paths"),
        returns: "list of {path, mode, before}",
        caps: &[Write],
        examples: &["chmod \"scripts/deploy.sh\" mode:\"+x\"", "chmod glob(\"bin/*\") mode:\"755\""],
    },
    run: chmod,
};

fn chmod(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let mode = args.str("mode")?;
    let mut out = Vec::new();
    for raw in args.strings("paths")? {
        let path = ctx.resolve_for_write(&raw)?;
        if !path.exists() {
            return Err(ctx.not_found(&raw, &path));
        }
        let current = agentsh_core::file_mode(&path).map_err(|e| ShellError::io(&e, &raw))?;
        let target = parse_mode(&mode, current)?;
        let before = ctx.set_mode(&path, target)?;
        out.push(Value::record([
            ("path", Value::Str(ctx.display(&path))),
            ("mode", Value::Str(format!("{target:o}"))),
            ("before", Value::Str(format!("{before:o}"))),
        ]));
    }
    Ok(Value::List(out))
}

fn parse_mode(mode: &str, current: u32) -> Result<u32> {
    match mode {
        "+x" => Ok(current | ((current & 0o444) >> 2)),
        "-x" => Ok(current & !0o111),
        octal => u32::from_str_radix(octal, 8).ok().filter(|m| *m <= 0o7777).ok_or_else(|| {
            ShellError::new(ErrorCode::InvalidValue, format!("invalid mode `{mode}`"))
                .with_arg("mode")
                .with_hint("use octal like \"755\" or \"644\", or \"+x\" / \"-x\"")
        }),
    }
}

pub const MKDIR: Builtin = Builtin {
    spec: CommandSpec {
        name: "mkdir",
        summary: "create a folder and any missing parents",
        params: &[P::pos("path", T::Path, "folder to create").required()],
        input: None,
        returns: "nothing; see effects",
        caps: &[Write],
        examples: &["mkdir \"src/utils\""],
    },
    run: mkdir,
};

fn mkdir(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let raw = args.str("path")?;
    let path = ctx.resolve_for_write(&raw)?;
    if path.exists() && !path.is_dir() {
        return Err(
            ShellError::new(ErrorCode::AlreadyExists, format!("`{raw}` exists and is a file")).with_value(raw.as_str())
        );
    }
    ctx.make_dir(&path)?;
    Ok(Value::Null)
}

pub const CD: Builtin = Builtin {
    spec: CommandSpec {
        name: "cd",
        summary: "change the session's current folder (default: workspace root)",
        params: &[P::pos("path", T::Path, "folder")],
        input: None,
        returns: "the new current folder",
        caps: &[Read],
        examples: &["cd \"src\"", "cd \"..\"", "cd"],
    },
    run: cd,
};

fn cd(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let path = match args.opt_str("path")? {
        Some(raw) => {
            let p = ctx.existing(&raw)?;
            if !p.is_dir() {
                return Err(ShellError::new(ErrorCode::InvalidValue, format!("`{raw}` is not a folder"))
                    .with_value(raw.as_str()));
            }
            p
        }
        None => ctx.root.clone(),
    };
    ctx.state.cwd = path.strip_prefix(&ctx.root).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    Ok(Value::Str(ctx.display(&path)))
}

pub const PWD: Builtin = Builtin {
    spec: CommandSpec {
        name: "pwd",
        summary: "the session's current folder, relative to the workspace root",
        params: &[],
        input: None,
        returns: "string",
        caps: &[],
        examples: &["pwd"],
    },
    run: pwd,
};

fn pwd(ctx: &mut Ctx, _args: &mut Args) -> Result<Value> {
    Ok(Value::Str(ctx.display(&ctx.cwd())))
}
