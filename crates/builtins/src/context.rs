//! Commands about what the agent has already seen: `unseen` (leave it out of a diff) and
//! `forget` (show everything in full again, e.g. after the agent's context was compacted).

use agentsh_core::seen::View;
use agentsh_core::{Args, Builtin, CommandSpec, Ctx, ParamSpec as P, ParamType as T, Result};
use agentsh_platform::Capability::Read;
use agentsh_spec::Value;
use std::path::PathBuf;

pub const UNSEEN: Builtin = Builtin {
    spec: CommandSpec {
        name: "unseen",
        summary: "a git diff without the hunks you've already seen (your own edits, earlier diffs)",
        params: &[
            P::pos("text", T::Str, "unified diff text, e.g. from git diff").required(),
            P::named(
                "worktree",
                T::Bool,
                "the diff's new side is the work tree (default true; false for --cached or a..b)",
            ),
        ],
        input: Some("text"),
        returns: "the diff, with hunks you've seen in their current form left out and listed at the end",
        caps: &[Read],
        examples: &["exec \"git diff\" parse:\"text\" | unseen"],
    },
    run: unseen,
};

pub const FORGET: Builtin = Builtin {
    spec: CommandSpec {
        name: "forget",
        summary: "forget what you've seen in this session, so reads and diffs show everything again",
        params: &[P::pos("paths", T::Paths, "only these files or folders").variadic()],
        input: None,
        returns: "a note",
        caps: &[],
        examples: &["forget", "forget \"src/a.ts\""],
    },
    run: forget,
};

fn forget(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let raws = args.strings("paths")?;
    let paths: Vec<PathBuf> = raws.iter().map(|r| ctx.resolve(r)).collect::<Result<_>>()?;
    let before = ctx.state.views.len();
    ctx.state.forget(&paths);
    let forgotten = before - ctx.state.views.len();
    let what = if paths.is_empty() { "everything you'd seen".to_string() } else { format!("{forgotten} file(s)") };
    Ok(Value::Str(format!("forgot {what}; reads and diffs show it in full again")))
}

struct Hunk<'a> {
    lines: Vec<&'a str>,
    new_start: usize,
}

struct Section<'a> {
    header: Vec<&'a str>,
    path: Option<String>,
    hunks: Vec<Hunk<'a>>,
}

fn parse(text: &str) -> (Vec<&str>, Vec<Section<'_>>) {
    let mut preamble = Vec::new();
    let mut sections: Vec<Section> = Vec::new();
    for line in text.lines() {
        if line.starts_with("diff --git ") {
            sections.push(Section { header: vec![line], path: None, hunks: Vec::new() });
            continue;
        }
        let Some(section) = sections.last_mut() else {
            preamble.push(line);
            continue;
        };
        if let Some(rest) = line.strip_prefix("@@ ") {
            let new_start = rest
                .split_whitespace()
                .find_map(|w| w.strip_prefix('+'))
                .and_then(|w| w.split(',').next())
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(1);
            section.hunks.push(Hunk { lines: vec![line], new_start });
        } else if let Some(hunk) = section.hunks.last_mut() {
            hunk.lines.push(line);
        } else {
            if let Some(p) = line.strip_prefix("+++ b/") {
                section.path = Some(p.to_string());
            }
            section.header.push(line);
        }
    }
    (preamble, sections)
}

fn unseen(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let text = args.str("text")?;
    let worktree = !matches!(args.value("worktree"), Some(Value::Bool(false)));
    if !ctx.dedupes() || !text.contains("diff --git ") {
        return Ok(Value::Str(text));
    }
    let (preamble, sections) = parse(&text);
    let mut out: Vec<String> = preamble.iter().map(|l| l.to_string()).collect();
    let mut seen_files: Vec<String> = Vec::new();
    for section in sections {
        let file = section.path.as_ref().map(|p| ctx.root.join(p)).filter(|p| p.is_file());
        let current = file.as_ref().and_then(|p| std::fs::read_to_string(p).ok());
        let (Some(path), Some(current), Some(rel)) = (file, current, section.path.clone()) else {
            out.extend(section.header.iter().map(|l| l.to_string()));
            out.extend(section.hunks.iter().flat_map(|h| h.lines.iter().map(|l| l.to_string())));
            continue;
        };
        let hash = agentsh_platform::hash_bytes(current.as_bytes());
        let mut view: View = ctx.view(&path).cloned().unwrap_or_default();
        let up_to_date = view.hash == hash;
        let (mut known, _) = ctx.carried_view(&path, &current);
        let mut shown: Vec<&Hunk> = Vec::new();
        let mut skipped = 0;
        for hunk in &section.hunks {
            let mut new_line = hunk.new_start.saturating_sub(1);
            let (mut plus, mut minus, mut context) = (Vec::new(), Vec::new(), Vec::new());
            let mut plus_text = Vec::new();
            for line in &hunk.lines[1..] {
                match line.as_bytes().first() {
                    Some(b'+') => {
                        plus.push(new_line);
                        plus_text.push(&line[1..]);
                        new_line += 1;
                    }
                    Some(b'-') => minus.push(&line[1..]),
                    Some(b'\\') => {}
                    _ => {
                        context.push(new_line);
                        new_line += 1;
                    }
                }
            }
            let seen = up_to_date
                && !(plus.is_empty() && minus.is_empty())
                && if worktree {
                    plus.iter().all(|&i| known.get(i).copied().unwrap_or(false))
                } else {
                    plus_text.iter().all(|l| view.has_seen(l))
                }
                && minus.iter().all(|l| view.has_seen(l));
            if seen {
                skipped += 1;
                continue;
            }
            if worktree {
                for i in plus.iter().chain(&context) {
                    if let Some(k) = known.get_mut(*i) {
                        *k = true;
                    }
                }
            }
            view.remember(hunk.lines[1..].iter().filter(|l| !l.starts_with('\\')).map(|l| l.get(1..).unwrap_or("")));
            shown.push(hunk);
        }
        if shown.is_empty() && skipped > 0 {
            seen_files.push(if skipped == 1 { rel.clone() } else { format!("{rel} ({skipped} hunks)") });
        } else {
            out.extend(section.header.iter().map(|l| l.to_string()));
            if skipped > 0 {
                let what = if skipped == 1 { "1 more hunk".to_string() } else { format!("{skipped} more hunks") };
                out.push(format!("[{what} in this file you've already seen]"));
            }
            out.extend(shown.iter().flat_map(|h| h.lines.iter().map(|l| l.to_string())));
        }
        ctx.set_view_text(&path, view, &current, &known);
    }
    if !seen_files.is_empty() {
        let lead = if out.iter().any(|l| l.starts_with("diff --git ")) {
            "also changed"
        } else {
            "no changes you haven't seen; changed"
        };
        out.push(format!("[{lead}, as you've already seen: {}; full:true shows them]", seen_files.join(", ")));
    }
    Ok(Value::Str(out.join("\n") + "\n"))
}
