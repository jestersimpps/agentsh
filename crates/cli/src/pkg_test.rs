//! `agentsh pkg test <dir>`: validate a package and run its `tests/*.json` cases against this binary,
//! in throwaway workspaces, the same way the repo's conformance suite does.

use agentsh_core::package::{Origin, load_package_dir};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

pub fn run(dir: &Path) -> ExitCode {
    match run_inner(dir) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("agentsh pkg test: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run_inner(dir: &Path) -> Result<bool, String> {
    let dir = dir.canonicalize().map_err(|e| format!("can't open {}: {e}", dir.display()))?;
    let package = load_package_dir(&dir, Origin::Path(dir.clone()))?;
    let n = package.commands.len();
    println!(
        "{} {}: package.toml is valid ({n} command{})",
        package.name,
        package.version,
        if n == 1 { "" } else { "s" }
    );
    let builtin =
        agentsh_builtins::EMBEDDED.iter().any(|e| e.manifest.contains(&format!("name = \"{}\"", package.name)));
    if builtin {
        println!(
            "  `{}` is a built-in package: testing the copy compiled into this binary (rebuild to test edits)",
            package.name
        );
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir.join("tests"))
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "json")).collect())
        .unwrap_or_default();
    files.sort();
    if files.is_empty() {
        println!("  no tests/*.json; add cases so `pkg test` can check your commands");
        return Ok(true);
    }
    let exe = std::env::current_exe().map_err(|e| format!("can't find the agentsh binary: {e}"))?;
    let (mut passed, mut total) = (0, 0);
    for file in files {
        let text = std::fs::read_to_string(&file).map_err(|e| format!("{}: {e}", file.display()))?;
        let cases: Vec<Value> = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
        for case in cases {
            total += 1;
            let name = case["name"].as_str().unwrap_or("unnamed").to_string();
            let problems = run_case(&exe, &dir, builtin, &case)?;
            if problems.is_empty() {
                passed += 1;
                println!("  ok    {name}");
            } else {
                println!("  FAIL  {name}\n        {}", problems.join("\n        "));
            }
        }
    }
    println!("{passed} of {total} cases passed");
    Ok(passed == total)
}

fn run_case(exe: &Path, dir: &Path, builtin: bool, case: &Value) -> Result<Vec<String>, String> {
    let root_dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let home = tempfile::tempdir().map_err(|e| e.to_string())?;
    let root = root_dir.path().canonicalize().map_err(|e| e.to_string())?;
    for (path, content) in case["files"].as_object().into_iter().flatten() {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap_or(&root)).map_err(|e| e.to_string())?;
        std::fs::write(full, content.as_str().unwrap_or_default()).map_err(|e| e.to_string())?;
    }
    if let Some(config) = case["config"].as_str() {
        std::fs::write(root.join(".agentsh.toml"), config).map_err(|e| e.to_string())?;
    }
    let package_dir = (!builtin).then_some(dir);
    let mut problems = Vec::new();
    for (i, step) in case["steps"].as_array().ok_or("a case needs steps")?.iter().enumerate() {
        let args: Vec<String> = match (step["run"].as_str(), step["cli"].as_array()) {
            (Some(program), _) => vec!["run".into(), program.into()],
            (None, Some(cli)) => cli.iter().filter_map(|a| a.as_str().map(String::from)).collect(),
            _ => return Err(format!("step {i}: needs `run` or `cli`")),
        };
        let stdout = invoke(exe, &root, home.path(), package_dir, case, &args);
        let label = format!("step {i} ({})", args.join(" ").replace('\n', "; "));
        if step.get("expect_text").is_some() || step.get("expect_text~").is_some() {
            problems.extend(check_text(&stdout, step).into_iter().map(|p| format!("{label}: {p}")));
            continue;
        }
        let envelope: Value = match serde_json::from_str(&stdout) {
            Ok(v) => v,
            Err(_) => {
                problems.push(format!("{label}: output isn't JSON: {stdout}"));
                continue;
            }
        };
        let found = check_expect(&envelope, &step["expect"]);
        if !found.is_empty() {
            problems.extend(found.into_iter().map(|p| format!("{label}: {p}")));
            problems.push(format!("  got: {envelope}"));
        }
        problems.extend(check_files(&root, &step["files"]).into_iter().map(|p| format!("{label} files: {p}")));
    }
    Ok(problems)
}

fn invoke(exe: &Path, root: &Path, home: &Path, package: Option<&Path>, case: &Value, args: &[String]) -> String {
    let mut cmd = Command::new(exe);
    cmd.current_dir(root).env("AGENTSH_HOME", home).args(["--root", &root.display().to_string()]);
    for key in [
        "AGENTSH_ALLOW",
        "AGENTSH_DENY",
        "AGENTSH_TRACE",
        "AGENTSH_SESSION",
        "AGENTSH_ROOT",
        "AGENTSH_FORMAT",
        "AGENTSH_PACKAGES",
    ] {
        cmd.env_remove(key);
    }
    if let Some(dir) = package {
        cmd.env("AGENTSH_PACKAGES", dir);
    }
    for cap in case["allow"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        cmd.args(["--allow", cap]);
    }
    for (k, v) in case["env"].as_object().into_iter().flatten() {
        cmd.env(k, v.as_str().unwrap_or_default());
    }
    if !args.iter().any(|a| a == "--format") {
        cmd.args(["--format", "json"]);
    }
    match cmd.args(args).output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
            if stdout.is_empty() {
                format!("<no stdout; stderr: {}>", String::from_utf8_lossy(&out.stderr).trim())
            } else {
                stdout
            }
        }
        Err(e) => format!("<couldn't run agentsh: {e}>"),
    }
}

fn check_text(stdout: &str, step: &Value) -> Vec<String> {
    match (step["expect_text"].as_str(), step["expect_text~"].as_str()) {
        (Some(exact), _) if stdout != exact => vec![format!("text: expected {exact:?}, got {stdout:?}")],
        (_, Some(part)) if !stdout.contains(part) => vec![format!("text should contain {part:?}, got {stdout:?}")],
        _ => Vec::new(),
    }
}

fn lookup<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(value, |v, key| match key.parse::<usize>() {
        Ok(i) if v.is_array() => v.get(i),
        _ => v.get(key),
    })
}

fn check_expect(envelope: &Value, expect: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    for (path, wanted) in expect.as_object().into_iter().flatten() {
        if let Some(needle) = path.strip_suffix('~') {
            let actual = lookup(envelope, needle).map(|v| v.to_string()).unwrap_or_default();
            if !actual.contains(wanted.as_str().unwrap_or_default()) {
                problems.push(format!("{needle} should contain {wanted}, got {actual}"));
            }
            continue;
        }
        let actual = lookup(envelope, path);
        let matches = match (actual, wanted) {
            (None, Value::Null) => true,
            (Some(a), w) => a == w,
            (None, _) => false,
        };
        if !matches {
            let got = actual.map(|v| v.to_string()).unwrap_or("nothing".into());
            problems.push(format!("{path}: expected {wanted}, got {got}"));
        }
    }
    problems
}

fn check_files(root: &Path, files: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    for (path, wanted) in files.as_object().into_iter().flatten() {
        let full = root.join(path);
        match (wanted, std::fs::read_to_string(&full)) {
            (Value::Null, Err(_)) => {}
            (Value::Null, Ok(_)) => problems.push(format!("{path} should not exist")),
            (Value::Bool(true), _) if full.exists() => {}
            (Value::Bool(true), _) => problems.push(format!("{path} should exist")),
            (Value::String(s), Ok(actual)) if &actual == s => {}
            (w, actual) => problems.push(format!("{path}: expected {w}, got {actual:?}")),
        }
    }
    problems
}
