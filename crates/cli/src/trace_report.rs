use agentsh_lang::{Arg, Call, Expr, InterpPart, Pipeline, Program, Stage, Stmt};
use agentsh_spec::{Envelope, ErrorInfo};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

const LABEL_CHARS: usize = 90;

#[derive(Deserialize)]
pub struct TraceLine {
    pub session: String,
    pub program: String,
    pub envelope: Envelope,
}

#[derive(Serialize)]
pub struct FixPair<'a> {
    pub failed_program: &'a str,
    pub error: &'a ErrorInfo,
    pub fixed_program: &'a str,
}

pub fn load(path: &Path) -> Result<Vec<TraceLine>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    Ok(text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect())
}

pub fn fix_pairs(lines: &[TraceLine]) -> Vec<FixPair<'_>> {
    lines
        .windows(2)
        .filter(|w| w[0].session == w[1].session && !w[0].envelope.ok && w[1].envelope.ok)
        .filter_map(|w| {
            w[0].envelope.error.as_ref().map(|error| FixPair {
                failed_program: &w[0].program,
                error,
                fixed_program: &w[1].program,
            })
        })
        .collect()
}

pub fn write_pairs(pairs: &[FixPair], path: &Path) -> Result<(), String> {
    let mut file = std::fs::File::create(path).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    for pair in pairs {
        let line = serde_json::to_string(pair).map_err(|e| e.to_string())?;
        writeln!(file, "{line}").map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn report(lines: &[TraceLine], top: usize) -> String {
    let calls = lines.len();
    let failed: Vec<&TraceLine> = lines.iter().filter(|l| !l.envelope.ok).collect();
    let tokens: usize = lines.iter().map(|l| l.envelope.tokens_est).sum();
    let sessions: std::collections::BTreeSet<&str> = lines.iter().map(|l| l.session.as_str()).collect();
    let mut out = vec![
        format!(
            "calls {calls}, failed {} ({}%), sessions {}",
            failed.len(),
            percent(failed.len(), calls),
            sessions.len()
        ),
        format!("output tokens ≈ {tokens} (avg {} per call)", tokens.checked_div(calls).unwrap_or(0)),
    ];
    out.push(section(
        "failures by code",
        counted(failed.iter().filter_map(|l| l.envelope.error.as_ref()).map(|e| e.code.as_str())),
        top,
    ));
    out.push(section("commands used", counted(lines.iter().flat_map(|l| command_names(&l.program))), top));
    let mut expensive: Vec<&TraceLine> = lines.iter().collect();
    expensive.sort_by_key(|l| std::cmp::Reverse(l.envelope.tokens_est));
    let costly: Vec<String> =
        expensive.iter().take(top).map(|l| format!("  {:>6}t  {}", l.envelope.tokens_est, label(&l.program))).collect();
    out.push(format!("most expensive calls\n{}", costly.join("\n")));
    let pairs = fix_pairs(lines);
    let fixes: Vec<String> = pairs
        .iter()
        .take(top)
        .map(|p| {
            format!(
                "  {}: {}\n    failed: {}\n    fixed:  {}",
                p.error.code.as_str(),
                clip(&p.error.message),
                label(p.failed_program),
                label(p.fixed_program)
            )
        })
        .collect();
    out.push(format!(
        "failed then fixed ({} pairs; these hints and errors cost a retry)\n{}",
        pairs.len(),
        fixes.join("\n")
    ));
    out.join("\n\n")
}

fn percent(part: usize, whole: usize) -> usize {
    (part * 100).checked_div(whole).unwrap_or(0)
}

fn counted<'a>(items: impl Iterator<Item = impl Into<String> + 'a>) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for item in items {
        *counts.entry(item.into()).or_default() += 1;
    }
    let mut sorted: Vec<(String, usize)> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    sorted
}

fn section(title: &str, counts: Vec<(String, usize)>, top: usize) -> String {
    if counts.is_empty() {
        return format!("{title}: none");
    }
    let parts: Vec<String> = counts.iter().take(top).map(|(k, n)| format!("{k} {n}")).collect();
    format!("{title}: {}", parts.join(", "))
}

fn label(program: &str) -> String {
    clip(&program.trim().replace('\n', " ⏎ "))
}

fn clip(text: &str) -> String {
    if text.chars().count() <= LABEL_CHARS {
        return text.to_string();
    }
    format!("{}…", text.chars().take(LABEL_CHARS).collect::<String>())
}

fn command_names(program: &str) -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(parsed) = agentsh_lang::parse(program) {
        collect_program(&parsed, &mut names);
    }
    names
}

fn collect_program(program: &Program, names: &mut Vec<String>) {
    for stmt in &program.stmts {
        match stmt {
            Stmt::Let { value, .. } => collect_pipeline(value, names),
            Stmt::If { cond, then, otherwise, .. } => {
                collect_expr(cond, names);
                collect_program(then, names);
                if let Some(other) = otherwise {
                    collect_program(other, names);
                }
            }
            Stmt::For { iter, body, .. } => {
                collect_expr(iter, names);
                collect_program(body, names);
            }
            Stmt::Try { body, handler, .. } => {
                collect_program(body, names);
                if let Some(h) = handler {
                    collect_program(h, names);
                }
            }
            Stmt::Pipeline(p) => collect_pipeline(p, names),
        }
    }
}

fn collect_pipeline(pipeline: &Pipeline, names: &mut Vec<String>) {
    match &pipeline.head {
        Stage::Call(call) => collect_call(call, names),
        Stage::Expr(expr, _) => collect_expr(expr, names),
    }
    for call in &pipeline.rest {
        collect_call(call, names);
    }
}

fn collect_call(call: &Call, names: &mut Vec<String>) {
    names.push(call.name.clone());
    for arg in &call.args {
        match arg {
            Arg::Positional(e) | Arg::Named(_, e) => collect_expr(e, names),
        }
    }
}

fn collect_expr(expr: &Expr, names: &mut Vec<String>) {
    match expr {
        Expr::Sub(p) => collect_pipeline(p, names),
        Expr::List(items) | Expr::Func(_, items, _) => items.iter().for_each(|e| collect_expr(e, names)),
        Expr::Record(entries) => entries.iter().for_each(|(_, e)| collect_expr(e, names)),
        Expr::Binary(l, _, r) => {
            collect_expr(l, names);
            collect_expr(r, names);
        }
        Expr::Unary(_, e) => collect_expr(e, names),
        Expr::Interp(parts) => parts.iter().for_each(|p| {
            if let InterpPart::Expr(e) = p {
                collect_expr(e, names);
            }
        }),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_nested_commands() {
        assert_eq!(
            command_names("let x = (grep \"a\" | count)\nfor $f in glob(\"*\") { read $f }"),
            vec!["grep", "count", "read"]
        );
    }
}
