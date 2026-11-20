#!/usr/bin/env python3
"""Mine agent transcripts (Claude Code and Codex JSONL) for the shell commands agents actually run.

Prints aggregate counts only: programs, subcommands, pipe targets, shell constructs and output
sizes. No command text is printed beyond the program and subcommand words, so secrets and file
contents in the transcripts stay out of the report.

    python3 -I bench/mine_sessions.py ~/.claude/projects ~/.codex/sessions [--exclude WORD ...]
"""

import argparse
import collections
import glob
import json
import os
import re
import shlex
import sys

SUBCOMMAND_PROGRAMS = {
    "git", "gh", "npm", "pnpm", "yarn", "bun", "npx", "cargo", "docker", "kubectl", "brew", "go",
    "swift", "uv", "pip", "pip3", "poetry", "make", "terraform", "gcloud", "aws", "az", "deno",
    "xcodebuild", "xcrun", "launchctl", "defaults", "security", "op", "tmux", "ollama", "rustup",
    "codex", "claude", "vercel", "supabase", "firebase", "fly", "railway", "heroku", "pnpx",
}
WRAPPERS = {"sudo", "timeout", "gtimeout", "time", "env", "command", "exec", "nohup", "caffeinate"}
SPLIT = re.compile(r"\s*(?:&&|\|\||;|\n)\s*")
CODEX_CMD = re.compile(r"""\bcmd\s*:\s*("(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|`(?:[^`\\]|\\.)*`)""", re.S)


def claude_calls(path):
    """(command, output_bytes) for every Bash tool call in a Claude Code transcript."""
    pending, out = {}, []
    for line in open(path, errors="replace"):
        try:
            o = json.loads(line)
        except ValueError:
            continue
        m = o.get("message")
        if not isinstance(m, dict) or not isinstance(m.get("content"), list):
            continue
        for b in m["content"]:
            if not isinstance(b, dict):
                continue
            if b.get("type") == "tool_use" and b.get("name") == "Bash":
                cmd = (b.get("input") or {}).get("command")
                if isinstance(cmd, str):
                    pending[b.get("id")] = cmd
            elif b.get("type") == "tool_result" and b.get("tool_use_id") in pending:
                content = b.get("content")
                size = len(content) if isinstance(content, str) else len(json.dumps(content))
                out.append((pending.pop(b["tool_use_id"]), size))
    out.extend((cmd, None) for cmd in pending.values())
    return out


def codex_calls(path):
    out = []
    for line in open(path, errors="replace"):
        try:
            p = json.loads(line).get("payload")
        except ValueError:
            continue
        if not isinstance(p, dict):
            continue
        if p.get("type") == "function_call" and p.get("name") == "exec_command":
            try:
                cmd = json.loads(p.get("arguments") or "{}").get("cmd")
            except ValueError:
                cmd = None
            if isinstance(cmd, str):
                out.append((cmd, None))
        elif p.get("type") == "custom_tool_call" and p.get("name") == "exec":
            for lit in CODEX_CMD.findall(p.get("input") or ""):
                body = lit[1:-1]
                try:
                    body = json.loads('"' + body.replace('"', '\\"') + '"') if lit[0] != '"' else json.loads(lit)
                except ValueError:
                    pass
                out.append((body, None))
    return out


HEREDOC = re.compile(r"<<-?\s*(['\"]?)(\w+)\1[^\n]*\n(.*?)\n\s*\2\b", re.S)


def split_outside_quotes(text):
    """Split into pipelines (on && || ; newline) and stages (on |), ignoring quoted text."""
    pipelines, stages, cur, quote, i = [], [], [], None, 0
    while i < len(text):
        c = text[i]
        if quote:
            cur.append(c)
            if c == "\\" and quote == '"' and i + 1 < len(text):
                cur.append(text[i + 1])
                i += 1
            elif c == quote:
                quote = None
        elif c in "'\"":
            quote = c
            cur.append(c)
        elif text.startswith("&&", i) or text.startswith("||", i) or c in ";\n":
            stages.append("".join(cur))
            pipelines.append(stages)
            stages, cur = [], []
            i += 1 if c in ";\n" else 2
            continue
        elif c == "|":
            stages.append("".join(cur))
            cur = []
        else:
            cur.append(c)
        i += 1
    stages.append("".join(cur))
    pipelines.append(stages)
    return [[s.strip() for s in p if s.strip()] for p in pipelines if any(s.strip() for s in p)]


def segments(command):
    """(program, words, piped_into) for each command in a shell line; heredoc bodies are dropped."""
    command = HEREDOC.sub(lambda m: m.group(0).split("\n", 1)[0], command)
    for stages in split_outside_quotes(command):
        for i, part in enumerate(stages):
            try:
                words = shlex.split(part, comments=True)
            except ValueError:
                words = part.split()
            while words and (re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", words[0]) or words[0] in WRAPPERS):
                words = words[1:]
                if words and words[0].startswith("-"):
                    words = words[1:]
            if not words or not re.match(r"^[\w./~+-]+$", words[0]):
                continue
            if words[0] in ("then", "do", "else", "fi", "done", "esac", "{", "}", "(", ")"):
                continue
            program = os.path.basename(words[0])
            yield program, words, i > 0


def subcommand(program, words):
    rest = [w for w in words[1:] if not w.startswith("-")]
    if program in ("python", "python3") and "-m" in words:
        i = words.index("-m")
        return f"{program} -m {words[i + 1]}" if i + 1 < len(words) else program
    if program in ("npx", "pnpx", "bunx") and rest:
        return f"{program} {rest[0]}"
    if program in SUBCOMMAND_PROGRAMS and rest:
        if program in ("gh", "docker", "kubectl", "gcloud", "aws", "az") and len(rest) > 1:
            return f"{program} {rest[0]} {rest[1]}"
        if program in ("npm", "pnpm", "yarn", "bun") and rest[0] == "run" and len(rest) > 1:
            return f"{program} run <script>"
        return f"{program} {rest[0]}"
    return program


def subcommand_of_first(cmd, program):
    for p, words, piped in segments(cmd):
        if p == program and not piped:
            return subcommand(p, words)
    return program


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("roots", nargs="+")
    ap.add_argument("--exclude", action="append", default=[], help="drop commands containing this word")
    ap.add_argument("--json", help="also write the aggregates here")
    args = ap.parse_args()
    exclude = [w.lower() for w in args.exclude]

    calls, sessions, excluded = [], 0, 0
    for root in args.roots:
        for path in glob.glob(os.path.join(os.path.expanduser(root), "**", "*.jsonl"), recursive=True):
            found = claude_calls(path) if "/.codex/" not in path else codex_calls(path)
            sessions += bool(found)
            for cmd, size in found:
                if any(w in cmd.lower() for w in exclude) or any(p == "wful" for p, _, _ in segments(cmd)):
                    excluded += 1
                    continue
                calls.append((cmd, size))

    programs, subs, pipes, constructs = (collections.Counter() for _ in range(4))
    out_bytes = collections.defaultdict(list)
    for cmd, size in calls:
        heads = []
        for program, words, piped in segments(cmd):
            if piped:
                pipes[program] += 1
                continue
            programs[program] += 1
            subs[subcommand(program, words)] += 1
            heads.append(program)
        for name, pattern in {
            "pipe": r"(?<![|])\|(?![|])", "&&/; chain": r"&&|;", "2>&1": r"2>&1", "redirect >": r"(?<![2&])>(?!&)",
            "heredoc": r"<<", "for/while loop": r"\b(for|while)\b.*\bdo\b", "subshell $(": r"\$\(",
            "background &": r"[^&]&\s*$", "sleep": r"\bsleep\b", "cd": r"(^|&&|;)\s*cd\b",
        }.items():
            if re.search(pattern, cmd, re.S):
                constructs[name] += 1
        work = [h for h in heads if h not in ("cd", "echo", "printf", "set", "export", "true", "source")]
        if size is not None and work:
            out_bytes[subcommand_of_first(cmd, work[0])].append(size)

    def table(title, counter, n):
        print(f"\n## {title}")
        for k, v in counter.most_common(n):
            print(f"{v:7d}  {k}")

    print(f"sessions with shell calls: {sessions}; shell calls: {len(calls)} (excluded {excluded})")
    table("programs (first word of each command segment)", programs, 45)
    table("program + subcommand", subs, 90)
    table("piped into", pipes, 20)
    table("shell constructs (calls using them)", constructs, 12)
    print("\n## output size by program (Claude Code calls with results)")
    rows = sorted(((sum(v), len(v), k) for k, v in out_bytes.items() if len(v) >= 15), reverse=True)[:30]
    for total, n, k in rows:
        print(f"{total / 4 / 1000:8.0f}k tok  {n:6d} calls  {total / n / 4:7.0f} tok/call  {k}")
    if args.json:
        json.dump({"programs": programs, "subcommands": subs, "pipes": pipes, "constructs": constructs},
                  open(args.json, "w"), indent=1)


if __name__ == "__main__":
    sys.exit(main())
