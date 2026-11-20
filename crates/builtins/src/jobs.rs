use crate::exec::{env_pairs, locate, split_single_command};
use crate::util::{strip_ansi, tail_lines};
use agentsh_core::{Args, Builtin, CommandSpec, Ctx, ParamSpec as P, ParamType as T, Result, ShellError, closest};
use agentsh_platform::Capability::Exec;
use agentsh_platform::{SpawnRequest, process_alive, spawn_detached, stop_group};
use agentsh_spec::{ErrorCode, Value};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const JOBS_FILE: &str = "jobs.json";
const LOGS_DIR: &str = "jobs";
const DEFAULT_WAIT_SECS: f64 = 30.0;
const READY_POLL: Duration = Duration::from_millis(100);
const STOP_GRACE: Duration = Duration::from_secs(3);
const START_TAIL_LINES: usize = 10;
const START_TAIL_BYTES: usize = 1_500;
const DEFAULT_LOG_LINES: usize = 40;

pub const START: Builtin = Builtin {
    spec: CommandSpec {
        name: "start",
        summary: "run a program in the background (detached, sandboxed, output to a log); until: waits for a line that says it is ready",
        params: &[
            P::pos(
                "argv",
                T::Arg,
                "\"cmd arg 'two words'\" split like a shell (no expansion), or each argument separately",
            )
            .required()
            .variadic(),
            P::named("name", T::Str, "job name for jobs/logs/stop (default: the program name, made unique)"),
            P::named("until", T::Str, "regex: wait until the output matches it, the program exits, or timeout: passes"),
            P::named("timeout", T::Duration, "how long until: waits (default 30s); the job keeps running either way"),
            P::named("cwd", T::Path, "folder to run in; default: current folder"),
            P::named("env", T::Record, "extra environment variables, e.g. {PORT: \"3000\"}"),
        ],
        input: None,
        returns: "{job, pid, ready, tail}",
        caps: &[Exec],
        examples: &[
            "start \"npm run dev\" name:\"web\" until:\"ready|listening\"",
            "start \"python3 -m http.server 8000\" until:\"Serving\" timeout:10s",
        ],
    },
    run: start,
};

pub const JOBS: Builtin = Builtin {
    spec: CommandSpec {
        name: "jobs",
        summary: "list background jobs started with start in this session",
        params: &[],
        input: None,
        returns: "table of {job, command, pid, running, uptime}",
        caps: &[Exec],
        examples: &["jobs", "jobs | where .running"],
    },
    run: jobs,
};

pub const LOGS: Builtin = Builtin {
    spec: CommandSpec {
        name: "logs",
        summary: "recent output (stdout and stderr) of a background job",
        params: &[
            P::pos("job", T::Str, "job name, as shown by jobs").required(),
            P::named("tail", T::Int, "how many lines (default 40)"),
            P::named("grep", T::Str, "regex: only lines that match"),
        ],
        input: None,
        returns: "text: the last lines of the job's output",
        caps: &[Exec],
        examples: &["logs \"web\"", "logs \"web\" tail:200 grep:\"(?i)error\""],
    },
    run: logs,
};

pub const STOP: Builtin = Builtin {
    spec: CommandSpec {
        name: "stop",
        summary: "stop a background job and its children (no name: every job); SIGTERM, then SIGKILL after 3s",
        params: &[P::pos("job", T::Str, "job name; omit to stop all jobs")],
        input: None,
        returns: "table of {job, pid, stopped}",
        caps: &[Exec],
        examples: &["stop \"web\"", "stop"],
    },
    run: stop,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Job {
    name: String,
    pid: u32,
    command: String,
    log: PathBuf,
    started: u64,
}

enum Readiness {
    Running,
    Matched,
    Exited(Option<i32>),
    TimedOut,
}

fn start(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let argv = split_single_command(args.strings("argv")?)?;
    let until = args.opt_str("until")?.map(|pattern| ctx.regex(&pattern)).transpose()?;
    let wait = args.opt_secs("timeout")?.unwrap_or(DEFAULT_WAIT_SECS);
    let mut jobs = load_jobs(ctx)?;
    let name = job_name(args.opt_str("name")?, &argv, &jobs)?;
    let log = ctx.handles_dir.with_file_name(LOGS_DIR).join(format!("{name}.log"));
    let child = launch(ctx, args, &argv, &log)?;
    let job = Job { name, pid: child.id(), command: argv.join(" "), log, started: unix_now() };
    ctx.started_jobs.push((job.name.clone(), job.pid));
    jobs.push(job.clone());
    save_jobs(ctx, &jobs)?;
    let readiness = wait_ready(child, &job.log, until.as_ref(), wait);
    warn_not_ready(ctx, &job, &readiness, wait);
    Ok(Value::record([
        ("job", Value::str(&job.name)),
        ("pid", Value::Int(i64::from(job.pid))),
        ("ready", Value::Bool(matches!(readiness, Readiness::Running | Readiness::Matched))),
        ("tail", Value::Str(tail_lines(&strip_ansi(&read_log(&job.log)), START_TAIL_LINES, START_TAIL_BYTES))),
    ]))
}

fn launch(ctx: &mut Ctx, args: &Args, argv: &[String], log: &Path) -> Result<Child> {
    let Some((program, rest)) = argv.split_first() else {
        return Err(ShellError::new(ErrorCode::MissingArgument, "start needs a program").with_arg("argv"));
    };
    let program_path = locate(ctx, program)?.to_string_lossy().into_owned();
    let cwd = match args.opt_str("cwd")? {
        Some(raw) => ctx.existing(&raw)?,
        None => ctx.cwd(),
    };
    let env = env_pairs(args)?;
    let policy = ctx.sandbox_policy();
    ctx.exec_used = true;
    let request = SpawnRequest {
        program: &program_path,
        args: rest,
        cwd: &cwd,
        env: &env,
        log: create_log(log)?,
        policy: &policy,
    };
    spawn_detached(ctx.sandbox, request).map_err(|e| {
        ShellError::new(ErrorCode::ExecFailed, format!("couldn't start `{program}`: {e}")).with_value(program.as_str())
    })
}

fn create_log(path: &Path) -> Result<File> {
    let shown = path.display().to_string();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| ShellError::io(&e, &shown))?;
    }
    File::create(path).map_err(|e| ShellError::io(&e, &shown))
}

fn wait_ready(mut child: Child, log: &Path, until: Option<&Regex>, wait: f64) -> Readiness {
    let Some(pattern) = until else {
        return match child.try_wait() {
            Ok(Some(status)) => Readiness::Exited(status.code()),
            _ => Readiness::Running,
        };
    };
    let deadline = Instant::now() + agentsh_core::wait_duration(wait);
    loop {
        let exited = child.try_wait().ok().flatten();
        if pattern.is_match(&read_log(log)) {
            return Readiness::Matched;
        }
        if let Some(status) = exited {
            return Readiness::Exited(status.code());
        }
        if Instant::now() >= deadline {
            return Readiness::TimedOut;
        }
        thread::sleep(READY_POLL);
    }
}

fn warn_not_ready(ctx: &mut Ctx, job: &Job, readiness: &Readiness, wait: f64) {
    let name = &job.name;
    match readiness {
        Readiness::Exited(code) => {
            let code = code.map(|c| format!(" with code {c}")).unwrap_or_default();
            ctx.warn(format!("`{name}` exited{code} before it was ready; its last output is in tail (logs \"{name}\" for more)"));
        }
        Readiness::TimedOut => ctx.warn(format!(
            "`{name}` printed nothing matching until: within {wait}s but is still running; check logs \"{name}\", or stop \"{name}\""
        )),
        Readiness::Running | Readiness::Matched => {}
    }
}

fn jobs(ctx: &mut Ctx, _args: &mut Args) -> Result<Value> {
    let now = unix_now();
    Ok(Value::List(load_jobs(ctx)?.iter().map(|job| job_row(job, now)).collect()))
}

fn job_row(job: &Job, now: u64) -> Value {
    let running = process_alive(job.pid);
    let uptime = if running { Value::Str(format_uptime(now.saturating_sub(job.started))) } else { Value::Null };
    Value::record([
        ("job", Value::str(&job.name)),
        ("command", Value::str(&job.command)),
        ("pid", Value::Int(i64::from(job.pid))),
        ("running", Value::Bool(running)),
        ("uptime", uptime),
    ])
}

fn format_uptime(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m{}s", secs / 60, secs % 60),
        _ => format!("{}h{}m", secs / 3600, secs % 3600 / 60),
    }
}

fn logs(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let jobs = load_jobs(ctx)?;
    let job = find_job(&jobs, &args.str("job")?)?;
    let count = args.opt_usize("tail")?.unwrap_or(DEFAULT_LOG_LINES);
    let filter = args.opt_str("grep")?.map(|pattern| ctx.regex(&pattern)).transpose()?;
    let text = strip_ansi(&read_log(&job.log));
    let lines: Vec<&str> = text.lines().filter(|line| filter.as_ref().is_none_or(|re| re.is_match(line))).collect();
    Ok(Value::Str(lines[lines.len().saturating_sub(count)..].join("\n")))
}

fn stop(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let mut jobs = load_jobs(ctx)?;
    let targets: Vec<Job> = match args.opt_str("job")? {
        Some(name) => vec![find_job(&jobs, &name)?.clone()],
        None => jobs.clone(),
    };
    let rows = targets.iter().map(stop_job).collect();
    jobs.retain(|job| !targets.iter().any(|t| t.name == job.name));
    save_jobs(ctx, &jobs)?;
    Ok(Value::List(rows))
}

fn stop_job(job: &Job) -> Value {
    let outcome = stop_group(job.pid, STOP_GRACE);
    let _ = std::fs::remove_file(&job.log);
    Value::record([
        ("job", Value::str(&job.name)),
        ("pid", Value::Int(i64::from(job.pid))),
        ("stopped", Value::str(outcome.as_str())),
    ])
}

fn find_job<'a>(jobs: &'a [Job], name: &str) -> Result<&'a Job> {
    jobs.iter().find(|job| job.name == name).ok_or_else(|| unknown_job(jobs, name))
}

fn unknown_job(jobs: &[Job], name: &str) -> ShellError {
    let err = ShellError::new(ErrorCode::NotFound, format!("no job named `{name}`")).with_arg("job").with_value(name);
    let names: Vec<&str> = jobs.iter().map(|job| job.name.as_str()).collect();
    if names.is_empty() {
        return err.with_hint("no jobs in this session; start one with start \"cmd args\"");
    }
    match closest(name, names.iter().copied()) {
        Some(best) => err.with_hint(format!("did you mean \"{best}\"?")),
        None => err.with_hint(format!("jobs in this session: {}", names.join(", "))),
    }
}

fn job_name(requested: Option<String>, argv: &[String], jobs: &[Job]) -> Result<String> {
    let Some(name) = requested else {
        return Ok(unique_name(&default_name(argv), jobs));
    };
    if name.is_empty() || name.starts_with('.') || !name.chars().all(is_name_char) {
        return Err(ShellError::new(ErrorCode::InvalidValue, format!("`{name}` isn't a valid job name"))
            .with_arg("name")
            .with_value(name)
            .with_hint("use letters, digits, - _ and ."));
    }
    if jobs.iter().any(|job| job.name == name) {
        return Err(ShellError::new(ErrorCode::AlreadyExists, format!("a job named `{name}` already exists"))
            .with_arg("name")
            .with_value(name.clone())
            .with_hint(format!("stop \"{name}\" first, or pick another name:")));
    }
    Ok(name)
}

fn default_name(argv: &[String]) -> String {
    let program = argv.first().map(|p| Path::new(p).file_name().unwrap_or_default().to_string_lossy().into_owned());
    let name: String = program.unwrap_or_default().chars().filter(|c| is_name_char(*c)).collect();
    if name.is_empty() || name.starts_with('.') { "job".into() } else { name }
}

fn unique_name(base: &str, jobs: &[Job]) -> String {
    let taken = |candidate: &str| jobs.iter().any(|job| job.name == candidate);
    if !taken(base) {
        return base.to_string();
    }
    (2..).map(|i| format!("{base}-{i}")).find(|candidate| !taken(candidate)).unwrap_or_default()
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')
}

fn load_jobs(ctx: &Ctx) -> Result<Vec<Job>> {
    let path = ctx.handles_dir.with_file_name(JOBS_FILE);
    let shown = path.display().to_string();
    match std::fs::read(&path) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|e| ShellError::internal(format!("{shown} is corrupt: {e}")))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(ShellError::io(&e, &shown)),
    }
}

fn save_jobs(ctx: &Ctx, jobs: &[Job]) -> Result<()> {
    let path = ctx.handles_dir.with_file_name(JOBS_FILE);
    let bytes = serde_json::to_vec_pretty(jobs).map_err(|e| ShellError::internal(e.to_string()))?;
    std::fs::write(&path, bytes).map_err(|e| ShellError::io(&e, &path.display().to_string()))
}

fn read_log(path: &Path) -> String {
    std::fs::read(path).map(|bytes| String::from_utf8_lossy(&bytes).into_owned()).unwrap_or_default()
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}
