//! `agentsh pkg`: create, test, install, update and remove packages. This is a human's tool: agents
//! run programs through `agentsh run`, and an `exec`'d `agentsh pkg add` can't write the data folder
//! from inside the sandbox.

use crate::cli_args::{Cli, PkgAction};
use crate::pkg_test;
use agentsh_core::Registry;
use agentsh_core::package::{Origin, Package, installed_packages_dir, load_package_dir};
use agentsh_platform::{Config, Overrides, find_project_root};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Written next to an installed package so `pkg update` can fetch it again.
const SOURCE_FILE: &str = ".agentsh-source.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Source {
    source: String,
    path: Option<PathBuf>,
    rev: Option<String>,
    commit: Option<String>,
}

pub fn run(action: &PkgAction, cli: &Cli) -> ExitCode {
    let result = match action {
        PkgAction::Ls => list(cli),
        PkgAction::Add { source, path, rev, force } => {
            let src = Source { source: source.clone(), path: path.clone(), rev: rev.clone(), commit: None };
            add(cli, &src, *force)
        }
        PkgAction::Update { names } => update(cli, names),
        PkgAction::Rm { name } => remove(cli, name),
        PkgAction::New { name, dir } => new_package(name, dir.as_deref()),
        PkgAction::Test { dir } => return pkg_test::run(dir),
    };
    match result {
        Ok(text) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Err(message) => {
            eprintln!("agentsh pkg: {message}");
            ExitCode::FAILURE
        }
    }
}

fn root(cli: &Cli) -> PathBuf {
    let start = std::env::current_dir().unwrap_or_else(|_| ".".into());
    cli.root.clone().and_then(|r| r.canonicalize().ok()).unwrap_or_else(|| find_project_root(&start))
}

/// The registry with every package loaded for the current project, and that project's root.
pub fn active_registry(cli: &Cli) -> Result<(Registry, PathBuf), String> {
    let root = root(cli);
    let config = Config::load(&root, &Overrides::default())?;
    let mut registry = agentsh_builtins::registry();
    registry.activate(&config)?;
    Ok((registry, root))
}

fn read_source(dir: &Path) -> Option<Source> {
    serde_json::from_str(&std::fs::read_to_string(dir.join(SOURCE_FILE)).ok()?).ok()
}

fn list(cli: &Cli) -> Result<String, String> {
    let (registry, root) = active_registry(cli)?;
    let mut out = agentsh_builtins::packages_text(&registry, &root);
    let installed: Vec<String> = registry
        .packages()
        .iter()
        .filter_map(|p| match &p.origin {
            Origin::Installed(dir) => {
                let from = read_source(dir).map(|s| describe_source(&s)).unwrap_or_else(|| "unknown source".into());
                Some(format!("  {}: {from}", p.name))
            }
            _ => None,
        })
        .collect();
    for warning in registry.warnings() {
        out.push_str(&format!("\nwarning: {warning}"));
    }
    if !installed.is_empty() {
        out.push_str(&format!("\n\ninstalled from:\n{}", installed.join("\n")));
    }
    Ok(out)
}

fn describe_source(s: &Source) -> String {
    let mut text = s.source.clone();
    if let Some(path) = &s.path {
        text.push_str(&format!(" --path {}", path.display()));
    }
    if let Some(rev) = &s.rev {
        text.push_str(&format!(" --rev {rev}"));
    }
    if let Some(commit) = &s.commit {
        text.push_str(&format!(" (commit {})", &commit[..commit.len().min(10)]));
    }
    text
}

fn is_git_url(source: &str) -> bool {
    ["https://", "http://", "git@", "ssh://", "file://"].iter().any(|p| source.starts_with(p))
        || source.ends_with(".git")
}

fn add(cli: &Cli, wanted: &Source, force: bool) -> Result<String, String> {
    let installed = installed_packages_dir().ok_or("can't find the agentsh data folder (set AGENTSH_HOME)")?;
    std::fs::create_dir_all(&installed).map_err(|e| format!("can't create {}: {e}", installed.display()))?;
    let mut source = wanted.clone();
    let clone = if is_git_url(&source.source) {
        let clone = clone_repo(&source.source, source.rev.as_deref())?;
        source.commit = git_head(&clone.0);
        Some(clone)
    } else {
        if source.rev.is_some() {
            return Err("--rev only works with git URLs".into());
        }
        let abs = Path::new(&source.source).canonicalize().map_err(|e| format!("can't open {}: {e}", source.source))?;
        source.source = abs.display().to_string();
        None
    };
    let base = clone.as_ref().map(|c| c.0.clone()).unwrap_or_else(|| PathBuf::from(&source.source));
    let dir = source.path.as_ref().map(|s| base.join(s)).unwrap_or(base);
    let staged = load_package_dir(&dir, Origin::Path(dir.clone()))?;
    let (registry, _) = active_registry(cli)?;
    let dest = installed.join(&staged.name);
    if let Some(existing) = registry.package(&staged.name) {
        let replacing_installed = existing.origin == Origin::Installed(dest.clone());
        if !replacing_installed {
            return Err(format!("`{}` is already taken by the {} package", staged.name, existing.origin.describe()));
        }
        if !force {
            return Err(format!("`{}` is already installed; pass --force to replace it", staged.name));
        }
    }
    let temp = installed.join(format!(".incoming-{}", staged.name));
    let _ = std::fs::remove_dir_all(&temp);
    copy_package(&dir, &temp).map_err(|e| format!("can't copy {}: {e}", dir.display()))?;
    let record = serde_json::to_string_pretty(&source).unwrap_or_default();
    std::fs::write(temp.join(SOURCE_FILE), record).map_err(|e| format!("can't record the source: {e}"))?;
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&temp, &dest).map_err(|e| format!("can't install into {}: {e}", dest.display()))?;
    if let Err(message) = active_registry(cli) {
        let _ = std::fs::remove_dir_all(&dest);
        return Err(format!("{message}; nothing was installed"));
    }
    Ok(describe_install(&staged, &dest, &source))
}

fn describe_install(package: &Package, dest: &Path, source: &Source) -> String {
    let caps: Vec<&str> = package.caps.iter().map(|c| c.as_str()).collect();
    let commands: Vec<&str> = package.commands.iter().map(|c| c.spec.name).collect();
    let tasks: Vec<&str> = package.check.iter().flat_map(|c| c.tasks.keys().map(String::as_str)).collect();
    let mut lines = vec![
        format!("installed {} {} into {}", package.name, package.version, dest.display()),
        format!("  from: {}", describe_source(source)),
        format!("  caps it may use: {}", if caps.is_empty() { "none".into() } else { caps.join(", ") }),
    ];
    if !commands.is_empty() {
        lines.push(format!("  commands: {}", commands.join(" ")));
    }
    if !tasks.is_empty() {
        lines.push(format!("  check tasks: {}", tasks.join(", ")));
    }
    lines.push("  agents still need the session's own caps (e.g. --allow exec) to use them".into());
    lines.join("\n")
}

fn update(cli: &Cli, names: &[String]) -> Result<String, String> {
    let installed = installed_packages_dir().ok_or("can't find the agentsh data folder (set AGENTSH_HOME)")?;
    let mut targets: Vec<(String, Source)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&installed) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || (!names.is_empty() && !names.contains(&name)) {
                continue;
            }
            match read_source(&entry.path()) {
                Some(source) => targets.push((name, source)),
                None => return Err(format!("`{name}` has no recorded source; reinstall it with pkg add")),
            }
        }
    }
    if let Some(missing) = names.iter().find(|n| !targets.iter().any(|(t, _)| t == *n)) {
        return Err(format!("no installed package `{missing}`"));
    }
    if targets.is_empty() {
        return Ok("no installed packages".into());
    }
    targets.sort_by(|a, b| a.0.cmp(&b.0));
    let mut out = Vec::new();
    for (name, mut source) in targets {
        let before = source.commit.take();
        add(cli, &source, true).map_err(|e| format!("updating `{name}`: {e}"))?;
        let after = read_source(&installed.join(&name)).and_then(|s| s.commit);
        let note = match (before, after) {
            (Some(b), Some(a)) if a == b => "already up to date".to_string(),
            (Some(b), Some(a)) => format!("{} -> {}", &b[..b.len().min(10)], &a[..a.len().min(10)]),
            _ => "reinstalled".to_string(),
        };
        out.push(format!("{name}: {note}"));
    }
    Ok(out.join("\n"))
}

struct Clone(PathBuf);

impl Drop for Clone {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn clone_repo(url: &str, rev: Option<&str>) -> Result<Clone, String> {
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("agentsh-pkg-{}-{nanos}", std::process::id()));
    let mut cmd = std::process::Command::new("git");
    cmd.args(["clone", "--quiet", "--depth", "1"]);
    if let Some(rev) = rev {
        cmd.args(["--branch", rev]);
    }
    // Fail instead of waiting at a password prompt; private repos need SSH keys or a credential helper.
    let out = cmd
        .arg(url)
        .arg(&dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_SSH_COMMAND", std::env::var("GIT_SSH_COMMAND").unwrap_or_else(|_| "ssh -o BatchMode=yes".into()))
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("can't run git: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!(
            "git clone {url} failed: {err}\n  private repo? check `git ls-remote {url}` works without a prompt (SSH key or credential helper)"
        ));
    }
    Ok(Clone(dir))
}

fn git_head(dir: &Path) -> Option<String> {
    let out = std::process::Command::new("git").args(["rev-parse", "HEAD"]).current_dir(dir).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Copy a package folder: regular files and folders only. Symlinks and `.git` are skipped so a
/// package can't pull in files from outside its own folder.
fn copy_package(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let name = entry.file_name();
        if name == ".git" || name == SOURCE_FILE || kind.is_symlink() {
            continue;
        }
        let target = to.join(&name);
        if kind.is_dir() {
            copy_package(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

fn remove(cli: &Cli, name: &str) -> Result<String, String> {
    let installed = installed_packages_dir().ok_or("can't find the agentsh data folder (set AGENTSH_HOME)")?;
    let dest = installed.join(name);
    if dest.join("package.toml").is_file() {
        std::fs::remove_dir_all(&dest).map_err(|e| format!("can't remove {}: {e}", dest.display()))?;
        return Ok(format!("removed {name}"));
    }
    let (registry, _) = active_registry(cli)?;
    Err(match registry.package(name) {
        Some(p) if p.origin == Origin::BuiltIn => {
            format!("`{name}` is built in; turn it off with [packages] disable = [\"{name}\"] in .agentsh.toml")
        }
        Some(p) => format!("`{name}` comes from {}; remove it from [packages] paths", p.origin.describe()),
        None => format!("no installed package `{name}`"),
    })
}

fn new_package(name: &str, parent: Option<&Path>) -> Result<String, String> {
    let valid = name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        return Err(format!("`{name}` must start with a letter and use only letters, digits and _"));
    }
    let dir = parent.unwrap_or(Path::new(".")).join(name);
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    let test_file = format!("tests/{name}.json");
    let files = [
        ("package.toml", NEW_MANIFEST.replace("NAME", name)),
        ("brief.md", NEW_BRIEF.replace("NAME", name)),
        (test_file.as_str(), NEW_TEST.replace("NAME", name)),
        ("README.md", NEW_README.replace("NAME", name)),
    ];
    for (rel, text) in &files {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap_or(&dir))
            .map_err(|e| format!("can't create {}: {e}", dir.display()))?;
        std::fs::write(&path, text).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    }
    let shown = dir.display();
    Ok(format!(
        "created {shown}/ (package.toml, brief.md, tests/{name}.json, README.md)\n\
         next:\n  agentsh pkg test {shown}      run its tests\n  agentsh pkg add {shown}       install it for every project\n  \
         or, for one project, add to .agentsh.toml:  [packages] paths = [\"{shown}\"]"
    ))
}

const NEW_MANIFEST: &str = r#"name = "NAME"
version = "0.1.0"
summary = "one line: what NAME helps an agent do"
# The most this package's commands may use. A session must also grant exec/net itself.
caps = ["read"]
# Files at the project root that mean this package applies (shown in `env`). Optional.
detect = []

[[command]]
name = "hello"
summary = "greet someone (replace me with something useful)"
params = [
  { name = "who", type = "str", positional = true, required = true, doc = "who to greet" },
  { name = "loud", type = "bool", doc = "shout" },
]
returns = "string"
examples = ['NAME.hello "world"', 'NAME.hello "world" loud:true']
run = '''
let text = "hello $who"
if $loud { upper($text) } else { $text }
'''

# A command that runs a program needs "exec" in caps above and in its own caps:
# [[command]]
# name = "version"
# summary = "the installed tool's version"
# caps = ["read", "exec"]
# run = 'exec "git" "--version" parse:"text" | trim'
"#;

const NEW_BRIEF: &str = r#"NAME: what this package is for, in one line.

- `NAME.hello "world"`: what it returns, and when to use it.
- Keep this under ~200 tokens: it's what an agent reads after `help "NAME"`.
"#;

const NEW_TEST: &str = r#"[
  {
    "name": "NAME.hello greets",
    "steps": [
      { "run": "NAME.hello \"world\"", "expect": { "ok": true, "value": "hello world" } },
      { "run": "NAME.hello \"world\" loud:true", "expect": { "value": "HELLO WORLD" } },
      { "run": "NAME.hello", "expect": { "ok": false, "error.code": "missing_argument" } }
    ]
  }
]
"#;

const NEW_README: &str = r#"# NAME

An [agentsh](https://github.com/jestersimpps/agentsh) package.

```sh
agentsh pkg add <this repo's git URL>            # add --path <folder> if it isn't at the repo root
agentsh pkg test .                               # run the tests
```
"#;
