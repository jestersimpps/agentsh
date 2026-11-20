mod cli_args;
mod render;
mod trace_report;

use agentsh_core::{Shell, ShellOptions};
use agentsh_platform::{Capability, Overrides};
use agentsh_spec::{Envelope, ErrorCode, ErrorInfo};
use clap::Parser;
use cli_args::{Cli, Command, Format};
use std::io::Read;
use std::process::ExitCode;

fn main() -> ExitCode {
    let cli = Cli::parse();
    match &cli.command {
        Command::Schema => print_raw(&schema()),
        Command::Sandbox { policy, argv } => sandbox_exec(policy, argv),
        Command::Help { topic } => help(topic.as_deref()),
        Command::Trace { file, top, pairs } => trace(&cli, file.clone(), *top, pairs.clone()),
        _ => run(cli),
    }
}

fn run(cli: Cli) -> ExitCode {
    let overrides = match overrides(&cli) {
        Ok(o) => o,
        Err(message) => return emit(&setup_error(&cli.session, message), &cli),
    };
    let start_dir = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let options = ShellOptions { start_dir, root: cli.root.clone(), session: cli.session.clone(), overrides };
    let shell = match Shell::open(options, agentsh_builtins::registry()) {
        Ok(shell) => shell,
        Err(message) => return emit(&setup_error(&cli.session, message), &cli),
    };
    let envelope = match &cli.command {
        Command::Run { program, file } => match program_source(program.as_deref(), file.as_deref()) {
            Ok(source) => shell.run(&source),
            Err(message) => setup_error(&cli.session, message),
        },
        Command::Undo { token, force } => shell.undo(token.as_deref(), *force),
        Command::Changes => shell.changes(),
        Command::Commit => shell.commit(),
        Command::Discard { force } => shell.discard(*force),
        Command::Checkpoint { name } => shell.checkpoint(name),
        Command::Rollback { name, force } => shell.rollback(name, *force),
        Command::Help { .. } | Command::Schema | Command::Sandbox { .. } | Command::Trace { .. } => unreachable!(),
    };
    emit(&envelope, &cli)
}

fn overrides(cli: &Cli) -> Result<Overrides, String> {
    let caps = |list: &[String]| -> Result<Vec<Capability>, String> {
        list.iter().map(|s| Capability::parse_list(s)).collect::<Result<Vec<_>, _>>().map(|v| v.concat())
    };
    Ok(Overrides {
        allow: caps(&cli.allow)?,
        deny: caps(&cli.deny)?,
        sandbox: cli.sandbox.as_deref().map(str::parse).transpose()?,
        trace: cli.trace.clone(),
    })
}

fn program_source(program: Option<&str>, file: Option<&std::path::Path>) -> Result<String, String> {
    match (program, file) {
        (Some(_), Some(_)) => Err("pass a program or --file, not both".into()),
        (Some("-") | None, None) => {
            let mut source = String::new();
            std::io::stdin().read_to_string(&mut source).map_err(|e| format!("can't read stdin: {e}"))?;
            Ok(source)
        }
        (Some(p), None) => Ok(p.to_string()),
        (None, Some(path)) => std::fs::read_to_string(path).map_err(|e| format!("can't read {}: {e}", path.display())),
    }
}

fn help(topic: Option<&str>) -> ExitCode {
    let registry = agentsh_builtins::registry();
    match topic {
        None | Some("compact") => print_raw(&agentsh_builtins::compact(&registry)),
        Some("brief") => print_raw(&agentsh_builtins::brief(&registry)),
        Some(name) => match registry.get(name) {
            Some(builtin) => print_raw(&agentsh_builtins::command_text(&builtin.spec)),
            None => {
                eprintln!("no command `{name}`; run `agentsh help` for the list");
                ExitCode::from(2)
            }
        },
    }
}

fn trace(cli: &Cli, file: Option<std::path::PathBuf>, top: usize, pairs: Option<std::path::PathBuf>) -> ExitCode {
    let path = file.or_else(|| cli.trace.clone()).or_else(|| {
        let start = std::env::current_dir().ok()?;
        let root = agentsh_platform::find_project_root(&start);
        agentsh_platform::Config::load(&root, &Overrides::default()).ok()?.trace_path
    });
    let Some(path) = path else {
        eprintln!("no trace file: pass one, or enable tracing with --trace, AGENTSH_TRACE or [trace] in config");
        return ExitCode::from(2);
    };
    let lines = match trace_report::load(&path) {
        Ok(lines) => lines,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    if let Some(out) = pairs {
        let fixes = trace_report::fix_pairs(&lines);
        if let Err(message) = trace_report::write_pairs(&fixes, &out) {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
        eprintln!("wrote {} failed-then-fixed pairs to {}", fixes.len(), out.display());
    }
    print_raw(&trace_report::report(&lines, top))
}

fn schema() -> String {
    serde_json::to_string_pretty(&schemars::schema_for!(Envelope)).unwrap_or_default()
}

fn setup_error(session: &str, message: String) -> Envelope {
    Envelope {
        ok: false,
        session: session.to_string(),
        error: Some(ErrorInfo {
            code: ErrorCode::InvalidValue,
            message,
            arg: None,
            value: None,
            hint: Some("check the agentsh flags, environment variables and .agentsh.toml".into()),
            command: None,
            span: None,
            rolled_back: false,
            kept: false,
            restored: Vec::new(),
        }),
        ..Envelope::default()
    }
}

fn emit(envelope: &Envelope, cli: &Cli) -> ExitCode {
    let output = match cli.format {
        Format::Text => render::render_text(envelope).trim_end().to_string(),
        Format::Json if cli.pretty => serde_json::to_string_pretty(envelope).unwrap_or_default(),
        Format::Json => serde_json::to_string(envelope).unwrap_or_default(),
    };
    println!("{output}");
    if envelope.ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

fn print_raw(text: &str) -> ExitCode {
    println!("{text}");
    ExitCode::SUCCESS
}

#[cfg(target_os = "linux")]
fn sandbox_exec(policy: &str, argv: &[String]) -> ExitCode {
    use std::os::unix::process::CommandExt;
    let policy: agentsh_platform::SandboxPolicy = match serde_json::from_str(policy) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("agentsh sandbox: bad policy: {e}");
            return ExitCode::from(126);
        }
    };
    if let Err(e) = agentsh_platform::landlock_restrict_self(&policy) {
        eprintln!("agentsh sandbox: landlock failed: {e}");
        return ExitCode::from(126);
    }
    let Some((program, args)) = argv.split_first() else { return ExitCode::from(126) };
    let err = std::process::Command::new(program).args(args).exec();
    eprintln!("agentsh sandbox: can't run {program}: {err}");
    ExitCode::from(127)
}

#[cfg(not(target_os = "linux"))]
fn sandbox_exec(_policy: &str, _argv: &[String]) -> ExitCode {
    eprintln!("agentsh sandbox helper is only used on Linux");
    ExitCode::from(126)
}
