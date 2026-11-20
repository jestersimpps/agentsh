use crate::util::{failure_digest, strip_ansi, tail_lines};
use agentsh_core::{Args, Builtin, CommandSpec, Ctx, ParamSpec as P, ParamType as T, Result, ShellError, find_on_path};
use agentsh_platform::Capability::Exec;
use agentsh_platform::{ExecOutput, ExecRequest, run_exec};
use agentsh_spec::{ErrorCode, Value};
use std::path::PathBuf;
use std::time::Duration;

const ERROR_TAIL_LINES: usize = 15;
const ERROR_TAIL_BYTES: usize = 1_500;

pub const EXEC: Builtin = Builtin {
    spec: CommandSpec {
        name: "exec",
        summary: "run a program non-interactively (no shell expansion, no pager, no prompts, timeout, sandboxed)",
        params: &[
            P::pos(
                "argv",
                T::Arg,
                "\"cmd arg 'two words'\" split like a shell (no expansion), or each argument separately",
            )
            .required()
            .variadic(),
            P::named("input", T::Any, "stdin: text, or any value as JSON"),
            P::named("cwd", T::Path, "folder to run in; default: current folder"),
            P::named("timeout", T::Duration, "kill after this long (default from config, 60s)"),
            P::named("env", T::Record, "extra environment variables, e.g. {RUST_LOG: \"debug\"}"),
            P::named("ok", T::List, "exit codes that count as success (default [0])"),
            P::named(
                "keep",
                T::Bool,
                "if this command fails, keep the run's earlier file changes instead of rolling them back",
            ),
            P::named(
                "parse",
                T::Str,
                "\"text\", \"lines\" or \"json\": return stdout (parsed) instead of the full record",
            ),
        ],
        input: Some("input"),
        returns: "{exit, stdout, stderr}, or stdout (parsed) with parse:",
        caps: &[Exec],
        examples: &[
            "exec \"git status --porcelain\" parse:\"lines\"",
            "exec \"cargo\" \"test\" timeout:5m parse:\"text\" | grep \"FAILED|panicked\"",
            "exec \"gh\" \"pr\" \"list\" \"--json\" \"number,title\" parse:\"json\"",
        ],
    },
    run: exec,
};

pub struct Captured {
    pub out: ExecOutput,
    pub stdout: String,
    pub stderr: String,
    pub network: bool,
}

pub struct RunSpec<'a> {
    pub argv: &'a [String],
    pub cwd: PathBuf,
    pub stdin: Option<Vec<u8>>,
    pub env: Vec<(String, String)>,
    pub timeout: f64,
}

pub fn run_captured(ctx: &mut Ctx, spec: RunSpec) -> Result<Captured> {
    let Some((program, rest)) = spec.argv.split_first() else {
        return Err(ShellError::new(ErrorCode::MissingArgument, "no program to run").with_arg("argv"));
    };
    let program_path = locate(ctx, program)?;
    let policy = ctx.sandbox_policy();
    ctx.exec_used = true;
    let program_str = program_path.to_string_lossy().into_owned();
    let out = run_exec(
        ctx.sandbox,
        ExecRequest {
            program: &program_str,
            args: rest,
            cwd: &spec.cwd,
            stdin: spec.stdin,
            env: &spec.env,
            timeout: Duration::from_secs_f64(spec.timeout.max(0.01)),
            capture_limit: ctx.config.limits.exec_capture_bytes,
            policy: &policy,
        },
    )
    .map_err(|e| launch_error(ctx, program, &e))?;
    let stdout = strip_ansi(&String::from_utf8_lossy(&out.stdout));
    let stderr = strip_ansi(&String::from_utf8_lossy(&out.stderr));
    if out.stdout_total > out.stdout.len() as u64 || out.stderr_total > out.stderr.len() as u64 {
        ctx.warn(format!(
            "output was larger than the capture limit; kept the first {} bytes (stdout {} bytes total)",
            ctx.config.limits.exec_capture_bytes, out.stdout_total
        ));
    }
    Ok(Captured { out, stdout, stderr, network: policy.network })
}

fn launch_error(ctx: &Ctx, program: &str, error: &std::io::Error) -> ShellError {
    let err =
        ShellError::new(ErrorCode::ExecFailed, format!("couldn't start `{program}`: {error}")).with_value(program);
    if error.kind() == std::io::ErrorKind::PermissionDenied && ctx.sandbox.enforced() {
        return err.with_hint("the sandbox blocks this program (setuid tools like ps or sudo can't run sandboxed); a human can allow it with --sandbox none");
    }
    err
}

pub fn exit_failure(ctx: &Ctx, program: &str, captured: &Captured, ok_codes: &[i64], timeout: f64) -> Result<()> {
    check_outcome(Outcome {
        program,
        out: &captured.out,
        ok_codes,
        timeout,
        stdout: &captured.stdout,
        stderr: &captured.stderr,
        sandboxed: ctx.sandbox.enforced(),
        network: captured.network,
    })
}

fn exec(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let argv = split_single_command(args.strings("argv")?)?;
    let Some(program) = argv.first().cloned() else {
        return Err(ShellError::new(ErrorCode::MissingArgument, "exec needs a program").with_arg("argv"));
    };
    let cwd = match args.opt_str("cwd")? {
        Some(raw) => ctx.existing(&raw)?,
        None => ctx.cwd(),
    };
    let stdin = match args.take("input") {
        None | Some(Value::Null) => None,
        Some(Value::Str(s)) => Some(s.into_bytes()),
        Some(other) => Some(serde_json::to_vec(&other).unwrap_or_default()),
    };
    let env = env_pairs(args)?;
    let ok_codes = ok_codes(args)?;
    let parse = args.opt_str("parse")?;
    if let Some(p) = parse.as_deref().filter(|p| !matches!(*p, "json" | "lines" | "text")) {
        return Err(ShellError::new(
            ErrorCode::InvalidValue,
            format!("parse must be \"text\", \"lines\" or \"json\", got \"{p}\""),
        )
        .with_arg("parse"));
    }
    let keep = args.flag("keep")?;
    let timeout = args.opt_secs("timeout")?.unwrap_or(ctx.config.limits.exec_timeout_secs as f64);
    let captured = run_captured(ctx, RunSpec { argv: &argv, cwd, stdin, env, timeout })?;
    let outcome = exit_failure(ctx, &program, &captured, &ok_codes, timeout);
    let Captured { out, stdout, stderr, .. } = captured;
    if outcome.is_err() && keep {
        ctx.keep_on_failure = true;
    }
    outcome?;
    ctx.exec_completed = true;
    match parse.as_deref() {
        Some("json") => {
            warn_stderr(ctx, &stderr);
            serde_json::from_str::<serde_json::Value>(&stdout).map(Value::from).map_err(|e| {
                ShellError::new(ErrorCode::InvalidValue, format!("`{program}` didn't print valid JSON: {e}"))
                    .with_value(tail_lines(&stdout, 5, 500))
            })
        }
        Some("text") => {
            warn_stderr(ctx, &stderr);
            Ok(Value::Str(stdout))
        }
        Some(_) => {
            warn_stderr(ctx, &stderr);
            Ok(Value::List(stdout.lines().filter(|l| !l.trim().is_empty()).map(Value::str).collect()))
        }
        None => Ok(Value::record([
            ("exit", Value::Int(out.exit_code.unwrap_or(-1) as i64)),
            ("stdout", Value::Str(stdout)),
            ("stderr", Value::Str(stderr)),
        ])),
    }
}

pub fn split_single_command(argv: Vec<String>) -> Result<Vec<String>> {
    match argv.as_slice() {
        [single] if single.contains(char::is_whitespace) && find_on_path(single).is_none() => shell_words(single),
        _ => Ok(argv),
    }
}

fn shell_words(line: &str) -> Result<Vec<String>> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => current.push(ch),
                        None => return Err(unbalanced(line)),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => current.push(chars.next().ok_or_else(|| unbalanced(line))?),
                        Some(ch) => current.push(ch),
                        None => return Err(unbalanced(line)),
                    }
                }
            }
            '\\' => {
                in_word = true;
                current.push(chars.next().unwrap_or('\\'));
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                current.push(c);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

fn unbalanced(line: &str) -> ShellError {
    ShellError::new(ErrorCode::InvalidValue, "unbalanced quote in the command").with_value(line).with_hint(
        "the command string is split like a shell would (quotes respected, nothing expanded); or pass each argument separately",
    )
}

pub fn locate(ctx: &Ctx, program: &str) -> Result<PathBuf> {
    if program.contains('/') {
        let path = ctx.resolve(program).or_else(|_| Ok::<_, ShellError>(PathBuf::from(program)))?;
        if path.is_file() {
            return Ok(path);
        }
    } else if let Some(found) = find_on_path(program) {
        return Ok(found);
    }
    let err = ShellError::new(ErrorCode::ExecFailed, format!("program `{program}` not found")).with_value(program);
    if program.contains(' ') {
        let parts: Vec<String> = program.split_whitespace().map(|p| format!("\"{p}\"")).collect();
        return Err(err.with_hint(format!("pass each argument separately: exec {}", parts.join(" "))));
    }
    Err(err.with_hint("check the name; `env` lists the common tools available on this machine"))
}

pub fn env_pairs(args: &Args) -> Result<Vec<(String, String)>> {
    match args.value("env") {
        Some(Value::Record(r)) => Ok(r.iter().map(|(k, v)| (k.clone(), v.to_display_string())).collect()),
        _ => Ok(Vec::new()),
    }
}

fn ok_codes(args: &Args) -> Result<Vec<i64>> {
    match args.value("ok") {
        None => Ok(vec![0]),
        Some(Value::List(items)) => items
            .iter()
            .map(|v| match v {
                Value::Int(i) => Ok(*i),
                other => Err(ShellError::type_error("ok", "a list of exit codes", other).with_arg("ok")),
            })
            .collect(),
        Some(other) => Err(ShellError::type_error("ok", "a list of exit codes", other).with_arg("ok")),
    }
}

pub struct Outcome<'a> {
    pub program: &'a str,
    pub out: &'a ExecOutput,
    pub ok_codes: &'a [i64],
    pub timeout: f64,
    pub stdout: &'a str,
    pub stderr: &'a str,
    pub sandboxed: bool,
    pub network: bool,
}

fn check_outcome(o: Outcome) -> Result<()> {
    let combined = format!("{}\n{}", o.stdout, o.stderr);
    let detail = combined.as_str();
    let tail = failure_digest(detail, ERROR_TAIL_LINES, ERROR_TAIL_BYTES);
    let program = o.program;
    let err = if o.out.timed_out {
        ShellError::new(ErrorCode::Timeout, format!("`{program}` was stopped after {}s", o.timeout)).with_hint(
            "raise it with timeout:5m if the work is expected to take longer; programs can't wait for input here",
        )
    } else if let Some(signal) = o.out.signal {
        ShellError::new(ErrorCode::ExecFailed, format!("`{program}` was killed by signal {signal}"))
    } else {
        let code = o.out.exit_code.unwrap_or(-1) as i64;
        if o.ok_codes.contains(&code) {
            return Ok(());
        }
        ShellError::new(ErrorCode::ExecFailed, format!("`{program}` exited with code {code}"))
            .with_hint(format!("value = key output lines; ok:[0, {code}] accepts this exit code"))
    };
    let err = if tail.trim().is_empty() { err } else { err.with_value(tail) };
    Err(err.with_optional_hint(sandbox_hint(detail, o.sandboxed, o.network)))
}

const NETWORK_ERRORS: &[&str] = &[
    "could not resolve host",
    "couldn't connect",
    "failed to connect",
    "network is unreachable",
    "name or service not known",
    "nodename nor servname",
    "temporary failure in name resolution",
    "getaddrinfo",
    "enotfound",
    "network access",
];

const WRITE_ERRORS: &[&str] = &["operation not permitted", "read-only file system"];

fn sandbox_hint(output: &str, sandboxed: bool, network: bool) -> Option<String> {
    if !sandboxed {
        return None;
    }
    let lower = output.to_lowercase();
    if !network && NETWORK_ERRORS.iter().any(|p| lower.contains(p)) {
        return Some("network access is blocked by the sandbox; a human can grant it with --allow net".into());
    }
    if WRITE_ERRORS.iter().any(|p| lower.contains(p)) {
        return Some(
            "the sandbox blocks writes outside the workspace and to .agentsh; keep changes in the workspace, or a human can add [sandbox] writable paths".into(),
        );
    }
    None
}

fn warn_stderr(ctx: &mut Ctx, stderr: &str) {
    if !stderr.trim().is_empty() {
        ctx.warn(format!("stderr: {}", tail_lines(stderr, 5, 500)));
    }
}
