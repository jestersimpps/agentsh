use crate::exec::{Captured, RunSpec, exit_failure, run_captured, split_single_command};
use agentsh_core::{Args, Builtin, CommandSpec, Ctx, ParamSpec as P, ParamType as T, Result, ShellError, find_on_path};
use agentsh_platform::Capability::Exec;
use agentsh_spec::{ErrorCode, Value};
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

const DEFAULT_TIMEOUT_SECS: f64 = 600.0;
const MAX_FAILURES: usize = 15;
const MAX_MESSAGE: usize = 300;

pub const CHECK: Builtin = Builtin {
    spec: CommandSpec {
        name: "check",
        summary: "run the project's test, build or lint command and summarize the result",
        params: &[
            P::pos("task", T::Str, "\"test\" (default), \"build\" or \"lint\""),
            P::named("cmd", T::Str, "command to run instead of the detected one"),
            P::named("args", T::Arg, "extra arguments, e.g. a test name filter").variadic(),
            P::named("timeout", T::Duration, "default 10m"),
            P::named("keep", T::Bool, "on failure keep the run's earlier file changes"),
        ],
        input: None,
        returns: "{command, passed, failed, ...}; failures make the run fail with the failing tests and messages",
        caps: &[Exec],
        examples: &[
            "check",
            "check \"lint\"",
            "check args:\"parser\"",
            "edit \"src/a.rs\" old:\"x\" new:\"y\"; check keep:true",
        ],
    },
    run: check,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Runner {
    Cargo,
    Pytest,
    Js,
    Go,
    Swift,
    Generic,
}

#[derive(Default)]
struct Summary {
    passed: Option<u64>,
    failed: Option<u64>,
    failures: Vec<(String, String)>,
}

fn check(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let task = args.opt_str("task")?.unwrap_or_else(|| "test".into());
    if !matches!(task.as_str(), "test" | "build" | "lint") {
        return Err(ShellError::new(ErrorCode::InvalidValue, format!("unknown task `{task}`"))
            .with_arg("task")
            .with_hint("use \"test\", \"build\" or \"lint\""));
    }
    let (mut argv, runner) = match args.opt_str("cmd")? {
        Some(cmd) => {
            let argv = split_single_command(vec![cmd])?;
            let runner = guess_runner(&argv);
            (argv, runner)
        }
        None => detect(&ctx.root, &task).ok_or_else(|| {
            ShellError::new(ErrorCode::NotFound, format!("can't tell how to {task} this project"))
                .with_hint(format!("pass the command: check \"{task}\" cmd:\"...\""))
        })?,
    };
    argv.extend(args.strings("args")?);
    let command = argv.join(" ");
    let timeout = args.opt_secs("timeout")?.unwrap_or(DEFAULT_TIMEOUT_SECS);
    let keep = args.flag("keep")?;
    let cwd = ctx.root.clone();
    let captured = run_captured(ctx, RunSpec { argv: &argv, cwd, stdin: None, env: Vec::new(), timeout })?;
    let summary = summarize(runner, &captured);
    let exit_ok = exit_failure(ctx, &argv[0], &captured, &[0], timeout);
    if exit_ok.is_ok() && summary.failed.unwrap_or(0) == 0 {
        ctx.exec_completed = true;
        let mut record = vec![("command", Value::Str(command))];
        if let Some(p) = summary.passed {
            record.push(("passed", Value::Int(p as i64)));
        }
        record.push(("ms", Value::Int(captured.out.elapsed.as_millis() as i64)));
        return Ok(Value::record(record));
    }
    if keep {
        ctx.keep_on_failure = true;
    }
    Err(failure_error(&command, &summary, exit_ok.err()))
}

fn failure_error(command: &str, summary: &Summary, exit_error: Option<ShellError>) -> ShellError {
    if summary.failures.is_empty() {
        let mut err = exit_error.unwrap_or_else(|| ShellError::new(ErrorCode::ExecFailed, "the check failed"));
        err.message = format!("`{command}` failed");
        err.hint = Some("value = key output lines; check keep:true keeps this run's changes for a small fix".into());
        return err;
    }
    let counts = match (summary.failed, summary.passed) {
        (Some(f), Some(p)) => format!("{f} failed, {p} passed"),
        (Some(f), None) => format!("{f} failed"),
        _ => format!("{} failed", summary.failures.len()),
    };
    let listed: Vec<String> = summary
        .failures
        .iter()
        .take(MAX_FAILURES)
        .map(|(name, message)| if message.is_empty() { name.clone() } else { format!("{name}: {message}") })
        .collect();
    let more = summary.failures.len().saturating_sub(MAX_FAILURES);
    let mut value = listed.join("\n");
    if more > 0 {
        value.push_str(&format!("\n… {more} more"));
    }
    ShellError::new(ErrorCode::ExecFailed, format!("`{command}`: {counts}"))
        .with_value(value)
        .with_hint("check keep:true keeps this run's changes for a small fix")
}

fn detect(root: &Path, task: &str) -> Option<(Vec<String>, Runner)> {
    let argv = |parts: &[&str]| parts.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    if root.join("Cargo.toml").is_file() {
        return Some(match task {
            "test" => (argv(&["cargo", "test", "-q"]), Runner::Cargo),
            "build" => (argv(&["cargo", "build", "-q"]), Runner::Generic),
            _ => (argv(&["cargo", "clippy", "-q", "--all-targets"]), Runner::Generic),
        });
    }
    if let Some(found) = detect_node(root, task) {
        return Some(found);
    }
    if root.join("go.mod").is_file() {
        return Some(match task {
            "test" => (argv(&["go", "test", "./..."]), Runner::Go),
            "build" => (argv(&["go", "build", "./..."]), Runner::Generic),
            _ => (argv(&["go", "vet", "./..."]), Runner::Generic),
        });
    }
    if root.join("Package.swift").is_file() {
        return match task {
            "test" => Some((argv(&["swift", "test"]), Runner::Swift)),
            "build" => Some((argv(&["swift", "build"]), Runner::Generic)),
            _ => None,
        };
    }
    let python = ["pyproject.toml", "setup.py", "setup.cfg", "requirements.txt", "pytest.ini"]
        .iter()
        .any(|f| root.join(f).is_file());
    if python {
        return match task {
            "test" => Some((argv(&["python3", "-m", "pytest", "-q"]), Runner::Pytest)),
            "lint" if find_on_path("ruff").is_some() => Some((argv(&["ruff", "check", "."]), Runner::Generic)),
            _ => None,
        };
    }
    let makefile = std::fs::read_to_string(root.join("Makefile")).ok()?;
    makefile.lines().any(|l| l.starts_with(&format!("{task}:"))).then(|| (argv(&["make", task]), Runner::Generic))
}

fn detect_node(root: &Path, task: &str) -> Option<(Vec<String>, Runner)> {
    let text = std::fs::read_to_string(root.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let scripts = json.get("scripts")?.as_object()?;
    let script = match task {
        "lint" => ["lint", "typecheck", "check"].into_iter().find(|s| scripts.contains_key(*s))?,
        other => scripts.contains_key(other).then_some(other)?,
    };
    let manager = [("pnpm-lock.yaml", "pnpm"), ("yarn.lock", "yarn"), ("bun.lockb", "bun"), ("bun.lock", "bun")]
        .iter()
        .find(|(lock, _)| root.join(lock).is_file())
        .map(|(_, m)| *m)
        .unwrap_or("npm");
    let runner = if task == "test" { Runner::Js } else { Runner::Generic };
    Some((vec![manager.to_string(), "run".to_string(), script.to_string()], runner))
}

fn guess_runner(argv: &[String]) -> Runner {
    let joined = argv.join(" ");
    if joined.starts_with("cargo test") || joined.starts_with("cargo nextest") {
        Runner::Cargo
    } else if joined.contains("pytest") {
        Runner::Pytest
    } else if joined.starts_with("go test") {
        Runner::Go
    } else if joined.starts_with("swift test") || joined.contains("xcodebuild") {
        Runner::Swift
    } else if ["npm", "pnpm", "yarn", "bun", "npx", "jest", "vitest", "mocha"].iter().any(|p| argv[0] == *p) {
        Runner::Js
    } else {
        Runner::Generic
    }
}

fn summarize(runner: Runner, captured: &Captured) -> Summary {
    let text = format!("{}\n{}", captured.stdout, captured.stderr);
    match runner {
        Runner::Cargo => cargo_summary(&text),
        Runner::Pytest => pytest_summary(&text),
        Runner::Js => js_summary(&text),
        Runner::Go => go_summary(&text),
        Runner::Swift => swift_summary(&text),
        Runner::Generic => Summary::default(),
    }
}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("valid built-in regex")
}

fn sum_captures(re: &Regex, text: &str, group: usize) -> Option<u64> {
    let mut found = false;
    let total = re
        .captures_iter(text)
        .filter_map(|c| c.get(group).and_then(|m| m.as_str().parse::<u64>().ok()))
        .inspect(|_| found = true)
        .sum();
    found.then_some(total)
}

fn clip(message: &str) -> String {
    let flat = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX_MESSAGE {
        return flat;
    }
    format!("{}…", flat.chars().take(MAX_MESSAGE).collect::<String>())
}

fn cargo_summary(text: &str) -> Summary {
    static RESULT: LazyLock<Regex> = LazyLock::new(|| regex(r"test result: \w+\. (\d+) passed; (\d+) failed"));
    static HEADER: LazyLock<Regex> = LazyLock::new(|| regex(r"^---- (\S+) stdout ----$"));
    let lines: Vec<&str> = text.lines().collect();
    let mut failures = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let Some(c) = HEADER.captures(line) else { continue };
        let body: Vec<&str> = lines[i + 1..]
            .iter()
            .skip_while(|l| l.trim().is_empty())
            .take_while(|l| !l.starts_with("---- ") && !l.trim().is_empty() && **l != "failures:")
            .filter(|l| !l.starts_with("note: run with"))
            .copied()
            .collect();
        failures.push((c[1].to_string(), clip(&body.join(" "))));
    }
    Summary { passed: sum_captures(&RESULT, text, 1), failed: sum_captures(&RESULT, text, 2), failures }
}

fn pytest_summary(text: &str) -> Summary {
    static PASSED: LazyLock<Regex> = LazyLock::new(|| regex(r"(\d+) passed"));
    static FAILED: LazyLock<Regex> = LazyLock::new(|| regex(r"(\d+) (?:failed|error)"));
    static FAILURE: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)^(?:FAILED|ERROR) (\S+)(?: - (.*))?$"));
    let last = text
        .lines()
        .rev()
        .find(|l| l.contains(" in ") && (l.contains("passed") || l.contains("failed") || l.contains("error")))
        .unwrap_or("");
    let failures =
        FAILURE.captures_iter(text).map(|c| (c[1].to_string(), clip(c.get(2).map_or("", |m| m.as_str())))).collect();
    Summary { passed: sum_captures(&PASSED, last, 1), failed: sum_captures(&FAILED, last, 1), failures }
}

fn js_summary(text: &str) -> Summary {
    static JEST: LazyLock<Regex> =
        LazyLock::new(|| regex(r"Tests:\s+(?:(\d+) failed, )?(?:\d+ \w+, )*?(?:(\d+) passed, )?\d+ total"));
    static VITEST: LazyLock<Regex> = LazyLock::new(|| regex(r"Tests\s+(?:(\d+) failed)?(?: \| )?(?:(\d+) passed)?"));
    static MOCHA_PASS: LazyLock<Regex> = LazyLock::new(|| regex(r"(\d+) passing"));
    static MOCHA_FAIL: LazyLock<Regex> = LazyLock::new(|| regex(r"(\d+) failing"));
    static BULLET: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)^\s*(?:●|×|✕|FAIL\s+)\s*(.+?)\s*$"));
    let failures: Vec<(String, String)> = BULLET
        .captures_iter(text)
        .map(|c| (clip(&c[1]), String::new()))
        .filter(|(name, _)| !name.starts_with("Console"))
        .collect();
    let pick =
        |re: &Regex, g: usize| re.captures(text).and_then(|c| c.get(g)).and_then(|m| m.as_str().parse::<u64>().ok());
    let (failed, passed) = if JEST.is_match(text) {
        (pick(&JEST, 1).or(Some(0)), pick(&JEST, 2))
    } else if MOCHA_PASS.is_match(text) {
        (pick(&MOCHA_FAIL, 1).or(Some(0)), pick(&MOCHA_PASS, 1))
    } else if VITEST.is_match(text) {
        (pick(&VITEST, 1).or(Some(0)), pick(&VITEST, 2))
    } else {
        (None, None)
    };
    Summary { passed, failed, failures }
}

fn go_summary(text: &str) -> Summary {
    static FAIL: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)^\s*--- FAIL: (\S+)"));
    static OK: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)^ok\s"));
    let lines: Vec<&str> = text.lines().collect();
    let failures: Vec<(String, String)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| FAIL.captures(l).map(|c| (i, c[1].to_string())))
        .map(|(i, name)| {
            let detail: Vec<&str> =
                lines[i + 1..].iter().take_while(|l| l.starts_with("    ")).take(3).map(|l| l.trim()).collect();
            (name, clip(&detail.join(" ")))
        })
        .collect();
    let failed = failures.len() as u64;
    Summary { passed: Some(OK.find_iter(text).count() as u64).filter(|_| failed == 0), failed: Some(failed), failures }
}

fn swift_summary(text: &str) -> Summary {
    static EXECUTED: LazyLock<Regex> = LazyLock::new(|| regex(r"Executed (\d+) tests?, with (\d+) failures?"));
    static XCTEST: LazyLock<Regex> = LazyLock::new(|| regex(r"(?m)error: -\[(\S+ \S+)\] : (.*)$"));
    static SWIFT_TESTING: LazyLock<Regex> = LazyLock::new(|| regex(r#"(?m)✘ Test (\S+) recorded an issue.*?: (.*)$"#));
    let mut failures: Vec<(String, String)> =
        XCTEST.captures_iter(text).map(|c| (c[1].to_string(), clip(&c[2]))).collect();
    failures.extend(SWIFT_TESTING.captures_iter(text).map(|c| (c[1].to_string(), clip(&c[2]))));
    let last = EXECUTED.captures_iter(text).last();
    let total = last.as_ref().and_then(|c| c[1].parse::<u64>().ok());
    let failed = last.as_ref().and_then(|c| c[2].parse::<u64>().ok());
    Summary {
        passed: total.zip(failed).map(|(t, f)| t.saturating_sub(f)),
        failed: failed.or(Some(failures.len() as u64)),
        failures,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cargo_output() {
        let out = "running 3 tests\ntest a ... ok\ntest b ... FAILED\n\nfailures:\n\n---- tests::b stdout ----\n\nthread 'tests::b' panicked at src/lib.rs:9:5:\nassertion `left == right` failed\n  left: 1\n right: 2\nnote: run with `RUST_BACKTRACE=1`\n\nfailures:\n    tests::b\n\ntest result: FAILED. 2 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out\n";
        let s = cargo_summary(out);
        assert_eq!((s.passed, s.failed), (Some(2), Some(1)));
        assert_eq!(s.failures[0].0, "tests::b");
        assert!(s.failures[0].1.contains("src/lib.rs:9:5"));
        assert!(s.failures[0].1.contains("right: 2"));
    }

    #[test]
    fn pytest_output() {
        let out = "..F.\n=========================== short test summary info ============================\nFAILED tests/test_x.py::test_add - assert 3 == 4\n1 failed, 3 passed in 0.05s\n";
        let s = pytest_summary(out);
        assert_eq!((s.passed, s.failed), (Some(3), Some(1)));
        assert_eq!(s.failures, vec![("tests/test_x.py::test_add".into(), "assert 3 == 4".into())]);
    }

    #[test]
    fn jest_and_go_output() {
        let jest =
            "  ● math › adds\n\n    expect(received).toBe(expected)\n\nTests:       1 failed, 4 passed, 5 total\n";
        let s = js_summary(jest);
        assert_eq!((s.failed, s.passed), (Some(1), Some(4)));
        assert_eq!(s.failures[0].0, "math › adds");
        let go = "--- FAIL: TestAdd (0.00s)\n    add_test.go:8: got 3, want 4\nFAIL\nFAIL\texample.com/m\t0.1s\n";
        let s = go_summary(go);
        assert_eq!(s.failed, Some(1));
        assert_eq!(s.failures[0], ("TestAdd".into(), "add_test.go:8: got 3, want 4".into()));
    }
}
