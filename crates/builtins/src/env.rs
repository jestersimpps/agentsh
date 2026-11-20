use agentsh_core::{Args, Builtin, CommandSpec, Ctx, Result, find_on_path};
use agentsh_platform::Capability::Read;
use agentsh_spec::Value;

const TOOLS: &[&str] = &[
    "git",
    "gh",
    "node",
    "npm",
    "pnpm",
    "yarn",
    "bun",
    "deno",
    "python3",
    "pip3",
    "uv",
    "cargo",
    "rustc",
    "go",
    "java",
    "mvn",
    "gradle",
    "ruby",
    "make",
    "cmake",
    "docker",
    "kubectl",
    "terraform",
    "rg",
    "fd",
    "jq",
    "curl",
    "sqlite3",
    "psql",
    "ffmpeg",
    "swift",
    "xcodebuild",
];

const PROJECT_MARKERS: &[(&str, &str)] = &[
    ("Cargo.toml", "rust"),
    ("package.json", "node"),
    ("pnpm-lock.yaml", "pnpm"),
    ("yarn.lock", "yarn"),
    ("bun.lockb", "bun"),
    ("bun.lock", "bun"),
    ("package-lock.json", "npm"),
    ("tsconfig.json", "typescript"),
    ("pyproject.toml", "python"),
    ("requirements.txt", "python"),
    ("go.mod", "go"),
    ("Gemfile", "ruby"),
    ("pom.xml", "maven"),
    ("build.gradle", "gradle"),
    ("build.gradle.kts", "gradle"),
    ("Package.swift", "swift"),
    ("Makefile", "make"),
    ("Dockerfile", "docker"),
    ("docker-compose.yml", "docker-compose"),
    ("compose.yaml", "docker-compose"),
    ("next.config.js", "nextjs"),
    ("next.config.ts", "nextjs"),
    ("next.config.mjs", "nextjs"),
    ("vite.config.ts", "vite"),
    ("convex", "convex"),
];

pub const ENV: Builtin = Builtin {
    spec: CommandSpec {
        name: "env",
        summary: "orientation: root, project type, git branch, capabilities, sandbox, tools",
        params: &[],
        input: None,
        returns: "record",
        caps: &[Read],
        examples: &["env"],
    },
    run: env,
};

fn env(ctx: &mut Ctx, _args: &mut Args) -> Result<Value> {
    let project: Vec<Value> = PROJECT_MARKERS
        .iter()
        .filter(|(file, _)| ctx.root.join(file).exists())
        .map(|(_, kind)| Value::str(*kind))
        .collect();
    let tools: Vec<Value> = TOOLS.iter().filter(|t| find_on_path(t).is_some()).map(|t| Value::str(*t)).collect();
    let caps: Vec<Value> = ctx.config.capabilities.iter().map(|c| Value::str(c.as_str())).collect();
    let undoable = ctx.journal.entries().iter().map(|e| e.run).collect::<std::collections::BTreeSet<_>>().len();
    let mut record = vec![
        ("root", Value::Str(ctx.root.display().to_string())),
        ("cwd", Value::Str(ctx.display(&ctx.cwd()))),
        ("os", Value::Str(format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH))),
        ("project", Value::List(project)),
        ("git_branch", git_branch(ctx).map(Value::Str).unwrap_or_default()),
        ("capabilities", Value::List(caps)),
        ("sandbox", Value::str(ctx.sandbox.name())),
        ("tools", Value::List(tools)),
    ];
    if ctx.session_id != "default" {
        record.push(("session", Value::str(ctx.session_id)));
    }
    if undoable > 0 {
        record.push(("undoable_runs", Value::Int(undoable as i64)));
    }
    Ok(Value::record(record))
}

fn git_branch(ctx: &Ctx) -> Option<String> {
    let head = std::fs::read_to_string(ctx.root.join(".git/HEAD")).ok()?;
    let head = head.trim();
    Some(match head.strip_prefix("ref: refs/heads/") {
        Some(branch) => branch.to_string(),
        None => format!("detached at {}", &head[..head.len().min(12)]),
    })
}
