use agentsh_spec::Value;

#[derive(Debug, Default, Clone)]
pub struct Redactor {
    secrets: Vec<(String, String)>,
}

impl Redactor {
    pub fn new(pairs: Vec<(String, String)>) -> Self {
        let mut redactor = Redactor::default();
        for (name, value) in pairs {
            redactor.add(&name, &value);
        }
        redactor
    }

    pub fn add(&mut self, name: &str, value: &str) {
        if value.is_empty() || self.secrets.iter().any(|(_, v)| v == value) {
            return;
        }
        self.secrets.push((name.to_string(), value.to_string()));
        self.secrets.sort_by_key(|(_, v)| std::cmp::Reverse(v.len()));
    }

    pub fn redact_str(&self, text: &str) -> String {
        let mut out = text.to_string();
        for (name, value) in &self.secrets {
            if out.contains(value.as_str()) {
                out = out.replace(value.as_str(), &format!("[redacted:{name}]"));
            }
        }
        out
    }

    pub fn redact_value(&self, value: &mut Value) {
        if self.secrets.is_empty() {
            return;
        }
        match value {
            Value::Str(s) => *s = self.redact_str(s),
            Value::List(items) => items.iter_mut().for_each(|v| self.redact_value(v)),
            Value::Record(record) => record.values_mut().for_each(|v| self.redact_value(v)),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_nested_values_longest_first() {
        let r = Redactor::new(vec![("A".into(), "secret".into()), ("B".into(), "secret-long".into())]);
        let mut v = Value::record([("out", Value::List(vec![Value::str("x secret-long y secret")]))]);
        r.redact_value(&mut v);
        assert_eq!(v, Value::record([("out", Value::List(vec![Value::str("x [redacted:B] y [redacted:A]")]))]));
    }
}
