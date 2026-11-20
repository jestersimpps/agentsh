use crate::journal::Journal;
use crate::redact::Redactor;
use crate::session::SessionState;
use crate::suggest::closest;
use crate::{Registry, Result, ShellError};
use agentsh_platform::{Capability, Config, Sandbox, SandboxPolicy, Secrets, hash_file};
use agentsh_spec::{Effect, EffectOp, ErrorCode, Value};
use regex::Regex;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

pub struct Ctx<'a> {
    pub root: PathBuf,
    pub extra_roots: Vec<PathBuf>,
    pub config: &'a Config,
    pub registry: &'a Registry,
    pub sandbox: &'a dyn Sandbox,
    pub secrets: &'a Secrets,
    pub session_id: &'a str,
    pub handles_dir: PathBuf,
    pub state: SessionState,
    pub journal: Journal,
    pub run_id: u64,
    pub effects: Vec<Effect>,
    pub printed: Vec<Value>,
    pub warnings: Vec<String>,
    pub redactor: Redactor,
    pub exec_used: bool,
    pub exec_completed: bool,
    pub keep_on_failure: bool,
    pub read_log: Vec<ReadRecord>,
    pub outputs: Vec<(String, Value)>,
    pub last_label: Option<String>,
    pub started_jobs: Vec<(String, u32)>,
    pub unshortened: bool,
    pub regex_cache: HashMap<String, Regex>,
    /// While a package command runs: its package and the capabilities that package declared.
    pub cap_ceiling: Option<(&'static str, &'static [Capability])>,
    pub script_depth: usize,
}

pub struct ReadRecord {
    pub path: PathBuf,
    pub hash: String,
    pub full: bool,
}

const UNCHANGED_NOTE: &str = "[unchanged since you last read it in this session; pass full:true to see it again]";
const CHANGED_NOTE: &str =
    "[changed since you last read it; diff against that version below; pass full:true for the whole file]";
const REPEAT_NOTE: &str = "[same result as the last time you ran this in this session; the full value is in $_]";
const MIN_REPEAT_BYTES: usize = 300;
const MAX_SHOWN_BYTES: usize = 1024 * 1024;

const CAPABILITY_HINT: &str =
    "a human can enable it with --allow <cap>, AGENTSH_ALLOW=<cap>, or [capabilities] allow in .agentsh.toml";

impl<'a> Ctx<'a> {
    pub fn cwd(&self) -> PathBuf {
        normalize(&self.root.join(&self.state.cwd))
    }

    pub fn require(&self, cap: Capability) -> Result<()> {
        if let Some((package, caps)) = self.cap_ceiling
            && !caps.contains(&cap)
        {
            return Err(ShellError::new(
                ErrorCode::CapabilityDenied,
                format!("package `{package}` doesn't declare the `{cap}` capability"),
            )
            .with_value(cap.as_str())
            .with_hint(format!(
                "package commands can only use the caps listed in their package.toml; `{package}` needs a fix"
            )));
        }
        if self.config.allows(cap) {
            return Ok(());
        }
        Err(ShellError::new(
            ErrorCode::CapabilityDenied,
            format!("the `{cap}` capability is not enabled in this session"),
        )
        .with_value(cap.as_str())
        .with_hint(CAPABILITY_HINT.replace("<cap>", cap.as_str())))
    }

    pub fn resolve(&self, raw: &str) -> Result<PathBuf> {
        let expanded = expand_tilde(raw);
        let joined = if expanded.is_absolute() { expanded } else { self.cwd().join(expanded) };
        let path = normalize(&joined);
        let real = real_path(&path);
        let inside = |p: &Path| self.roots().any(|root| p.starts_with(root));
        if inside(&path) && inside(&real) {
            return Ok(path);
        }
        Err(ShellError::new(ErrorCode::OutsideWorkspace, format!("`{raw}` is outside the workspace"))
            .with_value(raw)
            .with_hint(format!(
                "paths must stay inside {} (add more under [workspace] roots in .agentsh.toml)",
                self.root.display()
            )))
    }

    pub fn resolve_for_write(&self, raw: &str) -> Result<PathBuf> {
        let path = self.resolve(raw)?;
        if self.protected_paths().iter().any(|p| path.starts_with(p)) {
            return Err(ShellError::new(
                ErrorCode::ProtectedPath,
                format!("`{raw}` is managed by agentsh and can't be changed"),
            )
            .with_value(raw)
            .with_hint("session data and .agentsh.toml are off-limits to agents"));
        }
        Ok(path)
    }

    pub fn protected_paths(&self) -> Vec<PathBuf> {
        vec![self.root.join(".agentsh"), self.root.join(".agentsh.toml")]
    }

    pub fn sandbox_policy(&self) -> SandboxPolicy {
        let mut writable: Vec<PathBuf> = self.roots().cloned().collect();
        writable.extend(self.config.sandbox_writable.iter().cloned());
        writable.extend(temp_dirs());
        SandboxPolicy { writable, protected: self.protected_paths(), network: self.config.allows(Capability::Net) }
    }

    fn roots(&self) -> impl Iterator<Item = &PathBuf> {
        std::iter::once(&self.root).chain(self.extra_roots.iter())
    }

    pub fn display(&self, path: &Path) -> String {
        match path.strip_prefix(&self.root) {
            Ok(rel) if rel.as_os_str().is_empty() => ".".into(),
            Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
            Err(_) => path.to_string_lossy().into_owned(),
        }
    }

    pub fn not_found(&self, raw: &str, path: &Path) -> ShellError {
        let err = ShellError::new(ErrorCode::NotFound, format!("`{raw}` does not exist")).with_value(raw);
        if raw.contains(' ') {
            let parts: Vec<String> = raw.split_whitespace().map(|p| format!("\"{p}\"")).collect();
            return err.with_hint(format!("for several paths pass a list: [{}]", parts.join(", ")));
        }
        let Some(parent) = path.parent().filter(|p| p.is_dir()) else {
            return err.with_hint("its parent folder doesn't exist either; try ls or find");
        };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let siblings: Vec<String> = std::fs::read_dir(parent)
            .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        match closest(&name, siblings.iter().map(String::as_str)) {
            Some(best) => {
                let suggestion = self.display(&parent.join(best));
                err.with_hint(format!("did you mean \"{suggestion}\"?"))
            }
            None => err.with_hint(format!("nothing similar in \"{0}\"; list it with ls \"{0}\"", self.display(parent))),
        }
    }

    pub fn existing_or_close(&mut self, raw: &str) -> Result<PathBuf> {
        let path = self.resolve(raw)?;
        if path.exists() || path.is_symlink() {
            return Ok(path);
        }
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let close: Vec<PathBuf> = path
            .parent()
            .and_then(|parent| std::fs::read_dir(parent).ok().map(|rd| (parent.to_path_buf(), rd)))
            .map(|(parent, rd)| {
                rd.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|candidate| strsim::damerau_levenshtein(&name, candidate) <= 2)
                    .map(|candidate| parent.join(candidate))
                    .collect()
            })
            .unwrap_or_default();
        match close.as_slice() {
            [only] if only.is_file() => {
                let shown = self.display(only);
                self.warn(format!("`{raw}` doesn't exist; showing \"{shown}\" instead"));
                Ok(only.clone())
            }
            _ => Err(self.not_found(raw, &path)),
        }
    }

    pub fn split_path_list(&self, raws: Vec<String>) -> Vec<String> {
        raws.into_iter()
            .flat_map(|raw| {
                let exists = |p: &str| self.resolve(p).is_ok_and(|path| path.exists());
                let parts: Vec<String> = raw.split_whitespace().map(str::to_owned).collect();
                if parts.len() > 1 && !exists(&raw) && parts.iter().all(|p| exists(p)) { parts } else { vec![raw] }
            })
            .collect()
    }

    pub fn existing(&self, raw: &str) -> Result<PathBuf> {
        let path = self.resolve(raw)?;
        if path.exists() || path.is_symlink() { Ok(path) } else { Err(self.not_found(raw, &path)) }
    }

    pub fn check_fresh(&self, path: &Path) -> Result<()> {
        let Some(seen) = self.state.seen.get(path) else { return Ok(()) };
        let current = hash_file(path).unwrap_or_default();
        if &current == seen {
            return Ok(());
        }
        let shown = self.display(path);
        Err(ShellError::new(ErrorCode::Stale, format!("`{shown}` changed since you last read it"))
            .with_value(shown.clone())
            .with_hint(format!("read \"{shown}\" again, then retry")))
    }

    pub fn mark_seen(&mut self, path: &Path) {
        if let Ok(hash) = hash_file(path) {
            self.state.seen.insert(path.to_path_buf(), hash);
        }
    }

    pub fn write_file(&mut self, path: &Path, bytes: &[u8], append: bool) -> Result<EffectOp> {
        let shown = self.display(path);
        let op = if append {
            self.journal.append_file(self.run_id, path, bytes)
        } else {
            self.journal.write_file(self.run_id, path, bytes)
        }
        .map_err(|e| ShellError::io(&e, &shown))?;
        self.mark_seen(path);
        let size = std::fs::metadata(path).map(|m| m.len()).ok();
        match self.effects.iter_mut().find(|e| e.path == shown && matches!(e.op, EffectOp::Create | EffectOp::Modify)) {
            Some(existing) => existing.bytes = size,
            None => self.effects.push(Effect { op, path: shown, to: None, bytes: size }),
        }
        Ok(op)
    }

    pub fn remove_path(&mut self, path: &Path) -> Result<()> {
        let shown = self.display(path);
        self.journal.remove(self.run_id, path).map_err(|e| ShellError::io(&e, &shown))?;
        self.state.seen.retain(|p, _| !p.starts_with(path));
        self.effects.push(Effect { op: EffectOp::Delete, path: shown, to: None, bytes: None });
        Ok(())
    }

    pub fn move_path(&mut self, from: &Path, to: &Path) -> Result<()> {
        let (shown_from, shown_to) = (self.display(from), self.display(to));
        self.journal.rename(self.run_id, from, to).map_err(|e| ShellError::io(&e, &shown_from))?;
        self.state.seen.retain(|p, _| !p.starts_with(from));
        self.effects.push(Effect { op: EffectOp::Move, path: shown_from, to: Some(shown_to), bytes: None });
        Ok(())
    }

    pub fn copy_path(&mut self, from: &Path, to: &Path) -> Result<()> {
        let shown_to = self.display(to);
        self.journal.copy(self.run_id, from, to).map_err(|e| ShellError::io(&e, &shown_to))?;
        self.effects.push(Effect { op: EffectOp::Create, path: shown_to, to: None, bytes: None });
        Ok(())
    }

    pub fn set_mode(&mut self, path: &Path, mode: u32) -> Result<u32> {
        let shown = self.display(path);
        let before = self.journal.chmod(self.run_id, path, mode).map_err(|e| ShellError::io(&e, &shown))?;
        if before != mode {
            self.effects.push(Effect { op: EffectOp::Modify, path: shown, to: None, bytes: None });
        }
        Ok(before)
    }

    pub fn make_dir(&mut self, path: &Path) -> Result<bool> {
        let shown = self.display(path);
        let created = self.journal.mkdir_all(self.run_id, path).map_err(|e| ShellError::io(&e, &shown))?;
        if created {
            self.effects.push(Effect { op: EffectOp::Mkdir, path: shown, to: None, bytes: None });
        }
        Ok(created)
    }

    pub fn note_read(&mut self, path: &Path, text: &str, full: bool) {
        self.read_log.push(ReadRecord {
            path: path.to_path_buf(),
            hash: agentsh_platform::hash_bytes(text.as_bytes()),
            full,
        });
    }

    pub fn dedupe_reads(&mut self, value: &mut Value) {
        if self.read_log.is_empty() {
            return;
        }
        let dedupe = self.session_id != "default";
        match value {
            Value::Str(text) => self.dedupe_text(text, dedupe),
            Value::List(items) => {
                for item in items {
                    if let Value::Record(record) = item
                        && let Some(Value::Str(content)) = record.get_mut("content")
                    {
                        self.dedupe_text(content, dedupe);
                    }
                }
            }
            _ => {}
        }
    }

    fn dedupe_text(&mut self, text: &mut String, dedupe: bool) {
        let hash = agentsh_platform::hash_bytes(text.as_bytes());
        let Some(record) = self.read_log.iter().find(|r| r.hash == hash) else { return };
        let (path, full) = (record.path.clone(), record.full);
        let previous = self.state.shown.get(&path).cloned();
        if dedupe && !full && previous.as_deref() == Some(hash.as_str()) {
            *text = UNCHANGED_NOTE.into();
            return;
        }
        let original = std::mem::take(text);
        *text = match previous.as_deref().filter(|_| dedupe && !full).and_then(|h| self.load_shown(h)) {
            Some(old) => {
                let diff = similar::TextDiff::from_lines(&old, &original).unified_diff().context_radius(2).to_string();
                if diff.len() * 10 < original.len() * 6 { format!("{CHANGED_NOTE}\n{diff}") } else { original.clone() }
            }
            None => original.clone(),
        };
        if let Some(old_hash) = previous.filter(|h| *h != hash) {
            let _ = std::fs::remove_file(self.shown_dir().join(old_hash));
        }
        self.save_shown(&hash, &original);
        self.state.shown.insert(path, hash);
    }

    pub fn dedupe_repeat(&mut self, label: &str, value: &mut Value) {
        if self.session_id == "default" || *value == Value::Null {
            return;
        }
        let Ok(serialized) = serde_json::to_string(value) else { return };
        let key = agentsh_platform::hash_bytes(label.as_bytes());
        let output = agentsh_platform::hash_bytes(serialized.as_bytes());
        let repeated = self.state.last_outputs.get(&key) == Some(&output);
        self.state.last_outputs.insert(key, output);
        if repeated && serialized.len() >= MIN_REPEAT_BYTES {
            *value = Value::str(REPEAT_NOTE);
        }
    }

    fn shown_dir(&self) -> PathBuf {
        self.handles_dir.with_file_name("shown")
    }

    fn save_shown(&self, hash: &str, text: &str) {
        if text.len() <= MAX_SHOWN_BYTES && std::fs::create_dir_all(self.shown_dir()).is_ok() {
            let _ = std::fs::write(self.shown_dir().join(hash), text);
        }
    }

    fn load_shown(&self, hash: &str) -> Option<String> {
        std::fs::read_to_string(self.shown_dir().join(hash)).ok()
    }

    pub fn warn(&mut self, message: impl Into<String>) {
        let message = message.into();
        if !self.warnings.contains(&message) {
            self.warnings.push(message);
        }
    }
}

pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn real_path(path: &Path) -> PathBuf {
    let existing = path.ancestors().find(|p| p.exists()).unwrap_or(Path::new("/"));
    let rest = path.strip_prefix(existing).unwrap_or(Path::new(""));
    existing.canonicalize().unwrap_or_else(|_| existing.to_path_buf()).join(rest)
}

fn expand_tilde(raw: &str) -> PathBuf {
    match raw.strip_prefix("~/").or(if raw == "~" { Some("") } else { None }) {
        Some(rest) => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join(rest),
        None => PathBuf::from(raw),
    }
}

fn temp_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![std::env::temp_dir()];
    if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/private/tmp"));
    } else {
        dirs.push(PathBuf::from("/tmp"));
    }
    dirs.into_iter().filter_map(|d| d.canonicalize().ok()).collect()
}
