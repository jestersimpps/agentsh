# Packages vs bash (darwin, median of 3 runs, tokens = cl100k_base)

| task | calls bash / ash | tokens bash | tokens ash text | tokens ash json | ms bash | ms ash |
|---|---|---|---|---|---|---|
| what changed (status + line counts) | 1 / 1 | 40 | 33 | 61 | 56 | 74 |
| review a branch vs main | 3 / 1 | 326 | 367 | 511 | 95 | 132 |
| read one function at another revision | 2 / 1 | 549 | 388 | 451 | 62 | 42 |
| search code at a revision | 1 / 1 | 212 | 187 | 258 | 38 | 49 |
| list files at a revision | 1 / 1 | 149 | 146 | 162 | 31 | 46 |
| recent commits touching a file | 1 / 1 | 23 | 36 | 61 | 31 | 45 |
| inspect the last commit | 1 / 1 | 167 | 98 | 158 | 31 | 74 |
| PR status with checks | 1 / 1 | 409 | 96 | 116 | 974 | 904 |
| which checks didn't pass | 1 / 1 | 617 | 17 | 33 | 1212 | 1215 |
| read a failed CI log | 1 / 1 | 5118 | 3865 | 3980 | 3103 | 2937 |
| TypeScript errors | 1 / 1 | 179 | 171 | 254 | 644 | 401 |
| what's listening on which port | 1 / 1 | 1018 | 320 | 503 | 509 | 537 |
| is a process running | 1 / 1 | 255 | 177 | 194 | 66 | 39 |
| query a local JSON API | 1 / 1 | 458 | 164 | 277 | 32 | 35 |
| read page 2 of a PDF | 1 / 1 | 1011 | 1002 | 1020 | 25 | 36 |
| list a zip archive | 1 / 1 | 398 | 106 | 126 | 13 | 26 |
| read the briefings used (once per session) | - / 1 | - | 1471 | 1581 | 0 | 13 |
| **total** | **19 / 17** | **10929** | **8644** | **9746** | **6924** | **6602** |

Tokens = command written + everything printed back, summed over a task's calls. Time excludes model thinking.
The briefings row is a one-time cost per session; bash has none because models already know bash.
