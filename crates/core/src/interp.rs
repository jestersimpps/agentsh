use crate::args::ArgVal;
use crate::registry::{Builtin, CommandSpec, ParamSpec, ParamType};
use crate::suggest::{bash_equivalent, closest};
use crate::{Args, Ctx, Result, ShellError};
use agentsh_lang::{Accessor, Arg, BinOp, Call, Expr, InterpPart, Pipeline, Program, Stage, Stmt, UnOp};
use agentsh_platform::Capability;
use agentsh_spec::{ErrorCode, Value};
use regex::Regex;
use std::cmp::Ordering;

impl Ctx<'_> {
    pub fn run_program(&mut self, program: &Program) -> Result<Value> {
        let mut last = Value::Null;
        for stmt in &program.stmts {
            last = self.run_stmt(stmt)?;
        }
        Ok(last)
    }

    fn run_stmt(&mut self, stmt: &Stmt) -> Result<Value> {
        match stmt {
            Stmt::Let { name, value, .. } => {
                let v = self.run_pipeline(value)?;
                self.state.vars.insert(name.clone(), v);
                Ok(Value::Null)
            }
            Stmt::If { cond, then, otherwise, span } => {
                let c = self.eval(cond, None).map_err(|e| e.with_command_if_missing("if", *span))?;
                match (c.is_truthy(), otherwise) {
                    (true, _) => self.run_program(then),
                    (false, Some(other)) => self.run_program(other),
                    (false, None) => Ok(Value::Null),
                }
            }
            Stmt::For { var, iter, body, span } => self.run_for(var, iter, body, *span),
            Stmt::Try { body, catch_var, handler, .. } => self.run_try(body, catch_var.as_deref(), handler.as_ref()),
            Stmt::Pipeline(p) => self.run_pipeline(p),
        }
    }

    fn run_for(&mut self, var: &str, iter: &Expr, body: &Program, span: agentsh_spec::Span) -> Result<Value> {
        let items = match self.eval(iter, None).map_err(|e| e.with_command_if_missing("for", span))? {
            Value::List(items) => items,
            Value::Null => Vec::new(),
            other => {
                return Err(ShellError::type_error("the value after `in`", "a list", &other)
                    .with_hint("split text into a list with: \"...\" | lines"));
            }
        };
        let mut results = Vec::with_capacity(items.len());
        for item in items {
            self.state.vars.insert(var.to_string(), item);
            results.push(self.run_program(body)?);
        }
        if results.iter().all(|v| *v == Value::Null) { Ok(Value::Null) } else { Ok(Value::List(results)) }
    }

    fn run_try(&mut self, body: &Program, catch_var: Option<&str>, handler: Option<&Program>) -> Result<Value> {
        let journal_mark = self.journal.len();
        let effects_mark = self.effects.len();
        match self.run_program(body) {
            Ok(v) => Ok(v),
            Err(e) if matches!(e.code, ErrorCode::Internal | ErrorCode::SessionBusy) => Err(e),
            Err(e) => {
                self.rollback_since(journal_mark)?;
                self.effects.truncate(effects_mark);
                if let Some(var) = catch_var {
                    self.state.vars.insert(var.to_string(), e.to_value());
                }
                match handler {
                    Some(h) => self.run_program(h),
                    None => Ok(Value::Null),
                }
            }
        }
    }

    pub fn rollback_since(&mut self, mark: usize) -> Result<Vec<String>> {
        let actions = self.journal.undo_where(|i, _| i >= mark, true)?;
        let mut restored = Vec::new();
        for action in actions {
            if self.state.seen.contains_key(&action.path) {
                self.mark_seen(&action.path.clone());
            }
            let shown = self.display(&action.path);
            if !restored.contains(&shown) {
                restored.push(shown);
            }
        }
        Ok(restored)
    }

    pub fn run_top_level(&mut self, program: &Program, source: &str) -> Result<Value> {
        let count = program.stmts.len();
        let mut last = Value::Null;
        for (i, stmt) in program.stmts.iter().enumerate() {
            let value = self.run_stmt(stmt)?;
            let label = statement_label(stmt, source);
            if i + 1 < count {
                if value != Value::Null {
                    self.outputs.push((label, value));
                }
            } else {
                self.last_label = Some(label);
                last = value;
            }
        }
        Ok(last)
    }

    pub fn run_pipeline(&mut self, pipeline: &Pipeline) -> Result<Value> {
        let mut value = match &pipeline.head {
            Stage::Call(call) => self.run_call(call, None)?,
            Stage::Expr(expr, _) => self.eval(expr, None)?,
        };
        for call in &pipeline.rest {
            value = self.run_call(call, Some(value))?;
        }
        Ok(value)
    }

    fn run_call(&mut self, call: &Call, input: Option<Value>) -> Result<Value> {
        let builtin = *self
            .registry
            .get(&call.name)
            .ok_or_else(|| self.unknown_command(call).with_command_if_missing(&call.name, call.span))?;
        self.call_builtin(builtin, call, input).map_err(|e| e.with_command_if_missing(&call.name, call.span))
    }

    fn call_builtin(&mut self, builtin: Builtin, call: &Call, input: Option<Value>) -> Result<Value> {
        for cap in builtin.spec.caps {
            self.require(*cap)?;
        }
        let mut args = self.bind(&builtin.spec, call, input)?;
        (builtin.run)(self, &mut args)
    }

    fn unknown_command(&self, call: &Call) -> ShellError {
        let name = call.name.as_str();
        let err = ShellError::new(ErrorCode::UnknownCommand, format!("unknown command `{name}`")).with_value(name);
        if let Some(equivalent) = bash_equivalent(name) {
            return err.with_hint(format!("this isn't bash; use: {equivalent}"));
        }
        if let Some(best) = closest(name, self.registry.names()) {
            return err.with_hint(format!("did you mean `{best}`?"));
        }
        if on_path(name) {
            let note = if self.config.allows(Capability::Exec) { "" } else { " (needs the exec capability)" };
            return err.with_hint(match plain_words(call) {
                Some(line) => format!("`{name}` is an external program; run it with: exec '{line}'{note}"),
                None => format!("run external programs with exec: exec \"{name}\" \"arg\" ...{note}"),
            });
        }
        err.with_hint("run `help` to list commands")
    }

    fn bind(&mut self, spec: &CommandSpec, call: &Call, input: Option<Value>) -> Result<Args> {
        let mut args = Args::default();
        let positional: Vec<&ParamSpec> = spec.params.iter().filter(|p| p.positional).collect();
        let mut next_positional = 0;
        let mut variadic: Vec<(&ParamSpec, Vec<&Expr>)> = Vec::new();
        for arg in &call.args {
            let (param, expr) = match arg {
                Arg::Positional(expr) => {
                    let Some(param) = positional.get(next_positional).copied() else {
                        return Err(ShellError::new(
                            ErrorCode::UnknownArgument,
                            format!("too many positional arguments for `{}`", spec.name),
                        )
                        .with_hint(format!("usage: {}", spec.usage())));
                    };
                    if !param.variadic {
                        next_positional += 1;
                    }
                    (param, expr)
                }
                Arg::Named(name, expr) => {
                    let param = spec.param(name).ok_or_else(|| unknown_argument(spec, name))?;
                    if args.has(param.name) && !param.variadic {
                        return Err(ShellError::new(ErrorCode::InvalidValue, format!("`{name}` given twice"))
                            .with_arg(name.as_str()));
                    }
                    (param, expr)
                }
            };
            if param.variadic {
                match variadic.iter_mut().find(|(p, _)| p.name == param.name) {
                    Some((_, exprs)) => exprs.push(expr),
                    None => variadic.push((param, vec![expr])),
                }
                continue;
            }
            let val = self.bind_one(param, expr)?;
            args.insert(param.name, val);
        }
        for (param, exprs) in variadic {
            let val = self.bind_variadic(param, &exprs)?;
            args.insert(param.name, val);
        }
        if let Some(value) = input {
            self.bind_input(spec, &mut args, value)?;
        }
        if let Some(missing) = spec.params.iter().find(|p| p.required && !args.has(p.name)) {
            let mut err =
                ShellError::new(ErrorCode::MissingArgument, format!("`{}` needs `{}`", spec.name, missing.name))
                    .with_arg(missing.name)
                    .with_hint(format!("usage: {}", spec.usage()));
            if let Some(example) = spec.examples.first() {
                err = err.with_hint(format!("usage: {}  e.g. {example}", spec.usage()));
            }
            return Err(err);
        }
        Ok(args)
    }

    fn bind_one(&mut self, param: &ParamSpec, expr: &Expr) -> Result<ArgVal> {
        if param.ty == ParamType::Expr {
            return Ok(ArgVal::Lazy(bare_to_field(expr)));
        }
        let value = self.eval(expr, None)?;
        coerce(param, value).map(ArgVal::Value)
    }

    fn bind_variadic(&mut self, param: &ParamSpec, exprs: &[&Expr]) -> Result<ArgVal> {
        if param.ty == ParamType::Expr {
            return Ok(ArgVal::Lazy(Expr::List(exprs.iter().map(|e| bare_to_field(e)).collect())));
        }
        let mut items = Vec::new();
        for expr in exprs {
            match coerce(param, self.eval(expr, None)?)? {
                Value::List(list) => items.extend(list),
                single => items.push(single),
            }
        }
        Ok(ArgVal::Value(Value::List(items)))
    }

    fn bind_input(&mut self, spec: &CommandSpec, args: &mut Args, value: Value) -> Result<()> {
        let Some(name) = spec.input else {
            return Err(ShellError::new(ErrorCode::InvalidValue, format!("`{}` doesn't take piped input", spec.name))
                .with_hint(format!("start a new statement instead; usage: {}", spec.usage())));
        };
        if args.has(name) {
            return Err(ShellError::new(
                ErrorCode::InvalidValue,
                format!("`{}` got both piped input and `{name}`", spec.name),
            )
            .with_arg(name)
            .with_hint("pass the value one way only"));
        }
        let param = spec.param(name).ok_or_else(|| ShellError::internal(format!("{}: bad input param", spec.name)))?;
        let value = coerce(param, value)?;
        let value = match (param.variadic, value) {
            (true, Value::List(l)) => Value::List(l),
            (true, single) => Value::List(vec![single]),
            (false, v) => v,
        };
        args.insert(param.name, ArgVal::Value(value));
        Ok(())
    }

    pub fn eval(&mut self, expr: &Expr, row: Option<&Value>) -> Result<Value> {
        match expr {
            Expr::Lit(v) => Ok(v.clone()),
            Expr::Var(name, accessors) => {
                let base = self.lookup_var(name)?;
                self.apply_accessors(base, accessors, row, name)
            }
            Expr::Bare(name) => Err(ShellError::new(ErrorCode::InvalidValue, format!("`{name}` is not a value here"))
                .with_value(name.as_str())
                .with_hint(format!(
                    "quote text (\"{name}\"), use $ for variables (${name}) or a dot for row fields (.{name})"
                ))),
            Expr::Field(path) => {
                let Some(row) = row else {
                    return Err(ShellError::new(
                        ErrorCode::InvalidValue,
                        format!("`.{}` refers to a row field", path.join(".")),
                    )
                    .with_hint("row fields work inside where, sort, pick and sum; for variables use $name.field"));
                };
                Ok(path.iter().fold(row.clone(), |v, key| field(&v, key).unwrap_or(Value::Null)))
            }
            Expr::List(items) => items.iter().map(|e| self.eval(e, row)).collect::<Result<Vec<_>>>().map(Value::List),
            Expr::Record(entries) => {
                let mut out = agentsh_spec::Record::new();
                for (k, e) in entries {
                    out.insert(k.clone(), self.eval(e, row)?);
                }
                Ok(Value::Record(out))
            }
            Expr::Interp(parts) => {
                let mut out = String::new();
                for part in parts {
                    match part {
                        InterpPart::Lit(s) => out.push_str(s),
                        InterpPart::Expr(e) => out.push_str(&self.eval(e, row)?.to_display_string()),
                    }
                }
                Ok(Value::Str(out))
            }
            Expr::Unary(op, inner) => {
                let v = self.eval(inner, row)?;
                match (op, v) {
                    (UnOp::Not, v) => Ok(Value::Bool(!v.is_truthy())),
                    (UnOp::Neg, Value::Int(i)) => Ok(Value::Int(-i)),
                    (UnOp::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
                    (UnOp::Neg, other) => Err(ShellError::type_error("`-`", "a number", &other)),
                }
            }
            Expr::Binary(l, op, r) => self.eval_binary(l, *op, r, row),
            Expr::Func(name, args, span) => {
                let mut values = Vec::with_capacity(args.len());
                for a in args {
                    values.push(self.eval(a, row)?);
                }
                self.call_function(name, values).map_err(|e| e.with_command_if_missing(&format!("{name}()"), *span))
            }
            Expr::Sub(pipeline) => self.run_pipeline(pipeline),
        }
    }

    fn lookup_var(&self, name: &str) -> Result<Value> {
        if let Some(v) = self.state.vars.get(name) {
            return Ok(v.clone());
        }
        if is_handle_name(name)
            && let Some(v) = self.load_handle(name)
        {
            return Ok(v);
        }
        let err =
            ShellError::new(ErrorCode::NotFound, format!("unknown variable `${name}`")).with_value(format!("${name}"));
        Err(match closest(name, self.state.vars.keys().map(String::as_str)) {
            Some(best) => err.with_hint(format!("did you mean `${best}`?")),
            None => err.with_hint(format!(
                "define it first (let {name} = ...); for a literal $ in a string write \\${name} or use single quotes"
            )),
        })
    }

    fn apply_accessors(
        &mut self,
        mut value: Value,
        accessors: &[Accessor],
        row: Option<&Value>,
        name: &str,
    ) -> Result<Value> {
        for accessor in accessors {
            value = match accessor {
                Accessor::Field(key) => match &value {
                    Value::List(items) => {
                        Value::List(items.iter().map(|v| field(v, key).unwrap_or(Value::Null)).collect())
                    }
                    Value::Record(r) => r.get(key).cloned().ok_or_else(|| {
                        let keys: Vec<&str> = r.keys().map(String::as_str).collect();
                        ShellError::new(ErrorCode::NotFound, format!("`${name}` has no field `{key}`"))
                            .with_hint(format!("fields: {}", keys.join(", ")))
                    })?,
                    other => {
                        return Err(ShellError::type_error(
                            &format!("`${name}`"),
                            "a record or list of records",
                            other,
                        ));
                    }
                },
                Accessor::Index(index) => {
                    let index = self.eval(index, row)?;
                    index_value(&value, &index, name)?
                }
            };
        }
        Ok(value)
    }

    fn eval_binary(&mut self, l: &Expr, op: BinOp, r: &Expr, row: Option<&Value>) -> Result<Value> {
        match op {
            BinOp::And => {
                let left = self.eval(l, row)?;
                return Ok(Value::Bool(left.is_truthy() && self.eval(r, row)?.is_truthy()));
            }
            BinOp::Or => {
                let left = self.eval(l, row)?;
                return Ok(Value::Bool(left.is_truthy() || self.eval(r, row)?.is_truthy()));
            }
            _ => {}
        }
        let left = self.eval(l, row)?;
        let right = self.eval(r, row)?;
        match op {
            BinOp::Eq => Ok(Value::Bool(left.loose_eq(&right))),
            BinOp::Ne => Ok(Value::Bool(!left.loose_eq(&right))),
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => compare(&left, op, &right),
            BinOp::Match | BinOp::NotMatch => {
                let (Value::Str(text), Value::Str(pattern)) = (&left, &right) else {
                    return Err(ShellError::new(ErrorCode::TypeError, "=~ needs a string on both sides"));
                };
                let matched = self.regex(pattern)?.is_match(text);
                Ok(Value::Bool(matched == (op == BinOp::Match)))
            }
            BinOp::In => contains(&right, &left),
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem => arithmetic(left, op, right),
            BinOp::And | BinOp::Or => unreachable!(),
        }
    }

    pub fn regex(&mut self, pattern: &str) -> Result<Regex> {
        if let Some(re) = self.regex_cache.get(pattern) {
            return Ok(re.clone());
        }
        let re = Regex::new(pattern).map_err(|e| {
            ShellError::new(ErrorCode::InvalidValue, format!("invalid regex: {e}"))
                .with_value(pattern)
                .with_hint("regex syntax is Rust/RE2-style; escape special characters with \\\\")
        })?;
        self.regex_cache.insert(pattern.to_string(), re.clone());
        Ok(re)
    }
}

fn statement_label(stmt: &Stmt, source: &str) -> String {
    const MAX_LABEL: usize = 70;
    let line = match stmt {
        Stmt::Let { span, .. } | Stmt::If { span, .. } | Stmt::For { span, .. } | Stmt::Try { span, .. } => span.line,
        Stmt::Pipeline(p) => match &p.head {
            Stage::Call(call) => call.span.line,
            Stage::Expr(_, span) => span.line,
        },
    };
    let text = source.lines().nth(line.saturating_sub(1)).unwrap_or("").trim();
    let text = if text.chars().count() <= MAX_LABEL {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(MAX_LABEL).collect::<String>())
    };
    format!("[{line}] {text}")
}

fn plain_words(call: &Call) -> Option<String> {
    let mut words = vec![call.name.clone()];
    for arg in &call.args {
        match arg {
            Arg::Positional(Expr::Bare(word)) => words.push(word.clone()),
            Arg::Positional(Expr::Lit(Value::Str(text))) if !text.contains(['\'', ' ']) => words.push(text.clone()),
            _ => return None,
        }
    }
    Some(words.join(" "))
}

fn bare_to_field(expr: &Expr) -> Expr {
    match expr {
        Expr::Bare(name) => Expr::Field(vec![name.clone()]),
        Expr::Binary(l, op, r) => Expr::Binary(Box::new(bare_to_field(l)), *op, Box::new(bare_to_field(r))),
        Expr::Unary(op, inner) => Expr::Unary(*op, Box::new(bare_to_field(inner))),
        Expr::List(items) => Expr::List(items.iter().map(bare_to_field).collect()),
        Expr::Func(name, args, span) => Expr::Func(name.clone(), args.iter().map(bare_to_field).collect(), *span),
        Expr::Record(entries) => Expr::Record(entries.iter().map(|(k, v)| (k.clone(), bare_to_field(v))).collect()),
        other => other.clone(),
    }
}

pub fn describe_binding(name: &str, value: &Value) -> String {
    const INLINE_LIMIT: usize = 120;
    let inline = match value {
        Value::Str(s) => format!("{s:?}"),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    if inline.len() <= INLINE_LIMIT && !inline.contains("\\n") {
        return format!("${name} = {inline}");
    }
    let shape = match value {
        Value::List(items) => match items.first() {
            Some(Value::Record(r)) => {
                format!("list of {} records {{{}}}", items.len(), r.keys().cloned().collect::<Vec<_>>().join(", "))
            }
            _ => format!("list of {} items", items.len()),
        },
        Value::Str(s) => format!("text, {} lines, {} bytes", s.lines().count(), s.len()),
        Value::Record(r) => format!("record {{{}}}", r.keys().cloned().collect::<Vec<_>>().join(", ")),
        other => other.type_name().to_string(),
    };
    format!("${name}: {shape}")
}

fn is_handle_name(name: &str) -> bool {
    name == "_" || name.strip_prefix('r').is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

fn unknown_argument(spec: &CommandSpec, name: &str) -> ShellError {
    let err =
        ShellError::new(ErrorCode::UnknownArgument, format!("`{}` has no argument `{name}`", spec.name)).with_arg(name);
    match closest(name, spec.params.iter().map(|p| p.name)) {
        Some(best) => err.with_hint(format!("did you mean `{best}:`? usage: {}", spec.usage())),
        None => err.with_hint(format!("usage: {}", spec.usage())),
    }
}

pub fn coerce(param: &ParamSpec, value: Value) -> Result<Value> {
    let fail =
        |v: &Value| ShellError::type_error(&format!("`{}`", param.name), param.ty.describe(), v).with_arg(param.name);
    match param.ty {
        ParamType::Any | ParamType::Expr => Ok(value),
        ParamType::Str => match value {
            Value::Str(_) => Ok(value),
            other => Err(fail(&other)),
        },
        ParamType::Int => match value {
            Value::Int(_) => Ok(value),
            Value::Float(f) if f.fract() == 0.0 => Ok(Value::Int(f as i64)),
            other => Err(fail(&other)),
        },
        ParamType::Bool => match value {
            Value::Bool(_) => Ok(value),
            other => Err(fail(&other)),
        },
        ParamType::Duration => match value {
            Value::Int(_) | Value::Float(_) => Ok(value),
            other => Err(fail(&other)),
        },
        ParamType::List => match value {
            Value::List(_) => Ok(value),
            Value::Str(_) => {
                Err(fail(&value).with_hint("this works on lists; split text into lines first: ... | lines | ..."))
            }
            other => Err(fail(&other)),
        },
        ParamType::Record => match value {
            Value::Record(_) => Ok(value),
            other => Err(fail(&other)),
        },
        ParamType::Path => path_of(&value).map(Value::Str).ok_or_else(|| fail(&value)),
        ParamType::Paths => match &value {
            Value::List(items) => items
                .iter()
                .map(|v| path_of(v).map(Value::Str).ok_or_else(|| fail(v)))
                .collect::<Result<Vec<_>>>()
                .map(Value::List),
            Value::Null => Ok(Value::List(Vec::new())),
            single => path_of(single).map(|p| Value::List(vec![Value::Str(p)])).ok_or_else(|| fail(single)),
        },
        ParamType::Arg => match value {
            Value::Str(_) => Ok(value),
            Value::Int(_) | Value::Float(_) | Value::Bool(_) => Ok(Value::Str(value.to_display_string())),
            Value::List(items) => items
                .into_iter()
                .map(|v| match v {
                    Value::Str(_) | Value::Int(_) | Value::Float(_) | Value::Bool(_) => {
                        Ok(Value::Str(v.to_display_string()))
                    }
                    other => Err(fail(&other)),
                })
                .collect::<Result<Vec<_>>>()
                .map(Value::List),
            other => Err(fail(&other)),
        },
    }
}

fn path_of(value: &Value) -> Option<String> {
    match value {
        Value::Str(s) => Some(s.clone()),
        Value::Record(r) => match r.get("path") {
            Some(Value::Str(s)) => Some(s.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn field(value: &Value, key: &str) -> Option<Value> {
    match value {
        Value::Record(r) => r.get(key).cloned(),
        _ => None,
    }
}

fn index_value(value: &Value, index: &Value, name: &str) -> Result<Value> {
    match (value, index) {
        (Value::List(items), Value::Int(i)) => {
            let len = items.len() as i64;
            let idx = if *i < 0 { len + i } else { *i };
            items.get(idx as usize).filter(|_| idx >= 0).cloned().ok_or_else(|| {
                ShellError::new(
                    ErrorCode::InvalidValue,
                    format!("index {i} is out of range for `${name}` (length {len})"),
                )
            })
        }
        (Value::Record(r), Value::Str(key)) => r
            .get(key)
            .cloned()
            .ok_or_else(|| ShellError::new(ErrorCode::NotFound, format!("`${name}` has no field `{key}`"))),
        (Value::Str(s), Value::Int(i)) => {
            let chars: Vec<char> = s.chars().collect();
            let idx = if *i < 0 { chars.len() as i64 + i } else { *i };
            chars
                .get(idx as usize)
                .filter(|_| idx >= 0)
                .map(|c| Value::Str(c.to_string()))
                .ok_or_else(|| ShellError::new(ErrorCode::InvalidValue, format!("index {i} is out of range")))
        }
        (other, idx) => Err(ShellError::new(
            ErrorCode::TypeError,
            format!("can't index a {} with a {}", other.type_name(), idx.type_name()),
        )),
    }
}

fn compare(left: &Value, op: BinOp, right: &Value) -> Result<Value> {
    if *left == Value::Null || *right == Value::Null {
        return Ok(Value::Bool(false));
    }
    let ordering = left.compare(right).ok_or_else(|| {
        ShellError::new(ErrorCode::TypeError, format!("can't compare {} with {}", left.type_name(), right.type_name()))
    })?;
    Ok(Value::Bool(match op {
        BinOp::Lt => ordering == Ordering::Less,
        BinOp::Le => ordering != Ordering::Greater,
        BinOp::Gt => ordering == Ordering::Greater,
        _ => ordering != Ordering::Less,
    }))
}

fn contains(haystack: &Value, needle: &Value) -> Result<Value> {
    match (haystack, needle) {
        (Value::List(items), n) => Ok(Value::Bool(items.iter().any(|i| i.loose_eq(n)))),
        (Value::Str(s), Value::Str(n)) => Ok(Value::Bool(s.contains(n.as_str()))),
        (Value::Record(r), Value::Str(k)) => Ok(Value::Bool(r.contains_key(k))),
        (Value::Null, _) => Ok(Value::Bool(false)),
        (h, n) => Err(ShellError::new(
            ErrorCode::TypeError,
            format!("can't check whether a {} is in a {}", n.type_name(), h.type_name()),
        )),
    }
}

fn arithmetic(left: Value, op: BinOp, right: Value) -> Result<Value> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => int_arith(a, op, b),
        (Value::Str(a), Value::Str(b)) if op == BinOp::Add => Ok(Value::Str(a + &b)),
        (Value::Str(a), b) if op == BinOp::Add && b.as_f64().is_some() => Ok(Value::Str(a + &b.to_display_string())),
        (Value::List(mut a), Value::List(b)) if op == BinOp::Add => {
            a.extend(b);
            Ok(Value::List(a))
        }
        (a, b) => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => float_arith(x, op, y),
            _ => Err(ShellError::new(
                ErrorCode::TypeError,
                format!("can't apply {} to {} and {}", op_symbol(op), a.type_name(), b.type_name()),
            )),
        },
    }
}

fn int_arith(a: i64, op: BinOp, b: i64) -> Result<Value> {
    let overflow = || ShellError::new(ErrorCode::InvalidValue, "integer overflow");
    match op {
        BinOp::Add => a.checked_add(b).map(Value::Int).ok_or_else(overflow),
        BinOp::Sub => a.checked_sub(b).map(Value::Int).ok_or_else(overflow),
        BinOp::Mul => a.checked_mul(b).map(Value::Int).ok_or_else(overflow),
        BinOp::Div if b == 0 => Err(ShellError::new(ErrorCode::InvalidValue, "division by zero")),
        BinOp::Div if a % b == 0 => Ok(Value::Int(a / b)),
        BinOp::Div => Ok(Value::Float(a as f64 / b as f64)),
        BinOp::Rem if b == 0 => Err(ShellError::new(ErrorCode::InvalidValue, "division by zero")),
        _ => Ok(Value::Int(a % b)),
    }
}

fn float_arith(a: f64, op: BinOp, b: f64) -> Result<Value> {
    Ok(Value::Float(match op {
        BinOp::Add => a + b,
        BinOp::Sub => a - b,
        BinOp::Mul => a * b,
        BinOp::Div if b == 0.0 => return Err(ShellError::new(ErrorCode::InvalidValue, "division by zero")),
        BinOp::Div => a / b,
        _ => a % b,
    }))
}

fn op_symbol(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "+",
        BinOp::Sub => "-",
        BinOp::Mul => "*",
        BinOp::Div => "/",
        _ => "%",
    }
}

pub fn on_path(program: &str) -> bool {
    find_on_path(program).is_some()
}

pub fn find_on_path(program: &str) -> Option<std::path::PathBuf> {
    if program.contains('/') {
        let p = std::path::PathBuf::from(program);
        return p.is_file().then_some(p);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).map(|dir| dir.join(program)).find(|candidate| candidate.is_file())
    })
}
