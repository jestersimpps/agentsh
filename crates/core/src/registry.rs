use crate::package::{EmbeddedPackage, Package, load_packages};
use crate::{Args, Ctx, Result};
use agentsh_lang::Program;
use agentsh_platform::{Capability, Config};
use agentsh_spec::Value;
use indexmap::IndexMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    Any,
    Str,
    Int,
    Bool,
    Path,
    Paths,
    Arg,
    List,
    Record,
    Duration,
    Expr,
}

impl ParamType {
    pub fn describe(&self) -> &'static str {
        match self {
            ParamType::Any => "any",
            ParamType::Str => "string",
            ParamType::Int => "int",
            ParamType::Bool => "bool",
            ParamType::Path => "path",
            ParamType::Paths => "path or list of paths",
            ParamType::Arg => "string or number",
            ParamType::List => "list",
            ParamType::Record => "record",
            ParamType::Duration => "duration (e.g. 30s)",
            ParamType::Expr => "row expression (e.g. .size > 1mb)",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ParamSpec {
    pub name: &'static str,
    pub ty: ParamType,
    pub positional: bool,
    pub required: bool,
    pub variadic: bool,
    pub doc: &'static str,
}

impl ParamSpec {
    pub const fn pos(name: &'static str, ty: ParamType, doc: &'static str) -> Self {
        Self { name, ty, positional: true, required: false, variadic: false, doc }
    }

    pub const fn named(name: &'static str, ty: ParamType, doc: &'static str) -> Self {
        Self { name, ty, positional: false, required: false, variadic: false, doc }
    }

    pub const fn required(self) -> Self {
        Self { required: true, ..self }
    }

    pub const fn variadic(self) -> Self {
        Self { variadic: true, ..self }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CommandSpec {
    pub name: &'static str,
    pub summary: &'static str,
    pub params: &'static [ParamSpec],
    pub input: Option<&'static str>,
    pub returns: &'static str,
    pub caps: &'static [Capability],
    pub examples: &'static [&'static str],
}

impl CommandSpec {
    pub fn param(&self, name: &str) -> Option<&ParamSpec> {
        self.params.iter().find(|p| p.name == name)
    }

    pub fn usage(&self) -> String {
        let mut out = String::from(self.name);
        for p in self.params.iter().filter(|p| Some(p.name) != self.input || p.positional) {
            let piece = match (p.positional, p.variadic) {
                (true, true) => format!("{}...", p.name),
                (true, false) => p.name.to_string(),
                (false, _) => format!("{}:{}", p.name, short_type(p.ty)),
            };
            out.push(' ');
            out.push_str(&if p.required { piece } else { format!("[{piece}]") });
        }
        out
    }

    pub fn brief_usage(&self) -> String {
        let mut parts = vec![self.name.to_string()];
        let mut optional = Vec::new();
        for p in self.params.iter().filter(|p| Some(p.name) != self.input || p.positional) {
            let piece = match (p.positional, p.variadic) {
                (true, true) => format!("{}...", p.name),
                (true, false) => p.name.to_string(),
                (false, _) => format!("{}:", p.name),
            };
            match (p.required, p.positional) {
                (true, _) => parts.push(piece),
                (false, true) => parts.push(format!("[{piece}]")),
                (false, false) => optional.push(piece),
            }
        }
        if !optional.is_empty() {
            parts.push(format!("[{}]", optional.join(" ")));
        }
        parts.join(" ")
    }

    pub fn to_value(&self) -> Value {
        let params = self
            .params
            .iter()
            .map(|p| {
                Value::record([
                    ("name", Value::str(p.name)),
                    ("type", Value::str(p.ty.describe())),
                    ("positional", Value::Bool(p.positional)),
                    ("required", Value::Bool(p.required)),
                    ("variadic", Value::Bool(p.variadic)),
                    ("doc", Value::str(p.doc)),
                ])
            })
            .collect();
        Value::record([
            ("name", Value::str(self.name)),
            ("summary", Value::str(self.summary)),
            ("usage", Value::str(self.usage())),
            ("params", Value::List(params)),
            ("input", self.input.map(Value::str).unwrap_or(Value::Null)),
            ("returns", Value::str(self.returns)),
            ("capabilities", Value::List(self.caps.iter().map(|c| Value::str(c.as_str())).collect())),
            ("examples", Value::List(self.examples.iter().map(|e| Value::str(*e)).collect())),
        ])
    }
}

fn short_type(ty: ParamType) -> &'static str {
    match ty {
        ParamType::Any => "value",
        ParamType::Str | ParamType::Arg => "str",
        ParamType::Int => "int",
        ParamType::Bool => "bool",
        ParamType::Path => "path",
        ParamType::Paths => "paths",
        ParamType::List => "list",
        ParamType::Record => "record",
        ParamType::Duration => "duration",
        ParamType::Expr => "expr",
    }
}

pub type RunFn = fn(&mut Ctx, &mut Args) -> Result<Value>;

/// A native command as defined in Rust.
#[derive(Clone, Copy)]
pub struct Builtin {
    pub spec: CommandSpec,
    pub run: RunFn,
}

#[derive(Clone, Copy)]
pub enum Runner {
    Native(RunFn),
    /// An agentsh program from a package's `[[command]]`; its params are bound as `$name`.
    Script(&'static Program),
}

/// A registered command: its spec, how it runs, and the package it belongs to.
#[derive(Clone, Copy)]
pub struct Command {
    pub spec: CommandSpec,
    pub runner: Runner,
    pub package: &'static str,
    /// Capabilities a script command may use (its package's `caps`); unused for native commands.
    pub ceiling: &'static [Capability],
}

impl Command {
    pub fn is_native(&self) -> bool {
        matches!(self.runner, Runner::Native(_))
    }
}

/// The core package: commands every installation has and that can't be turned off.
pub const CORE: &str = "core";

#[derive(Default, Clone)]
pub struct Registry {
    commands: IndexMap<&'static str, Command>,
    embedded: Vec<EmbeddedPackage>,
    packages: Vec<Package>,
    /// Native commands removed because their package is turned off: command -> package.
    turned_off: IndexMap<&'static str, &'static str>,
    warnings: Vec<String>,
}

impl Registry {
    /// Add a native command to the core package.
    pub fn add(&mut self, builtin: Builtin) {
        self.add_to(CORE, builtin);
    }

    /// Add a native command owned by a package (removed if that package is turned off).
    pub fn add_to(&mut self, package: &'static str, builtin: Builtin) {
        let command = Command { spec: builtin.spec, runner: Runner::Native(builtin.run), package, ceiling: &[] };
        self.commands.insert(builtin.spec.name, command);
    }

    /// Register package sources compiled into the binary; they load in `activate`.
    pub fn embed(&mut self, packages: &[EmbeddedPackage]) {
        self.embedded.extend_from_slice(packages);
    }

    /// Load every package (built-in, configured paths, installed), drop commands of packages that are
    /// turned off, and add the script commands of those that are on.
    pub fn activate(&mut self, config: &Config) -> std::result::Result<(), String> {
        let (packages, warnings) = load_packages(&self.embedded, config)?;
        self.warnings = warnings;
        let native_owners: Vec<&'static str> = self.commands.values().map(|c| c.package).collect();
        for owner in native_owners {
            if owner != CORE && !packages.iter().any(|p| p.name == owner) {
                return Err(format!("native commands belong to `{owner}`, but no package has that name"));
            }
        }
        let on = |c: &Command| c.package == CORE || packages.iter().any(|p| p.name == c.package && p.enabled);
        for command in self.commands.values().filter(|c| !on(c)) {
            self.turned_off.insert(command.spec.name, command.package);
        }
        self.commands.retain(|_, c| on(c));
        for package in packages.iter().filter(|p| p.enabled) {
            let owner: &'static str = Box::leak(package.name.clone().into_boxed_str());
            for script in &package.commands {
                let command = Command {
                    spec: script.spec,
                    runner: Runner::Script(script.program),
                    package: owner,
                    ceiling: package.caps,
                };
                self.commands.insert(script.spec.name, command);
            }
        }
        self.packages = packages;
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Command> {
        self.commands.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.commands.keys().copied()
    }

    pub fn all(&self) -> impl Iterator<Item = &Command> {
        self.commands.values()
    }

    /// Native commands only: the stable set the cheat sheet describes.
    pub fn natives(&self) -> impl Iterator<Item = &Command> {
        self.commands.values().filter(|c| c.is_native())
    }

    /// Every known package, including turned-off ones (empty until `activate`).
    pub fn packages(&self) -> &[Package] {
        &self.packages
    }

    /// Problems found while loading packages (skipped broken packages); shown with every result.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The package that owns a native command removed because that package is turned off.
    pub fn turned_off_owner(&self, command: &str) -> Option<&'static str> {
        self.turned_off.get(command).copied()
    }

    pub fn package(&self, name: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.name == name)
    }

    /// Turned-on packages in load order.
    pub fn enabled_packages(&self) -> impl Iterator<Item = &Package> {
        self.packages.iter().filter(|p| p.enabled)
    }
}
