"""Benchmark the packages against bash on tasks mined from real agent sessions.

The tasks come from `bench/mine_sessions.py` run over ~800 Claude Code and Codex sessions: git
(status/diff/show/log/grep/ls-tree), gh (pr view, pr checks, run view --log-failed), tsc, lsof/ps,
curl + jq, pdftotext and unzip. Each bash side uses the flags those sessions used most.

Every "call" is one tool invocation: the agent writes the command (tokens in) and reads everything
it prints (tokens out). Model thinking time between calls is not measured. The packages' briefings
are counted once, as their own row, because a model has to read them before using a package.

usage: <python with tiktoken> bench/packages_bench.py [--runs 3] [--out bench/packages_results.md] [--offline]
"""

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import tiktoken

REPO = Path(__file__).resolve().parent.parent
AGENTSH = REPO / "target" / "release" / "agentsh"
ENC = tiktoken.get_encoding("cl100k_base")
PORT = 18766
TS_CACHE = Path(tempfile.gettempdir()) / "agentsh-bench-typescript"

PR = ("15109", "cli/cli")
RUN = ("44686986388", "vercel/next.js")

TS_FILES = {
    "tsproj/tsconfig.json": '{"compilerOptions": {"strict": true, "noEmit": true, "target": "es2022", "module": "esnext", "moduleResolution": "bundler"}, "include": ["src"]}',
    "tsproj/src/user.ts": "export interface User { id: number; name: string; email?: string }\n"
    "export function label(u: User): string { return u.name.toUpperCase() + ' <' + u.email.trim() + '>' }\n",
    "tsproj/src/cart.ts": "import { User } from './user'\n"
    "export type Item = { sku: string; price: number; qty: number }\n"
    "export function total(items: Item[]): number { return items.reduce((s, i) => s + i.price * i.qty, '0') }\n"
    "export function owner(u: User) { return u.nickname }\n"
    "export const first: Item = { sku: 'a', price: '3', qty: 1 }\n",
    "tsproj/src/api.ts": "export async function load(id) { const r = await fetch('/api/' + id); return r.jsn() }\n",
}

TASKS = [
    # ---- git (21k calls in the sessions; git show and git diff are the costliest outputs) ----
    {
        "name": "what changed (status + line counts)",
        "setup": "printf '\\n// tweak\\n' >> crates/core/src/time.rs && echo draft > notes.txt",
        "bash": ["git status --short && git diff --stat"],
        "agentsh": ["git.status"],
    },
    {
        "name": "review a branch vs main",
        "bash": [
            "git log --oneline main..HEAD",
            "git diff --stat main...HEAD",
            "git diff main...HEAD -- crates/core/src/limits.rs",
        ],
        "agentsh": ['git.log rev:"main..HEAD"\ngit.changed\ngit.diff "crates/core/src/limits.rs" rev:"main...HEAD"'],
    },
    {
        "name": "read one function at another revision",
        "bash": [
            'git show main:crates/core/src/interp.rs | grep -n "fn eval_binary"',
            "git show main:crates/core/src/interp.rs | sed -n '{start},{end}p'",
        ],
        "agentsh": ['git.show "main" "crates/core/src/interp.rs" symbol:"eval_binary"'],
    },
    {
        "name": "search code at a revision",
        "bash": ['git grep -n -i -E "smart_truncate" main -- crates | head -40'],
        "agentsh": ['git.grep "smart_truncate" rev:"main" in:"crates"'],
    },
    {
        "name": "list files at a revision",
        "bash": ["git ls-tree -r --name-only main -- crates/core | grep '\\.rs$'"],
        "agentsh": ['git.files "crates/core" rev:"main" match:"\\\\.rs$"'],
    },
    {
        "name": "recent commits touching a file",
        "bash": ["git log --oneline -5 -- README.md"],
        "agentsh": ['git.log "README.md" n:5'],
    },
    {
        "name": "inspect the last commit",
        "bash": ["git show --stat HEAD"],
        "agentsh": ['git.show "HEAD"'],
    },
    # ---- gh (pr view / pr checks / run view are the top gh calls) ----
    {
        "name": "PR status with checks",
        "network": True,
        "bash": [
            f"gh pr view {PR[0]} --repo {PR[1]} --json number,title,state,reviewDecision,mergeable,additions,deletions,changedFiles,statusCheckRollup "
            "--jq '{number,title,state,reviewDecision,mergeable,additions,deletions,changedFiles,checks:[.statusCheckRollup[]|{name,conclusion}]}'"
        ],
        "agentsh": [f'gh.pr {PR[0]} repo:"{PR[1]}"'],
        "allow": "exec,net",
    },
    {
        "name": "which checks didn't pass",
        "network": True,
        "bash": [f"gh pr checks {PR[0]} --repo {PR[1]}"],
        "agentsh": [f'gh.checks {PR[0]} repo:"{PR[1]}"'],
        "allow": "exec,net",
    },
    {
        "name": "read a failed CI log",
        "network": True,
        "bash": [f"gh run view {RUN[0]} --repo {RUN[1]} --log-failed | tail -120"],
        "agentsh": [f'gh.run_log {RUN[0]} repo:"{RUN[1]}"'],
        "allow": "exec,net",
    },
    # ---- js: npx tsc --noEmit (366 calls) ----
    {
        "name": "TypeScript errors",
        "needs_ts": True,
        "bash": ["npx tsc --noEmit -p tsproj"],
        "agentsh": ['js.typecheck "tsproj"'],
    },
    # ---- sys: lsof / ps ----
    {
        "name": "what's listening on which port",
        "bash": ["lsof -nP -iTCP -sTCP:LISTEN"],
        "agentsh": ["sys.ports"],
    },
    {
        "name": "is a process running",
        "bash": ["ps aux | grep -i 'http.server' | grep -v grep"],
        "agentsh": ['sys.procs "http.server"'],
    },
    # ---- http: curl + jq ----
    {
        "name": "query a local JSON API",
        "bash": [f"curl -sS http://127.0.0.1:{PORT}/users.json | jq '[.users[] | select(.active) | {{id, email}}]'"],
        "agentsh": [f'let d = (http.get "http://127.0.0.1:{PORT}/users.json")\n$d.users | where .active | pick .id .email'],
        "allow": "exec,net",
    },
    # ---- docs: pdftotext / unzip ----
    {
        "name": "read page 2 of a PDF",
        "bash": ["pdftotext -layout -f 2 -l 2 fixtures/report.pdf -"],
        "agentsh": ['docs.pdf "fixtures/report.pdf" pages:2'],
    },
    {
        "name": "list a zip archive",
        "bash": ["unzip -l fixtures/bundle.zip"],
        "agentsh": ['docs.zip "fixtures/bundle.zip"'],
    },
]

BRIEFINGS = {"name": "read the briefings used (once per session)", "bash": [], "agentsh": [
    'help "git"\nhelp "gh"\nhelp "js"\nhelp "sys"\nhelp "http"\nhelp "docs"'
]}


def tokens(text: str) -> int:
    return len(ENC.encode(text, disallowed_special=()))


def git(ws: Path, *args: str) -> str:
    cmd = ["git", "-c", "user.name=bench", "-c", "user.email=bench@localhost", *args]
    return subprocess.run(cmd, cwd=ws, capture_output=True, text=True, check=True).stdout


def make_workspace(dest: Path, with_ts: bool) -> None:
    files = subprocess.run(["git", "ls-files", "-co", "--exclude-standard"], cwd=REPO, capture_output=True, text=True, check=True).stdout.split()
    for rel in files:
        if rel.startswith(("target", "bench/")):
            continue
        target = dest / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(REPO / rel, target)
    git(dest, "init", "-q")
    git(dest, "add", "-A")
    git(dest, "commit", "-qm", "base")
    git(dest, "branch", "-M", "main")
    git(dest, "checkout", "-q", "-b", "feature")
    limits = dest / "crates/core/src/limits.rs"
    limits.write_text(limits.read_text().replace("const MAX_DIFF_FILES: usize = 30;", "const MAX_DIFF_FILES: usize = 40;", 1))
    git(dest, "commit", "-qam", "limits: document head budget")
    readme = dest / "README.md"
    readme.write_text(readme.read_text() + "\n## Changelog\n\n- packages\n")
    git(dest, "commit", "-qam", "docs: changelog")
    fixtures = dest / "fixtures"
    fixtures.mkdir()
    users = [{"id": i, "email": f"user{i}@example.com", "name": f"User {i}", "active": i % 3 != 0,
              "roles": ["member"] + (["admin"] if i % 7 == 0 else []), "created": f"2026-0{1 + i % 9}-1{i % 9}T10:00:00Z"} for i in range(1, 31)]
    (fixtures / "users.json").write_text(json.dumps({"users": users, "total": len(users)}, indent=2))
    report = "\n".join(f"Line {i:03d}  quarterly figures  region {i % 7}  value {i * 37 % 1000:>5}" for i in range(1, 181))
    (fixtures / "report.txt").write_text(report)
    with open(fixtures / "report.pdf", "wb") as out:
        subprocess.run(["cupsfilter", "-i", "text/plain", "-m", "application/pdf", str(fixtures / "report.txt")], stdout=out, stderr=subprocess.DEVNULL, check=True)
    bundle = fixtures / "bundle"
    for i in range(12):
        (bundle / f"part{i % 3}").mkdir(parents=True, exist_ok=True)
        (bundle / f"part{i % 3}" / f"file{i}.txt").write_text("x" * (i * 50))
    subprocess.run(["zip", "-qr", "bundle.zip", "bundle"], cwd=fixtures, check=True)
    shutil.rmtree(bundle)
    if with_ts:
        for rel, text in TS_FILES.items():
            (dest / rel).parent.mkdir(parents=True, exist_ok=True)
            (dest / rel).write_text(text)
        shutil.copytree(TS_CACHE / "node_modules", dest / "node_modules", symlinks=True)
    (dest / ".git" / "info" / "exclude").write_text("node_modules\n")
    git(dest, "add", "-A")
    git(dest, "commit", "-qm", "fixtures")


def ensure_typescript() -> bool:
    if (TS_CACHE / "node_modules" / ".bin" / "tsc").exists():
        return True
    TS_CACHE.mkdir(parents=True, exist_ok=True)
    out = subprocess.run(["npm", "install", "--silent", "--no-audit", "--no-fund", "--prefix", str(TS_CACHE), "typescript@5"], capture_output=True, text=True)
    return out.returncode == 0


def reset(ws: Path) -> None:
    subprocess.run(["git", "reset", "-q", "--hard"], cwd=ws, check=True)
    subprocess.run(["git", "clean", "-qfd"], cwd=ws, check=True)


def interp_lines(ws: Path) -> tuple[int, int]:
    """The line range of eval_binary at main, as an agent would read it off grep -n."""
    text = git(ws, "show", "main:crates/core/src/interp.rs").splitlines()
    start = next(i for i, l in enumerate(text, 1) if "fn eval_binary" in l)
    return start, start + 38


def run(ws: Path, mode: str, call: str, env: dict, allow: str | None) -> str:
    if mode == "bash":
        out = subprocess.run(["bash", "-c", call], cwd=ws, capture_output=True, text=True, env=env)
    else:
        args = [str(AGENTSH), "--format", mode.split("-")[1], "--allow", allow or "exec", "run", call]
        out = subprocess.run(args, cwd=ws, capture_output=True, text=True, env=env)
    return out.stdout + out.stderr


def measure(ws: Path, task: dict, mode: str, runs: int, env: dict) -> dict:
    calls = task["bash" if mode == "bash" else "agentsh"]
    times, outputs = [], []
    for _ in range(runs):
        reset(ws)
        if task.get("setup"):
            subprocess.run(["bash", "-c", task["setup"]], cwd=ws, check=True)
        run_env = {**env, "AGENTSH_HOME": tempfile.mkdtemp(prefix="agentsh-bench-state-")}
        start = time.perf_counter()
        outputs = [run(ws, mode, c, run_env, task.get("allow")) for c in calls]
        times.append(time.perf_counter() - start)
        shutil.rmtree(run_env["AGENTSH_HOME"], ignore_errors=True)
    return {
        "calls": len(calls),
        "tokens_in": sum(tokens(c) for c in calls),
        "tokens_out": sum(tokens(o) for o in outputs),
        "ms": statistics.median(times) * 1000 if times else 0.0,
        "outputs": outputs,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--out", default=str(REPO / "bench" / "packages_results.md"))
    parser.add_argument("--offline", action="store_true", help="skip the tasks that call GitHub")
    parser.add_argument("--show", help="print outputs of tasks whose name contains this")
    opts = parser.parse_args()

    with_ts = ensure_typescript()
    ws = Path(tempfile.mkdtemp(prefix="agentsh-pkgbench-ws-"))
    env = {k: v for k, v in os.environ.items() if not k.startswith("AGENTSH_")}
    server = None
    try:
        make_workspace(ws, with_ts)
        server = subprocess.Popen([sys.executable, "-m", "http.server", str(PORT), "--bind", "127.0.0.1", "--directory", str(ws / "fixtures")],
                                  stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        start, end = interp_lines(ws)
        rows = []
        for task in [*TASKS, BRIEFINGS]:
            if (task.get("network") and opts.offline) or (task.get("needs_ts") and not with_ts):
                continue
            task = {**task, "bash": [c.replace("{start}", str(start)).replace("{end}", str(end)) for c in task["bash"]]}
            result = {m: measure(ws, task, m, opts.runs, env) for m in ("bash", "agentsh-text", "agentsh-json")}
            rows.append((task["name"], result))
            b, t = result["bash"], result["agentsh-text"]
            print(f"{task['name']}: bash {b['calls']} calls {b['tokens_in'] + b['tokens_out']} tok | agentsh {t['calls']} calls {t['tokens_in'] + t['tokens_out']} tok", file=sys.stderr)
            if opts.show and opts.show in task["name"]:
                for mode, r in result.items():
                    print(f"\n----- {mode}", file=sys.stderr)
                    for o in r["outputs"]:
                        print(o, file=sys.stderr)
        report = render(rows, opts.runs)
        Path(opts.out).write_text(report)
        Path(opts.out).with_suffix(".json").write_text(json.dumps(
            [{"task": n, **{m: {k: v for k, v in r.items() if k != "outputs"} for m, r in res.items()}} for n, res in rows], indent=2))
        print(report)
    finally:
        if server:
            server.terminate()
        shutil.rmtree(ws, ignore_errors=True)


def render(rows: list, runs: int) -> str:
    lines = [
        f"# Packages vs bash ({sys.platform}, median of {runs} runs, tokens = cl100k_base)",
        "",
        "| task | calls bash / ash | tokens bash | tokens ash text | tokens ash json | ms bash | ms ash |",
        "|---|---|---|---|---|---|---|",
    ]
    tot = {m: {"calls": 0, "tok": 0, "ms": 0.0} for m in ("bash", "agentsh-text", "agentsh-json")}
    for name, res in rows:
        for m, r in res.items():
            tot[m]["calls"] += r["calls"]
            tot[m]["tok"] += r["tokens_in"] + r["tokens_out"]
            tot[m]["ms"] += r["ms"]
        b, t, j = res["bash"], res["agentsh-text"], res["agentsh-json"]
        bt = b["tokens_in"] + b["tokens_out"]
        lines.append(f"| {name} | {b['calls'] or '-'} / {t['calls']} | {bt or '-'} | {t['tokens_in'] + t['tokens_out']} | {j['tokens_in'] + j['tokens_out']} | {b['ms']:.0f} | {t['ms']:.0f} |")
    b, t, j = tot["bash"], tot["agentsh-text"], tot["agentsh-json"]
    lines.append(f"| **total** | **{b['calls']} / {t['calls']}** | **{b['tok']}** | **{t['tok']}** | **{j['tok']}** | **{b['ms']:.0f}** | **{t['ms']:.0f}** |")
    lines += ["", "Tokens = command written + everything printed back, summed over a task's calls. Time excludes model thinking.",
              "The briefings row is a one-time cost per session; bash has none because models already know bash."]
    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    main()
