use agentsh_core::{Args, Builtin, CommandSpec, Ctx, ParamSpec as P, ParamType as T, Result, ShellError};
use agentsh_lang::Expr;
use agentsh_spec::{ErrorCode, Record, Value};
use std::cmp::Ordering;
use std::collections::HashSet;

const LIST_HINT: &str = "this works on lists; split text into lines first: ... | lines | ...";

fn input_list(args: &mut Args, command: &str) -> Result<Vec<Value>> {
    match args.take("input") {
        Some(Value::List(items)) => Ok(items),
        Some(Value::Null) | None => Ok(Vec::new()),
        Some(other) => Err(ShellError::type_error(&format!("{command} input"), "a list", &other).with_hint(LIST_HINT)),
    }
}

pub const WHERE: Builtin = Builtin {
    spec: CommandSpec {
        name: "where",
        summary: "keep rows where the condition is true (.field refers to the row)",
        params: &[
            P::pos("cond", T::Expr, "condition, e.g. .size > 1mb and .path =~ \"test\"").required(),
            P::named("input", T::List, "rows"),
        ],
        input: Some("input"),
        returns: "list",
        caps: &[],
        examples: &["find \".\" | where .type == \"file\" and .modified > ago(1d)"],
    },
    run: where_,
};

fn where_(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let cond = args.expr("cond").cloned().ok_or_else(|| ShellError::internal("where: missing cond"))?;
    let mut kept = Vec::new();
    for row in input_list(args, "where")? {
        if ctx.eval(&cond, Some(&row))?.is_truthy() {
            kept.push(row);
        }
    }
    Ok(Value::List(kept))
}

pub const SORT: Builtin = Builtin {
    spec: CommandSpec {
        name: "sort",
        summary: "sort rows by a key (default: the value itself); nulls last",
        params: &[
            P::pos("key", T::Expr, "sort key, e.g. .size"),
            P::named("desc", T::Bool, "descending"),
            P::named("input", T::List, "rows"),
        ],
        input: Some("input"),
        returns: "list",
        caps: &[],
        examples: &["ls | sort .size desc:true", "read \"names.txt\" | lines | sort"],
    },
    run: sort,
};

fn sort(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let key = args.expr("key").cloned();
    let desc = args.flag("desc")?;
    let rows = input_list(args, "sort")?;
    let mut keyed = Vec::with_capacity(rows.len());
    for row in rows {
        let k = match &key {
            Some(expr) => ctx.eval(expr, Some(&row))?,
            None => row.clone(),
        };
        keyed.push((k, row));
    }
    keyed.sort_by(|(a, _), (b, _)| {
        let ord = match (a, b) {
            (Value::Null, Value::Null) => Ordering::Equal,
            (Value::Null, _) => return Ordering::Greater,
            (_, Value::Null) => return Ordering::Less,
            _ => a.compare(b).unwrap_or(Ordering::Equal),
        };
        if desc { ord.reverse() } else { ord }
    });
    Ok(Value::List(keyed.into_iter().map(|(_, row)| row).collect()))
}

pub const TAKE: Builtin = Builtin {
    spec: CommandSpec {
        name: "take",
        summary: "first N rows",
        params: &[P::pos("n", T::Int, "how many").required(), P::named("input", T::List, "rows")],
        input: Some("input"),
        returns: "list",
        caps: &[],
        examples: &["ls | sort .modified desc:true | take 5"],
    },
    run: take,
};

fn take(_ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let n = args.opt_usize("n")?.unwrap_or(0);
    Ok(Value::List(input_list(args, "take")?.into_iter().take(n).collect()))
}

pub const SKIP: Builtin = Builtin {
    spec: CommandSpec {
        name: "skip",
        summary: "drop the first N rows",
        params: &[P::pos("n", T::Int, "how many").required(), P::named("input", T::List, "rows")],
        input: Some("input"),
        returns: "list",
        caps: &[],
        examples: &["$r3 | skip 50 | take 50"],
    },
    run: skip,
};

fn skip(_ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let n = args.opt_usize("n")?.unwrap_or(0);
    Ok(Value::List(input_list(args, "skip")?.into_iter().skip(n).collect()))
}

pub const FIRST: Builtin = Builtin {
    spec: CommandSpec {
        name: "first",
        summary: "the first row (or null)",
        params: &[P::named("input", T::List, "rows")],
        input: Some("input"),
        returns: "value",
        caps: &[],
        examples: &["find \".\" name:\"Cargo.toml\" | first"],
    },
    run: first,
};

fn first(_ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    Ok(input_list(args, "first")?.into_iter().next().unwrap_or_default())
}

pub const LAST: Builtin = Builtin {
    spec: CommandSpec {
        name: "last",
        summary: "the last row (or null)",
        params: &[P::named("input", T::List, "rows")],
        input: Some("input"),
        returns: "value",
        caps: &[],
        examples: &["ls | sort .modified | last"],
    },
    run: last,
};

fn last(_ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    Ok(input_list(args, "last")?.into_iter().last().unwrap_or_default())
}

pub const PICK: Builtin = Builtin {
    spec: CommandSpec {
        name: "pick",
        summary: "keep only some fields of each row (or of one record)",
        params: &[
            P::pos("fields", T::Expr, "fields, e.g. .path .size").required().variadic(),
            P::named("input", T::Any, "rows or a record"),
        ],
        input: Some("input"),
        returns: "list of records (or a record)",
        caps: &[],
        examples: &["find \"src\" | pick .path .size"],
    },
    run: pick,
};

fn pick(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let fields = args.exprs("fields");
    match args.take("input").unwrap_or_default() {
        Value::List(rows) => {
            rows.iter().map(|row| pick_row(ctx, &fields, row)).collect::<Result<Vec<_>>>().map(Value::List)
        }
        row @ Value::Record(_) => pick_row(ctx, &fields, &row),
        other => Err(ShellError::type_error("pick input", "rows or a record", &other)),
    }
}

fn pick_row(ctx: &mut Ctx, fields: &[Expr], row: &Value) -> Result<Value> {
    let mut out = Record::new();
    for (i, field) in fields.iter().enumerate() {
        let name = match field {
            Expr::Field(path) => path.last().cloned().unwrap_or_default(),
            Expr::Lit(Value::Str(s)) => s.clone(),
            _ => format!("value{}", i + 1),
        };
        let value = match field {
            Expr::Lit(Value::Str(s)) => match row {
                Value::Record(r) => r.get(s).cloned().unwrap_or_default(),
                _ => Value::Null,
            },
            other => ctx.eval(other, Some(row))?,
        };
        out.insert(name, value);
    }
    Ok(Value::Record(out))
}

pub const UNIQ: Builtin = Builtin {
    spec: CommandSpec {
        name: "uniq",
        summary: "remove duplicate rows (keeps first occurrence)",
        params: &[
            P::pos("key", T::Expr, "compare by this key instead of the whole row"),
            P::named("input", T::List, "rows"),
        ],
        input: Some("input"),
        returns: "list",
        caps: &[],
        examples: &["grep \"TODO\" | uniq .path | pick .path"],
    },
    run: uniq,
};

fn uniq(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let key = args.expr("key").cloned();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for row in input_list(args, "uniq")? {
        let k = match &key {
            Some(expr) => ctx.eval(expr, Some(&row))?,
            None => row.clone(),
        };
        if seen.insert(serde_json::to_string(&k).unwrap_or_default()) {
            out.push(row);
        }
    }
    Ok(Value::List(out))
}

pub const GROUP: Builtin = Builtin {
    spec: CommandSpec {
        name: "group",
        summary: "count rows per key, most common first (like sort | uniq -c)",
        params: &[
            P::pos("key", T::Expr, "grouping key, e.g. ext(.path) or .type").required(),
            P::named("rows", T::Bool, "include the grouped rows"),
            P::named("input", T::List, "rows"),
        ],
        input: Some("input"),
        returns: "list of {key, count} (plus rows with rows:true)",
        caps: &[],
        examples: &["find \".\" type:\"file\" | group ext(.path)", "grep \"TODO\" | group .path"],
    },
    run: group,
};

fn group(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let key = args.expr("key").cloned().ok_or_else(|| ShellError::internal("group: missing key"))?;
    let with_rows = args.flag("rows")?;
    let mut groups: Vec<(Value, Vec<Value>)> = Vec::new();
    let mut index = std::collections::HashMap::new();
    for row in input_list(args, "group")? {
        let k = ctx.eval(&key, Some(&row))?;
        let id = serde_json::to_string(&k).unwrap_or_default();
        let slot = *index.entry(id).or_insert_with(|| {
            groups.push((k, Vec::new()));
            groups.len() - 1
        });
        groups[slot].1.push(row);
    }
    groups.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.compare(&b.0).unwrap_or(Ordering::Equal)));
    Ok(Value::List(
        groups
            .into_iter()
            .map(|(k, rows)| {
                let mut record = vec![("key", k), ("count", Value::Int(rows.len() as i64))];
                if with_rows {
                    record.push(("rows", Value::List(rows)));
                }
                Value::record(record)
            })
            .collect(),
    ))
}

pub const SUM: Builtin = Builtin {
    spec: CommandSpec {
        name: "sum",
        summary: "add up numbers (or a numeric field of each row)",
        params: &[P::pos("key", T::Expr, "field to sum, e.g. .size"), P::named("input", T::List, "rows")],
        input: Some("input"),
        returns: "number",
        caps: &[],
        examples: &["find \"src\" | sum .size"],
    },
    run: sum,
};

fn sum(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let key = args.expr("key").cloned();
    let (mut int_total, mut float_total, mut is_float) = (0i64, 0f64, false);
    for row in input_list(args, "sum")? {
        let v = match &key {
            Some(expr) => ctx.eval(expr, Some(&row))?,
            None => row,
        };
        match v {
            Value::Int(i) => int_total = int_total.saturating_add(i),
            Value::Float(f) => {
                float_total += f;
                is_float = true;
            }
            Value::Null => {}
            other => return Err(ShellError::type_error("sum", "numbers", &other)),
        }
    }
    Ok(if is_float { Value::Float(float_total + int_total as f64) } else { Value::Int(int_total) })
}

pub const JSON: Builtin = Builtin {
    spec: CommandSpec {
        name: "json",
        summary: "parse piped JSON text into a value",
        params: &[P::named("input", T::Str, "JSON text").required()],
        input: Some("input"),
        returns: "value",
        caps: &[],
        examples: &["read \"package.json\" | json | pick .name .version"],
    },
    run: json,
};

fn json(_ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let text = args.str("input")?;
    serde_json::from_str::<serde_json::Value>(&text).map(Value::from).map_err(|e| {
        ShellError::new(
            ErrorCode::InvalidValue,
            format!("invalid JSON at line {} column {}: {e}", e.line(), e.column()),
        )
    })
}

pub const FAIL: Builtin = Builtin {
    spec: CommandSpec {
        name: "fail",
        summary: "stop the run with an error (expectation_failed); its file changes roll back",
        params: &[
            P::pos("message", T::Str, "what went wrong").required(),
            P::named("hint", T::Str, "what to do about it"),
            P::named("value", T::Any, "details to show with the error"),
        ],
        input: None,
        returns: "never returns",
        caps: &[],
        examples: &["if len($rows) == 0 { fail \"no rows\" hint:\"check the filter\" }"],
    },
    run: fail,
};

fn fail(_ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    let mut err = ShellError::new(ErrorCode::ExpectationFailed, args.str("message")?);
    if let Some(hint) = args.opt_str("hint")? {
        err = err.with_hint(hint);
    }
    if let Some(value) = args.take("value") {
        err = err.with_value(value.to_display_string());
    }
    Err(err)
}

pub const PRINT: Builtin = Builtin {
    spec: CommandSpec {
        name: "print",
        summary: "add values to the envelope's printed list; with piped input, prints it and passes it on",
        params: &[
            P::pos("values", T::Any, "values to print").variadic(),
            P::named("input", T::Any, "value to print and pass through"),
        ],
        input: Some("input"),
        returns: "null, or the piped input",
        caps: &[],
        examples: &["print \"checked $n files\"", "ls | print | count"],
    },
    run: print,
};

fn print(ctx: &mut Ctx, args: &mut Args) -> Result<Value> {
    if let Some(Value::List(values)) = args.take("values") {
        ctx.printed.extend(values);
    }
    match args.take("input") {
        Some(input) => {
            ctx.printed.push(input.clone());
            Ok(input)
        }
        None => Ok(Value::Null),
    }
}
