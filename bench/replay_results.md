# Replaying a real coding session: bash vs agentsh

Session: a Codex session that added customizable keyboard shortcuts to vbcdr, an Electron/TypeScript app (38 shell calls, 18 edits, one commit). Replayed from its starting commit with 18 edits and 38 shell calls; 9 output polls and 0 refused calls left out.

Model: base prompt 2500 tokens; every turn re-reads the conversation; outputs capped at each call's recorded max_output_tokens; edits and visible messages identical in both arms; tokens = cl100k_base. "agentsh, full output" turns agentsh's output shortening off (it never shows less than bash).

| | bash | agentsh | change | agentsh, full output | change |
|---|---|---|---|---|---|
| shell calls replayed | 38 | 38 | +0% | 38 | +0% |
| tokens written (commands) | 2188 | 2360 | +8% | 2360 | +8% |
| tokens read (command output) | 88448 | 45465 | -49% | 60476 | -32% |
| one-time: cheat sheet + briefings (git, js) | 0 | 1872 |  | 1872 |  |
| final context size | 117756 | 76817 | -35% | 91828 | -22% |
| input tokens processed over the session | 3956028 | 2861525 | -28% | 3577312 | -10% |
| tool time (s) | 55.8 | 47.7 | -15% | 48.0 | -14% |

## By kind of call

| kind | calls | bash out | agentsh out | change | full output | change |
|---|---|---|---|---|---|---|
| git read | 6 | 25403 | 8507 | -67% | 9140 | -64% |
| git write | 5 | 233 | 298 | +28% | 298 | +28% |
| read/explore | 18 | 52346 | 34142 | -35% | 45242 | -14% |
| search | 1 | 4719 | 1261 | -73% | 4538 | -4% |
| test/build | 8 | 5747 | 1257 | -78% | 1258 | -78% |

Translation: 106 statements used native agentsh commands, 9 fell back to `exec "bash" "-c"`. agentsh errors in 3 calls; 24 notes left out lines or hunks already seen; 16 outputs shortened with a handle to the full value.
Already in context: 44137 of the 88448 tokens bash printed (50%) were lines the agent had already seen, in an earlier output or in its own edits.
Used afterwards: of the 88448 tokens bash printed, 4064 (5%) were lines the agent later edited around, and 8667 (10%) named something it later wrote or typed.
Edits that didn't re-apply: 0 (and 0 from concurrent sessions). Both arms ended with the same tree and diff: yes.
The recorded session's own outputs for these calls were 84781 tokens (the replay's bash arm: 88448); they differ where a concurrent session had changed files.

## What it shows

agentsh now remembers, per named session, which lines of each file the agent has seen: from reads, its own edits (`edit`, `write`, `replace`) and diffs. It leaves those lines out of later output.

- **Nothing shortened, only repeats left out ("full output" column):** −32% tokens read and −10% input processed, against −4% and ±0% before. All of that saving is content the agent already had.
- **Diffs: −64%.** `git.diff` leaves out hunks the agent wrote itself or saw in an earlier diff, and lists them in one line. The session diffed its own change 15 times.
- **Reads: −14%.** A re-read shows only lines that are new to the agent: `[lines 21–75: already shown, unchanged]`. A file the agent just wrote itself isn't shown back to it. When another process changed a file (here, the concurrent session), the read starts with `[changed since you last saw it: lines 2, 16, 76; …]`.
- **With the default output limits on top:** −49% tokens read and −28% input processed.
- **What the 50% bound leaves:** the bash output repeated 44k tokens of lines already in context; agentsh now leaves out 28k of those. The rest are short repeats (fewer than 4 lines, cheaper to repeat than to point at) and lines repeated across different files, which a per-file record doesn't cover.

## Not measured here

- **Outline-first.** A whole-file read of a file over 400 lines shows its outline instead. The replay's reads were all line ranges, so it doesn't exercise this.
- **Repeated test failures are named, not repeated.** The session's tests passed, so this didn't come up.
- **Whether a model trusts "already shown".** A replay can't show it. A live run with a model working in agentsh can.
