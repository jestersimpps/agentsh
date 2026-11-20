git: read-only views of the repo as tables (writes stay with exec: `exec "git commit -m '...'"`).

- `git.status`: branch line + changed files with line counts (replaces `git status` + `git diff --stat`).
- `git.diff` (patch), `git.diff stat:true rev:"main"`, `git.changed` (files this branch changes vs main).
- `git.show "origin/main" "src/a.rs" symbol:"parse"`: a file or one declaration at any revision;
  `from:/to:/tail:` like read. `git.show "HEAD~1"`: commit line + files.
- `git.log "path" n:5`, `git.grep "pattern" rev:"origin/main" in:"src"`, `git.files "src" rev:"main"`, `git.branches`.
