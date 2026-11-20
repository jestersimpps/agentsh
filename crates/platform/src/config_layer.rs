use crate::{Capability, SandboxKind};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigLayer {
    #[serde(default)]
    pub capabilities: CapabilitiesLayer,
    #[serde(default)]
    pub limits: LimitsLayer,
    #[serde(default)]
    pub sandbox: SandboxLayer,
    #[serde(default)]
    pub workspace: WorkspaceLayer,
    #[serde(default)]
    pub trace: TraceLayer,
    #[serde(default)]
    pub secrets: SecretsLayer,
    #[serde(default)]
    pub packages: PackagesLayer,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitiesLayer {
    pub allow: Option<Vec<Capability>>,
    #[serde(default)]
    pub deny: Vec<Capability>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsLayer {
    pub max_items: Option<usize>,
    pub max_string_bytes: Option<usize>,
    pub exec_timeout_secs: Option<u64>,
    pub exec_capture_bytes: Option<usize>,
    pub max_handles: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SandboxLayer {
    pub backend: Option<SandboxKind>,
    #[serde(default)]
    pub writable: Vec<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceLayer {
    #[serde(default)]
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TraceLayer {
    pub enabled: Option<bool>,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretsLayer {
    pub dotenv: Option<PathBuf>,
    pub redact_env: Option<Vec<String>>,
    #[serde(default)]
    pub commands: BTreeMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackagesLayer {
    #[serde(default)]
    pub enable: Vec<String>,
    #[serde(default)]
    pub disable: Vec<String>,
    #[serde(default)]
    pub paths: Vec<PathBuf>,
}
