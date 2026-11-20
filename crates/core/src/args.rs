use crate::{Result, ShellError};
use agentsh_lang::Expr;
use agentsh_spec::{ErrorCode, Value};
use indexmap::IndexMap;

#[derive(Debug, Clone)]
pub enum ArgVal {
    Value(Value),
    Lazy(Expr),
}

#[derive(Debug, Clone, Default)]
pub struct Args {
    vals: IndexMap<&'static str, ArgVal>,
}

impl Args {
    pub fn insert(&mut self, name: &'static str, val: ArgVal) {
        self.vals.insert(name, val);
    }

    pub fn has(&self, name: &str) -> bool {
        self.vals.contains_key(name)
    }

    pub fn value(&self, name: &str) -> Option<&Value> {
        match self.vals.get(name) {
            Some(ArgVal::Value(v)) => Some(v),
            _ => None,
        }
    }

    pub fn take(&mut self, name: &str) -> Option<Value> {
        match self.vals.shift_remove(name) {
            Some(ArgVal::Value(v)) => Some(v),
            _ => None,
        }
    }

    pub fn expr(&self, name: &str) -> Option<&Expr> {
        match self.vals.get(name) {
            Some(ArgVal::Lazy(e)) => Some(e),
            _ => None,
        }
    }

    pub fn exprs(&self, name: &str) -> Vec<Expr> {
        match self.vals.get(name) {
            Some(ArgVal::Lazy(Expr::List(items))) => items.clone(),
            Some(ArgVal::Lazy(e)) => vec![e.clone()],
            _ => Vec::new(),
        }
    }

    pub fn str(&self, name: &str) -> Result<String> {
        self.opt_str(name)?.ok_or_else(|| missing(name))
    }

    pub fn opt_str(&self, name: &str) -> Result<Option<String>> {
        match self.value(name) {
            None => Ok(None),
            Some(Value::Str(s)) => Ok(Some(s.clone())),
            Some(other) => Err(ShellError::type_error(name, "a string", other).with_arg(name)),
        }
    }

    pub fn opt_int(&self, name: &str) -> Result<Option<i64>> {
        match self.value(name) {
            None => Ok(None),
            Some(Value::Int(i)) => Ok(Some(*i)),
            Some(other) => Err(ShellError::type_error(name, "an int", other).with_arg(name)),
        }
    }

    pub fn opt_usize(&self, name: &str) -> Result<Option<usize>> {
        match self.opt_int(name)? {
            Some(i) if i < 0 => {
                Err(ShellError::new(ErrorCode::InvalidValue, format!("{name} can't be negative")).with_arg(name))
            }
            other => Ok(other.map(|i| i as usize)),
        }
    }

    pub fn flag(&self, name: &str) -> Result<bool> {
        match self.value(name) {
            None => Ok(false),
            Some(Value::Bool(b)) => Ok(*b),
            Some(other) => Err(ShellError::type_error(name, "true or false", other).with_arg(name)),
        }
    }

    pub fn opt_secs(&self, name: &str) -> Result<Option<f64>> {
        match self.value(name) {
            None => Ok(None),
            Some(v) => v
                .as_f64()
                .map(Some)
                .ok_or_else(|| ShellError::type_error(name, "a duration like 30s", v).with_arg(name)),
        }
    }

    pub fn strings(&self, name: &str) -> Result<Vec<String>> {
        match self.value(name) {
            None => Ok(Vec::new()),
            Some(Value::List(items)) => items
                .iter()
                .map(|v| match v {
                    Value::Str(s) => Ok(s.clone()),
                    other => Err(ShellError::type_error(name, "strings", other).with_arg(name)),
                })
                .collect(),
            Some(Value::Str(s)) => Ok(vec![s.clone()]),
            Some(other) => Err(ShellError::type_error(name, "a string or list of strings", other).with_arg(name)),
        }
    }
}

fn missing(name: &str) -> ShellError {
    ShellError::new(ErrorCode::MissingArgument, format!("missing argument `{name}`")).with_arg(name)
}
