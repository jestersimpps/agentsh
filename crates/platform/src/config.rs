use crate::Capability;
use crate::config_layer::ConfigLayer;
use crate::dirs::{expand_home, global_config_path};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SandboxKind {
    Auto,
    None,
    Seatbelt,
    Landlock,
}

impl std::str::FromStr for SandboxKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_value(serde_json::Value::String(s.to_string()))
            .map_err(|_| format!("unknown sandbox backend `{s}` (valid: auto, none, seatbelt, landlock)"))
    }
}

#[derive(Debug, Clone)]
pub struct Limits {
    pub max_items: usize,
    pub max_string_bytes: usize,
    pub exec_timeout_secs: u64,
    pub exec_capture_bytes: usize,
    pub max_handles: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_items: 50,
            max_string_bytes: 24_000,
            exec_timeout_secs: 60,
            exec_capture_bytes: 4 * 1024 * 1024,
            max_handles: 20,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub capabilities: BTreeSet<Capability>,
    pub limits: Limits,
    pub sandbox: SandboxKind,
    pub sandbox_writable: Vec<PathBuf>,
    pub extra_roots: Vec<PathBuf>,
    pub trace_path: Option<PathBuf>,
    pub dotenv: Option<PathBuf>,
    pub redact_env: Vec<String>,
    pub secret_commands: BTreeMap<String, String>,
    pub package_enable: Vec<String>,
    pub package_disable: Vec<String>,
    pub package_paths: Vec<PathBuf>,
    pub sources: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct Overrides {
    pub allow: Vec<Capability>,
    pub deny: Vec<Capability>,
    pub sandbox: Option<SandboxKind>,
    pub trace: Option<PathBuf>,
}

const DEFAULT_REDACT_ENV: &[&str] =
    &["*_KEY", "*_TOKEN", "*_SECRET", "*PASSWORD*", "*_PAT", "*CREDENTIALS*", "*_API_KEY"];

impl Default for Config {
    fn default() -> Self {
        Self {
            capabilities: [Capability::Read, Capability::Write, Capability::Delete].into(),
            limits: Limits::default(),
            sandbox: SandboxKind::Auto,
            sandbox_writable: Vec::new(),
            extra_roots: Vec::new(),
            trace_path: None,
            dotenv: None,
            redact_env: DEFAULT_REDACT_ENV.iter().map(|s| s.to_string()).collect(),
            secret_commands: BTreeMap::new(),
            package_enable: Vec::new(),
            package_disable: Vec::new(),
            package_paths: Vec::new(),
            sources: Vec::new(),
        }
    }
}

impl Config {
    pub fn load(root: &Path, overrides: &Overrides) -> Result<Config, String> {
        let mut config = Config::default();
        if let Some(global) = global_config_path().filter(|p| p.is_file()) {
            let base = global.parent().map(Path::to_path_buf).unwrap_or_default();
            config.apply_file(&global, &base)?;
        }
        let project = root.join(".agentsh.toml");
        if project.is_file() {
            config.apply_file(&project, root)?;
        }
        config.apply_env(root)?;
        config.apply_overrides(overrides);
        Ok(config)
    }

    pub fn allows(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    fn apply_file(&mut self, path: &Path, base: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
        let layer: ConfigLayer =
            toml::from_str(&text).map_err(|e| format!("invalid config {}: {e}", path.display()))?;
        self.apply_layer(layer, base);
        self.sources.push(path.to_path_buf());
        Ok(())
    }

    fn apply_layer(&mut self, layer: ConfigLayer, base: &Path) {
        let resolve = |p: PathBuf| resolve_path(&p, base);
        if let Some(allow) = layer.capabilities.allow {
            self.capabilities = allow.into_iter().collect();
        }
        for cap in layer.capabilities.deny {
            self.capabilities.remove(&cap);
        }
        let limits = layer.limits;
        let l = &mut self.limits;
        l.max_items = limits.max_items.unwrap_or(l.max_items);
        l.max_string_bytes = limits.max_string_bytes.unwrap_or(l.max_string_bytes);
        l.exec_timeout_secs = limits.exec_timeout_secs.unwrap_or(l.exec_timeout_secs);
        l.exec_capture_bytes = limits.exec_capture_bytes.unwrap_or(l.exec_capture_bytes);
        l.max_handles = limits.max_handles.unwrap_or(l.max_handles);
        if let Some(backend) = layer.sandbox.backend {
            self.sandbox = backend;
        }
        self.sandbox_writable.extend(layer.sandbox.writable.into_iter().map(resolve));
        self.extra_roots.extend(layer.workspace.roots.into_iter().map(resolve));
        match (layer.trace.enabled, layer.trace.path) {
            (Some(false), _) => self.trace_path = None,
            (_, Some(path)) => self.trace_path = Some(resolve(path)),
            (Some(true), None) => self.trace_path = crate::global_data_dir().map(|d| d.join("trace.jsonl")),
            (None, None) => {}
        }
        if let Some(dotenv) = layer.secrets.dotenv {
            self.dotenv = Some(resolve(dotenv));
        }
        if let Some(patterns) = layer.secrets.redact_env {
            self.redact_env = patterns;
        }
        self.secret_commands.extend(layer.secrets.commands);
        for name in layer.packages.enable {
            self.package_disable.retain(|n| *n != name);
            if !self.package_enable.contains(&name) {
                self.package_enable.push(name);
            }
        }
        for name in layer.packages.disable {
            self.package_enable.retain(|n| *n != name);
            if !self.package_disable.contains(&name) {
                self.package_disable.push(name);
            }
        }
        self.package_paths.extend(layer.packages.paths.into_iter().map(resolve));
    }

    fn apply_env(&mut self, root: &Path) -> Result<(), String> {
        if let Ok(allow) = std::env::var("AGENTSH_ALLOW") {
            self.capabilities.extend(Capability::parse_list(&allow)?);
        }
        if let Ok(deny) = std::env::var("AGENTSH_DENY") {
            for cap in Capability::parse_list(&deny)? {
                self.capabilities.remove(&cap);
            }
        }
        if let Ok(backend) = std::env::var("AGENTSH_SANDBOX") {
            self.sandbox = backend.parse()?;
        }
        if let Some(paths) = std::env::var_os("AGENTSH_PACKAGES") {
            self.package_paths.extend(
                std::env::split_paths(&paths).filter(|p| !p.as_os_str().is_empty()).map(|p| resolve_path(&p, root)),
            );
        }
        if let Ok(trace) = std::env::var("AGENTSH_TRACE") {
            self.trace_path = Some(resolve_path(Path::new(&trace), root));
        }
        Ok(())
    }

    fn apply_overrides(&mut self, overrides: &Overrides) {
        self.capabilities.extend(overrides.allow.iter().copied());
        for cap in &overrides.deny {
            self.capabilities.remove(cap);
        }
        if let Some(backend) = overrides.sandbox {
            self.sandbox = backend;
        }
        if let Some(trace) = &overrides.trace {
            self.trace_path = Some(trace.clone());
        }
    }
}

fn resolve_path(path: &Path, base: &Path) -> PathBuf {
    let expanded = expand_home(path);
    if expanded.is_absolute() { expanded } else { base.join(expanded) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_layer_overrides_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(".agentsh.toml"),
            "[capabilities]\nallow = [\"read\", \"exec\"]\n[limits]\nmax_items = 7\n[secrets]\ndotenv = \".env\"\n",
        )
        .unwrap();
        let config =
            Config::load(dir.path(), &Overrides { deny: vec![Capability::Exec], ..Default::default() }).unwrap();
        assert_eq!(config.capabilities, [Capability::Read].into());
        assert_eq!(config.limits.max_items, 7);
        assert_eq!(config.dotenv, Some(dir.path().join(".env")));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".agentsh.toml"), "[limits]\nmax_itemz = 7\n").unwrap();
        assert!(Config::load(dir.path(), &Overrides::default()).unwrap_err().contains("max_itemz"));
    }
}
