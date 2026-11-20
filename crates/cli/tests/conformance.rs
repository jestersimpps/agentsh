use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

struct Case {
    file: String,
    name: String,
    body: Value,
}

fn repo_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect()
}

/// `spec/tests/*.json`, then every package's `packages/<name>/tests/*.json`.
fn load_cases() -> Vec<Case> {
    let repo = repo_dir();
    let mut files = json_files(&repo.join("spec/tests"));
    assert!(!files.is_empty(), "spec/tests has no cases");
    let mut packages: Vec<PathBuf> =
        std::fs::read_dir(repo.join("packages")).expect("packages/ exists").flatten().map(|e| e.path()).collect();
    packages.sort();
    for package in packages {
        files.extend(json_files(&package.join("tests")));
    }
    files.sort();
    files
        .into_iter()
        .flat_map(|path| {
            let text = std::fs::read_to_string(&path).unwrap();
            let cases: Vec<Value> = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            let file = path.strip_prefix(repo_dir()).unwrap_or(&path).to_string_lossy().into_owned();
            cases.into_iter().map(move |body| Case {
                file: file.clone(),
                name: body["name"].as_str().unwrap_or("unnamed").to_string(),
                body,
            })
        })
        .collect()
}

fn agentsh(root: &Path, home: &Path, case: &Value, args: &[&str]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agentsh"));
    cmd.current_dir(root)
        .env("AGENTSH_HOME", home)
        .env_remove("AGENTSH_ALLOW")
        .env_remove("AGENTSH_DENY")
        .env_remove("AGENTSH_TRACE")
        .env_remove("AGENTSH_SESSION")
        .env_remove("AGENTSH_ROOT")
        .args(["--root", root.to_str().unwrap()]);

    if let Some(allow) = case["allow"].as_array() {
        for cap in allow {
            cmd.args(["--allow", cap.as_str().unwrap()]);
        }
    }
    if let Some(env) = case["env"].as_object() {
        for (k, v) in env {
            cmd.env(k, v.as_str().unwrap());
        }
    }
    if !args.contains(&"--format") {
        cmd.args(["--format", "json"]);
    }
    let out = cmd.args(args).output().expect("agentsh runs");
    let stdout = String::from_utf8_lossy(&out.stdout).trim_end().to_string();
    if stdout.is_empty() {
        return format!("<no stdout; stderr: {}>", String::from_utf8_lossy(&out.stderr).trim());
    }
    stdout
}

fn parse_envelope(stdout: &str) -> Value {
    serde_json::from_str(stdout).unwrap_or_else(|e| panic!("bad JSON ({e}): {stdout}"))
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
        if let Some(needle) = path.strip_suffix("~") {
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
            problems.push(format!(
                "{path}: expected {wanted}, got {}",
                actual.map(|v| v.to_string()).unwrap_or("nothing".into())
            ));
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

fn run_case(case: &Case) -> Vec<String> {
    let root_dir = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let root = root_dir.path().canonicalize().unwrap();
    for (path, content) in case.body["files"].as_object().into_iter().flatten() {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content.as_str().unwrap()).unwrap();
    }
    if let Some(config) = case.body["config"].as_str() {
        std::fs::write(root.join(".agentsh.toml"), config).unwrap();
    }
    let mut problems = Vec::new();
    for (i, step) in case.body["steps"].as_array().expect("steps").iter().enumerate() {
        let args: Vec<String> = if let Some(program) = step["run"].as_str() {
            vec!["run".into(), program.into()]
        } else {
            step["cli"].as_array().expect("run or cli").iter().map(|a| a.as_str().unwrap().to_string()).collect()
        };
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let stdout = agentsh(&root, home.path(), &case.body, &arg_refs);
        if step.get("expect_text").is_some() || step.get("expect_text~").is_some() {
            problems
                .extend(check_text(&stdout, step).into_iter().map(|p| format!("step {i} ({}): {p}", args.join(" "))));
            continue;
        }
        let envelope = parse_envelope(&stdout);
        for p in check_expect(&envelope, &step["expect"]) {
            problems.push(format!("step {i} ({}): {p}\n    envelope: {envelope}", args.join(" ")));
        }
        for p in check_files(&root, &step["files"]) {
            problems.push(format!("step {i} files: {p}"));
        }
    }
    problems
}

#[test]
fn conformance() {
    let cases = load_cases();
    assert!(!cases.is_empty(), "no conformance cases found");
    let mut failures = Vec::new();
    for case in &cases {
        let problems = run_case(case);
        if !problems.is_empty() {
            failures.push(format!("[{}] {}\n  {}", case.file, case.name, problems.join("\n  ")));
        }
    }
    assert!(failures.is_empty(), "{} of {} cases failed:\n\n{}", failures.len(), cases.len(), failures.join("\n\n"));
    eprintln!("{} conformance cases passed", cases.len());
}

/// The packages in docs/examples are the tutorial's; keep them passing with `agentsh pkg test`.
#[test]
fn example_packages() {
    let examples = repo_dir().join("docs/examples");
    for entry in std::fs::read_dir(&examples).expect("docs/examples exists").flatten() {
        let home = tempfile::tempdir().unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_agentsh"))
            .env("AGENTSH_HOME", home.path())
            .env_remove("AGENTSH_PACKAGES")
            .args(["pkg", "test"])
            .arg(entry.path())
            .output()
            .expect("agentsh runs");
        let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{}:\n{text}", entry.path().display());
    }
}
