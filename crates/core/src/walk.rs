use crate::{Ctx, Result, ShellError};
use agentsh_spec::ErrorCode;
use globset::{Glob, GlobBuilder, GlobMatcher};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};

pub struct WalkOptions {
    pub max_depth: Option<usize>,
    pub hidden: bool,
    pub respect_ignore: bool,
}

pub fn walk(root: &Path, options: &WalkOptions) -> impl Iterator<Item = ignore::DirEntry> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!options.hidden)
        .git_ignore(options.respect_ignore)
        .git_global(options.respect_ignore)
        .git_exclude(options.respect_ignore)
        .ignore(options.respect_ignore)
        .parents(options.respect_ignore)
        .require_git(false)
        .max_depth(options.max_depth)
        .sort_by_file_name(|a, b| a.cmp(b))
        .filter_entry(|e| e.file_name() != ".agentsh");
    builder.build().flatten().filter(|e| e.depth() > 0)
}

pub fn compile_glob(pattern: &str, literal_separator: bool) -> Result<GlobMatcher> {
    GlobBuilder::new(pattern).literal_separator(literal_separator).build().map(|g: Glob| g.compile_matcher()).map_err(
        |e| {
            ShellError::new(ErrorCode::InvalidValue, format!("invalid glob: {e}"))
                .with_value(pattern)
                .with_hint("globs use * ? ** [abc] {a,b}")
        },
    )
}

pub fn glob_paths(ctx: &Ctx, pattern: &str) -> Result<Vec<String>> {
    let (base_raw, rest) = split_literal_prefix(pattern);
    let base = if base_raw.is_empty() { ctx.cwd() } else { ctx.resolve(&base_raw)? };
    if rest.is_empty() {
        return Ok(if base.exists() { vec![ctx.display(&base)] } else { Vec::new() });
    }
    let matcher = compile_glob(&rest, true)?;
    let recursive = rest.contains("**");
    let options = WalkOptions {
        max_depth: if recursive { None } else { Some(rest.split('/').count()) },
        hidden: rest.starts_with('.') || rest.contains("/."),
        respect_ignore: recursive,
    };
    let mut out: Vec<String> = walk(&base, &options)
        .filter(|e| e.path().strip_prefix(&base).is_ok_and(|rel| matcher.is_match(rel)))
        .map(|e| ctx.display(e.path()))
        .collect();
    out.sort();
    Ok(out)
}

fn split_literal_prefix(pattern: &str) -> (String, String) {
    let parts: Vec<&str> = pattern.split('/').collect();
    let first_glob = parts.iter().position(|p| p.contains(['*', '?', '[', '{'])).unwrap_or(parts.len());
    let mut base: PathBuf = parts[..first_glob].iter().collect();
    if pattern.starts_with('/') {
        base = PathBuf::from("/").join(base);
    }
    (base.to_string_lossy().into_owned(), parts[first_glob..].join("/"))
}

#[cfg(test)]
mod tests {
    use super::split_literal_prefix;

    #[test]
    fn literal_prefix() {
        assert_eq!(split_literal_prefix("*.rs"), ("".into(), "*.rs".into()));
        assert_eq!(split_literal_prefix("src/**/*.rs"), ("src".into(), "**/*.rs".into()));
        assert_eq!(split_literal_prefix("../x/*.md"), ("../x".into(), "*.md".into()));
        assert_eq!(split_literal_prefix("README.md"), ("README.md".into(), "".into()));
    }
}
