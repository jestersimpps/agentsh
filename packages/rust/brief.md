rust: `check` runs `cargo test -q` and lists failing tests with their panic message and location.
`check "build"` runs `cargo build -q`; `check "lint"` runs clippy on all targets.

- One test: `check args:"module::name"`. A workspace member: `check cmd:"cargo test -q -p crate_name"`.
- `outline` and `read symbol:` know fn, struct, enum, trait, impl, mod, type and macro_rules!.
- Dependency questions: `exec "cargo tree -i serde"`, `exec "cargo metadata --no-deps --format-version 1" parse:"json"`.
