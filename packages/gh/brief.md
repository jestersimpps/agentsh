gh: GitHub via the gh CLI. Needs the exec and net capabilities and a logged-in gh.

- `gh.pr` (current branch) or `gh.pr 123 repo:"owner/name"`: one record with state, review, size and
  a checks summary naming the failing checks. Cheaper than `gh pr view` + `gh pr checks`.
- `gh.checks 123`: only the checks that didn't pass, with links; `all:true` for every check.
- `gh.run_log <run id>`: failed steps' logs grouped by step, prefixes stripped; long logs keep the
  head, tail and error lines. The run id is in a check's link (.../runs/<id>/job/...).
- `gh.prs`: your open PRs; `gh.prs repo:"o/n" author:"" state:"merged" n:5`.
- Anything else: `exec "gh api repos/o/n/..." parse:"json"`.
