code: map, outline, refs and check (native commands, in every cheat sheet).

- `map` first in an unknown repo; `outline "f"` before reading a big file; `read "f" symbol:"name"` for one declaration.
- `refs "name"` finds definitions and uses (whole word); cheaper than grep for identifiers.
- `check` runs the detected test command and returns only the failing tests. `check "build"`, `check "lint"`,
  `check args:"filter"`. Language packages (rust, js, go, swift, python) supply the commands; `help "<lang>"`.
- Edit, then test in one run: `edit ...; check` rolls the edit back if tests fail; `check keep:true` keeps it.
