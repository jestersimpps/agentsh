use agentsh_core::{Ctx, Result, ShellError, iso};
use agentsh_spec::{ErrorCode, Value};
use std::fs::Metadata;
use std::io::Read;
use std::path::Path;

const BINARY_SNIFF_BYTES: usize = 8_000;

pub fn kind_of(meta: &Metadata) -> &'static str {
    if meta.is_symlink() {
        "symlink"
    } else if meta.is_dir() {
        "dir"
    } else {
        "file"
    }
}

pub fn file_row(ctx: &Ctx, path: &Path) -> Value {
    let meta = std::fs::symlink_metadata(path);
    let (kind, size, modified) = match &meta {
        Ok(m) => (
            kind_of(m),
            if m.is_file() { Value::Int(m.len() as i64) } else { Value::Null },
            m.modified().map(|t| Value::Str(iso(t))).unwrap_or(Value::Null),
        ),
        Err(_) => ("missing", Value::Null, Value::Null),
    };
    Value::record([
        ("path", Value::Str(ctx.display(path))),
        ("type", Value::str(kind)),
        ("size", size),
        ("modified", modified),
    ])
}

pub fn is_binary_file(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let mut buf = vec![0u8; BINARY_SNIFF_BYTES];
    let n = file.read(&mut buf).unwrap_or(0);
    buf[..n].contains(&0)
}

pub fn read_text(ctx: &Ctx, path: &Path) -> Result<String> {
    let shown = ctx.display(path);
    let bytes = std::fs::read(path).map_err(|e| ShellError::io(&e, &shown))?;
    if bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
        return Err(ShellError::new(
            ErrorCode::BinaryFile,
            format!("`{shown}` is a binary file ({} bytes)", bytes.len()),
        )
        .with_value(shown)
        .with_hint("use stat for metadata; binary content isn't shown"));
    }
    Ok(match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
    })
}

pub fn check_expect(expect: Option<usize>, actual: usize, what: &str) -> Result<()> {
    match expect {
        Some(expected) if expected != actual => Err(ShellError::new(
            ErrorCode::ExpectationFailed,
            format!("expected {expected} {what}, found {actual}; nothing was changed"),
        )
        .with_value(actual.to_string())
        .with_hint("inspect the targets first, then adjust the command or expect:")),
        _ => Ok(()),
    }
}

pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        if chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

pub fn failure_digest(text: &str, max_lines: usize, max_bytes: usize) -> String {
    static IMPORTANT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(concat!(
            r#"(?i)(error|fatal|panic|fail|exception|traceback|denied|not found|no such|expected|found|cannot|unable|undefined|unresolved|mismatch|missing|invalid)"#,
            r#"|^\s*--> |^\s*File ".*", line \d+|^\s+at .+:\d+|^\s*\|?\s*\^+\s*\S"#
        ))
        .unwrap()
    });
    let lines: Vec<&str> = text.trim_end().lines().filter(|l| !l.trim().is_empty()).collect();
    let mut picked: Vec<usize> = (0..lines.len()).filter(|&i| IMPORTANT.is_match(lines[i])).collect();
    if picked.is_empty() {
        return tail_lines(text, max_lines, max_bytes);
    }
    if let Some(last) = lines.len().checked_sub(1)
        && !picked.contains(&last)
    {
        picked.push(last);
    }
    let mut kept: Vec<&str> = Vec::new();
    for &i in &picked {
        let line = lines[i];
        let caret_message = line.trim_start_matches([' ', '|']).trim_start_matches(['^', '-', '~']).trim();
        let is_caret = line.trim_start_matches([' ', '|']).starts_with(['^', '-', '~']);
        if is_caret && !caret_message.is_empty() && kept.iter().any(|k| k.contains(caret_message)) {
            continue;
        }
        kept.push(line);
    }
    let start = kept.len().saturating_sub(max_lines);
    let digest = kept[start..].join("\n");
    tail_lines(&digest, max_lines, max_bytes)
}

pub fn tail_lines(text: &str, max_lines: usize, max_bytes: usize) -> String {
    let lines: Vec<&str> = text.trim_end().lines().filter(|l| !l.trim().is_empty()).collect();
    let start = lines.len().saturating_sub(max_lines);
    let joined = lines[start..].join("\n");
    if joined.len() <= max_bytes {
        return joined;
    }
    let mut cut = joined.len() - max_bytes;
    while !joined.is_char_boundary(cut) {
        cut += 1;
    }
    format!("…{}", &joined[cut..])
}
