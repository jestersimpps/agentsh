"""Compare bash and agentsh on common agent tasks: calls, tokens in/out, wall time.

Each task is written the way a competent agent would do it in each shell. Every
"call" is one tool invocation: the agent writes the command (tokens in) and reads
everything it prints (tokens out). Model thinking time between calls is not measured.

usage: python3.11 bench/compare.py [--runs 5] [--out bench/results.md]
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

EDIT_FILE = "crates/platform/src/secrets.rs"
LANG_TEST = "cargo test -q -p agentsh-lang --offline 2>&1 | grep -E 'test result|FAILED|panicked|error'"

TASKS = [
    {
        "name": "orient in a new repo",
        "bash": ["pwd; git branch --show-current; ls; for t in git cargo node python3 docker; do command -v $t >/dev/null && echo $t; done"],
        "agentsh": ["env"],
    },
    {
        "name": "5 largest Rust files",
        "bash": ["find crates -name '*.rs' -exec wc -c {} + | sort -rn | head -6"],
        "agentsh": ['find "crates" name:"*.rs" | sort .size desc:true | take 5 | pick .path .size'],
    },
    {
        "name": "file types in the repo",
        "bash": ["find . -type f -not -path './target/*' -not -path './.git/*' | sed 's/.*\\.//' | sort | uniq -c | sort -rn"],
        "agentsh": ['find "." type:"file" | group ext(.path)'],
    },
    {
        "name": "find a definition and read it",
        "bash": ['grep -rn "fn smart_truncate" crates', "sed -n '54,70p' crates/core/src/limits.rs"],
        "agentsh": ['grep "fn smart_truncate" in:"crates" after:16'],
    },
    {
        "name": "read a whole file",
        "bash": ["cat crates/core/src/redact.rs"],
        "agentsh": ['read "crates/core/src/redact.rs"'],
    },
    {
        "name": "read three files",
        "bash": ["cat Cargo.toml crates/spec/Cargo.toml crates/lang/Cargo.toml"],
        "agentsh": ['read "Cargo.toml" "crates/spec/Cargo.toml" "crates/lang/Cargo.toml"'],
    },
    {
        "name": "typo in a path, then recover",
        "bash": ["cat crates/core/src/limit.rs", "ls crates/core/src", "head -20 crates/core/src/limits.rs"],
        "agentsh": ['read "crates/core/src/limit.rs" to:20'],
    },
    {
        "name": "edit one line and check the diff",
        "bash": [
            f"sed -i '' 's/MIN_REDACT_LEN: usize = 8/MIN_REDACT_LEN: usize = 10/' {EDIT_FILE}" if sys.platform == "darwin"
            else f"sed -i 's/MIN_REDACT_LEN: usize = 8/MIN_REDACT_LEN: usize = 10/' {EDIT_FILE}",
            "git diff",
        ],
        "agentsh": [f'edit "{EDIT_FILE}" old:"const MIN_REDACT_LEN: usize = 8;" new:"const MIN_REDACT_LEN: usize = 10;"'],
    },
    {
        "name": "rename a function across files",
        "bash": [
            ("grep -rl smart_truncate crates | xargs sed -i '' 's/smart_truncate/shorten_text/g'" if sys.platform == "darwin"
             else "grep -rl smart_truncate crates | xargs sed -i 's/smart_truncate/shorten_text/g'"),
            "grep -rc shorten_text crates | grep -v ':0$'",
        ],
        "agentsh": ['replace "smart_truncate" "shorten_text" in:"crates"'],
    },
    {
        "name": "run tests, show the summary",
        "bash": [LANG_TEST],
        "agentsh": ['exec "cargo test -q -p agentsh-lang --offline" timeout:5m parse:"text" | grep "test result|FAILED|panicked|error"'],
        "allow": "exec",
    },
    {
        "name": "bad edit + tests, then revert",
        "bash": [
            (f"sed -i '' 's/fn wildcard_match(/fn wildcard_match BROKEN(/' {EDIT_FILE}" if sys.platform == "darwin"
             else f"sed -i 's/fn wildcard_match(/fn wildcard_match BROKEN(/' {EDIT_FILE}"),
            "cargo test -q -p agentsh-platform --offline 2>&1 | grep -E 'test result|FAILED|panicked|^error' | head -5",
            f"git checkout -- {EDIT_FILE}",
        ],
        "agentsh": [
            f'edit "{EDIT_FILE}" old:"fn wildcard_match(" new:"fn wildcard_match BROKEN("\n'
            'exec "cargo test -q -p agentsh-platform --offline" timeout:5m parse:"text" | grep "test result|FAILED|panicked"'
        ],
        "allow": "exec",
    },
    {
        "name": "explore the project layout",
        "bash": ["ls; ls crates; ls crates/*/src | head -60"],
        "agentsh": ["map"],
    },
    {
        "name": "find where a function is used",
        "bash": ['grep -rnw "smart_truncate" crates'],
        "agentsh": ['refs "smart_truncate" in:"crates"'],
    },
    {
        "name": "read one function from a big file",
        "bash": ['grep -n "fn eval_binary" crates/core/src/interp.rs', "sed -n '322,360p' crates/core/src/interp.rs"],
        "agentsh": ['read "crates/core/src/interp.rs" symbol:"eval_binary"'],
    },
    {
        "name": "re-read a file after editing it",
        "bash": [
            "cat crates/core/src/redact.rs",
            (f"sed -i '' 's/if value.is_empty()/if value.trim().is_empty()/' crates/core/src/redact.rs" if sys.platform == "darwin"
             else "sed -i 's/if value.is_empty()/if value.trim().is_empty()/' crates/core/src/redact.rs"),
            "cat crates/core/src/redact.rs",
        ],
        "agentsh": [
            'read "crates/core/src/redact.rs"',
            'exec "sed -i \'\' \'s/if value.is_empty()/if value.trim().is_empty()/\' crates/core/src/redact.rs"' if sys.platform == "darwin"
            else 'exec "sed -i \'s/if value.is_empty()/if value.trim().is_empty()/\' crates/core/src/redact.rs"',
            'read "crates/core/src/redact.rs"',
        ],
        "allow": "exec",
        "session": "agent-1",
    },
    {
        "name": "revisit files during a session",
        "bash": [
            "cat crates/core/src/redact.rs",
            "cat crates/core/src/time.rs",
            "cat crates/core/src/redact.rs",
            "cat crates/core/src/time.rs",
        ],
        "agentsh": [
            'read "crates/core/src/redact.rs"',
            'read "crates/core/src/time.rs"',
            'read "crates/core/src/redact.rs"',
            'read "crates/core/src/time.rs"',
        ],
        "session": "agent-1",
    },
    {
        "name": "trivial call (overhead)",
        "bash": ["pwd"],
        "agentsh": ["pwd"],
    },
]


def tokens(text: str) -> int:
    return len(ENC.encode(text, disallowed_special=()))


def make_workspace(dest: Path) -> None:
    files = subprocess.run(["git", "ls-files", "-co", "--exclude-standard"], cwd=REPO, capture_output=True, text=True, check=True).stdout.split()
    for rel in files:
        if rel.startswith(("target", "bench/")):
            continue
        target = dest / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(REPO / rel, target)
    git = ["git", "-c", "user.name=bench", "-c", "user.email=bench@localhost"]
    subprocess.run([*git, "init", "-q"], cwd=dest, check=True)
    subprocess.run([*git, "add", "-A"], cwd=dest, check=True)
    subprocess.run([*git, "commit", "-qm", "base"], cwd=dest, check=True)
    subprocess.run(["cargo", "test", "-q", "--offline", "--no-run", "-p", "agentsh-lang", "-p", "agentsh-platform"], cwd=dest, capture_output=True, check=True)


def reset(ws: Path) -> None:
    subprocess.run(["git", "reset", "-q", "--hard"], cwd=ws, check=True)
    subprocess.run(["git", "clean", "-qfd"], cwd=ws, check=True)


def run_bash(ws: Path, command: str, env: dict) -> str:
    out = subprocess.run(["bash", "-c", command], cwd=ws, capture_output=True, text=True, env=env)
    return out.stdout + out.stderr


def run_agentsh(ws: Path, program: str, env: dict, fmt: str, allow: str | None, session: str | None) -> str:
    args = [str(AGENTSH), "--format", fmt]
    if session:
        args += ["--session", session]
    if allow:
        args += ["--allow", allow]
    out = subprocess.run([*args, "run", program], cwd=ws, capture_output=True, text=True, env=env)
    return out.stdout + out.stderr


def measure(ws: Path, task: dict, mode: str, runs: int, env: dict) -> dict:
    times, outputs = [], []
    for run in range(runs):
        reset(ws)
        run_env = {**env, "AGENTSH_HOME": tempfile.mkdtemp(prefix="agentsh-bench-state-")}
        start = time.perf_counter()
        outs = []
        for call in task["bash" if mode == "bash" else "agentsh"]:
            if mode == "bash":
                outs.append(run_bash(ws, call, run_env))
            else:
                outs.append(run_agentsh(ws, call, run_env, mode.split("-")[1], task.get("allow"), task.get("session")))
        times.append(time.perf_counter() - start)
        shutil.rmtree(run_env["AGENTSH_HOME"], ignore_errors=True)
        outputs = outs
    calls = task["bash" if mode == "bash" else "agentsh"]
    return {
        "calls": len(calls),
        "tokens_in": sum(tokens(c) for c in calls),
        "tokens_out": sum(tokens(o) for o in outputs),
        "bytes_out": sum(len(o.encode()) for o in outputs),
        "ms": statistics.median(times) * 1000,
        "outputs": outputs,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--out", default=str(REPO / "bench" / "results.md"))
    parser.add_argument("--show", help="print the outputs of tasks whose name contains this text")
    opts = parser.parse_args()

    home = Path(tempfile.mkdtemp(prefix="agentsh-bench-home-"))
    ws = Path(tempfile.mkdtemp(prefix="agentsh-bench-ws-"))
    env = {**os.environ, "AGENTSH_HOME": str(home)}
    for key in ("AGENTSH_ALLOW", "AGENTSH_DENY", "AGENTSH_FORMAT", "AGENTSH_SESSION", "AGENTSH_TRACE"):
        env.pop(key, None)
    try:
        make_workspace(ws)
        rows = []
        for task in TASKS:
            result = {mode: measure(ws, task, mode, opts.runs, env) for mode in ("bash", "agentsh-json", "agentsh-text")}
            rows.append((task["name"], result))
            print(f"{task['name']}: " + ", ".join(f"{m} {r['calls']} calls {r['tokens_in'] + r['tokens_out']} tok {r['ms']:.0f} ms" for m, r in result.items()), file=sys.stderr)
            if opts.show and opts.show in task["name"]:
                for mode, r in result.items():
                    print(f"\n----- {mode}", file=sys.stderr)
                    for o in r["outputs"]:
                        print(o, file=sys.stderr)
        report = render(rows, opts.runs)
        Path(opts.out).write_text(report)
        Path(opts.out).with_suffix(".json").write_text(json.dumps(
            [{"task": name, **{m: {k: v for k, v in r.items() if k != "outputs"} for m, r in res.items()}} for name, res in rows], indent=2))
        print(report)
    finally:
        shutil.rmtree(ws, ignore_errors=True)
        shutil.rmtree(home, ignore_errors=True)


def render(rows: list, runs: int) -> str:
    lines = [
        f"# bash vs agentsh ({sys.platform}, median of {runs} runs, tokens = cl100k_base estimate)",
        "",
        "| task | calls bash / ash | tokens bash | tokens ash json | tokens ash text | ms bash | ms ash |",
        "|---|---|---|---|---|---|---|",
    ]
    totals = {m: {"calls": 0, "tok": 0, "ms": 0.0} for m in ("bash", "agentsh-json", "agentsh-text")}
    for name, res in rows:
        b, j, t = res["bash"], res["agentsh-json"], res["agentsh-text"]
        for m, r in res.items():
            totals[m]["calls"] += r["calls"]
            totals[m]["tok"] += r["tokens_in"] + r["tokens_out"]
            totals[m]["ms"] += r["ms"]
        lines.append(
            f"| {name} | {b['calls']} / {t['calls']} | {b['tokens_in'] + b['tokens_out']} | {j['tokens_in'] + j['tokens_out']} "
            f"| {t['tokens_in'] + t['tokens_out']} | {b['ms']:.0f} | {t['ms']:.0f} |"
        )
    b, j, t = totals["bash"], totals["agentsh-json"], totals["agentsh-text"]
    lines.append(f"| **total** | **{b['calls']} / {t['calls']}** | **{b['tok']}** | **{j['tok']}** | **{t['tok']}** | **{b['ms']:.0f}** | **{t['ms']:.0f}** |")
    lines += ["", "Tokens = command written + everything printed back, summed over the task's calls. Time excludes model thinking between calls."]
    return "\n".join(lines) + "\n"


if __name__ == "__main__":
    main()
