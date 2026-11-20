use crate::{Args, Ctx, Result};
use agentsh_platform::Capability;
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

#[derive(Clone, Copy)]
pub struct Builtin {
    pub spec: CommandSpec,
    pub run: RunFn,
}

#[derive(Default, Clone)]
pub struct Registry {
    commands: IndexMap<&'static str, Builtin>,
}

impl Registry {
    pub fn add(&mut self, builtin: Builtin) {
        self.commands.insert(builtin.spec.name, builtin);
    }

    pub fn get(&self, name: &str) -> Option<&Builtin> {
        self.commands.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.commands.keys().copied()
    }

    pub fn all(&self) -> impl Iterator<Item = &Builtin> {
        self.commands.values()
    }
}
