http: requests through curl. Needs exec, and net for anything but localhost on macOS.

- `http.get "http://localhost:3000/api/health"` prints `200 application/json` and returns the parsed
  body, so you can pipe it: `http.get "$url/users" | where .active | pick .id .email`.
- `http.post url json:{...}` or `body:"text"`; `http.request "PATCH" url json:{...}` for other methods.
- Headers: `headers:{Authorization: "Bearer ${secret("API_TOKEN")}"}`; secrets are redacted from output.
