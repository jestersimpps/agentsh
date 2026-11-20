use crate::Config;
use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::Command;

const MIN_REDACT_LEN: usize = 8;

pub struct Secrets {
    dotenv: Option<PathBuf>,
    commands: BTreeMap<String, String>,
    redact_env: Vec<String>,
    dotenv_cache: OnceCell<HashMap<String, String>>,
}

impl Secrets {
    pub fn new(config: &Config) -> Self {
        Self {
            dotenv: config.dotenv.clone(),
            commands: config.secret_commands.clone(),
            redact_env: config.redact_env.clone(),
            dotenv_cache: OnceCell::new(),
        }
    }

    pub fn get(&self, name: &str) -> Result<Option<String>, String> {
        if let Ok(value) = std::env::var(name) {
            return Ok(Some(value));
        }
        if let Some(value) = self.dotenv_values().get(name) {
            return Ok(Some(value.clone()));
        }
        match self.commands.get(name) {
            Some(cmd) => run_secret_command(name, cmd).map(Some),
            None => Ok(None),
        }
    }

    pub fn sensitive_env(&self) -> Vec<(String, String)> {
        std::env::vars()
            .chain(self.dotenv_values().clone())
            .filter(|(k, v)| v.len() >= MIN_REDACT_LEN && self.redact_env.iter().any(|p| wildcard_match(p, k)))
            .collect()
    }

    fn dotenv_values(&self) -> &HashMap<String, String> {
        self.dotenv_cache.get_or_init(|| {
            self.dotenv
                .as_ref()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|text| parse_dotenv(&text))
                .unwrap_or_default()
        })
    }
}

fn run_secret_command(name: &str, cmd: &str) -> Result<String, String> {
    let output = Command::new("/bin/sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .map_err(|e| format!("secret command for {name} failed to start: {e}"))?;
    if !output.status.success() {
        return Err(format!("secret command for {name} exited with {}", output.status));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim_end_matches(['\n', '\r']).to_string())
}

fn parse_dotenv(text: &str) -> HashMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let line = l.strip_prefix("export ").unwrap_or(l);
            let (key, value) = line.split_once('=')?;
            let value = value.trim();
            let unquoted = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            Some((key.trim().to_string(), unquoted.to_string()))
        })
        .collect()
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || !text.ends_with(last) || text.len() < first.len() + last.len() {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcard() {
        assert!(wildcard_match("*_TOKEN", "GITHUB_TOKEN"));
        assert!(wildcard_match("*PASSWORD*", "DB_PASSWORD_MAIN"));
        assert!(!wildcard_match("*_KEY", "KEYBOARD"));
        assert!(wildcard_match("EXACT", "EXACT"));
    }

    #[test]
    fn dotenv() {
        let env = parse_dotenv("# c\nexport A=\"x y\"\nB='z'\nC=plain\n");
        assert_eq!(env["A"], "x y");
        assert_eq!(env["B"], "z");
        assert_eq!(env["C"], "plain");
    }
}
