# Writing and sharing packages

A package teaches agentsh about one tool or ecosystem: a few commands named `yourpkg.command`,
optional `check` tasks, and a short briefing the agent reads only when it needs the package.
You'll build one in about 15 minutes. This guide covers keeping it private (just you, your team,
a private repo) and making it public.

The reference for every field is [spec/packages.md](../spec/packages.md). The finished example
from this guide is in [docs/examples/compose](examples/compose).

## 1. Create a package

```sh
agentsh pkg new compose
agentsh pkg test compose
```

```
compose/
  package.toml     # name, caps, commands
  brief.md         # what `help "compose"` returns
  tests/compose.json
  README.md
```

`pkg test` checks that `package.toml` is valid and runs the tests against your agentsh binary,
each in a throwaway workspace. The generated `compose.hello` command passes. Replace it with
something real.

## 2. Write a command

Commands are agentsh programs. This one turns `docker compose ps` into a table an agent can filter:

```toml
name = "compose"
version = "0.1.0"
summary = "Docker Compose: service status as a table"
caps = ["read", "exec"]                  # the most any command here may use
detect = ["compose.yaml", "compose.yml", "docker-compose.yml"]

[[command]]
name = "services"                        # called as compose.services
summary = "each service's state, health and ports"
params = [
  { name = "all", type = "bool", doc = "include stopped services" },
  { name = "output", type = "str", doc = "`docker compose ps --format json` output to parse instead of running it" },
]
input = "output"                         # a piped value fills this param
returns = "table of {service, state, health, ports}"
caps = ["read"]                          # checked before it runs (exec is checked when it's used)
examples = ["compose.services", "compose.services | where .state != \"running\""]
run = '''
let text = $output
if $text == null {
  let argv = ["docker", "compose", "ps", "--format", "json"]
  if $all { let argv = $argv + ["--all"] }
  let text = (exec $argv parse:"text")
}
let rows = []
for $line in ($text | lines) {
  let s = ($line | json)
  let rows = $rows + [{service: $s.Service, state: $s.State, health: get($s, "Health", ""), ports: get($s, "Ports", "")}]
}
if len($rows) == 0 { fail "no services are running" hint:"start them with: exec \"docker compose up -d\"" }
$rows
'''
```

What to know when writing `run`:

- **Parameters are variables.** `$all` and `$output`; one that wasn't passed is `null`. Passing
  `null` on (`exec ... timeout:$t`) means "not passed", so optional arguments forward cleanly.
- **The last statement is the result.** Return flat tables (lists of records with plain values);
  they print as compact tab-separated rows. Use `print "..."` for a one-line header such as a branch
  or a status code.
- **Build lists with `let rows = $rows + [...]`** inside `for`. The command runs in its own scope,
  so this doesn't touch the agent's variables.
- **Parse tool output** with `exec ... parse:"text"|"lines"|"json"`, `| lines`, `| json`,
  `match(s, regex)` (capture groups, or a record with named groups), `split`, `trim` and `int`.
  Use `get(record, "key", default)` for fields that may be missing; `$rec.key` errors when it's absent.
- **Fail with a hint**: `fail "message" hint:"what to do"`. The agent gets an `expectation_failed`
  error, and file changes made earlier in the run are rolled back.
- **Accept the tool's output as input** (`output` above). Agents can pipe in output they already
  have, and your tests don't need Docker installed.
- **Don't write files with exec'd programs** if you can avoid it. Built-in commands (`write`,
  `edit`) are undoable; files changed by an exec'd program are not.

## 3. Add check tasks (optional)

For a language or build tool, tell `check` how to test, build and lint:

```toml
[check]
recognize = ["mix test*"]              # check cmd:"..." lines your parser understands
[check.patterns]                       # or: parser = "cargo" | "pytest" | "js" | "go" | "swift"
passed = '(\d+) tests?, \d+ failures?'
failed = '\d+ tests?, (\d+) failures?'
failure = '(?m)^\s+\d+\) test (.+?) \((.+)\)$'
[check.test]
cmd = ["mix", "test"]
```

`check` uses the first package whose `detect` files exist and that has the task.

## 4. Write the briefing

`brief.md` is what an agent reads after `help "compose"`. Keep it under ~200 tokens: what each
command returns and when to use it, plus the one or two tricks that save a round trip. The
cheat sheet never lists package commands. `env` shows detected packages, and error hints point
to yours (`services` → "did you mean `compose.services`?"), so the agent finds it when it's needed.

## 5. Test it

`tests/*.json` holds cases; every step runs the real binary:

```json
[
  {
    "name": "compose.services turns docker compose ps into a table",
    "files": { "ps.jsonl": "{\"Service\":\"db\",\"State\":\"running\",\"Health\":\"healthy\",\"Ports\":\"\"}\n" },
    "steps": [
      { "run": "read \"ps.jsonl\" | compose.services", "expect": { "value.0.service": "db" } },
      { "run": "compose.services", "expect": { "error.code": "capability_denied" } }
    ]
  }
]
```

- `files` are created in an empty workspace; `config` becomes its `.agentsh.toml`; `allow` lists
  capabilities (`["exec"]`); `env` sets environment variables.
- `expect` paths go into the JSON envelope (`value.0.service`, `error.hint`). A `~` suffix means
  "contains". `files` after a step checks file contents (`null` = must not exist).
- `{"cli": ["pkg", "ls"], "expect_text~": "..."}` runs a CLI command instead of a program.

```sh
agentsh pkg test compose          # 1 of 1 cases passed
```

To try a package by hand without installing it:

```sh
AGENTSH_PACKAGES=./compose agentsh --allow exec run 'compose.services'
```

## 6. Use it and share it

Pick the scope you need. In every case only a human installs or turns on a package; agents can't
(`.agentsh.toml` and the data folder are off-limits to them).

| Who should get it | How |
|---|---|
| Just you, every project | `agentsh pkg add ./compose` (copies it into your agentsh data folder) |
| Everyone working in one repo (private to the repo) | commit it, e.g. `tools/agentsh/compose/`, and add `[packages] paths = ["tools/agentsh/compose"]` to the repo's `.agentsh.toml` |
| Your team, across repos (private git) | push it to a private repo, then `agentsh pkg add git@github.com:acme/agentsh-packages.git --path compose --rev v0.1.0` |
| Anyone (public git) | push it to a public repo; put the `agentsh pkg add https://...` line in its README |
| Everyone, built in | open a PR that adds it to [`packages/`](../packages) (see [packages/README.md](../packages/README.md)) |

**Private git repos** use your normal git access: an SSH key, or a credential helper for HTTPS
(`gh auth setup-git` on GitHub). agentsh never prompts. If `git ls-remote <url>` works without
a password prompt, `pkg add` works. One repo can hold many packages; pick one with `--path`.

**Pinning and updating.** `--rev` takes a branch or tag. agentsh records the source and the commit:

```sh
agentsh pkg ls                     # status, and where each installed package came from
agentsh pkg update                 # re-fetch every installed package (or: pkg update compose)
agentsh pkg rm compose
```

**Overriding while you work on it.** A copy in `[packages] paths` or `AGENTSH_PACKAGES` takes
precedence over the installed one with the same name (agentsh warns about the duplicate), so you
can edit a package in place and test it before you push.

**Turning packages off** per project: `[packages] disable = ["compose"]`.

## 7. Before you install someone else's package

- `pkg add` prints the capabilities the package may use (`caps it may use: …`). `read`-only packages can't run programs.
  Even an `exec` or `net` package gets nothing the session hasn't granted (`--allow exec`).
- Package commands only compose built-in commands, so they run inside the same workspace limits,
  sandbox, undo and secret redaction as anything else the agent does.
- Read its `package.toml`; it's short. Pin a tag with `--rev` rather than following a branch.

## Troubleshooting

| You see | Meaning |
|---|---|
| `warning: skipped a package: …` | A configured or installed package failed to load; the message says why. Everything else still works. |
| ``two packages are named `x`: using the … one`` | A duplicate name; the first one found wins (built-in, then `[packages] paths`, then `AGENTSH_PACKAGES`, then installed). |
| ``command `x` needs `exec`, which the package doesn't declare in caps`` | Add the capability to the package's `caps`, or drop it from the command's. |
| ``package `x` doesn't declare the `exec` capability`` | A command used a capability its package didn't declare; same fix. |
| ``the `exec` capability is not enabled in this session`` | The package is fine; the session needs `--allow exec`. |
| `git clone … failed … check git ls-remote` | No non-interactive access to the repo: set up an SSH key or a credential helper. |
| `run doesn't parse: …` | A syntax error in a command's `run`; `pkg test` shows it before anyone installs the package. |
