use crate::time::{iso_ago, iso_now};
use crate::walk::glob_paths;
use crate::{Ctx, Result, ShellError};
use agentsh_platform::Capability;
use agentsh_spec::{ErrorCode, Value};
use std::path::Path;

pub const FUNCTIONS: &[(&str, &str)] = &[
    ("glob(pattern)", "paths matching a glob relative to the cwd; ** recurses and respects .gitignore"),
    ("exists(path)", "true if the path exists"),
    ("now()", "current UTC time as ISO-8601"),
    ("ago(duration)", "UTC time `duration` ago, e.g. ago(1d); compare with .modified"),
    ("len(x)", "length of a list, string or record"),
    ("lower(s) upper(s) trim(s)", "string case and whitespace"),
    ("split(s, sep) join(list, sep)", "split a string / join a list of strings"),
    ("int(x) str(x)", "convert to int / string"),
    ("keys(record)", "a record's field names"),
    ("basename(path) dirname(path) ext(path)", "last path component / parent path / extension without the dot"),
    ("secret(name)", "a secret from env, .env or a configured command; redacted from all output"),
];

impl Ctx<'_> {
    pub fn call_function(&mut self, name: &str, args: Vec<Value>) -> Result<Value> {
        match (name, args.as_slice()) {
            ("glob", [Value::Str(pattern)]) => {
                self.require(Capability::Read)?;
                Ok(Value::List(glob_paths(self, pattern)?.into_iter().map(Value::Str).collect()))
            }
            ("exists", [Value::Str(path)]) => {
                self.require(Capability::Read)?;
                let resolved = self.resolve(path)?;
                Ok(Value::Bool(resolved.exists()))
            }
            ("now", []) => Ok(Value::Str(iso_now())),
            ("ago", [secs]) if secs.as_f64().is_some() => Ok(Value::Str(iso_ago(secs.as_f64().unwrap_or(0.0)))),
            ("len", [Value::List(l)]) => Ok(Value::Int(l.len() as i64)),
            ("len", [Value::Str(s)]) => Ok(Value::Int(s.chars().count() as i64)),
            ("len", [Value::Record(r)]) => Ok(Value::Int(r.len() as i64)),
            ("len", [Value::Null]) => Ok(Value::Int(0)),
            ("lower", [Value::Str(s)]) => Ok(Value::Str(s.to_lowercase())),
            ("upper", [Value::Str(s)]) => Ok(Value::Str(s.to_uppercase())),
            ("trim", [Value::Str(s)]) => Ok(Value::Str(s.trim().to_string())),
            ("split", [Value::Str(s), Value::Str(sep)]) => {
                Ok(Value::List(s.split(sep.as_str()).map(Value::str).collect()))
            }
            ("join", [Value::List(items), Value::Str(sep)]) => {
                Ok(Value::Str(items.iter().map(Value::to_display_string).collect::<Vec<_>>().join(sep)))
            }
            ("int", [v]) => to_int(v),
            ("str", [v]) => Ok(Value::Str(v.to_display_string())),
            ("keys", [Value::Record(r)]) => Ok(Value::List(r.keys().map(Value::str).collect())),
            ("basename", [Value::Str(p)]) => {
                Ok(Value::Str(Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))
            }
            ("dirname", [Value::Str(p)]) => Ok(Value::Str(
                Path::new(p)
                    .parent()
                    .map(|n| n.to_string_lossy().into_owned())
                    .filter(|s| !s.is_empty())
                    .unwrap_or(".".into()),
            )),
            ("ext", [Value::Str(p)]) => {
                Ok(Value::Str(Path::new(p).extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default()))
            }
            ("secret", [Value::Str(secret_name)]) => self.secret(secret_name),
            _ => Err(bad_call(name, &args)),
        }
    }

    fn secret(&mut self, name: &str) -> Result<Value> {
        match self.secrets.get(name) {
            Ok(Some(value)) => {
                self.redactor.add(name, &value);
                Ok(Value::Str(value))
            }
            Ok(None) => Err(ShellError::new(ErrorCode::NotFound, format!("secret `{name}` is not set"))
                .with_value(name)
                .with_hint(
                    "a human can set it as an env var, in the configured .env file, or under [secrets.commands]",
                )),
            Err(message) => Err(ShellError::new(ErrorCode::InvalidValue, message).with_value(name)),
        }
    }
}

fn to_int(v: &Value) -> Result<Value> {
    match v {
        Value::Int(_) => Ok(v.clone()),
        Value::Float(f) => Ok(Value::Int(*f as i64)),
        Value::Bool(b) => Ok(Value::Int(*b as i64)),
        Value::Str(s) => s
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| ShellError::new(ErrorCode::InvalidValue, format!("`{s}` is not an integer"))),
        other => Err(ShellError::type_error("int()", "a number or numeric string", other)),
    }
}

fn bad_call(name: &str, args: &[Value]) -> ShellError {
    let known = FUNCTIONS.iter().find(|(sig, _)| sig.split_whitespace().any(|s| s.starts_with(&format!("{name}("))));
    let types: Vec<&str> = args.iter().map(Value::type_name).collect();
    match known {
        Some((sig, doc)) => {
            ShellError::new(ErrorCode::TypeError, format!("{name}() can't take ({})", types.join(", ")))
                .with_hint(format!("{sig}: {doc}"))
        }
        None => {
            let names: Vec<String> = FUNCTIONS
                .iter()
                .flat_map(|(sig, _)| sig.split_whitespace().map(|s| s.split('(').next().unwrap_or("").to_string()))
                .collect();
            let err = ShellError::new(ErrorCode::UnknownCommand, format!("unknown function `{name}()`"));
            match crate::suggest::closest(name, names.iter().map(String::as_str)) {
                Some(best) => err.with_hint(format!("did you mean {best}()?")),
                None => err.with_hint("commands are called without parentheses: ls \"src\"; functions: see help"),
            }
        }
    }
}
