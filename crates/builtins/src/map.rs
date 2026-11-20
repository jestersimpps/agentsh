use agentsh_core::{Args, Builtin, CommandSpec, Ctx, ParamSpec as P, ParamType as T, Result, WalkOptions, walk};
use agentsh_platform::Capability::Read;
use agentsh_spec::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MAX_FILES_LISTED: usize = 12;
const MAX_ENTRIES: usize = 400;

pub const MAP: Builtin = Builtin {
    spec: CommandSpec {
        name: "map",
        summary: "compact tree of the project: folders with file counts and sizes (respects .gitignore)",
        params: &[
            P::pos("path", T::Path, "folder; default: current folder"),
            P::named("depth", T::Int, "folder levels to expand (default 2)"),
        ],
        input: None,
        returns: "text",
        caps: &[Read],
        examples: &["map", "map \"src\" depth:3"],
    },
    run: map,
};

#[derive(Default)]
struct Node {
    files: Vec<(String, u64)>,
    dirs: BTreeMap<String, Node>,
}

impl Node {
    fn totals(&self) -> (usize, u64) {
        self.dirs
            .values()
            .map(Node::totals)
            .fold((self.files.len(), self.files.iter().map(|(_, s)| s).sum()), |(n, s), (dn, ds)| (n + dn, s + ds))
    }

    fn insert(&mut self, parts: &[String], size: u64) {
        match parts {
            [file] => self.files.push((file.clone(), size)),
            [dir, rest @ ..] => self.dirs.entry(dir.clone()).or_default().insert(rest, size),
            [] => {}
        }
    }
}

fn map(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let raw = args.opt_str("path")?.unwrap_or_else(|| ".".into());
    let base = ctx.existing(&raw)?;
    let depth = args.opt_usize("depth")?.unwrap_or(2);
    let mut root = Node::default();
    let options = WalkOptions { max_depth: None, hidden: false, respect_ignore: true };
    for entry in walk(&base, &options).filter(|e| e.file_type().is_some_and(|t| t.is_file())) {
        let rel: PathBuf = entry.path().strip_prefix(&base).unwrap_or(entry.path()).to_path_buf();
        let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        root.insert(&parts, entry.metadata().map(|m| m.len()).unwrap_or(0));
    }
    let (count, size) = root.totals();
    let mut lines = vec![format!("{} ({count} files, {})", display_base(ctx, &base), human(size))];
    render(&root, 1, depth, &mut lines);
    if lines.len() > MAX_ENTRIES {
        let hidden = lines.len() - MAX_ENTRIES;
        lines.truncate(MAX_ENTRIES);
        lines.push(format!("… {hidden} more lines; map a subfolder or lower depth:"));
    }
    Ok(Value::Str(lines.join("\n")))
}

fn display_base(ctx: &Ctx, base: &Path) -> String {
    let shown = ctx.display(base);
    if shown == "." { shown } else { format!("{shown}/") }
}

fn render(node: &Node, level: usize, max_depth: usize, lines: &mut Vec<String>) {
    let indent = "  ".repeat(level);
    for (name, child) in &node.dirs {
        let (count, size) = child.totals();
        lines.push(format!("{indent}{name}/ {count} files, {}", human(size)));
        if level < max_depth {
            render(child, level + 1, max_depth, lines);
        }
    }
    if node.files.len() <= MAX_FILES_LISTED {
        let mut files = node.files.clone();
        files.sort();
        lines.extend(files.iter().map(|(name, size)| format!("{indent}{name} {}", human(*size))));
    } else {
        let mut by_ext: BTreeMap<String, usize> = BTreeMap::new();
        for (name, _) in &node.files {
            let ext = Path::new(name)
                .extension()
                .map(|e| format!("*.{}", e.to_string_lossy()))
                .unwrap_or_else(|| "other".into());
            *by_ext.entry(ext).or_default() += 1;
        }
        let mut exts: Vec<(String, usize)> = by_ext.into_iter().collect();
        exts.sort_by(|a, b| b.1.cmp(&a.1));
        let summary: Vec<String> = exts.iter().map(|(e, n)| format!("{e} {n}")).collect();
        lines.push(format!("{indent}{} files: {}", node.files.len(), summary.join(", ")));
    }
}

fn human(bytes: u64) -> String {
    match bytes {
        b if b < 1_000 => format!("{b} B"),
        b if b < 1_000_000 => format!("{:.0} KB", b as f64 / 1_000.0),
        b if b < 1_000_000_000 => format!("{:.1} MB", b as f64 / 1_000_000.0),
        b => format!("{:.1} GB", b as f64 / 1_000_000_000.0),
    }
}
