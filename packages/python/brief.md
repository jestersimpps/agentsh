python: `check` runs `python3 -m pytest -q` and lists each FAILED/ERROR test with its message.
`check "lint"` runs `ruff check .` when ruff is installed.

- One test: `check args:"tests/test_api.py::test_login"`; a virtualenv: `check cmd:".venv/bin/pytest -q"`.
- `outline` and `read symbol:` find def and class blocks by indentation.
