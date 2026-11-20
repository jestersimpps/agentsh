# bash vs agentsh (linux, median of 5 runs, tokens = cl100k_base estimate)

| task | calls bash / ash | tokens bash | tokens ash json | tokens ash text | ms bash | ms ash |
|---|---|---|---|---|---|---|
| orient in a new repo | 1 / 1 | 73 | 85 | 69 | 5 | 3 |
| 5 largest Rust files | 1 / 1 | 76 | 118 | 84 | 5 | 3 |
| file types in the repo | 1 / 1 | 72 | 70 | 36 | 5 | 3 |
| find a definition and read it | 2 / 1 | 183 | 225 | 182 | 5 | 3 |
| read a whole file | 1 / 1 | 493 | 594 | 496 | 2 | 2 |
| read three files | 1 / 1 | 391 | 500 | 420 | 2 | 2 |
| typo in a path, then recover | 3 / 1 | 249 | 249 | 205 | 6 | 2 |
| edit one line and check the diff | 2 / 1 | 167 | 87 | 58 | 6 | 3 |
| rename a function across files | 2 / 1 | 58 | 107 | 51 | 9 | 4 |
| run tests, show the summary | 1 / 1 | 98 | 132 | 108 | 199 | 202 |
| bad edit + tests, then revert | 3 / 1 | 137 | 288 | 241 | 309 | 284 |
| explore the project layout | 1 / 1 | 228 | 164 | 147 | 5 | 3 |
| find where a function is used | 1 / 1 | 188 | 69 | 47 | 3 | 5 |
| read one function from a big file | 2 / 1 | 533 | 447 | 384 | 5 | 3 |
| re-read a file after editing it | 3 / 3 | 1012 | 805 | 649 | 6 | 8 |
| revisit files during a session | 4 / 4 | 1246 | 881 | 688 | 8 | 9 |
| trivial call (overhead) | 1 / 1 | 15 | 18 | 2 | 1 | 2 |
| **total** | **30 / 22** | **5219** | **4839** | **3867** | **580** | **543** |
Tokens = command written + everything printed back, summed over the task's calls. Time excludes model thinking between calls.  Run in Docker (rust:1.94-bookworm, arm64).
