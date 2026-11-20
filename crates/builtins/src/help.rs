use agentsh_core::package::Package;
use agentsh_core::{
    Args, Builtin, CommandSpec, Ctx, FUNCTIONS, ParamSpec as P, ParamType as T, Registry, Result, ShellError, closest,
};
use agentsh_spec::{ErrorCode, LANGUAGE_VERSION, Value};
use std::path::Path;

const SYNTAX: &str = r##"SYNTAX
  cmd arg key:value          no flags; always quote strings: "a $var ${expr}"  'raw'  r#"has "quotes""#
  $var $rec.f $list[0]       .f = field of the current row (where sort pick group sum uniq)
  1kb 5mb 30s 5m 1d  [1, 2]  {k: "v"}  true false null
  a | b                      pipes pass values; (cmd ...) uses a result as a value
  let x = ...  if c { } else { }  for $f in list { }  try { } catch $e { }
  and or not == != < <= > >= =~ in + - * / %    ; or newline separates statements
  A failing statement stops the run and undoes its file changes (exec ... keep:true keeps them)."##;

const RESULTS: &str = r#"RESULTS
  Value first, then effects + undo token (agentsh undo u_N), warnings; or `error <code>: msg` + hint.
  Long output is cut: $rN holds the full value, $_ the previous one. Edits fail `stale` if the
  file changed since you read it. help "<cmd>" shows every option."#;

const PACKAGES: &str = r#"PACKAGES
  Packages add check runners and commands named pkg.cmd (js.scripts). `env` lists the ones this
  project uses; help "<package>" briefs one; help "packages" lists them all."#;

const EXAMPLES: &str = r##"EXAMPLES
  find "src" name:"*.rs" | where .size > 10kb | sort .size desc:true | take 5
  grep "fn main" in:"src" after:20
  read "a.rs" "b.rs"      read "big.rs" symbol:"parse"      outline "big.rs"
  map    refs "parse_args"    check            check "lint"
  edit "src/a.rs" old:"x = 1" new:"x = 2"; check keep:true
  write "src/b.swift" r#"print("hi \(name)")"#
  exec "git log --oneline -5" parse:"lines""##;

pub const HELP: Builtin = Builtin {
    spec: CommandSpec {
        name: "help",
        summary: "\"compact\" for a cheat sheet, a command or package name for details, nothing for an index",
        params: &[P::pos("topic", T::Str, "\"compact\", \"syntax\", \"packages\", a command or a package name")],
        input: None,
        returns: "string or record",
        caps: &[],
        examples: &["help \"compact\"", "help \"find\"", "help \"packages\"", "help \"js\""],
    },
    run: help,
};

fn help(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    match args.opt_str("topic")?.as_deref() {
        None => Ok(index(ctx.registry)),
        Some("compact") => Ok(Value::Str(compact(ctx.registry))),
        Some("brief") => Ok(Value::Str(brief(ctx.registry))),
        Some("syntax") => Ok(Value::str(SYNTAX)),
        Some("packages") => Ok(packages_value(ctx.registry, &ctx.root)),
        Some(name) => match (ctx.registry.get(name), ctx.registry.package(name)) {
            (Some(command), _) => Ok(command.spec.to_value()),
            (None, Some(package)) => Ok(Value::Str(package_text(ctx.registry, package))),
            (None, None) => {
                let err = ShellError::new(ErrorCode::NotFound, format!("no help topic `{name}`"));
                let mut topics: Vec<&str> = ctx.registry.names().collect();
                topics.extend(ctx.registry.packages().iter().map(|p| p.name.as_str()));
                Err(match closest(name, topics) {
                    Some(best) => err.with_hint(format!("did you mean \"{best}\"?")),
                    None => {
                        err.with_hint("topics: \"compact\", \"syntax\", \"packages\", or any command or package name")
                    }
                })
            }
        },
    }
}

fn status(package: &Package, root: &Path) -> &'static str {
    match (package.enabled, package.detected(root)) {
        (true, true) => "on, detected",
        (true, false) => "on",
        (false, true) => "off, detected",
        (false, false) => "off",
    }
}

fn packages_value(registry: &Registry, root: &Path) -> Value {
    let rows = registry
        .packages()
        .iter()
        .map(|p| {
            Value::record([
                ("package", Value::str(&p.name)),
                ("version", Value::str(&p.version)),
                ("status", Value::str(status(p, root))),
                ("summary", Value::str(&p.summary)),
            ])
        })
        .collect();
    Value::List(rows)
}

/// `agentsh help packages` / `agentsh pkg ls`: one line per package.
pub fn packages_text(registry: &Registry, root: &Path) -> String {
    let width = registry.packages().iter().map(|p| p.name.len()).max().unwrap_or(0);
    let mut lines: Vec<String> = registry
        .packages()
        .iter()
        .map(|p| {
            let origin = if p.origin == agentsh_core::package::Origin::BuiltIn {
                String::new()
            } else {
                format!(" [{}]", p.origin.describe())
            };
            format!("{:width$}  {:8}  {:13}  {}{origin}", p.name, p.version, status(p, root), p.summary)
        })
        .collect();
    lines.push(String::new());
    lines.push(
        "help \"<package>\" shows one; turn packages on or off with [packages] enable/disable in .agentsh.toml".into(),
    );
    lines.join("\n")
}

/// The briefing for one package: its brief.md, then its commands.
pub fn package_text(registry: &Registry, package: &Package) -> String {
    let mut out =
        if package.brief.is_empty() { format!("{}: {}", package.name, package.summary) } else { package.brief.clone() };
    let commands: Vec<String> = registry
        .natives()
        .filter(|c| c.package == package.name)
        .map(|c| c.spec)
        .chain(package.commands.iter().map(|c| c.spec))
        .map(|spec| command_line(&spec))
        .collect();
    if !commands.is_empty() {
        out.push_str(&format!("\n\nCOMMANDS\n{}", commands.join("\n")));
    }
    if !package.enabled {
        out.push_str(&format!(
            "\n\n(turned off: a human can turn it on with [packages] enable = [\"{}\"] in .agentsh.toml)",
            package.name
        ));
    }
    out
}

fn index(registry: &Registry) -> Value {
    let commands = registry
        .all()
        .map(|b| Value::record([("usage", Value::Str(b.spec.usage())), ("summary", Value::str(b.spec.summary))]))
        .collect();
    let packages = registry.enabled_packages().map(|p| Value::Str(format!("{}: {}", p.name, p.summary))).collect();
    let functions = FUNCTIONS.iter().map(|(sig, doc)| Value::Str(format!("{sig}: {doc}"))).collect();
    Value::record([
        ("language", Value::str(LANGUAGE_VERSION)),
        ("commands", Value::List(commands)),
        ("functions", Value::List(functions)),
        ("packages", Value::List(packages)),
        (
            "session_commands",
            Value::str("agentsh undo [u_N] | changes | commit | discard | checkpoint NAME | rollback NAME"),
        ),
        (
            "more",
            Value::str("help \"compact\" for a cheat sheet; help \"<command>\" or help \"<package>\" for details"),
        ),
    ])
}

const BRIEF_COMMANDS: &[&str] =
    &["env", "map", "find", "read", "outline", "grep", "refs", "edit", "write", "rm", "exec", "check"];

pub fn brief(registry: &Registry) -> String {
    let commands: Vec<String> =
        BRIEF_COMMANDS.iter().filter_map(|name| registry.get(name)).map(|b| command_line(&b.spec)).collect();
    let others: Vec<&str> = registry.natives().map(|c| c.spec.name).filter(|n| !BRIEF_COMMANDS.contains(n)).collect();
    format!(
        "agentsh: a shell language for agents.\n\n{SYNTAX}\n\nCOMMANDS\n{}\n  also: {}\n  help \"<cmd>\" shows any command's options\n\n{PACKAGES}\n",
        commands.join("\n"),
        others.join(" ")
    )
}

pub fn compact(registry: &Registry) -> String {
    let commands: Vec<String> = registry.natives().map(|b| command_line(&b.spec)).collect();
    let functions: Vec<&str> = FUNCTIONS.iter().flat_map(|(sig, _)| function_names(sig)).collect();
    format!(
        "agentsh: a shell language for agents.\n\n{SYNTAX}\n\nCOMMANDS\n{}\n\nFUNCTIONS {}()\n\n{RESULTS}\n\n{PACKAGES}\n\n{EXAMPLES}\n",
        commands.join("\n"),
        functions.join("() ")
    )
}

fn function_names(signatures: &str) -> Vec<&str> {
    signatures
        .split(')')
        .filter_map(|part| part.split('(').next())
        .map(|name| name.trim_start_matches([',', ' ']).trim())
        .filter(|name| !name.is_empty())
        .collect()
}

fn command_line(spec: &CommandSpec) -> String {
    let brief = spec.summary.split([';', '(']).next().unwrap_or(spec.summary).trim_end().trim_end_matches(':');
    format!("  {} - {brief}", spec.brief_usage())
}

pub fn command_text(spec: &CommandSpec) -> String {
    let params: Vec<String> = spec
        .params
        .iter()
        .map(|p| {
            let kind = if p.positional { "positional" } else { "named" };
            let required = if p.required { ", required" } else { "" };
            format!("  {} ({}, {kind}{required}): {}", p.name, p.ty.describe(), p.doc)
        })
        .collect();
    let piped = spec.input.map(|i| format!("\npiped input goes to: {i}")).unwrap_or_default();
    let caps: Vec<&str> = spec.caps.iter().map(|c| c.as_str()).collect();
    format!(
        "{}\n{}\n\nparameters:\n{}{piped}\nreturns: {}\ncapabilities: {}\n\nexamples:\n  {}\n",
        spec.usage(),
        spec.summary,
        params.join("\n"),
        spec.returns,
        if caps.is_empty() { "none".into() } else { caps.join(", ") },
        spec.examples.join("\n  ")
    )
}
