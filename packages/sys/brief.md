sys: local processes and ports (lsof, pgrep).

- `sys.ports` / `sys.ports 3000`: who listens where; use it before starting a dev server or to find a stuck one.
- `sys.procs "vite|next dev"`: matching processes with their pid.
- Stop a process you started with `stop`; anything else with `exec "kill <pid>"`.
