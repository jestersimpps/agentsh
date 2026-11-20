js: `check` runs the package.json `test` script with the project's package manager (picked from the
lockfile: pnpm, yarn, bun, else npm) and lists failing tests (jest, vitest, mocha).
`check "build"` runs `build`; `check "lint"` runs the first of `lint`, `typecheck`, `check`.

- `js.scripts` lists the scripts; `js.run "build"` runs one with the project's package manager.
- `js.typecheck` (or `js.typecheck "apps/web"`): tsc errors as {file, line, col, code, message}.
- One test file: `check args:"src/auth.test.ts"`; check passes extra args after the script.
- Dev servers: `start "npm run dev" until:"ready"`, then `logs` and `stop`.
