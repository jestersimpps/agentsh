"""Simulate a full coding session in bash and in agentsh and compare what an agent pays.

Task: add a `reverse` command to agentsh's own codebase (explore, implement with a
realistic compile error, register it, fix it, add a conformance case, run the suite,
review). Both scripts run for real against fresh copies of the repo and must leave
the tests passing.

Cost model for an agent loop: every turn the model re-reads the whole conversation so
far (base prompt + all earlier commands, outputs and its own reasoning), then writes a
little reasoning and the next command. agentsh pays for its cheat sheet in the base
prompt; bash needs none.

usage: python3.11 bench/session_sim.py [--base 2500] [--reasoning 60] [--framing 25] [--turn-latency 4]
"""

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import tiktoken

REPO = Path(__file__).resolve().parent.parent
AGENTSH = REPO / "target" / "release" / "agentsh"
ENC = tiktoken.get_encoding("cl100k_base")
SED_I = "sed -i ''" if sys.platform == "darwin" else "sed -i"

DATA = "crates/builtins/src/data.rs"
LIB = "crates/builtins/src/lib.rs"
CASES = "spec/tests/data_and_exec.json"

REVERSE_BUGGY = '''pub const REVERSE: Builtin = Builtin {
    spec: CommandSpec {
        name: "reverse",
        summary: "reverse the order of rows",
        params: &[P::named("input", T::List, "rows")],
        input: Some("input"),
        returns: "list",
        caps: &[],
        examples: &["ls | sort .size | reverse"],
    },
    run: reverse,
};

fn reverse(_ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let mut rows = input_list(args, "reverse")?;
    rows.reverse();
    Ok(rows)
}
'''
REVERSE_FIXED = REVERSE_BUGGY.replace("    Ok(rows)\n", "    Ok(Value::List(rows))\n")

BASH_STEPS = [
    "pwd; git branch --show-current; ls; ls crates",
    'grep -rn "pub const SKIP\\|pub const FIRST" crates/builtins/src',
    f"sed -n '/^pub const SKIP/,/^pub const FIRST/p' {DATA}",
    f"cat {LIB}",
    f"head -40 {CASES}",
    f"""python3 - <<'EOF'
p = "{DATA}"
s = open(p).read()
s = s.replace("pub const FIRST: Builtin = Builtin {{", '''{REVERSE_BUGGY}
pub const FIRST: Builtin = Builtin {{''', 1)
open(p, "w").write(s)
EOF""",
    f"{SED_I} 's/        data::SKIP,/        data::SKIP,\\n        data::REVERSE,/' {LIB}",
    "cargo test -q -p agentsh-builtins --offline 2>&1 | grep -E '^error|-->|test result' | head -20",
    f"{SED_I} 's/^    Ok(rows)$/    Ok(Value::List(rows))/' {DATA}",
    "cargo test -q -p agentsh-builtins --offline 2>&1 | grep -E '^error|-->|test result' | head -20",
    f"""python3 - <<'EOF'
import json
p = "{CASES}"
cases = json.load(open(p))
cases.append({{"name": "reverse", "steps": [{{"run": "[1, 2, 3] | reverse", "expect": {{"value": [3, 2, 1]}}}}]}})
json.dump(cases, open(p, "w"), indent=2, ensure_ascii=False)
EOF""",
    "cargo test -q -p agentsh --test conformance --offline 2>&1 | grep -E 'test result|cases|FAILED' | head",
    "git diff",
]


def ash_edit_block(code: str) -> str:
    return (
        f'edit "{DATA}" old:"pub const FIRST: Builtin = Builtin {{" new:\'{code}\n'
        "pub const FIRST: Builtin = Builtin {'\n"
        f'edit "{LIB}" old:"        data::SKIP,\\n" new:"        data::SKIP,\\n        data::REVERSE,\\n"\n'
        'exec "cargo test -q -p agentsh-builtins --offline" timeout:10m parse:"text" | grep "test result"'
    )


AGENTSH_STEPS = [
    "env",
    'grep "pub const SKIP" in:"crates/builtins/src" after:22',
    f'read "{LIB}"',
    f'read "{CASES}" to:40',
    ash_edit_block(REVERSE_BUGGY).replace('parse:"text"', 'keep:true parse:"text"'),
    f'edit "{DATA}" old:"    Ok(rows)\\n" new:"    Ok(Value::List(rows))\\n"\n'
    'exec "cargo test -q -p agentsh-builtins --offline" timeout:10m parse:"text" | grep "test result"',
    f'let cases = read "{CASES}" | json\n'
    f'write "{CASES}" ($cases + [{{name: "reverse", steps: [{{run: "[1, 2, 3] | reverse", expect: {{value: [3, 2, 1]}}}}]}}])\n'
    'exec "cargo test -q -p agentsh --test conformance --offline" timeout:10m parse:"text" | grep "test result|cases|FAILED"',
    'exec "git diff --stat" parse:"text"',
]


def tokens(text: str) -> int:
    return len(ENC.encode(text, disallowed_special=()))


def make_workspace() -> Path:
    ws = Path(tempfile.mkdtemp(prefix="agentsh-sim-"))
    files = subprocess.run(["git", "ls-files", "-co", "--exclude-standard"], cwd=REPO, capture_output=True, text=True, check=True).stdout.split()
    for rel in files:
        if rel.startswith(("target", "bench/")):
            continue
        (ws / rel).parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(REPO / rel, ws / rel)
    git = ["git", "-c", "user.name=sim", "-c", "user.email=sim@localhost"]
    subprocess.run([*git, "init", "-q"], cwd=ws, check=True)
    subprocess.run([*git, "add", "-A"], cwd=ws, check=True)
    subprocess.run([*git, "commit", "-qm", "base"], cwd=ws, check=True)
    print(f"warming the build in {ws} ...", file=sys.stderr)
    subprocess.run(["cargo", "test", "-q", "--offline", "--no-run"], cwd=ws, capture_output=True, check=True)
    return ws


def run_steps(ws: Path, steps: list[str], shell: str, env: dict) -> list[dict]:
    results = []
    for step in steps:
        start = time.perf_counter()
        if shell == "bash":
            out = subprocess.run(["bash", "-c", step], cwd=ws, capture_output=True, text=True, env=env)
        else:
            cmd = [str(AGENTSH), "--session", "sim", "--allow", "exec", "run", step]
            out = subprocess.run(cmd, cwd=ws, capture_output=True, text=True, env=env)
        output = out.stdout + out.stderr
        results.append({"command": step, "output": output, "seconds": time.perf_counter() - start})
        print(f"[{shell}] {step.splitlines()[0][:70]}  ->  {len(output)} bytes, {time.perf_counter() - start:.1f}s", file=sys.stderr)
    return results


def verify(ws: Path, env: dict) -> bool:
    out = subprocess.run(["cargo", "test", "-q", "--offline"], cwd=ws, capture_output=True, text=True, env=env)
    data = (ws / DATA).read_text()
    cases = (ws / CASES).read_text()
    return (
        out.returncode == 0
        and "Ok(Value::List(rows))" in data
        and "data::REVERSE" in (ws / LIB).read_text()
        and '"[1, 2, 3] | reverse"' in cases
    )


def account(results: list[dict], base: int, reasoning: int, framing: int) -> dict:
    context = base
    cumulative_input = 0
    generated = 0
    for r in results:
        cumulative_input += context
        call_in = tokens(r["command"])
        generated += reasoning + call_in
        context += reasoning + framing + call_in + tokens(r["output"])
    return {
        "turns": len(results),
        "tool_output_tokens": sum(tokens(r["output"]) for r in results),
        "command_tokens": sum(tokens(r["command"]) for r in results),
        "generated_tokens": generated,
        "final_context": context,
        "cumulative_input": cumulative_input,
        "tool_seconds": sum(r["seconds"] for r in results),
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base", type=int, default=2500, help="system prompt + task description tokens (same for both)")
    parser.add_argument("--reasoning", type=int, default=60, help="model reasoning tokens written per turn")
    parser.add_argument("--framing", type=int, default=25, help="tool-call/result framing tokens per turn")
    parser.add_argument("--turn-latency", type=float, default=4.0, help="seconds of model time per turn")
    parser.add_argument("--out", default=str(REPO / "bench" / "session_results.md"))
    parser.add_argument("--transcript", help="write both transcripts to this file")
    parser.add_argument("--help-tier", choices=["full", "brief"], default="full", help="which cheat sheet the agent gets")
    opts = parser.parse_args()

    tier = [] if opts.help_tier == "full" else ["brief"]
    cheat_sheet = subprocess.run([str(AGENTSH), "help", *tier], capture_output=True, text=True, check=True).stdout
    cheat_tokens = tokens(cheat_sheet)
    home = Path(tempfile.mkdtemp(prefix="agentsh-sim-home-"))
    env = {**os.environ, "AGENTSH_HOME": str(home)}
    for key in ("AGENTSH_ALLOW", "AGENTSH_DENY", "AGENTSH_FORMAT", "AGENTSH_SESSION", "AGENTSH_TRACE"):
        env.pop(key, None)

    runs = {}
    for shell, steps in (("bash", BASH_STEPS), ("agentsh", AGENTSH_STEPS)):
        ws = make_workspace()
        try:
            results = run_steps(ws, steps, shell, env)
            ok = verify(ws, env)
        finally:
            shutil.rmtree(ws, ignore_errors=True)
        base = opts.base + (cheat_tokens if shell == "agentsh" else 0)
        runs[shell] = {"results": results, "ok": ok, **account(results, base, opts.reasoning, opts.framing)}
    shutil.rmtree(home, ignore_errors=True)

    if opts.transcript:
        with open(opts.transcript, "w") as f:
            for shell, run in runs.items():
                f.write(f"\n########## {shell}\n")
                for i, r in enumerate(run["results"], 1):
                    f.write(f"\n=== turn {i} command\n{r['command']}\n=== output\n{r['output']}\n")

    b, a = runs["bash"], runs["agentsh"]

    def pct(x: float, y: float) -> str:
        return f"{(y - x) / x * 100:+.0f}%"

    wall_b = b["tool_seconds"] + b["turns"] * opts.turn_latency
    wall_a = a["tool_seconds"] + a["turns"] * opts.turn_latency
    rows = [
        ("task completed, all tests pass", "yes" if b["ok"] else "NO", "yes" if a["ok"] else "NO", ""),
        ("turns (tool calls)", b["turns"], a["turns"], pct(b["turns"], a["turns"])),
        ("tokens in tool outputs", b["tool_output_tokens"], a["tool_output_tokens"], pct(b["tool_output_tokens"], a["tool_output_tokens"])),
        ("tokens in commands written", b["command_tokens"], a["command_tokens"], pct(b["command_tokens"], a["command_tokens"])),
        ("tokens generated by the model", b["generated_tokens"], a["generated_tokens"], pct(b["generated_tokens"], a["generated_tokens"])),
        ("final context size", b["final_context"], a["final_context"], pct(b["final_context"], a["final_context"])),
        ("input tokens processed over the session", b["cumulative_input"], a["cumulative_input"], pct(b["cumulative_input"], a["cumulative_input"])),
        ("tool execution time (s)", f"{b['tool_seconds']:.1f}", f"{a['tool_seconds']:.1f}", pct(b["tool_seconds"], a["tool_seconds"])),
        (f"wall time incl. {opts.turn_latency:.0f}s/turn model time (s)", f"{wall_b:.0f}", f"{wall_a:.0f}", pct(wall_b, wall_a)),
    ]
    lines = [
        f"# Coding session: bash vs agentsh ({sys.platform})",
        "",
        "Task: add a `reverse` command to agentsh (explore, implement with a compile error, fix, add a test, run the suite, review).",
        f"Assumptions: base prompt {opts.base} tokens (+{cheat_tokens} cheat sheet for agentsh), {opts.reasoning} reasoning + {opts.framing} framing tokens per turn, tokens = cl100k_base estimate.",
        "",
        "| | bash | agentsh | change |",
        "|---|---|---|---|",
        *[f"| {name} | {x} | {y} | {d} |" for name, x, y, d in rows],
    ]
    report = "\n".join(lines) + "\n"
    Path(opts.out).write_text(report)
    print(report)


if __name__ == "__main__":
    main()
