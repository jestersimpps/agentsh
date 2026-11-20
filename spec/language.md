# agentsh language 0.1

This document specifies the language and the envelope independently of the Rust implementation. `spec/tests/*.json` is the executable part of the spec.

## Lexical rules

- Statements are separated by newlines or `;`. A `\` at the end of a line continues it. `#` starts a comment.
- **Strings** are always quoted. `"..."` interpolates `$name` and `${expr}` (inside `${}`, a leading bare name means the variable, so `${e.code}` = `${$e.code}`). Escapes: `\n \t \r \\ \" \$ \'`; any other backslash sequence is kept as written, so regexes like `"\d+\.rs"` work. `'...'` is raw: no escapes, no interpolation. `r"..."`, `r#"..."#`, `r##"..."##` are raw strings that may contain quotes; they end at the matching `"#…`, so code can be written without escaping.
- **Numbers**: `42`, `-3`, `1.5`, `1_000`. Size suffixes give bytes: `b kb mb gb tb` (powers of 1000), `kib mib gib tib` (powers of 1024). Duration suffixes give seconds: `ms` (as a fraction) `s m h d w`.
- **Literals**: `true false null`, lists `[a, b]`, records `{key: value, "any key": value}`.
- **Names**: `$name` is a variable; `.field` (or `.a.b`) is a field of the current row; `name(` is a function call; `name:` is a named argument. A bare word is a command name in command position, and so is `package.command` (two plain names joined by a dot, see [packages](packages.md)); as an argument of a row-expression parameter (`where`, `sort`, `pick`, `group`, `uniq`, `sum`) it means `.word`; anywhere else it is an error.
- Unquoted paths, flags (`-la`), `&&`, `||`, `!`, redirections and backticks are parse errors with a hint showing the agentsh form.

## Grammar

```
program   = { stmt (newline | ";") }
stmt      = "let" name "=" pipeline
          | "if" expr block [ "else" ( block | if-stmt ) ]
          | "for" name "in" expr block
          | "try" block [ "catch" [name] block ]
          | pipeline
block     = "{" program "}"
pipeline  = ( call | expr ) { "|" call }
call      = command { key ":" expr | expr }
command   = name | name "." name           (package commands: js.scripts)
expr      = or ;  or = and {"or" and} ;  and = not {"and" not} ;  not = "not" not | cmp
cmp       = add [ ("==" | "!=" | "<" | "<=" | ">" | ">=" | "=~" | "!~" | "in") add ]
add       = mul { ("+" | "-") mul } ;  mul = unary { ("*" | "/" | "%") unary }
unary     = "-" unary | primary
primary   = literal | string | $name {accessor} | .field | func "(" args ")"
          | "(" pipeline ")" | "[" list "]" | "{" record "}"
accessor  = .field | "[" expr "]"         (no whitespace before an accessor)
```

Nesting deeper than 256 levels is a parse error. Operators need spaces around them where the left side could be part of a word (`5 - 1`, not `5-1`).

## Values

`null`, `bool`, `int` (i64), `float`, `string`, `list`, `record` (ordered keys). Tables are lists of records. Comparisons between numbers mix int and float; strings compare lexicographically, so ISO-8601 timestamps from `.modified`, `now()` and `ago()` compare correctly. `null` is never `<` or `>` anything. Truthiness: `false`, `null`, `0`, `""`, `[]`, `{}` are false. `int / int` is an int when exact, otherwise a float.

## Evaluation

- Every top-level statement's non-null value is returned: earlier ones in `outputs` (labelled `[line] statement`), the last one as `value`. Outputs collected before a failure are still returned. A program ending in `let` returns a one-line summary of the stored value. `let` evaluates to the assigned value; `for` evaluates to the list of body values (or `null` if they're all null); `if` evaluates to the taken branch.
- **Pipes** pass values. A piped value fills the command's input parameter (`help "<command>"` shows which). Giving both a piped value and that parameter is an error.
- **Arguments** bind by position, then by name. Types are checked before the command runs. A `path` parameter also accepts a record with a `path` field, so rows from `ls`, `find` and `grep` pipe into `read`, `rm`, `chmod`, `grep`, `replace`.
- **Variables** persist for the session. `$_` is the previous call's result. `$rN` handles hold full results that were shortened.
- **Failure**: the first failing statement stops the run. Every file change the run made through built-in commands is rolled back, and the envelope says `rolled_back: true`. `try { } catch $e { }` catches a failure, rolls back only the failed block, and binds `$e` to `{code, message, hint, command}`.
- **Paths** are relative to the session's current folder (`cd`), must stay inside the workspace roots after resolving symlinks, and may not touch `.agentsh/` or `.agentsh.toml` for writing.
- **Freshness**: reading a file records its hash. `edit`, `write` and `replace` fail with `stale` if the file changed since it was last read in the session. `write` refuses to replace a file that was never read unless `overwrite:true`.

## Envelope

Every call prints exactly one envelope (JSON by default, or the same content as text with `--format text`).

| field | when | meaning |
|---|---|---|
| `ok` | always | success |
| `value` | success, non-null | the result, possibly shortened |
| `shown`, `total` | a list was shortened | rows shown / rows produced |
| `truncated` | anything was shortened | lists capped or text excerpted |
| `handle` | `truncated` | variable holding the full result, e.g. `$r3` |
| `printed` | `print` was used | printed values in order |
| `effects` | files changed | `{op: create|modify|delete|move|mkdir, path, to?, bytes?}` |
| `undo` | files changed | token for `agentsh undo` |
| `warnings` | sometimes | things the agent should know (shortened reads, stderr, repeated failures) |
| `error` | failure | `{code, message, arg?, value?, hint?, command?, span?, rolled_back?}` |
| `session` | session isn't `default` | session id |
| `ms`, `tokens_est` | always | wall time; rough token cost of this envelope |

## Error codes

`parse_error`, `unknown_command`, `unknown_argument`, `missing_argument`, `type_error`, `invalid_value`, `not_found`, `already_exists`, `outside_workspace`, `protected_path`, `capability_denied`, `expectation_failed`, `stale`, `ambiguous_match`, `no_match`, `binary_file`, `exec_failed`, `timeout`, `conflict`, `nothing_to_undo`, `session_busy`, `io_error`, `internal`.

## Shortening

Lists longer than `max_items` (default 50) are cut. Text longer than `max_string_bytes` (default 24,000) is excerpted: repeated lines collapse, the head and tail are kept, and lines in the middle that mention errors, failures, panics, exceptions or warnings are kept with their line numbers. A list shares the text budget between its items.

## Capabilities

| capability | default | allows |
|---|---|---|
| `read` | on | reading files and metadata |
| `write` | on | creating and changing files (undoable) |
| `delete` | on | removing files (undoable) |
| `exec` | off | running external programs |
| `net` | off | network access for external programs (enforced by the sandbox) |

## Packages

Commands outside the core set come in packages, named `package.command`. A package may also supply `check` tasks (test, build, lint) and a briefing returned by `help "<package>"`. Packages run with the session's capabilities, further limited to the ones the package declares. See [packages.md](packages.md).

## Versioning

The language version (`0.1`) is reported by `env` and `help`. Additions that don't change the meaning of existing programs keep the major version; anything else bumps it.
