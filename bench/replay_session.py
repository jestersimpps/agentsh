"""Replay a recorded Codex coding session in bash and in agentsh and compare what the agent pays.

The session's own shell commands and apply_patch edits are replayed in order against two fresh
clones of the repo at the session's starting commit: one runs every command as recorded (bash), the
other runs a mechanical agentsh translation of it. Edits are applied identically to both, so both
arms see the same files at every step. Commands that could reach outside the clone (push, network,
release scripts) are refused.

Translation is 1:1 and literal: one agentsh call per recorded call, the same files, ranges and
searches. agentsh's native commands are used where a rule matches (read, grep, find, git.*,
check); anything else runs through `exec "bash" "-c" ...`, so agentsh never gets a call the
recording didn't make. That understates agentsh (no batching, no outline/symbol reads) but keeps
the comparison honest.

Reported: tokens the agent writes (commands) and reads (outputs, capped at the call's recorded
max_output_tokens like Codex does), and input tokens processed over the session, where every turn
re-reads the conversation so far. Edits and the session's visible messages count the same in both
arms; hidden reasoning isn't in the transcript and is left out of both.

The report is aggregates only: per-step categories and token counts, no command text or output.

usage: <python with tiktoken> bench/replay_session.py SESSION.jsonl --repo PATH [--out FILE]
"""

import argparse
import collections
import json
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import tiktoken

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from mine_sessions import split_outside_quotes  # noqa: E402

AGENTSH = HERE.parent / "target" / "release" / "agentsh"
ENC = tiktoken.get_encoding("cl100k_base")
BASE_PROMPT = 2500
REFUSE = re.compile(r"\bgit\s+push\b|\bnpm\s+(run\s+)?release\b|\bcurl\b|\bwget\b|\bgh\s|\bssh\b|\bscp\b|\bnpm\s+(i|install|ci|publish)\b|\bsudo\b")
SANDBOX = """(version 1)(allow default)(deny network*)(allow network* (local unix))
(deny file-write*)
(allow file-write* (subpath "{root}") (subpath "/private/tmp") (subpath "/private/var/folders") (subpath "/dev"))"""


def tokens(text: str) -> int:
    return len(ENC.encode(text, disallowed_special=()))


# ---------------------------------------------------------------- reading the session

def js_string(s: str, start: int) -> str | None:
    """The JSON/JS double-quoted string literal starting at s[start]."""
    m = re.compile(r'"(?:[^"\\]|\\.)*"', re.S).match(s, start)
    if not m:
        return None
    try:
        return json.loads(m.group(0))
    except ValueError:
        return None


def events(path: Path) -> list[dict]:
    """Ordered steps: exec (shell command), patch (apply_patch), poll (write_stdin), say (messages)."""
    out, pending = [], []
    for line in open(path, errors="replace"):
        o = json.loads(line)
        p = o.get("payload") or {}
        t = p.get("type")
        if t == "message" and p.get("role") in ("user", "assistant"):
            text = "".join(c.get("text", "") for c in p.get("content") or [] if isinstance(c, dict))
            if text and not text.startswith("<"):
                out.append({"kind": "say", "tokens": tokens(text), "ts": o.get("timestamp", "")})
        elif t == "custom_tool_call" and p.get("name") == "exec":
            s = p.get("input") or ""
            ev = None
            m = re.search(r"exec_command\((\{.*?\})\)", s, re.S)
            if m:
                try:
                    a = json.loads(m.group(1))
                    ev = {"kind": "exec", "cmd": a["cmd"], "cap": a.get("max_output_tokens") or 10000}
                except (ValueError, KeyError):
                    ev = None
            elif "apply_patch" in s:
                i = s.find('"', s.find("patch"))
                body = js_string(s, i) if i >= 0 else None
                if body:
                    ev = {"kind": "patch", "patch": body, "input_tokens": tokens(s)}
            elif "write_stdin" in s:
                ev = {"kind": "poll", "input_tokens": tokens(s)}
            if ev:
                ev["ts"] = o.get("timestamp", "")
                out.append(ev)
            pending.append(ev)
        elif t == "custom_tool_call_output" and pending:
            ev = pending.pop(0)
            o2 = p.get("output")
            text = "".join(x.get("text", "") for x in o2 if isinstance(x, dict)) if isinstance(o2, list) else str(o2)
            if ev:
                ev["recorded"] = text
                ev["recorded_ok"] = "Script failed" not in text[:40]
    return out


def start_commit(path: Path) -> str:
    for line in open(path, errors="replace"):
        o = json.loads(line)
        if o.get("type") == "session_meta":
            return o["payload"]["git"]["commit_hash"]
    raise SystemExit("no session_meta with a git commit in this session")


# ---------------------------------------------------------------- apply_patch

def apply_patch(root: Path, patch: str, prefix: str, writer=None) -> bool:
    """Apply a Codex apply_patch body. All-or-nothing; returns whether it applied."""
    lines = patch.split("\n")
    files: dict[Path, list[str] | None] = {}
    i = 0

    def resolve(p: str) -> Path:
        p = p.strip()
        if p.startswith(prefix):
            p = p[len(prefix):].lstrip("/")
        elif p.startswith(("/private/tmp/", "/tmp/")):
            return root.parent / "tmp" / p.split("tmp/", 1)[1]
        elif p.startswith("/"):
            raise ValueError("edit outside the repo")
        return root / p

    def current(path: Path) -> list[str]:
        if path not in files:
            files[path] = path.read_text().split("\n") if path.exists() else None
        if files[path] is None:
            raise ValueError("missing file")
        return files[path]

    try:
        while i < len(lines):
            line = lines[i]
            if line.startswith("*** Add File: "):
                path = resolve(line[len("*** Add File: "):])
                body = []
                i += 1
                while i < len(lines) and not lines[i].startswith("***"):
                    body.append(lines[i][1:] if lines[i].startswith("+") else lines[i])
                    i += 1
                files[path] = body + [""] if body and body[-1] != "" else body
                continue
            if line.startswith("*** Delete File: "):
                files[resolve(line[len("*** Delete File: "):])] = None
                i += 1
                continue
            if line.startswith("*** Update File: "):
                path = resolve(line[len("*** Update File: "):])
                text = current(path)
                i += 1
                move_to = None
                if i < len(lines) and lines[i].startswith("*** Move to: "):
                    move_to = resolve(lines[i][len("*** Move to: "):])
                    i += 1
                cursor = 0
                while i < len(lines) and (not lines[i].startswith("*** ") or lines[i] == "*** End of File"):
                    if lines[i] == "*** End of File":
                        i += 1
                        continue
                    header = lines[i][2:].strip() if lines[i].startswith("@@") else ""
                    if lines[i].startswith("@@"):
                        i += 1
                    old, new = [], []
                    while i < len(lines) and not lines[i].startswith("@@") and not lines[i].startswith("*** "):
                        l = lines[i]
                        tag, body = (l[:1], l[1:]) if l else (" ", "")
                        if tag in (" ", "-"):
                            old.append(body)
                        if tag in (" ", "+"):
                            new.append(body)
                        i += 1
                    if header:
                        for j in range(cursor, len(text)):
                            if text[j].strip() == header:
                                cursor = j + 1
                                break
                    at = find_block(text, old, cursor)
                    if at is None:
                        at = find_block(text, old, 0)
                    if at is None:
                        raise ValueError("hunk doesn't match")
                    text[at:at + len(old)] = new
                    cursor = at + len(new)
                files[path] = text
                if move_to:
                    files[move_to] = text
                    files[path] = None
                continue
            i += 1
    except (ValueError, OSError):
        return False
    if writer:
        return writer(files)
    for path, text in files.items():
        if text is None:
            if path.exists():
                path.unlink()
        else:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("\n".join(text))
    return True


def write_via_agentsh(root: Path, files: dict, env: dict) -> bool:
    """Apply an edit the way an agent using agentsh would: with `write`, so agentsh knows the agent
    wrote it. Files outside the clone (scratch patches in /tmp) are written directly."""
    stmts = []
    for path, text in files.items():
        try:
            rel = path.relative_to(root)
        except ValueError:
            if text is None:
                path.unlink(missing_ok=True)
            else:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("\n".join(text))
            continue
        if text is None:
            stmts.append(f"rm {q(str(rel))}")
        else:
            # Codex's apply_patch doesn't check for changes made by others; agentsh's `write` does.
            # `read | count` marks the file current without showing (or counting as seen) its text.
            if path.exists():
                stmts.append(f"read {q(str(rel))} full:true | count")
            stmts.append(f"write {q(str(rel))} {q(chr(10).join(text))} overwrite:true")
    if not stmts:
        return True
    prog = root.parent / f"edit-{root.name}.agentsh"
    prog.write_text("\n".join(stmts))
    r = subprocess.run([str(AGENTSH), "--session", "replay", "run", "--file", str(prog)], cwd=root, capture_output=True, text=True, env=env)
    prog.unlink()
    if r.returncode != 0:
        print("agentsh write failed:", (r.stdout + r.stderr).strip().splitlines()[0][:160], file=sys.stderr)
    return r.returncode == 0


def find_block(text: list[str], block: list[str], start: int) -> int | None:
    if not block:
        return len(text) - 1 if text and text[-1] == "" else len(text)
    for norm in (lambda s: s, str.rstrip, str.strip):
        want = [norm(b) for b in block]
        for j in range(start, len(text) - len(block) + 1):
            if all(norm(text[j + k]) == want[k] for k in range(len(block))):
                return j
    return None


# ---------------------------------------------------------------- bash -> agentsh

def q(s: str) -> str:
    if not re.search(r'["\\$]', s):
        return f'"{s}"'
    hashes = "#"
    while f'"{hashes}' in s:
        hashes += "#"
    return f'r{hashes}"{s}"{hashes}'


def qlist(items: list[str]) -> str:
    return q(items[0]) if len(items) == 1 else "[" + ", ".join(q(x) for x in items) + "]"


def limit_of(stage: str) -> int | None:
    """N for `head -N`, `head -n N` or `sed -n '1,Np'` used as a pipe filter."""
    w = shlex.split(stage)
    if w and w[0] == "head":
        if len(w) == 2 and re.fullmatch(r"-\d+", w[1]):
            return int(w[1][1:])
        if len(w) == 3 and w[1] == "-n":
            return int(w[2])
        if len(w) == 1:
            return 10
    if len(w) == 3 and w[:2] == ["sed", "-n"]:
        m = re.fullmatch(r"1,(\d+)p", w[2])
        if m:
            return int(m.group(1))
    return None


def sed_range(arg: str) -> tuple[int, int] | None:
    m = re.fullmatch(r"(\d+),(\d+)p", arg)
    return (int(m.group(1)), int(m.group(2))) if m else None


def translate_stage(words: list[str], limit: int | None) -> str | None:
    """agentsh for one simple command (already split, redirections removed), or None."""
    if not words:
        return None
    cmd, args = words[0], words[1:]
    if cmd == "pwd" and not args:
        return "pwd"
    if cmd == "true" and not args:
        return ""
    if cmd == "cat" and args and all(not a.startswith(("-", "/")) for a in args):
        return f"read {' '.join(q(a) for a in args)}"
    if cmd == "sed" and len(args) == 3 and args[0] == "-n" and sed_range(args[1]) and not args[2].startswith("/"):
        a, b = sed_range(args[1])
        if limit:
            b = min(b, a + limit - 1)
        return f"read {q(args[2])} from:{a} to:{b}"
    if cmd == "rg":
        return translate_rg(args, limit)
    if cmd == "git" and args:
        return translate_git(args, limit)
    if cmd == "npm" and args:
        if args[0] == "test":
            rest = args[1:]
            if rest[:1] == ["--"]:
                rest = rest[1:]
            return f"check args:{q(' '.join(rest))}" if rest else "check"
        if args[0] == "run" and len(args) == 2:
            return 'check "build"' if args[1] == "build" else f"js.run {q(args[1])}"
    return None


def translate_rg(args: list[str], limit: int | None) -> str | None:
    opts, pos, globs, i = {}, [], [], 0
    while i < len(args):
        a = args[i]
        if a in ("-n", "--line-number", "--no-heading", "-S", "--smart-case"):
            pass
        elif a in ("-i", "--ignore-case"):
            opts["ignore_case"] = "true"
        elif a in ("-F", "--fixed-strings"):
            opts["fixed"] = "true"
        elif a in ("-l", "--files-with-matches"):
            opts["files"] = "true"
        elif a == "--files":
            opts["--files"] = True
        elif a in ("-g", "--glob"):
            globs.append(args[i + 1])
            i += 1
        elif a in ("-C", "-A", "-B"):
            n = args[i + 1]
            if a in ("-C", "-B"):
                opts["before"] = n
            if a in ("-C", "-A"):
                opts["after"] = n
            i += 1
        elif a.startswith("-"):
            return None
        else:
            pos.append(a)
        i += 1
    positive = [g for g in globs if not g.startswith("!")]
    negative = [g for g in globs if g.startswith("!")]
    if any(g not in ("!node_modules", "!dist", "!.git", "!out") for g in negative):
        return None
    if opts.pop("--files", False):
        if len(pos) > 1 or len(positive) > 1:
            return None
        call = "find" + (f" {q(pos[0])}" if pos else "") + (f" name:{q(positive[0])}" if positive else "") + ' type:"file"'
        return call + (f" | take {limit}" if limit else "")
    if not pos or positive:
        return None
    pattern, paths = pos[0], pos[1:]
    call = f"grep {q(pattern)}" + (f" in:{qlist(paths)}" if paths else "")
    for k, v in opts.items():
        call += f" {k}:{v}"
    return call + (f" max:{limit}" if limit else "")


def translate_git(args: list[str], limit: int | None) -> str | None:
    sub, rest = args[0], args[1:]
    cut = f" | excerpt to:{limit}" if limit else ""
    if sub == "status" and rest in ([], ["--short"], ["-s"], ["--porcelain"]):
        return "git.status"
    if sub == "diff":
        opts, paths, i = [], [], 0
        while i < len(rest):
            a = rest[i]
            if a in ("--cached", "--staged"):
                opts.append("staged:true")
            elif a in ("--stat", "--name-only"):
                opts.append("stat:true")
            elif a == "--":
                paths = rest[i + 1:]
                break
            else:
                return None
            i += 1
        stat = "stat:true" in opts
        return "git.diff" + (f" {qlist(paths)}" if paths else "") + "".join(f" {o}" for o in opts) + ("" if stat else cut)
    if sub == "show" and len(rest) == 1 and ":" in rest[0]:
        rev, path = rest[0].split(":", 1)
        return f"git.show {q(rev)} {q(path)}" + (f" to:{limit}" if limit else "")
    if sub == "log" and rest in (["-1", "--oneline"], ["--oneline", "-1"]):
        return "git.log n:1"
    if sub in ("add", "commit", "restore", "apply") and all("\n" not in a for a in rest):
        return "exec " + " ".join(q(a) for a in ["git", sub, *rest])
    return None


REDIRECT = re.compile(r"\s+2>/dev/null|\s+2>&1|\s+>/dev/null")


def translate(cmd: str, prefix: str) -> tuple[str, int, int]:
    """agentsh program for a recorded command; (program, native statements, exec fallbacks)."""
    cmd = cmd.replace(prefix + "/", "").replace(prefix, ".")
    pipelines = split_outside_quotes(cmd)
    stmts, native, fallback = [], 0, 0
    k = 0
    while k < len(pipelines):
        stages = pipelines[k]
        tolerant = k + 1 < len(pipelines) and pipelines[k + 1] == ["true"]
        raw = " | ".join(stages)
        clean = [REDIRECT.sub("", s) for s in stages]
        out = None
        try:
            limit = limit_of(clean[1]) if len(clean) == 2 else None
            if len(clean) == 1 or limit:
                out = translate_stage(shlex.split(clean[0]), limit)
            elif len(clean) == 2 and clean[0].startswith("git show ") and sed_range(shlex.split(clean[1])[-1] if clean[1].startswith("sed -n") else ""):
                a, b = sed_range(shlex.split(clean[1])[-1])
                base = translate_git(shlex.split(clean[0])[1:], None)
                out = base and f"{base} from:{a} to:{b}"
            elif len(clean) == 2 and clean[0].startswith("nl -ba ") and clean[1].startswith("sed -n"):
                r = sed_range(shlex.split(clean[1])[-1])
                f = shlex.split(clean[0])[-1]
                out = r and f"read {q(f)} from:{r[0]} to:{r[1]} numbers:true"
        except (ValueError, IndexError):
            out = None
        if out is None:
            out = f'exec "bash" "-c" {q(raw)} ok:[0, 1]' if tolerant or raw.startswith("rg ") else f'exec "bash" "-c" {q(raw)}'
            fallback += 1
        elif out:
            native += 1
        if out and tolerant and not out.startswith("exec"):
            out = f"try {{ {out} }} catch $e {{ }}"
        if out:
            stmts.append(out)
        k += 2 if tolerant else 1
    return "\n".join(stmts), native, fallback


# ---------------------------------------------------------------- running

def clone(repo: Path, commit: str, dest: Path) -> None:
    subprocess.run(["git", "clone", "-q", "--no-checkout", str(repo), str(dest)], check=True)
    subprocess.run(["git", "-C", str(dest), "remote", "remove", "origin"], check=True)
    subprocess.run(["git", "-C", str(dest), "checkout", "-q", commit], check=True)
    for k, v in (("user.name", "replay"), ("user.email", "replay@localhost"), ("commit.gpgsign", "false")):
        subprocess.run(["git", "-C", str(dest), "config", k, v], check=True)
    if (repo / "node_modules").is_dir():
        subprocess.run(["cp", "-cR", str(repo / "node_modules"), str(dest / "node_modules")], check=True)


PREFIX = re.compile(r"^(?:[\w./@-]+:\d+[:-]|\s*\d+[\t:]\s?)")


def norm(line: str, diff: bool) -> str:
    line = PREFIX.sub("", line)
    if diff and line[:1] in "+- " and not line.startswith(("+++", "---")):
        line = line[1:]
    return line.strip()


def duplicated(text: str, seen: set[str], diff: bool = False) -> int:
    """Tokens of lines (4+ characters) that are already in the context, then remember the rest."""
    dup = 0
    for line in text.split("\n"):
        key = norm(line, diff)
        if len(key) < 4:
            continue
        if key in seen:
            dup += tokens(line + "\n")
        else:
            seen.add(key)
    return dup


IDENT = re.compile(r"[A-Za-z_$][A-Za-z0-9_$]{4,}")


def hindsight(trail) -> None:
    """For each output, how much the agent demonstrably used later.

    edited: lines that later appear as context or removed lines in one of its edits (it needed to
    know them to change the code). referenced: other lines naming an identifier it later wrote in an
    edit or typed in a command. The rest isn't proven waste (reading code to rule things out is
    work too), but nothing the agent did afterwards points at it.
    """
    # Identifiers on many lines (keywords, `string`, `props`) say nothing about what was used.
    df, lines_total = collections.Counter(), 0
    for kind, item in trail:
        if kind == "out":
            for line in item[1].split("\n"):
                lines_total += 1
                df.update(set(IDENT.findall(line)))
    common = {w for w, n in df.items() if n > max(5, lines_total * 0.002)}
    for i, (kind, item) in enumerate(trail):
        if kind != "out":
            continue
        step, text, diff = item
        later_old, later_new, later_cmd = set(), set(), set()
        for k2, it2 in trail[i + 1:]:
            if k2 == "patch":
                for l in it2.split("\n"):
                    if l[:1] in (" ", "-"):
                        later_old.add(l[1:].strip())
                    if l[:1] == "+":
                        later_new.update(IDENT.findall(l))
            elif k2 == "cmd":
                later_cmd.update(IDENT.findall(it2))
        used_idents = (later_new | later_cmd) - common
        edited = referenced = 0
        for line in text.split("\n"):
            key = norm(line, diff)
            if len(key) < 4:
                continue
            t = tokens(line + "\n")
            if key in later_old:
                edited += t
            elif any(w in used_idents for w in IDENT.findall(key)):
                referenced += t
        scale = min(1.0, step["bash_out"] / max(tokens(text), 1))
        step["bash_edited"], step["bash_referenced"] = round(edited * scale), round(referenced * scale)


def run_bash(root: Path, cmd: str, env: dict) -> tuple[str, float]:
    profile = SANDBOX.format(root=root.resolve())
    t = time.time()
    r = subprocess.run(["sandbox-exec", "-p", profile, "bash", "-c", cmd], cwd=root, capture_output=True, text=True, env=env, timeout=900)
    return f"{r.stdout}{r.stderr}" + (f"\nexit code {r.returncode}" if r.returncode else ""), time.time() - t


def run_agentsh(root: Path, program: str, env: dict) -> tuple[str, float]:
    t = time.time()
    r = subprocess.run([str(AGENTSH), "--allow", "exec", "--session", "replay", "run", program], cwd=root, capture_output=True, text=True, env=env, timeout=900)
    return r.stdout + r.stderr, time.time() - t


def classify(cmd: str) -> str:
    first = cmd.strip().split()[0] if cmd.strip() else ""
    if re.search(r"\bnpm\s+(test|run)\b", cmd):
        return "test/build"
    if re.search(r"\bgit\s+(add|commit|restore|apply)\b", cmd):
        return "git write"
    if re.search(r"\bgit\s+(diff|status|show|log)\b", cmd) and first == "git":
        return "git read"
    if re.search(r"^\s*rg\b", cmd) and not re.search(r"\bsed -n\b", cmd):
        return "search"
    return "read/explore"


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("session", type=Path)
    ap.add_argument("--repo", type=Path, required=True, help="local clone of the session's repo")
    ap.add_argument("--out", type=Path, default=HERE / "replay_results.md")
    ap.add_argument("--concurrent", type=Path, action="append", default=[],
                    help="another session that edited the same checkout at the same time; its edits are applied (not measured)")
    ap.add_argument("--title", default="a recorded Codex coding session", help="one line describing the session's task")
    ap.add_argument("--keep", action="store_true", help="keep the replay clones")
    ap.add_argument("--show", action="store_true", help="print each translation (local debugging; shows command text)")
    args = ap.parse_args()
    repo = args.repo.resolve()
    evs = events(args.session)
    for other in args.concurrent:
        evs += [dict(e, kind="foreign") for e in events(other) if e["kind"] == "patch" and e.get("recorded_ok")]
    evs.sort(key=lambda e: e.get("ts") or "")
    commit = start_commit(args.session)
    prefix = str(repo)

    work = Path(tempfile.mkdtemp(prefix="agentsh-replay-"))
    roots = {"bash": work / "bash", "agentsh": work / "agentsh", "full": work / "full"}
    for r in roots.values():
        clone(repo, commit, r)
    (work / "tmp").mkdir()
    env = {k: v for k, v in os.environ.items() if not k.startswith("AGENTSH_")}
    env.update(AGENTSH_HOME=str(work / "agentsh-home"), GIT_TERMINAL_PROMPT="0", CI="1", NO_COLOR="1", FORCE_COLOR="0")
    # The "full" arm turns agentsh's output shortening off, so it never shows less than bash did.
    full_env = dict(env, AGENTSH_HOME=str(work / "full-home"))
    (work / "full-home").mkdir()
    (work / "full-home" / "config.toml").write_text("[limits]\nmax_items = 1000000\nmax_string_bytes = 100000000\n")

    arm_env = {"agentsh": env, "full": full_env}
    cheat = subprocess.run([str(AGENTSH), "help"], capture_output=True, text=True).stdout
    briefs_used: set[str] = set()
    steps, patch_mismatch, foreign_failed, agentsh_edit_failed = [], 0, 0, 0
    seen_lines: set[str] = set()
    trail: list[tuple[str, object]] = []  # ("out", (step, lines)) or ("patch"/"cmd", text), in order
    for ev in evs:
        if ev["kind"] == "say":
            steps.append({"kind": "say", "same": ev["tokens"]})
        elif ev["kind"] == "foreign":
            for r in roots.values():
                foreign_failed += not apply_patch(r, ev["patch"], prefix)
        elif ev["kind"] == "poll":
            steps.append({"kind": "poll", "bash_in": ev["input_tokens"], "bash_out": tokens(ev.get("recorded", ""))})
        elif ev["kind"] == "patch":
            duplicated(ev["patch"], seen_lines, diff=True)
            trail.append(("patch", ev["patch"]))
            applied = {}
            if ev.get("recorded_ok"):
                for arm, r in roots.items():
                    writer = None if arm == "bash" else (lambda files, r=r, e=arm_env[arm]: write_via_agentsh(r, files, e))
                    applied[arm] = apply_patch(r, ev["patch"], prefix, writer)
                patch_mismatch += not applied["bash"]
                agentsh_edit_failed += not (applied["agentsh"] and applied["full"])
            steps.append({"kind": "patch", "same": ev["input_tokens"] + tokens(ev.get("recorded", ""))})
        else:
            cmd = ev["cmd"]
            if REFUSE.search(cmd):
                steps.append({"kind": "refused"})
                continue
            program, native, fallback = translate(cmd, prefix)
            tmp = str(work / "tmp") + "/"
            cmd = cmd.replace("/private/tmp/", tmp)
            program = program.replace("/private/tmp/", tmp)
            bash_cmd = cmd.replace(prefix, str(roots["bash"]))
            b_out, b_s = run_bash(roots["bash"], bash_cmd, env)
            a_out, a_s = run_agentsh(roots["agentsh"], program, env) if program else ("", 0.0)
            f_out, f_s = run_agentsh(roots["full"], program, full_env) if program else ("", 0.0)
            for pkg in ("git", "js"):
                if re.search(rf"\b{pkg}\.\w+", program) or (pkg == "js" and re.search(r"^check\b", program, re.M)):
                    briefs_used.add(pkg)
            cap = ev["cap"]
            s = {
                "kind": "exec", "category": classify(cmd), "native": native, "fallback": fallback,
                "bash_in": tokens(cmd), "agentsh_in": tokens(program), "full_in": tokens(program),
                "full_out": min(tokens(f_out), cap), "full_s": f_s,
                "bash_out": min(tokens(b_out), cap), "agentsh_out": min(tokens(a_out), cap),
                "recorded_out": min(tokens(ev.get("recorded", "")), cap),
                "bash_s": b_s, "agentsh_s": a_s,
                "agentsh_error": bool(re.match(r"error \w+:", a_out.lstrip())) or "\nerror " in a_out,
                "agentsh_cut": len(re.findall(r"\[shortened|\[showing \d+ of \d+\]", a_out)),
                "unchanged": len(re.findall(r"unchanged since|already shown, unchanged|you.ve already seen", f_out)),
            }
            s["bash_dup"] = min(duplicated(b_out, seen_lines, diff="git" in cmd), s["bash_out"])
            trail.append(("cmd", cmd))
            trail.append(("out", (s, b_out, "git" in cmd)))
            steps.append(s)
            if args.show:
                print("=" * 80, f"\n[{len(steps)}] {s['category']} bash {s['bash_out']} / agentsh {s['agentsh_out']}\n--- bash\n{cmd}\n--- agentsh\n{program}\n--- agentsh output (head)\n{a_out[:600]}\n--- agentsh output (tail)\n{a_out[-700:] if s["agentsh_error"] else ""}")

    hindsight(trail)
    if agentsh_edit_failed:
        print(f"warning: {agentsh_edit_failed} edits failed to apply through agentsh write", file=sys.stderr)
    brief_tokens = sum(tokens(subprocess.run([str(AGENTSH), "help", p], capture_output=True, text=True).stdout) for p in sorted(briefs_used))
    final = {arm: subprocess.run(["git", "-C", str(r), "diff", "HEAD"], capture_output=True, text=True).stdout for arm, r in roots.items()}
    heads = {arm: subprocess.run(["git", "-C", str(r), "rev-parse", "HEAD^{tree}"], capture_output=True, text=True).stdout.strip() for arm, r in roots.items()}
    end_commit_tree = subprocess.run(["git", "-C", str(roots["bash"]), "log", "--all", "-1", "--format=%T", "HEAD"], capture_output=True, text=True).stdout.strip()
    report = summarize(steps, tokens(cheat), brief_tokens, sorted(briefs_used), patch_mismatch, foreign_failed, len(set(final.values())) == 1 and len(set(heads.values())) == 1, args.title)
    args.out.write_text(report)
    (args.out.with_suffix(".json")).write_text(json.dumps({"steps": steps, "cheat_sheet": tokens(cheat), "briefs": brief_tokens}, indent=1))
    print(report)
    if args.keep:
        print(f"clones kept in {work}")
    else:
        shutil.rmtree(work, ignore_errors=True)
    return 0


def summarize(steps, cheat, briefs, briefs_used, patch_mismatch, foreign_failed, same_end, name) -> str:
    ex = [s for s in steps if s["kind"] == "exec"]

    def total(k, rows=ex):
        return sum(s.get(k, 0) for s in rows)

    def processed(arm):
        ctx = BASE_PROMPT + (0 if arm == "bash" else cheat + briefs)
        done = 0
        for s in steps:
            if s["kind"] == "say":
                ctx += s["same"]
            elif s["kind"] == "patch":
                done += ctx
                ctx += s["same"]
            elif s["kind"] == "exec":
                done += ctx
                ctx += s[f"{arm}_in"] + s[f"{arm}_out"]
        return done, ctx

    def pct(a, b):
        return f"{(b - a) / a * 100:+.0f}%" if a else ""

    bp, bc = processed("bash")
    ap_, ac = processed("agentsh")
    fp, fc = processed("full")
    rows = [
        ("shell calls replayed", len(ex), len(ex), len(ex)),
        ("tokens written (commands)", total("bash_in"), total("agentsh_in"), total("full_in")),
        ("tokens read (command output)", total("bash_out"), total("agentsh_out"), total("full_out")),
        ("one-time: cheat sheet + briefings (" + ", ".join(briefs_used) + ")", 0, cheat + briefs, cheat + briefs),
        ("final context size", bc, ac, fc),
        ("input tokens processed over the session", bp, ap_, fp),
        ("tool time (s)", round(total("bash_s"), 1), round(total("agentsh_s"), 1), round(total("full_s"), 1)),
    ]
    out = [f"# Replaying a real coding session: bash vs agentsh\n",
           f"Session: {name}. Replayed from its starting commit with {sum(s['kind'] == 'patch' for s in steps)} edits "
           f"and {len(ex)} shell calls; {sum(s['kind'] == 'poll' for s in steps)} output polls and "
           f"{sum(s['kind'] == 'refused' for s in steps)} refused calls left out.\n",
           f"Model: base prompt {BASE_PROMPT} tokens; every turn re-reads the conversation; outputs capped at each "
           f"call's recorded max_output_tokens; edits and visible messages identical in both arms; tokens = cl100k_base. "
           f"\"agentsh, full output\" turns agentsh's output shortening off (it never shows less than bash).\n",
           "| | bash | agentsh | change | agentsh, full output | change |", "|---|---|---|---|---|---|"]
    for label, b, a, f in rows:
        out.append(f"| {label} | {b} | {a} | {pct(b, a) if b else ''} | {f} | {pct(b, f) if b else ''} |")
    out.append("")
    out.append("## By kind of call\n")
    out.append("| kind | calls | bash out | agentsh out | change | full output | change |")
    out.append("|---|---|---|---|---|---|---|")
    cats = sorted({s["category"] for s in ex})
    for c in cats:
        rs = [s for s in ex if s["category"] == c]
        b, a, f = total("bash_out", rs), total("agentsh_out", rs), total("full_out", rs)
        out.append(f"| {c} | {len(rs)} | {b} | {a} | {pct(b, a)} | {f} | {pct(b, f)} |")
    out.append("")
    nat, fb = total("native"), total("fallback")
    out.append(f"Translation: {nat} statements used native agentsh commands, {fb} fell back to `exec \"bash\" \"-c\"`. "
               f"agentsh errors in {sum(s['agentsh_error'] for s in ex)} calls; {total('unchanged')} notes left out lines or hunks "
               f"already seen; {total('agentsh_cut')} outputs shortened with a handle to the full value.")
    out.append(f"Already in context: {total('bash_dup')} of the {total('bash_out')} tokens bash printed "
               f"({total('bash_dup') / max(total('bash_out'), 1):.0%}) were lines the agent had already seen, in an earlier output or in its own edits.")
    ed, rf = total("bash_edited"), total("bash_referenced")
    out.append(f"Used afterwards: of the {total('bash_out')} tokens bash printed, {ed} ({ed / max(total('bash_out'), 1):.0%}) were lines "
               f"the agent later edited around, and {rf} ({rf / max(total('bash_out'), 1):.0%}) named something it later wrote or typed.")
    out.append(f"Edits that didn't re-apply: {patch_mismatch} (and {foreign_failed} from concurrent sessions). Both arms ended with the same tree and diff: {'yes' if same_end else 'no'}.")
    out.append(f"The recorded session's own outputs for these calls were {total('recorded_out')} tokens "
               f"(the replay's bash arm: {total('bash_out')}); they differ where a concurrent session had changed files.")
    return "\n".join(out) + "\n"


if __name__ == "__main__":
    sys.exit(main())
