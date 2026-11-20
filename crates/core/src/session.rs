use crate::{Result, ShellError};
use agentsh_platform::{global_data_dir, hash_bytes};
use agentsh_spec::{ErrorCode, Value};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const HISTORY_LEN: usize = 20;
const LOCK_WAIT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HistoryItem {
    pub program: String,
    pub error: Option<ErrorCode>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionState {
    pub cwd: String,
    #[serde(default)]
    pub caller_cwd: Option<String>,
    pub vars: IndexMap<String, Value>,
    pub handle_order: VecDeque<String>,
    pub next_handle: u64,
    pub seen: HashMap<PathBuf, String>,
    #[serde(default)]
    pub shown: HashMap<PathBuf, String>,
    #[serde(default)]
    pub last_outputs: HashMap<String, String>,
    #[serde(default)]
    pub hints_seen: Vec<String>,
    pub run_counter: u64,
    pub checkpoints: IndexMap<String, u64>,
    pub history: VecDeque<HistoryItem>,
}

impl SessionState {
    pub fn record_history(&mut self, program: &str, error: Option<ErrorCode>) {
        self.history.push_back(HistoryItem { program: program.to_string(), error });
        while self.history.len() > HISTORY_LEN {
            self.history.pop_front();
        }
    }

    pub fn forget_seen_hints(&mut self, error: &mut agentsh_spec::ErrorInfo) {
        const MAX_HINTS: usize = 200;
        let Some(hint) = error.hint.as_ref() else { return };
        let key = agentsh_platform::hash_bytes(hint.as_bytes());
        if self.hints_seen.contains(&key) {
            error.hint = None;
            return;
        }
        self.hints_seen.push(key);
        if self.hints_seen.len() > MAX_HINTS {
            self.hints_seen.remove(0);
        }
    }

    pub fn repeated_failures(&self, program: &str, error: ErrorCode) -> usize {
        self.history.iter().filter(|h| h.program == program && h.error == Some(error)).count()
    }
}

pub struct SessionStore {
    pub dir: PathBuf,
    _lock: File,
}

impl SessionStore {
    pub fn open(root: &Path, id: &str) -> Result<SessionStore> {
        validate_id(id)?;
        let dir = session_dir(root, id)?;
        let lock_path = dir.join("lock");
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| ShellError::io(&e, &lock_path.display().to_string()))?;
        acquire(&lock, id)?;
        Ok(SessionStore { dir, _lock: lock })
    }

    pub fn load_state(&self) -> Result<SessionState> {
        load_json(&self.dir.join("state.json"))
    }

    pub fn save_state(&self, state: &SessionState) -> Result<()> {
        save_json(&self.dir.join("state.json"), state)
    }
}

fn session_dir(root: &Path, id: &str) -> Result<PathBuf> {
    if let Some(data) = global_data_dir() {
        let workspace = data.join("workspaces").join(workspace_key(root));
        let dir = workspace.join("sessions").join(id);
        if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(workspace.join("root.txt"), root.display().to_string());
            return Ok(dir);
        }
    }
    let local = root.join(".agentsh");
    let dir = local.join("sessions").join(id);
    if std::fs::create_dir_all(&dir).is_ok() {
        let ignore = local.join(".gitignore");
        if !ignore.exists() {
            let _ = std::fs::write(&ignore, "*\n");
        }
        return Ok(dir);
    }
    let temp = std::env::temp_dir().join("agentsh").join(workspace_key(root)).join("sessions").join(id);
    std::fs::create_dir_all(&temp).map_err(|e| {
        ShellError::io(&e, &temp.display().to_string())
            .with_hint("agentsh needs a writable folder for session state: set AGENTSH_HOME")
    })?;
    Ok(temp)
}

fn workspace_key(root: &Path) -> String {
    let name: String = root
        .file_name()
        .map(|n| {
            n.to_string_lossy().chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')).collect()
        })
        .unwrap_or_default();
    let hash = hash_bytes(root.to_string_lossy().as_bytes());
    format!("{}-{}", if name.is_empty() { "root" } else { &name }, &hash[..12])
}

fn acquire(lock: &File, id: &str) -> Result<()> {
    let start = Instant::now();
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < LOCK_WAIT => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(ShellError::new(ErrorCode::SessionBusy, format!("session `{id}` is busy"))
                    .with_hint("another call is using this session; wait, or use a different --session"));
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(ShellError::io(&e, "session lock")),
        }
    }
}

fn validate_id(id: &str) -> Result<()> {
    let valid =
        !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if valid {
        Ok(())
    } else {
        Err(ShellError::new(ErrorCode::InvalidValue, format!("invalid session id `{id}`"))
            .with_hint("use 1-64 letters, digits, - or _"))
    }
}

pub fn load_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> Result<T> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|e| ShellError::internal(format!("corrupt session file {}: {e}", path.display()))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(ShellError::io(&e, &path.display().to_string())),
    }
}

pub fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let bytes = serde_json::to_vec(value).map_err(|e| ShellError::internal(e.to_string()))?;
    std::fs::write(&tmp, bytes).map_err(|e| ShellError::io(&e, &tmp.display().to_string()))?;
    std::fs::rename(&tmp, path).map_err(|e| ShellError::io(&e, &path.display().to_string()))
}
