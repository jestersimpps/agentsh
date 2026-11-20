compose: Docker Compose services as a table. Needs exec.

- `compose.services` (`all:true` includes stopped ones): {service, state, health, ports}.
- Filter it: `compose.services | where .state != "running"`.
- Logs and restarts stay with exec: `exec "docker compose logs --tail 50 web"`.
