use agentsh_platform::Limits;
use agentsh_spec::Value;
use regex::Regex;
use std::sync::LazyLock;

static IMPORTANT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(error|errors|failed|failure|fail|panic|panicked|exception|fatal|warning|traceback)\b").unwrap()
});

const MIN_NESTED_BUDGET: usize = 300;
const MAX_LINE_IN_EXCERPT: usize = 400;
const MAX_DIFF_FILES: usize = 30;

pub struct Shaped {
    pub value: Value,
    pub shown: Option<usize>,
    pub total: Option<usize>,
    pub truncated: bool,
}

pub fn shape(value: Value, limits: &Limits) -> Shaped {
    let mut shaped = Shaped { value, shown: None, total: None, truncated: false };
    if let Value::List(items) = &mut shaped.value {
        let total = items.len();
        if total > limits.max_items {
            items.truncate(limits.max_items);
            shaped.shown = Some(limits.max_items);
            shaped.total = Some(total);
            shaped.truncated = true;
        }
    }
    shaped.truncated |= match &mut shaped.value {
        Value::List(items) => {
            let nested_budget =
                (limits.max_string_bytes / items.len().max(1)).clamp(MIN_NESTED_BUDGET, limits.max_string_bytes);
            items.iter_mut().fold(false, |acc, v| shrink(v, nested_budget) | acc)
        }
        Value::Record(record) => record.values_mut().fold(false, |acc, v| shrink(v, limits.max_string_bytes) | acc),
        other => shrink(other, limits.max_string_bytes),
    };
    shaped
}

fn shrink(value: &mut Value, budget: usize) -> bool {
    match value {
        Value::Str(s) => match smart_truncate(s, budget) {
            Some(short) => {
                *s = short;
                true
            }
            None => false,
        },
        Value::List(items) => items.iter_mut().fold(false, |acc, v| shrink(v, budget) | acc),
        Value::Record(record) => record.values_mut().fold(false, |acc, v| shrink(v, budget) | acc),
        _ => false,
    }
}

pub fn smart_truncate(text: &str, budget: usize) -> Option<String> {
    if text.len() <= budget {
        return None;
    }
    let collapsed = collapse_repeats(text);
    if collapsed.len() <= budget {
        return Some(collapsed);
    }
    let lines: Vec<&str> = collapsed.lines().collect();
    if lines.len() < 3 {
        return Some(clip_middle(&collapsed, budget));
    }
    if let Some(summary) = diff_summary(&lines) {
        let rest = budget.saturating_sub(summary.len()).max(budget / 2);
        return Some(format!("{summary}\n{}", excerpt(&lines, rest)));
    }
    Some(excerpt(&lines, budget))
}

fn diff_summary(lines: &[&str]) -> Option<String> {
    let mut files: Vec<(String, usize, usize, usize)> = Vec::new();
    for line in lines {
        if let Some(path) = line.strip_prefix("+++ ").map(|p| p.trim_start_matches("b/")) {
            files.push((path.to_string(), 0, 0, 0));
        } else if let Some(last) = files.last_mut() {
            if line.starts_with("@@") {
                last.3 += 1;
            } else if line.starts_with('+') {
                last.1 += 1;
            } else if line.starts_with('-') && !line.starts_with("--- ") {
                last.2 += 1;
            }
        }
    }
    if files.is_empty() || files.iter().all(|f| f.3 == 0) {
        return None;
    }
    let (adds, dels): (usize, usize) = files.iter().fold((0, 0), |(a, d), f| (a + f.1, d + f.2));
    let mut out = format!("[diff of {} files, +{adds} -{dels}; excerpt follows]", files.len());
    for (path, a, d, hunks) in files.iter().take(MAX_DIFF_FILES) {
        out.push_str(&format!("\n  {path} +{a} -{d} ({hunks} hunks)"));
    }
    if files.len() > MAX_DIFF_FILES {
        out.push_str(&format!("\n  … {} more files", files.len() - MAX_DIFF_FILES));
    }
    Some(out)
}

fn excerpt(lines: &[&str], budget: usize) -> String {
    let line_cap = MAX_LINE_IN_EXCERPT.max(budget / 6);
    let cost = |l: &str| l.len().min(line_cap) + 1;
    let mut head = 0;
    let mut used = 0;
    while head < lines.len() && used + cost(lines[head]) <= budget * 55 / 100 {
        used += cost(lines[head]);
        head += 1;
    }
    let mut tail = 0;
    let mut tail_used = 0;
    while tail < lines.len() - head && tail_used + cost(lines[lines.len() - 1 - tail]) <= budget * 30 / 100 {
        tail_used += cost(lines[lines.len() - 1 - tail]);
        tail += 1;
    }
    let middle_start = head;
    let middle_end = lines.len() - tail;
    let mut important_used = 0;
    let important: Vec<usize> = (middle_start..middle_end)
        .filter(|&i| IMPORTANT.is_match(lines[i]))
        .take_while(|&i| {
            important_used += cost(lines[i]) + 12;
            important_used <= budget * 15 / 100
        })
        .collect();

    let mut out = String::new();
    let push_line = |out: &mut String, line: &str| {
        out.push_str(&clip_line(line, line_cap));
        out.push('\n');
    };
    lines[..head].iter().for_each(|l| push_line(&mut out, l));
    let mut cursor = middle_start;
    for &i in &important {
        if i > cursor {
            out.push_str(&format!("[… {} lines omitted …]\n", i - cursor));
        }
        out.push_str(&format!("[line {}] ", i + 1));
        push_line(&mut out, lines[i]);
        cursor = i + 1;
    }
    if middle_end > cursor {
        out.push_str(&format!("[… {} lines omitted …]\n", middle_end - cursor));
    }
    lines[middle_end..].iter().for_each(|l| push_line(&mut out, l));
    out
}

fn collapse_repeats(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let mut repeats = 0;
        while lines.peek() == Some(&line) {
            lines.next();
            repeats += 1;
        }
        out.push_str(line);
        out.push('\n');
        if repeats > 0 {
            out.push_str(&format!("[previous line repeated {repeats} more times]\n"));
        }
    }
    if !text.ends_with('\n') {
        out.pop();
    }
    out
}

fn clip_line(line: &str, max: usize) -> String {
    if line.len() <= max {
        return line.to_string();
    }
    let cut = floor_char_boundary(line, max);
    format!("{}[… {} more bytes on this line]", &line[..cut], line.len() - cut)
}

fn clip_middle(text: &str, budget: usize) -> String {
    let head = floor_char_boundary(text, budget * 70 / 100);
    let tail_start = ceil_char_boundary(text, text.len() - budget * 20 / 100);
    format!("{}[… {} bytes omitted …]{}", &text[..head], tail_start - head, &text[tail_start..])
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

pub fn estimate_tokens(serialized_len: usize) -> usize {
    serialized_len.div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_untouched() {
        assert_eq!(smart_truncate("hello", 100), None);
    }

    #[test]
    fn repeats_collapsed() {
        let text = "start\n".to_string() + &"same\n".repeat(500) + "end\n";
        let out = smart_truncate(&text, 200).unwrap();
        assert_eq!(out, "start\nsame\n[previous line repeated 499 more times]\nend\n");
    }

    #[test]
    fn keeps_head_tail_and_errors() {
        let mut lines: Vec<String> = (0..2000).map(|i| format!("line {i} ok")).collect();
        lines[1000] = "error: something broke".into();
        let out = smart_truncate(&lines.join("\n"), 1000).unwrap();
        assert!(out.starts_with("line 0 ok\n"));
        assert!(out.contains("[line 1001] error: something broke"));
        assert!(out.trim_end().ends_with("line 1999 ok"));
        assert!(out.len() < 1300);
    }

    #[test]
    fn long_diffs_get_a_file_summary() {
        let mut diff = String::new();
        for f in 0..3 {
            diff.push_str(&format!("diff --git a/f{f}.rs b/f{f}.rs\n--- a/f{f}.rs\n+++ b/f{f}.rs\n@@ -1,3 +1,3 @@\n"));
            for i in 0..300 {
                diff.push_str(&format!("-old line {i}\n+new line {i}\n"));
            }
        }
        let out = smart_truncate(&diff, 2_000).unwrap();
        assert!(out.starts_with("[diff of 3 files, +900 -900; excerpt follows]\n  f0.rs +300 -300 (1 hunks)"));
    }

    #[test]
    fn list_capped() {
        let limits = Limits { max_items: 3, ..Limits::default() };
        let shaped = shape(Value::List((0..10).map(Value::Int).collect()), &limits);
        assert_eq!(shaped.total, Some(10));
        assert_eq!(shaped.shown, Some(3));
    }
}
