# agentsh

A shell for AI agents. One small binary that agents call instead of bash: a regular command language, one result per call (readable text, or a JSON envelope), and safety the agent can't switch off.

```console
$ agentsh run 'find "src" name:"*.rs" | where size > 10kb | sort size desc:true | take 3 | pick path size'
path	size
src/interp.rs	24597
src/fs.rs	20095
src/parser.rs	19677
```

## Why not bash

| Bash for agents | agentsh |
|---|---|
| Pagers, prompts and editors hang the agent | Nothing ever waits for input; every `exec` has a timeout and kills its process group |
| Text meant for eyes, scraped with regexes | Commands return typed values (tables, records, text) |
| `-r` means recursive here and reverse there | One call shape: `command positional key:value` |
| A command flood eats the context window | Lists and text are capped; the full result stays available as `$r3` |
| `rm -rf` with no way back | Every file change is journaled; `agentsh undo u_12` reverts a run |
| Half-applied scripts | A failing run rolls back all its file changes. `edit …; exec "cargo test"` keeps the edit only if tests pass, or add `keep:true` to keep it and fix it with a small edit |
| Prose errors | `{"code": "not_found", "hint": "did you mean \"src/limits.rs\"?"}` |
| Overwrites a file someone else just changed | Edits fail with `stale` if the file changed since the agent read it |
| Secrets end up in the transcript | `secret("NAME")` and secret-looking env vars are redacted from all output |
| Full user permissions | Capabilities (`read write delete exec net`) and an OS sandbox for `exec` |
| `ls`, `cat`, `grep` round trips to understand code | `map` (project tree), `outline` (declarations), `read … symbol:"name"` (one function), `refs` (definitions and uses) |
| Raw test logs | `check` runs the project's tests and returns the failing tests with their messages |
| `cmd &` and hoping | `start "npm run dev" until:"ready"`, `jobs`, `logs`, `stop` |

## Benchmark

`bench/compare.py` runs 17 common agent tasks the way a competent agent would write them in each shell ([macOS](bench/results.md), [Linux](bench/results-linux.md)):

| | bash | agentsh (text) | agentsh (json) |
|---|---|---|---|
| tool calls | 30 | **22** (−27%) | 22 |
| tokens in + out, macOS | 5,292 | **3,938** (−26%) | 4,911 (−7%) |
| tokens in + out, Linux | 5,219 | **3,867** (−26%) | 4,839 (−7%) |
| tool execution time, macOS | 1,264 ms | **480 ms** | |
| tool execution time, Linux | 580 ms | 543 ms | |

Tool execution time is only the time the commands themselves run; it excludes the model's thinking between calls, which dominates real sessions. For overall time, the session simulation below is the better guide: fewer turns matter far more than faster tools.

Where the savings come from: fewer round trips (pipelines, several statements per call, `grep ... after:N`, edit-and-test in one run, read-only typo correction), reading only what's needed (`read … symbol:`, `outline`, `refs`, `map`), not re-sending what the agent already has (unchanged files become a one-line note, changed files a diff, repeated results a note), and compact output (tab tables, relative times, constant columns printed once, failure digests, short edit confirmations).

### A full coding session

`bench/session_sim.py` runs a whole task in both shells against fresh copies of this repo: explore, add a `reverse` command with a realistic compile error, fix it, add a conformance case, run the suite, review. Both must end with all tests passing. Each turn the model re-reads the conversation so far ([full cheat sheet](bench/session_results.md), [brief cheat sheet](bench/session_results_brief.md)):

| | bash | agentsh, full help | agentsh, brief help |
|---|---|---|---|
| turns | 13 | **8** (−38%) | **8** (−38%) |
| tokens the model generates | 1,296 | **931** (−28%) | **931** (−28%) |
| input tokens processed over the session | 50,403 | 38,083 (−24%) | **33,426** (−34%) |
| final context | 5,710 | 6,122 (+7%) | **5,540** (−3%) |
| estimated wall time (tools + 4 s of model time per turn) | 63 s | **42 s** (−33%) | **43 s** (−32%) |

## Install

```sh
cargo install --path crates/cli
```

## Use it from an agent

Give the agent a cheat sheet and the binary:

```sh
agentsh help            # full cheat sheet, ~1,070 tokens
agentsh help brief      # syntax + the everyday commands, ~490 tokens
agentsh help edit       # one command in detail
```

Put the cheat sheet in the system prompt. It contains nothing machine- or time-specific, so it stays byte-identical across sessions and is served from the prompt cache.

Then every action is one call:

```sh
agentsh run 'grep "TODO" in:"src" | pick .path .line'
agentsh run --file plan.ash          # or a program from a file
agentsh run <<'EOF'                  # or multi-line from stdin
outline "src/main.rs"
read "src/main.rs" symbol:"parse_args"
refs "parse_args"
EOF
```

Every top-level statement's result is shown, so independent questions fit in one call. `let x = ...` stores a value outside the context and prints a one-line summary; long results are cut and kept whole as `$rN`.

Output is compact text by default (raw file contents, tables with one header row, `error <code>:` lines with hints). `--format json` (or `AGENTSH_FORMAT=json`) prints the full JSON envelope for programs.

Session commands: `agentsh undo [u_N]`, `changes`, `commit`, `discard`, `checkpoint NAME`, `rollback NAME`. Sessions (`--session ID`) keep the current folder, variables, result handles and the undo journal between calls. Give each agent conversation its own session id: in a named session, re-reading an unchanged file returns a one-line note, re-reading a changed file returns a diff against the version the agent saw, an identical repeated result returns a note (the value stays in `$_`), and generic hints are shown once.

## The JSON envelope

```json
{
  "ok": false,
  "error": {"code": "ambiguous_match", "message": "`old` matches 19 places in `src/limits.rs`",
            "hint": "include more surrounding text in old to pick one place, or pass all:true"},
  "ms": 8,
  "tokens_est": 77
}
```

Fields: `ok`, `outputs` (results of earlier statements), `label`, `value`, `shown`/`total`/`handle`/`truncated` (when shortened), `printed`, `effects`, `undo`, `warnings`, `error {code, message, arg, value, hint, command, span, rolled_back, kept, restored}`, `session`, `ms`, `tokens_est`. `agentsh schema` prints the JSON Schema; error codes are listed in [spec/language.md](spec/language.md).

## Safety model

- **Workspace**: the nearest folder with `.agentsh.toml` or `.git` (or `--root`). Built-in commands can't reach outside it, symlinks included.
- **Capabilities**: `read`, `write` and `delete` are on by default because everything they do can be undone. `exec` and `net` must be granted by a human (`--allow exec`, `AGENTSH_ALLOW`, or `.agentsh.toml`). Agents can't edit `.agentsh.toml`.
- **Sandbox for `exec`**: macOS uses Seatbelt (applied in-process, ~10 ms per call) and Linux uses Landlock. Writes are only allowed inside the workspace and temp folders. Without `net`, remote network access is blocked; on macOS localhost stays open so dev servers and their tests work. Linux Landlock filters by port, not host, so there a dev server (or anything using localhost) needs `--allow net`. `env` shows which sandbox is active. Where the OS can't sandbox, it reports `none (...)` instead of pretending.
- **Background jobs**: `start "npm run dev" until:"ready"` is the only way to leave a process running after a call. It needs `exec`, runs in the same sandbox, logs to the session folder, and is listed with `jobs`, read with `logs` and ended with `stop`.
- **Undo**: changes made by built-in commands are journaled with backups. Changes made by `exec`'d programs are not; use git for those.

## Configuration

Layered: defaults → user `config.toml` → project `.agentsh.toml` → `AGENTSH_*` env vars → flags. The user file lives in `~/.config/agentsh/` on Linux, `~/Library/Application Support/dev.agentsh.agentsh/` on macOS, or `$AGENTSH_HOME/`. Session state (undo journal, handles) is kept in the matching per-user data folder, never in the project.

```toml
[capabilities]
allow = ["read", "write", "delete", "exec"]

[limits]
max_items = 50             # rows per result before shortening
max_string_bytes = 24000   # text per result before smart truncation
exec_timeout_secs = 60

[sandbox]
backend = "auto"           # auto | none | seatbelt | landlock
writable = ["~/.cache"]    # extra writable paths for exec'd programs

[workspace]
roots = ["../shared-lib"]  # extra folders commands may touch

[trace]
enabled = true             # append every call to a JSONL trace (secrets redacted)

[secrets]
dotenv = ".env"
redact_env = ["*_KEY", "*_TOKEN", "*_SECRET", "*PASSWORD*"]
[secrets.commands]
GITHUB_TOKEN = "gh auth token"
```

## Traces

With tracing on, `agentsh trace` summarizes how agents actually use the shell: failures by code, the most expensive calls, command usage, and every failed-then-fixed pair (an error whose hint didn't prevent a retry). `agentsh trace --pairs fixes.jsonl` exports those pairs as training or eval data.

## Development

```sh
cargo test                 # unit tests, property tests for undo, and the conformance suite
cargo clippy --all-targets
```

The conformance suite (`spec/tests/*.json`) runs the real binary against fixtures and checks envelopes and resulting files. It passes on macOS and Linux.

Crates: `spec` (envelope and value types), `lang` (lexer, parser), `platform` (config, sandbox, process, secrets), `core` (interpreter, sessions, journal, limits), `builtins` (commands), `cli`.
