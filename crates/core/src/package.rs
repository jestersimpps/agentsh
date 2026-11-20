//! Packages: named groups of commands, check runners and a briefing the model loads on demand.
//!
//! A package is a folder with a `package.toml` and an optional `brief.md`. First-party packages are
//! embedded in the binary; others come from `[packages] paths` or `agentsh pkg add`. Script commands
//! are agentsh programs, so they inherit workspace limits, undo, rollback, the sandbox and redaction.

use crate::registry::{CommandSpec, ParamSpec, ParamType};
use agentsh_lang::Program;
use agentsh_platform::{Capability, Config, global_data_dir};
use indexmap::IndexMap;
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Test-output parsers implemented in Rust that a package can name with `[check] parser = "..."`.
pub const CHECK_PARSERS: &[&str] = &["cargo", "pytest", "js", "go", "swift"];

/// Most nested package-command calls allowed (a package command calling another).
pub const MAX_SCRIPT_DEPTH: usize = 16;

/// A package's source files: the manifest text and the briefing text.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddedPackage {
    pub manifest: &'static str,
    pub brief: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    BuiltIn,
    Path(PathBuf),
    Installed(PathBuf),
}

impl Origin {
    pub fn describe(&self) -> String {
        match self {
            Origin::BuiltIn => "built-in".into(),
            Origin::Path(p) => format!("path {}", p.display()),
            Origin::Installed(p) => format!("installed {}", p.display()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub summary: String,
    pub brief: String,
    pub caps: &'static [Capability],
    pub detect: Vec<String>,
    pub check: Option<CheckSpec>,
    pub origin: Origin,
    pub enabled: bool,
    pub commands: Vec<ScriptCommand>,
}

impl Package {
    /// True when one of the package's marker files exists at the root. Packages without markers
    /// (like `code`) apply everywhere and are never "detected".
    pub fn detected(&self, root: &Path) -> bool {
        !self.detect.is_empty() && self.detect.iter().any(|f| root.join(f).exists())
    }

    pub fn qualified(&self, command: &str) -> String {
        format!("{}.{command}", self.name)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ScriptCommand {
    pub spec: CommandSpec,
    pub program: &'static Program,
}

#[derive(Debug, Clone, Default)]
pub struct CheckSpec {
    pub parser: Option<TestParser>,
    pub recognize: Vec<String>,
    pub tasks: IndexMap<String, CheckTask>,
}

#[derive(Debug, Clone)]
pub enum TestParser {
    Named(String),
    Patterns { passed: Option<String>, failed: Option<String>, failure: Option<String> },
}

#[derive(Debug, Clone, Default)]
pub struct CheckTask {
    pub cmd: Vec<String>,
    pub package_script: Vec<String>,
    pub requires: Option<String>,
}

// ---- manifest format ------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    name: String,
    version: String,
    summary: String,
    #[serde(default)]
    caps: Vec<Capability>,
    #[serde(default)]
    detect: Vec<String>,
    #[serde(default = "yes")]
    enabled: bool,
    #[serde(default)]
    check: Option<CheckManifest>,
    #[serde(default, rename = "command")]
    commands: Vec<CommandManifest>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckManifest {
    parser: Option<String>,
    #[serde(default)]
    patterns: Option<PatternsManifest>,
    #[serde(default)]
    recognize: Vec<String>,
    test: Option<TaskManifest>,
    build: Option<TaskManifest>,
    lint: Option<TaskManifest>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PatternsManifest {
    passed: Option<String>,
    failed: Option<String>,
    failure: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TaskManifest {
    #[serde(default)]
    cmd: Vec<String>,
    #[serde(default)]
    package_script: Vec<String>,
    requires: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandManifest {
    name: String,
    summary: String,
    #[serde(default)]
    params: Vec<ParamManifest>,
    input: Option<String>,
    #[serde(default = "default_returns")]
    returns: String,
    #[serde(default)]
    caps: Vec<Capability>,
    #[serde(default)]
    examples: Vec<String>,
    run: String,
}

fn default_returns() -> String {
    "value".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParamManifest {
    name: String,
    #[serde(rename = "type", default = "default_type")]
    ty: String,
    #[serde(default)]
    positional: bool,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    variadic: bool,
    #[serde(default)]
    doc: String,
}

fn default_type() -> String {
    "any".into()
}

// ---- parsing --------------------------------------------------------------------------------

fn leak(s: impl Into<String>) -> &'static str {
    Box::leak(s.into().into_boxed_str())
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn param_type(name: &str) -> Option<ParamType> {
    Some(match name {
        "any" => ParamType::Any,
        "str" | "string" => ParamType::Str,
        "int" => ParamType::Int,
        "bool" => ParamType::Bool,
        "path" => ParamType::Path,
        "paths" => ParamType::Paths,
        "arg" => ParamType::Arg,
        "list" => ParamType::List,
        "record" => ParamType::Record,
        "duration" => ParamType::Duration,
        _ => return None,
    })
}

/// Parse and validate one package. Errors name the package and the problem.
pub fn parse_package(manifest: &str, brief: &str, origin: Origin) -> Result<Package, String> {
    let m: Manifest = toml::from_str(manifest).map_err(|e| format!("invalid package.toml: {e}"))?;
    let at = |msg: String| format!("package `{}`: {msg}", m.name);
    if !is_ident(&m.name) {
        return Err(format!("package name `{}` must be a plain identifier (letters, digits, _)", m.name));
    }
    let caps: &'static [Capability] = Box::leak(m.caps.clone().into_boxed_slice());
    let check = m.check.as_ref().map(|c| parse_check(c).map_err(at)).transpose()?;
    let mut commands = Vec::new();
    for cmd in &m.commands {
        commands.push(parse_command(&m.name, caps, cmd).map_err(at)?);
    }
    let mut seen = std::collections::HashSet::new();
    if let Some(dup) = commands.iter().find(|c| !seen.insert(c.spec.name)) {
        return Err(at(format!("command `{}` is defined twice", dup.spec.name)));
    }
    Ok(Package {
        name: m.name,
        version: m.version,
        summary: m.summary,
        brief: brief.trim().to_string(),
        caps,
        detect: m.detect,
        check,
        origin,
        enabled: m.enabled,
        commands,
    })
}

fn parse_check(c: &CheckManifest) -> Result<CheckSpec, String> {
    let parser = match (&c.parser, &c.patterns) {
        (Some(_), Some(_)) => return Err("[check] takes `parser` or `[check.patterns]`, not both".into()),
        (Some(name), None) if CHECK_PARSERS.contains(&name.as_str()) => Some(TestParser::Named(name.clone())),
        (Some(name), None) => {
            return Err(format!("unknown check parser `{name}` (built-in: {})", CHECK_PARSERS.join(", ")));
        }
        (None, Some(p)) => {
            for pattern in [&p.passed, &p.failed, &p.failure].into_iter().flatten() {
                regex::Regex::new(pattern).map_err(|e| format!("bad regex in [check.patterns]: {e}"))?;
            }
            Some(TestParser::Patterns {
                passed: p.passed.clone(),
                failed: p.failed.clone(),
                failure: p.failure.clone(),
            })
        }
        (None, None) => None,
    };
    let mut tasks = IndexMap::new();
    for (name, task) in [("test", &c.test), ("build", &c.build), ("lint", &c.lint)] {
        let Some(task) = task else { continue };
        if task.cmd.is_empty() == task.package_script.is_empty() {
            return Err(format!("[check.{name}] needs exactly one of `cmd` or `package_script`"));
        }
        tasks.insert(
            name.to_string(),
            CheckTask {
                cmd: task.cmd.clone(),
                package_script: task.package_script.clone(),
                requires: task.requires.clone(),
            },
        );
    }
    Ok(CheckSpec { parser, recognize: c.recognize.clone(), tasks })
}

fn parse_command(package: &str, package_caps: &[Capability], c: &CommandManifest) -> Result<ScriptCommand, String> {
    if !is_ident(&c.name) {
        return Err(format!("command name `{}` must be a plain identifier", c.name));
    }
    if let Some(cap) = c.caps.iter().find(|cap| !package_caps.contains(cap)) {
        return Err(format!("command `{}` needs `{cap}`, which the package doesn't declare in caps", c.name));
    }
    let mut params = Vec::new();
    for p in &c.params {
        if !is_ident(&p.name) {
            return Err(format!("command `{}`: parameter name `{}` must be a plain identifier", c.name, p.name));
        }
        let ty = param_type(&p.ty).ok_or_else(|| format!("command `{}`: unknown parameter type `{}`", c.name, p.ty))?;
        let base = if p.positional {
            ParamSpec::pos(leak(p.name.clone()), ty, leak(p.doc.clone()))
        } else {
            ParamSpec::named(leak(p.name.clone()), ty, leak(p.doc.clone()))
        };
        let base = if p.variadic { base.variadic() } else { base };
        params.push(if p.required { base.required() } else { base });
    }
    if let Some(input) = &c.input
        && !params.iter().any(|p: &ParamSpec| p.name == input)
    {
        return Err(format!("command `{}`: input `{input}` is not one of its params", c.name));
    }
    let program =
        agentsh_lang::parse(&c.run).map_err(|e| format!("command `{}`: run doesn't parse: {}", c.name, e.message))?;
    let spec = CommandSpec {
        name: leak(format!("{package}.{}", c.name)),
        summary: leak(c.summary.clone()),
        params: Box::leak(params.into_boxed_slice()),
        input: c.input.clone().map(leak),
        returns: leak(c.returns.clone()),
        caps: if c.caps.is_empty() {
            package_caps_static(package_caps)
        } else {
            Box::leak(c.caps.clone().into_boxed_slice())
        },
        examples: Box::leak(c.examples.iter().cloned().map(leak).collect::<Vec<_>>().into_boxed_slice()),
    };
    Ok(ScriptCommand { spec, program: Box::leak(Box::new(program)) })
}

fn package_caps_static(caps: &[Capability]) -> &'static [Capability] {
    Box::leak(caps.to_vec().into_boxed_slice())
}

/// Read a package folder (`package.toml` plus optional `brief.md`).
pub fn load_package_dir(dir: &Path, origin: Origin) -> Result<Package, String> {
    let manifest_path = dir.join("package.toml");
    let manifest =
        std::fs::read_to_string(&manifest_path).map_err(|e| format!("can't read {}: {e}", manifest_path.display()))?;
    let brief = std::fs::read_to_string(dir.join("brief.md")).unwrap_or_default();
    parse_package(&manifest, &brief, origin).map_err(|e| format!("{e} ({})", dir.display()))
}

/// Where `agentsh pkg add` puts packages: `<data dir>/packages/<name>`.
pub fn installed_packages_dir() -> Option<PathBuf> {
    global_data_dir().map(|d| d.join("packages"))
}

/// Every known package in load order: built-in, then `[packages] paths`, then installed.
/// `enabled` reflects the manifest default and the `[packages] enable/disable` config.
/// A broken built-in package is an error; a broken configured or installed one is skipped and
/// reported in the returned warnings, so one bad package doesn't take the shell down. When two
/// packages share a name the first one wins (built-in, then paths, then installed), so a local
/// copy in `[packages] paths` or `AGENTSH_PACKAGES` overrides the installed version while you work on it.
pub fn load_packages(embedded: &[EmbeddedPackage], config: &Config) -> Result<(Vec<Package>, Vec<String>), String> {
    let mut packages = Vec::new();
    let mut warnings = Vec::new();
    for e in embedded {
        packages.push(parse_package(e.manifest, e.brief, Origin::BuiltIn).map_err(|err| format!("built-in {err}"))?);
    }
    let mut add = |result: Result<Package, String>| match result {
        Ok(p) => packages.push(p),
        Err(e) => warnings.push(format!("skipped a package: {e}")),
    };
    for dir in &config.package_paths {
        add(load_package_dir(dir, Origin::Path(dir.clone())));
    }
    if let Some(installed) = installed_packages_dir().filter(|d| d.is_dir()) {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&installed)
            .map_err(|e| format!("can't list {}: {e}", installed.display()))?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("package.toml").is_file())
            .collect();
        dirs.sort();
        for dir in dirs {
            add(load_package_dir(&dir, Origin::Installed(dir.clone())));
        }
    }
    let mut unique: Vec<Package> = Vec::with_capacity(packages.len());
    for p in packages {
        match unique.iter().find(|q| q.name == p.name) {
            Some(first) => warnings.push(format!(
                "two packages are named `{}`: using the {} one, skipping the {} one",
                p.name,
                first.origin.describe(),
                p.origin.describe()
            )),
            None => unique.push(p),
        }
    }
    let mut packages = unique;
    for name in config.package_enable.iter().chain(&config.package_disable) {
        if !packages.iter().any(|p| &p.name == name) {
            return Err(format!("[packages] names `{name}`, but no package has that name"));
        }
    }
    for p in &mut packages {
        if config.package_enable.contains(&p.name) {
            p.enabled = true;
        }
        if config.package_disable.contains(&p.name) {
            p.enabled = false;
        }
    }
    Ok((packages, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEMO: &str = r#"
name = "demo"
version = "0.1.0"
summary = "demo"
caps = ["read"]

[[command]]
name = "hello"
summary = "say hello"
params = [{ name = "who", type = "str", positional = true, required = true }]
run = '"hello $who"'
"#;

    #[test]
    fn parses_a_script_command() {
        let p = parse_package(DEMO, "brief\n", Origin::BuiltIn).unwrap();
        assert_eq!(p.commands[0].spec.name, "demo.hello");
        assert_eq!(p.commands[0].spec.usage(), "demo.hello who");
        assert_eq!(p.brief, "brief");
    }

    #[test]
    fn command_caps_must_fit_the_package() {
        let bad = DEMO.replace("summary = \"say hello\"", "summary = \"say hello\"\ncaps = [\"exec\"]");
        let err = parse_package(&bad, "", Origin::BuiltIn).unwrap_err();
        assert!(err.contains("doesn't declare"), "{err}");
    }

    #[test]
    fn rejects_unknown_parser_and_bad_scripts() {
        let bad = "name = \"x\"\nversion = \"1\"\nsummary = \"s\"\n[check]\nparser = \"nope\"\n";
        assert!(parse_package(bad, "", Origin::BuiltIn).unwrap_err().contains("unknown check parser"));
        let bad = DEMO.replace("'\"hello $who\"'", "'let'");
        assert!(parse_package(&bad, "", Origin::BuiltIn).unwrap_err().contains("doesn't parse"));
    }
}
