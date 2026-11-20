use agentsh_spec::{Effect, EffectOp, Envelope, Value};

const MAX_CELL: usize = 120;

pub fn render_text(envelope: &Envelope) -> String {
    let mut out = String::new();
    for output in &envelope.outputs {
        out.push_str(&format!("─── {}\n", short_label(&output.statement)));
        out.push_str(render_value(&output.value).trim_end());
        out.push('\n');
        if output.truncated {
            out.push_str("[shortened]\n");
        }
    }
    if let Some(label) = envelope.label.as_ref().filter(|_| !envelope.outputs.is_empty()) {
        out.push_str(&format!("─── {}\n", short_label(label)));
    }
    match &envelope.error {
        Some(err) => {
            out.push_str(&format!("error {}: {}\n", err.code.as_str(), err.message));
            if let Some(value) = &err.value {
                out.push_str(&format!("value: {value}\n"));
            }
            if let Some(hint) = &err.hint {
                out.push_str(&format!("hint: {hint}\n"));
            }
            if let Some(span) = err.span {
                let at = err.command.as_deref().map(|c| format!(" in {c}")).unwrap_or_default();
                out.push_str(&format!("at: line {} col {}{at}\n", span.line, span.col));
            }
            if err.rolled_back {
                match err.restored.as_slice() {
                    [] => out.push_str("rolled back: this run's file changes were undone\n"),
                    paths => out.push_str(&format!("rolled back: undid this run's changes to {}\n", paths.join(", "))),
                }
            }
            if err.kept {
                out.push_str(
                    "kept: this run's file changes stay in place; fix them, or undo them with the token below\n",
                );
            }
        }
        None => {
            if let Some(value) = &envelope.value {
                out.push_str(&render_value(value));
                if !out.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
    }
    for printed in &envelope.printed {
        out.push_str(&format!("print: {}\n", inline(printed)));
    }
    if let (Some(shown), Some(total)) = (envelope.shown, envelope.total) {
        out.push_str(&format!("[showing {shown} of {total}]"));
        if let Some(handle) = &envelope.handle {
            out.push_str(&format!(" full result: {handle}"));
        }
        out.push('\n');
    } else if envelope.truncated {
        let handle = envelope.handle.as_deref().map(|h| format!("; full result: {h}")).unwrap_or_default();
        out.push_str(&format!("[shortened{handle}]\n"));
    }
    if !envelope.effects.is_empty() {
        let undo = envelope.undo.as_deref().map(|u| format!(" (undo {u})")).unwrap_or_default();
        let listed = envelope.effects.len() > 1 && envelope.effects.iter().all(|e| out.contains(&e.path));
        let effects =
            if listed { format!("{} files changed", envelope.effects.len()) } else { group_effects(&envelope.effects) };
        out.push_str(&format!("effects: {effects}{undo}\n"));
    } else if let Some(undo) = &envelope.undo {
        out.push_str(&format!("undo: {undo}\n"));
    }
    for warning in &envelope.warnings {
        out.push_str(&format!("warning: {warning}\n"));
    }
    if out.is_empty() {
        out.push_str(if envelope.ok { "ok\n" } else { "failed\n" });
    }
    out
}

fn short_label(statement: &str) -> String {
    match statement.split_once(' ') {
        Some((line, rest)) if line.starts_with('[') => {
            let command: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$').collect();
            format!("{line} {command}")
        }
        _ => statement.to_string(),
    }
}

fn group_effects(effects: &[Effect]) -> String {
    let mut groups: Vec<(&'static str, Vec<String>)> = Vec::new();
    for effect in effects {
        let op = match effect.op {
            EffectOp::Create => "created",
            EffectOp::Modify => "modified",
            EffectOp::Delete => "deleted",
            EffectOp::Move => "moved",
            EffectOp::Mkdir => "mkdir",
        };
        let item = match &effect.to {
            Some(to) => format!("{} -> {to}", effect.path),
            None => effect.path.clone(),
        };
        match groups.iter_mut().find(|(o, _)| *o == op) {
            Some((_, items)) => items.push(item),
            None => groups.push((op, vec![item])),
        }
    }
    groups.into_iter().map(|(op, items)| format!("{op} {}", items.join(", "))).collect::<Vec<_>>().join("; ")
}

fn render_value(value: &Value) -> String {
    match value {
        Value::Str(s) => s.clone(),
        Value::List(items) if items.is_empty() => "(empty list)".into(),
        Value::List(items) => match table_columns(items) {
            Some(_) if items.iter().any(has_block) => render_blocks(items),
            Some(columns) => render_table(items, &columns),
            None => items.iter().map(inline).collect::<Vec<_>>().join("\n"),
        },
        Value::Record(record) => render_record(record),
        other => inline(other),
    }
}

fn is_block(value: &Value) -> bool {
    matches!(value, Value::Str(s) if s.contains('\n') || s.chars().count() > MAX_CELL)
}

fn has_block(item: &Value) -> bool {
    matches!(item, Value::Record(r) if r.values().any(is_block))
}

fn is_blank(value: &Value) -> bool {
    matches!(value, Value::Null)
}

fn render_record(record: &agentsh_spec::Record) -> String {
    let width = record.iter().filter(|(_, v)| !is_blank(v) && !is_block(v)).map(|(k, _)| k.len()).max().unwrap_or(0);
    let mut lines: Vec<String> = record
        .iter()
        .filter(|(_, v)| !is_blank(v) && !is_block(v))
        .map(|(k, v)| format!("{k:width$}  {}", inline(v)))
        .collect();
    for (k, v) in record.iter().filter(|(_, v)| is_block(v)) {
        lines.push(format!("{k}:"));
        lines.push(inline(v).trim_end_matches('\n').to_string());
    }
    lines.join("\n")
}

fn render_blocks(items: &[Value]) -> String {
    let mut out = Vec::new();
    for item in items {
        let Value::Record(record) = item else { continue };
        let fields: Vec<(&String, &Value)> = record.iter().filter(|(_, v)| !is_blank(v) && !is_block(v)).collect();
        let header = match fields.as_slice() {
            [(k, v)] if k.as_str() == "path" => inline(v),
            _ => fields.iter().map(|(k, v)| format!("{k}: {}", inline(v))).collect::<Vec<_>>().join("  "),
        };
        out.push(format!("=== {header}"));
        let blocks: Vec<(&String, &Value)> = record.iter().filter(|(_, v)| is_block(v)).collect();
        for (k, v) in &blocks {
            if blocks.len() > 1 {
                out.push(format!("{k}:"));
            }
            out.push(inline(v).trim_end_matches('\n').to_string());
        }
    }
    out.join("\n")
}

fn table_columns(items: &[Value]) -> Option<Vec<String>> {
    let mut columns: Vec<String> = Vec::new();
    for item in items {
        let Value::Record(record) = item else { return None };
        for key in record.keys() {
            if !columns.contains(key) {
                columns.push(key.clone());
            }
        }
    }
    Some(columns)
}

const MIN_ROWS_TO_HOIST: usize = 4;

fn render_table(items: &[Value], columns: &[String]) -> String {
    let rows: Vec<Vec<String>> = items
        .iter()
        .map(|item| {
            columns
                .iter()
                .map(|c| match item {
                    Value::Record(r) => r.get(c).map(cell).unwrap_or_default(),
                    _ => String::new(),
                })
                .collect()
        })
        .collect();
    let constant: Vec<usize> = if rows.len() >= MIN_ROWS_TO_HOIST && columns.len() > 1 {
        (0..columns.len()).filter(|&i| rows.iter().all(|r| r[i] == rows[0][i])).collect()
    } else {
        Vec::new()
    };
    let keep: Vec<usize> = (0..columns.len()).filter(|i| !constant.contains(i)).collect();
    let line = |cells: Vec<&str>| keep.iter().map(|&i| cells[i]).collect::<Vec<_>>().join("\t");
    let mut out = vec![line(columns.iter().map(String::as_str).collect())];
    out.extend(rows.iter().map(|r| line(r.iter().map(String::as_str).collect())));
    if !constant.is_empty() {
        let same: Vec<String> = constant.iter().map(|&i| format!("{}={}", columns[i], rows[0][i])).collect();
        out.push(format!("(every row: {})", same.join(", ")));
    }
    out.join("\n")
}

fn cell(value: &Value) -> String {
    let text = match value {
        Value::Null => "-".into(),
        Value::Str(s) => relative_age(s).unwrap_or_else(|| s.clone()),
        other => inline(other),
    };
    let flat = text.replace('\n', "\\n").replace('\t', "\\t");
    if flat.chars().count() <= MAX_CELL {
        return flat;
    }
    let clipped: String = flat.chars().take(MAX_CELL).collect();
    format!("{clipped}…")
}

fn relative_age(text: &str) -> Option<String> {
    if text.len() != 20 || !text.ends_with('Z') || text.as_bytes().get(10) != Some(&b'T') {
        return None;
    }
    let time = chrono::DateTime::parse_from_rfc3339(text).ok()?;
    let secs = (chrono::Utc::now() - time.with_timezone(&chrono::Utc)).num_seconds();
    Some(match secs {
        s if s < 0 => text[..10].to_string(),
        s if s < 60 => "now".into(),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s if s < 45 * 86_400 => format!("{}d", s / 86_400),
        _ => text[..10].to_string(),
    })
}

fn inline(value: &Value) -> String {
    match value {
        Value::Str(s) if s.is_empty() => "\"\"".into(),
        Value::Str(s) => s.clone(),
        Value::Null => "null".into(),
        Value::List(items) if items.iter().all(is_scalar) => {
            items.iter().map(Value::to_display_string).collect::<Vec<_>>().join(", ")
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn is_scalar(value: &Value) -> bool {
    matches!(value, Value::Str(_) | Value::Int(_) | Value::Float(_) | Value::Bool(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentsh_spec::ErrorCode;

    #[test]
    fn constant_columns_are_hoisted() {
        let rows = (0..4)
            .map(|i| Value::record([("path", Value::Str(format!("f{i}"))), ("type", Value::str("file"))]))
            .collect();
        let envelope = Envelope { ok: true, value: Some(Value::List(rows)), ..Envelope::default() };
        assert_eq!(render_text(&envelope), "path\nf0\nf1\nf2\nf3\n(every row: type=file)\n");
    }

    #[test]
    fn timestamps_render_as_ages() {
        let two_hours = (chrono::Utc::now() - chrono::Duration::hours(2)).format("%Y-%m-%dT%H:%M:%SZ").to_string();
        assert_eq!(relative_age(&two_hours).as_deref(), Some("2h"));
        assert_eq!(relative_age("2020-01-02T03:04:05Z").as_deref(), Some("2020-01-02"));
        assert_eq!(relative_age("not a time"), None);
    }

    #[test]
    fn table_has_header_once() {
        let envelope = Envelope {
            ok: true,
            value: Some(Value::List(vec![
                Value::record([("path", Value::str("a.rs")), ("size", Value::Int(10))]),
                Value::record([("path", Value::str("bb.rs")), ("size", Value::Null)]),
            ])),
            ..Envelope::default()
        };
        assert_eq!(render_text(&envelope), "path\tsize\na.rs\t10\nbb.rs\t-\n");
    }

    #[test]
    fn multiline_records_render_as_blocks() {
        let envelope = Envelope {
            ok: true,
            value: Some(Value::List(vec![
                Value::record([("path", Value::str("a.txt")), ("content", Value::str("one\ntwo\n"))]),
                Value::record([("path", Value::str("b.txt")), ("content", Value::str("three\n"))]),
            ])),
            ..Envelope::default()
        };
        assert_eq!(render_text(&envelope), "=== a.txt\none\ntwo\n=== b.txt\nthree\n");
    }

    #[test]
    fn errors_show_code_hint_and_rollback() {
        let envelope = Envelope {
            ok: false,
            error: Some(agentsh_spec::ErrorInfo {
                code: ErrorCode::NotFound,
                message: "`x` does not exist".into(),
                arg: None,
                value: None,
                hint: Some("did you mean \"y\"?".into()),
                command: Some("read".into()),
                span: Some(agentsh_spec::Span { line: 2, col: 1 }),
                rolled_back: true,
                kept: false,
                restored: Vec::new(),
            }),
            ..Envelope::default()
        };
        assert_eq!(
            render_text(&envelope),
            "error not_found: `x` does not exist\nhint: did you mean \"y\"?\nat: line 2 col 1 in read\nrolled back: this run's file changes were undone\n"
        );
    }
}
