# Packages

A package groups what an agent needs for one ecosystem or job: commands, `check` tasks and a short
briefing. The cheat sheet only says that packages exist; an agent finds the ones it needs through
`env`, error hints and `help`, and pays for a briefing only when it asks for one.

## Where packages come from

Packages load in this order. When two have the same name, the first one wins and the other is
skipped with a warning:

1. **Built-in**: the folders in [`packages/`](../packages), compiled into the binary.
2. **Configured**: `[packages] paths = ["../my-package"]` in a config file (relative to that file).
   This is how a repo shares a private package with everyone who works in it.
3. **Environment**: `AGENTSH_PACKAGES`, a list of folders in the platform's `PATH` format, for
   trying a package without installing it.
4. **Installed**: `agentsh pkg add <folder | git URL> [--path sub/folder] [--rev branch-or-tag]
   [--force]` copies a package into `<data dir>/packages/<name>` (`$AGENTSH_HOME/packages` when
   that is set), for every project. Git sources are cloned non-interactively (`git clone --depth 1`,
   no prompts), so private repos need an SSH key or a credential helper. The source, path, rev and
   commit are recorded in `.agentsh-source.json` inside the installed folder; `agentsh pkg update
   [name…]` reinstalls from it, and `agentsh pkg ls` shows it. `pkg add` refuses a name a built-in
   or configured package already uses.

A built-in package that fails to load is a bug and stops agentsh. A configured, environment or
installed package that fails to load is skipped with a warning, so one broken package doesn't
take the shell down.

`agentsh pkg new <name> [dir]` writes a starter package (manifest, brief, a passing test, README).
`agentsh pkg test <dir>` validates a package and runs its `tests/*.json` against the current
binary, each case in a fresh workspace with the package loaded through `AGENTSH_PACKAGES`. See
[docs/writing-packages.md](../docs/writing-packages.md) for a walkthrough.

Turn packages on or off per project or per user:

```toml
[packages]
disable = ["swift"]      # its commands and check tasks go away
enable = ["net"]         # for packages that are off by default (enabled = false in package.toml)
paths = ["tools/agentsh-acme"]
```

The core commands (files, text, data, `exec`, jobs, `env`, `help`) can't be turned off. The `code`
package owns `map`, `outline`, `refs` and `check`.

## Folder layout

```
packages/<name>/
  package.toml     # required
  brief.md         # what help "<name>" returns; keep it under ~200 tokens
  tests/*.json     # conformance cases, same format as spec/tests
```

## package.toml

```toml
name = "demo"                  # plain identifier; prefix of its commands (demo.greet)
version = "0.1.0"
summary = "one line for help \"packages\" and agentsh pkg ls"
caps = ["read", "exec"]        # the most this package's commands may use
detect = ["demo.toml"]         # files at the project root that mean the package applies
enabled = true                 # default; false means a human must enable it

[check]                        # optional: tasks for `check`
parser = "cargo"               # built-in parser: cargo, pytest, js, go, swift
recognize = ["demo test*"]     # check cmd:"..." lines this parser understands (* = anything)

[check.patterns]               # or: regex parser instead of `parser`
passed = '(\d+) passed'        # group 1 = a count; all matches are summed
failed = '(\d+) failed'
failure = '(?m)^FAIL (\S+)(?: - (.*))?$'   # group 1 = test name, group 2 = message

[check.test]                   # also [check.build] and [check.lint]
cmd = ["demo", "test"]         # or: package_script = ["test"] (a package.json script, run
requires = "demo"              #     with the lockfile's manager); requires = a program on PATH

[[command]]
name = "greet"                 # called as demo.greet
summary = "greet someone"
params = [
  { name = "who", type = "str", positional = true, required = true, doc = "name" },
  { name = "loud", type = "bool" },
]
input = "who"                  # optional: the param a piped value fills
returns = "string"
caps = ["read"]                # checked before it runs; subset of the package's caps
                               # (default: all of the package's caps)
examples = ['demo.greet "jo"']
run = '''
let msg = "hi $who"
if $loud { upper($msg) } else { $msg }
'''
```

Parameter types: `any str int bool path paths arg list record duration`.

## How package commands run

- `run` is an agentsh program, parsed when the package loads (a parse error stops loading).
- Each parameter is a variable (`$who`); a parameter that wasn't passed is `null`. The program
  can't see the session's variables, and its own `let`s don't leak out.
- Its value is the value of the last statement.
- Every capability check is limited to the package's `caps` **and** the session's grants. A
  package that declares `exec` still can't run programs in a session without `--allow exec`.
- Because it only composes built-in commands, a package command inherits workspace limits,
  freshness checks, undo, rollback on failure, the `exec` sandbox and secret redaction.
- Package commands may call each other, up to 16 deep.
- Agents can't install packages or change `[packages]` (`.agentsh.toml` is protected). A package
  folder listed in `paths` that sits inside the workspace is an ordinary file to the agent, though:
  it can edit it, but the result still can't exceed the session's capabilities.

## How `check` picks a task

Without `cmd:`, `check "<task>"` takes the first turned-on package, in load order, whose `detect`
files exist and that has a usable task (its `requires` program is on PATH, its `package_script`
exists). The test parser applies to `test` only. If no package matches, a Makefile target with the
task's name runs.

With `cmd:`, the first package whose `recognize` pattern matches the command line supplies the
parser.

## Discovery without context cost

- The cheat sheet has one short PACKAGES paragraph and lists no package commands, so it stays the
  same on every machine and keeps its prompt-cache hit.
- `env` lists the turned-on packages detected for the project: `packages  rust, js`.
- `help "packages"` lists every package with its status; `help "<package>"` returns its brief
  and commands; `help "<package.command>"` returns one command's details.
- Unknown-command errors point to the right package: a missing prefix (`scripts` →
  `js.scripts`), a misspelling, or a package that is turned off.
- In a named session, asking for the same briefing again returns a one-line note.
