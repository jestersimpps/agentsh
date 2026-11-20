# Packages

Each folder here is a package compiled into agentsh. The format is in
[spec/packages.md](../spec/packages.md).

| package | adds |
|---|---|
| `code` | `map`, `outline`, `refs`, `check` |
| `rust` | `check` via cargo test / build / clippy |
| `js` | `check` via package.json scripts (npm, pnpm, yarn, bun); `js.scripts`, `js.run`, `js.typecheck` |
| `go` | `check` via go test / build / vet |
| `swift` | `check` via swift test / build (XCTest, Swift Testing) |
| `python` | `check` via pytest; ruff for lint |
| `git` | `git.status`, `git.diff`, `git.log`, `git.show` (file or symbol at a revision), `git.grep`, `git.files`, `git.branches`, `git.changed` |
| `gh` | `gh.pr`, `gh.checks`, `gh.run_log`, `gh.prs` (needs exec + net) |
| `http` | `http.get`, `http.post`, `http.request` with JSON parsing (needs exec + net) |
| `sys` | `sys.ports`, `sys.procs` |
| `docs` | `docs.pdf`, `docs.zip`, `docs.unzip` |

The git, gh, http, sys and docs packages and the js additions come from mining ~800 local agent
sessions for the shell commands they ran most (`bench/mine_sessions.py`); `bench/packages_bench.py`
compares them with the bash commands those sessions used.

## Contributing a package

[docs/writing-packages.md](../docs/writing-packages.md) walks through writing a package and sharing
it privately or publicly. To ship one built in:

1. Start from `agentsh pkg new <name>`, or copy a folder that is close to what you need, e.g. `packages/go`, and rename it. The folder name
   and `name` in `package.toml` must match.
2. Keep `brief.md` short (under ~200 tokens). It's what the model reads when it runs
   `help "<name>"`: say what `check` runs and the few commands and tricks worth knowing.
3. Declare only the `caps` your commands use. A command without its own `caps` requires all of the
   package's before it runs, so a clear `capability_denied` comes first, not a sandbox failure. `exec` and `net` need a reason in the PR.
4. Add `tests/*.json` cases (same format as `spec/tests`). Cases that need external tools must not
   assume they're installed; test the agentsh side (detection, parsing, hints) instead.
5. To ship it in the binary, add `embed!("<name>")` to `crates/builtins/src/packages.rs`. Order
   matters: `check` uses the first detected package.
6. `agentsh pkg test packages/<name>` runs its tests; `cargo test` runs every package's tests with
   the rest of the conformance suite.

Packages that don't need to be built in can live anywhere: a folder in your repo listed under
`[packages] paths`, or a public or private git repo installed with
`agentsh pkg add <git URL> --path <folder> --rev <tag>`.
