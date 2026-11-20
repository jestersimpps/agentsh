use crate::exec::{Captured, RunSpec, exit_failure, run_captured, split_single_command};
use agentsh_core::package::{CheckTask, TestParser};
use agentsh_core::{
    Args, Builtin, CommandSpec, Ctx, ParamSpec as P, ParamType as T, Registry, Result, ShellError, find_on_path,
};
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
    let (mut argv, parser) = match args.opt_str("cmd")? {
        Some(cmd) => {
            let argv = split_single_command(vec![cmd])?;
            let parser = recognize(ctx.registry, &argv);
            (argv, parser)
        }
        None => detect(ctx, &task).ok_or_else(|| {
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
    let summary = summarize(parser.as_ref(), &captured);
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

/// The first turned-on package whose marker files exist and that can run `task`, in load order;
/// then a Makefile target of the same name.
fn detect(ctx: &Ctx, task: &str) -> Option<(Vec<String>, Option<TestParser>)> {
    for package in ctx.registry.enabled_packages() {
        let Some(check) = &package.check else { continue };
        let Some(spec) = check.tasks.get(task) else { continue };
        if !package.detected(&ctx.root) {
            continue;
        }
        let Some(argv) = task_argv(&ctx.root, spec) else { continue };
        let parser = if task == "test" { check.parser.clone() } else { None };
        return Some((argv, parser));
    }
    let makefile = std::fs::read_to_string(ctx.root.join("Makefile")).ok()?;
    makefile
        .lines()
        .any(|l| l.starts_with(&format!("{task}:")))
        .then(|| (vec!["make".to_string(), task.to_string()], None))
}

fn task_argv(root: &Path, spec: &CheckTask) -> Option<Vec<String>> {
    if let Some(tool) = &spec.requires
        && find_on_path(tool).is_none()
    {
        return None;
    }
    if !spec.cmd.is_empty() {
        return Some(spec.cmd.clone());
    }
    node_script(root, &spec.package_script)
}

/// `<manager> run <script>` for the first of `scripts` defined in package.json; the manager comes
/// from the lockfile (pnpm, yarn, bun, else npm).
fn node_script(root: &Path, scripts: &[String]) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(root.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let defined = json.get("scripts")?.as_object()?;
    let script = scripts.iter().find(|s| defined.contains_key(s.as_str()))?;
    let manager = [("pnpm-lock.yaml", "pnpm"), ("yarn.lock", "yarn"), ("bun.lockb", "bun"), ("bun.lock", "bun")]
        .iter()
        .find(|(lock, _)| root.join(lock).is_file())
        .map(|(_, m)| *m)
        .unwrap_or("npm");
    Some(vec![manager.to_string(), "run".to_string(), script.clone()])
}

/// For `check cmd:"..."`: the parser of the first package whose `recognize` patterns match.
fn recognize(registry: &Registry, argv: &[String]) -> Option<TestParser> {
    let joined = argv.join(" ");
    registry
        .enabled_packages()
        .filter_map(|p| p.check.as_ref())
        .find(|c| c.parser.is_some() && c.recognize.iter().any(|pattern| wildcard_match(pattern, &joined)))
        .and_then(|c| c.parser.clone())
}

/// `*` matches any run of characters; everything else is literal; the whole string must match.
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let Some((first, rest)) = parts.split_first() else { return false };
    let Some(mut remaining) = text.strip_prefix(first) else { return false };
    let Some((last, middle)) = rest.split_last() else { return remaining.is_empty() };
    for part in middle {
        match remaining.find(part) {
            Some(i) => remaining = &remaining[i + part.len()..],
            None => return false,
        }
    }
    remaining.len() >= last.len() && remaining.ends_with(last)
}

fn summarize(parser: Option<&TestParser>, captured: &Captured) -> Summary {
    let text = format!("{}\n{}", captured.stdout, captured.stderr);
    match parser {
        None => Summary::default(),
        Some(TestParser::Named(name)) => match name.as_str() {
            "cargo" => cargo_summary(&text),
            "pytest" => pytest_summary(&text),
            "js" => js_summary(&text),
            "go" => go_summary(&text),
            "swift" => swift_summary(&text),
            _ => Summary::default(),
        },
        Some(TestParser::Patterns { passed, failed, failure }) => {
            pattern_summary(passed.as_deref(), failed.as_deref(), failure.as_deref(), &text)
        }
    }
}

/// A package's `[check.patterns]`: `passed`/`failed` capture a count in group 1 (summed over all
/// matches); `failure` captures a test name in group 1 and an optional message in group 2.
fn pattern_summary(passed: Option<&str>, failed: Option<&str>, failure: Option<&str>, text: &str) -> Summary {
    let compile = |p: Option<&str>| p.and_then(|p| Regex::new(p).ok());
    let count = |re: Option<Regex>| re.and_then(|re| sum_captures(&re, text, 1));
    let failures: Vec<(String, String)> = compile(failure)
        .map(|re| {
            re.captures_iter(text)
                .filter_map(|c| {
                    let name = c.get(1)?.as_str().to_string();
                    Some((name, clip(c.get(2).map_or("", |m| m.as_str()))))
                })
                .collect()
        })
        .unwrap_or_default();
    let failed = count(compile(failed)).or((failure.is_some()).then_some(failures.len() as u64));
    Summary { passed: count(compile(passed)), failed, failures }
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
    Summary { passed: (failed == 0).then(|| OK.find_iter(text).count() as u64), failed: Some(failed), failures }
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
    fn wildcards() {
        assert!(wildcard_match("cargo test*", "cargo test -q"));
        assert!(wildcard_match("*pytest*", "python3 -m pytest -q"));
        assert!(wildcard_match("npm", "npm"));
        assert!(!wildcard_match("npm", "npmx"));
        assert!(wildcard_match("npm *", "npm run test"));
        assert!(!wildcard_match("go test*", "cargo test"));
        assert!(wildcard_match("a*b*c", "a-b-c"));
        assert!(!wildcard_match("a*b*c", "a-c"));
    }

    #[test]
    fn package_patterns() {
        let out = "ok one\nFAIL two: boom\nFAIL three\n2 ok, 2 failed\n";
        let s = pattern_summary(Some(r"(\d+) ok"), None, Some(r"(?m)^FAIL (\w+)(?:: (.*))?$"), out);
        assert_eq!(s.passed, Some(2));
        assert_eq!(s.failed, Some(2));
        assert_eq!(s.failures, vec![("two".into(), "boom".into()), ("three".into(), String::new())]);
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
