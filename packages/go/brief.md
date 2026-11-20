go: `check` runs `go test ./...` and lists each `--- FAIL` with its first lines of output.
`check "build"` runs `go build ./...`; `check "lint"` runs `go vet ./...`.

- One test: `check cmd:"go test ./pkg/auth -run TestLogin"`.
- `outline` and `read symbol:` know func, type and struct declarations.
