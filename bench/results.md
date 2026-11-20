# bash vs agentsh (darwin, median of 5 runs, tokens = cl100k_base estimate)

| task | calls bash / ash | tokens bash | tokens ash json | tokens ash text | ms bash | ms ash |
|---|---|---|---|---|---|---|
| orient in a new repo | 1 / 1 | 106 | 156 | 139 | 38 | 5 |
| 5 largest Rust files | 1 / 1 | 83 | 118 | 84 | 49 | 6 |
| file types in the repo | 1 / 1 | 72 | 70 | 36 | 46 | 6 |
| find a definition and read it | 2 / 1 | 183 | 225 | 182 | 55 | 7 |
| read a whole file | 1 / 1 | 493 | 594 | 496 | 23 | 5 |
| read three files | 1 / 1 | 391 | 500 | 420 | 23 | 5 |
| typo in a path, then recover | 3 / 1 | 249 | 249 | 205 | 67 | 5 |
| edit one line and check the diff | 2 / 1 | 168 | 87 | 58 | 56 | 5 |
| rename a function across files | 2 / 1 | 59 | 107 | 51 | 80 | 8 |
| run tests, show the summary | 1 / 1 | 98 | 132 | 108 | 115 | 96 |
| bad edit + tests, then revert | 3 / 1 | 138 | 288 | 241 | 414 | 263 |
| explore the project layout | 1 / 1 | 230 | 164 | 147 | 50 | 5 |
| find where a function is used | 1 / 1 | 188 | 69 | 47 | 31 | 8 |
| read one function from a big file | 2 / 1 | 533 | 447 | 384 | 46 | 5 |
| re-read a file after editing it | 3 / 3 | 1013 | 806 | 650 | 70 | 30 |
| revisit files during a session | 4 / 4 | 1246 | 881 | 688 | 87 | 17 |
| trivial call (overhead) | 1 / 1 | 42 | 18 | 2 | 16 | 4 |
| **total** | **30 / 22** | **5292** | **4911** | **3938** | **1264** | **480** |

Tokens = command written + everything printed back, summed over the task's calls. Time excludes model thinking between calls.
