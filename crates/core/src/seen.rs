//! What the agent has already seen of each file, so output can leave it out.
//!
//! A session keeps one `View` per file: the version of the file the agent last saw (its text is in
//! the session's `shown` store, by hash), which lines of that version it has seen, and a set of
//! every line it has ever seen in any version. Reads, the agent's own edits and diffs update it.
//! When the file changes, known lines are carried over to the new version through a line diff, so
//! the agent is shown only what's new to it.

use serde::{Deserialize, Serialize};
use similar::{DiffOp, TextDiff};
use std::collections::HashSet;

/// Runs of at least this many known lines are left out; shorter ones are cheaper to repeat.
pub const MIN_RUN: usize = 4;
const MAX_EVER: usize = 50_000;

/// Sorted, merged ranges of 0-based line indexes, `[start, end)`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Lines(pub Vec<(u32, u32)>);

impl Lines {
    pub fn from_flags(flags: &[bool]) -> Lines {
        let mut out = Vec::new();
        let mut i = 0;
        while i < flags.len() {
            if flags[i] {
                let start = i;
                while i < flags.len() && flags[i] {
                    i += 1;
                }
                out.push((start as u32, i as u32));
            } else {
                i += 1;
            }
        }
        Lines(out)
    }

    pub fn flags(&self, len: usize) -> Vec<bool> {
        let mut flags = vec![false; len];
        for &(a, b) in &self.0 {
            for f in flags.iter_mut().take((b as usize).min(len)).skip(a as usize) {
                *f = true;
            }
        }
        flags
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct View {
    /// Hash of the file version `known` refers to.
    pub hash: String,
    pub known: Lines,
    /// Every line the agent has seen in any version of the file (trimmed, hashed).
    #[serde(default)]
    pub ever: HashSet<u64>,
}

impl View {
    pub fn remember<'a>(&mut self, lines: impl IntoIterator<Item = &'a str>) {
        for line in lines {
            if self.ever.len() >= MAX_EVER {
                break;
            }
            self.ever.insert(line_key(line));
        }
    }

    pub fn has_seen(&self, line: &str) -> bool {
        line.trim().is_empty() || self.ever.contains(&line_key(line))
    }
}

pub fn line_key(line: &str) -> u64 {
    let digest = agentsh_platform::hash_bytes(line.trim().as_bytes());
    u64::from_str_radix(&digest[..16], 16).unwrap_or_default()
}

/// Carry `known` lines of `old` over to `new`: equal lines keep their flag, changed ones are new.
pub fn carry(old: &str, old_known: &[bool], new: &str) -> Vec<bool> {
    let mut flags = vec![false; new.lines().count()];
    if old == new {
        for (f, k) in flags.iter_mut().zip(old_known) {
            *f = *k;
        }
        return flags;
    }
    for op in TextDiff::from_lines(old, new).ops() {
        if let DiffOp::Equal { old_index, new_index, len } = *op {
            for k in 0..len {
                if let (Some(f), Some(known)) = (flags.get_mut(new_index + k), old_known.get(old_index + k)) {
                    *f = *known;
                }
            }
        }
    }
    flags
}

/// After the agent writes `new` over `old` itself: lines it wrote are known, the rest carry over.
pub fn carry_own_edit(old: &str, old_known: &[bool], new: &str, view: &mut View) -> Vec<bool> {
    let mut flags = vec![false; new.lines().count()];
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    for op in TextDiff::from_lines(old, new).ops() {
        match *op {
            DiffOp::Equal { old_index, new_index, len } => {
                for k in 0..len {
                    if let (Some(f), Some(known)) = (flags.get_mut(new_index + k), old_known.get(old_index + k)) {
                        *f = *known;
                    }
                }
            }
            _ => {
                let (old_range, new_range) = (op.old_range(), op.new_range());
                view.remember(
                    old_lines[old_range.start.min(old_lines.len())..old_range.end.min(old_lines.len())].iter().copied(),
                );
                view.remember(
                    new_lines[new_range.start.min(new_lines.len())..new_range.end.min(new_lines.len())].iter().copied(),
                );
                for f in flags.iter_mut().take(new_range.end).skip(new_range.start) {
                    *f = true;
                }
            }
        }
    }
    flags
}

/// How the lines the agent had seen changed: spans of the new version (1-based, inclusive) that
/// replaced them, and `(after_line, count)` for ones that are gone.
#[derive(Debug, Default, PartialEq)]
pub struct Changes {
    pub changed: Vec<(usize, usize)>,
    pub removed: Vec<(usize, usize)>,
}

pub fn changes_to_known(old: &str, old_known: &[bool], new: &str) -> Changes {
    let (mut changed, mut removed) = (Vec::new(), Vec::new());
    if old == new {
        return Changes::default();
    }
    for op in TextDiff::from_lines(old, new).ops() {
        let (o, n) = (op.old_range(), op.new_range());
        let touched_known = o.clone().any(|i| old_known.get(i).copied().unwrap_or(false));
        match op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete { .. } if touched_known => removed.push((n.start, o.len())),
            DiffOp::Insert { .. } => {}
            _ if touched_known => changed.push((n.start + 1, n.end)),
            _ => {}
        }
    }
    Changes { changed, removed }
}

/// `lines[start..end]` with runs of known lines replaced by one note each; `None` if nothing is
/// left out. `numbers` prefixes each shown line with its line number.
pub fn elide(lines: &[&str], start: usize, end: usize, known: &[bool], numbers: bool) -> Option<String> {
    let end = end.min(lines.len());
    let is_known = |i: usize| known.get(i).copied().unwrap_or(false);
    let mut out: Vec<String> = Vec::new();
    let mut elided = false;
    let mut i = start;
    while i < end {
        if is_known(i) {
            let run_start = i;
            while i < end && is_known(i) {
                i += 1;
            }
            if i - run_start >= MIN_RUN {
                elided = true;
                let span = if i - run_start == 1 {
                    format!("line {}", run_start + 1)
                } else {
                    format!("lines {}–{}", run_start + 1, i)
                };
                out.push(format!("[{span}: already shown, unchanged]"));
                continue;
            }
            i = run_start;
            while i < end && is_known(i) {
                out.push(numbered(lines[i], i, numbers));
                i += 1;
            }
            continue;
        }
        out.push(numbered(lines[i], i, numbers));
        i += 1;
    }
    elided.then(|| out.join("\n") + "\n")
}

fn numbered(line: &str, i: usize, numbers: bool) -> String {
    if numbers { format!("{}\t{line}", i + 1) } else { line.to_string() }
}

pub fn spans_text(spans: &[(usize, usize)]) -> String {
    let mut parts: Vec<String> =
        spans.iter().take(6).map(|&(a, b)| if a >= b { format!("{a}") } else { format!("{a}–{b}") }).collect();
    if spans.len() > 6 {
        parts.push(format!("and {} more", spans.len() - 6));
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_round_trip() {
        let flags = [false, true, true, false, true];
        assert_eq!(Lines::from_flags(&flags), Lines(vec![(1, 3), (4, 5)]));
        assert_eq!(Lines::from_flags(&flags).flags(5), flags);
    }

    #[test]
    fn carry_keeps_unchanged_lines_known() {
        let old = "a\nb\nc\nd\n";
        let new = "a\nB\nc\nd\ne\n";
        assert_eq!(carry(old, &[true, true, true, false], new), vec![true, false, true, false, false]);
    }

    #[test]
    fn own_edits_are_known() {
        let mut view = View::default();
        let flags = carry_own_edit("a\nb\nc\n", &[true, false, false], "a\nx\ny\nc\n", &mut view);
        assert_eq!(flags, vec![true, true, true, false]);
        assert!(view.has_seen("b") && view.has_seen("x"));
    }

    #[test]
    fn elide_leaves_out_long_known_runs_only() {
        let lines: Vec<String> = (1..=10).map(|i| format!("l{i}")).collect();
        let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut known = vec![false; 10];
        known[..6].iter_mut().for_each(|k| *k = true);
        known[8] = true;
        let out = elide(&lines, 0, 10, &known, false).unwrap();
        assert_eq!(out, "[lines 1–6: already shown, unchanged]\nl7\nl8\nl9\nl10\n");
        assert_eq!(elide(&lines, 6, 10, &known, false), None);
    }

    #[test]
    fn changes_to_known_lines_are_reported() {
        let changes = changes_to_known("a\nb\nc\nd\n", &[true; 4], "a\nB\nc\n");
        assert_eq!(changes, Changes { changed: vec![(2, 2)], removed: vec![(3, 1)] });
    }
}
