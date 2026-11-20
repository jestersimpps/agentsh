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

/// One read this run made, so its output can be checked against what the agent has seen.
#[derive(Clone)]
pub struct ReadRecord {
    pub path: PathBuf,
    /// Hash of the text the read returned.
    pub hash: String,
    /// Hash of the whole file it came from (its text is saved in the shown store).
    pub file_hash: String,
    /// The 0-based line range the read returned.
    pub start: usize,
    pub end: usize,
    pub whole: bool,
    pub numbers: bool,
    pub full: bool,
    /// For a large whole-file read: an outline to show instead.
    pub outline: Option<String>,
    /// What to show instead of the text (worked out by `note_read`), if anything is left out.
    pub display: Option<String>,
    /// The file's lines the agent knows once this read is shown.
    pub known: Vec<bool>,
}

const UNCHANGED_NOTE: &str = "[unchanged since you last read it in this session; pass full:true to see it again]";
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
        let old = if self.dedupes() {
            std::fs::read(path).ok().filter(|b| b.len() <= MAX_SHOWN_BYTES).and_then(|b| String::from_utf8(b).ok())
        } else {
            None
        };
        let op = if append {
            self.journal.append_file(self.run_id, path, bytes)
        } else {
            self.journal.write_file(self.run_id, path, bytes)
        }
        .map_err(|e| ShellError::io(&e, &shown))?;
        self.mark_seen(path);
        if self.dedupes()
            && let Ok(written) = std::str::from_utf8(bytes)
        {
            let old = old.unwrap_or_default();
            let new = if append { format!("{old}{written}") } else { written.to_string() };
            self.note_own_write(path, &old, &new);
        }
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
        self.state.views.retain(|p, _| !p.starts_with(path));
        self.effects.push(Effect { op: EffectOp::Delete, path: shown, to: None, bytes: None });
        Ok(())
    }

    pub fn move_path(&mut self, from: &Path, to: &Path) -> Result<()> {
        let (shown_from, shown_to) = (self.display(from), self.display(to));
        self.journal.rename(self.run_id, from, to).map_err(|e| ShellError::io(&e, &shown_from))?;
        self.state.seen.retain(|p, _| !p.starts_with(from));
        self.state.views.retain(|p, _| !p.starts_with(from));
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

    /// Record a read. What it shows the agent is worked out now, in the order statements run
    /// (an edit later in the same program mustn't change it); the session only counts it as
    /// seen once the output is actually shown (see `dedupe_text`).
    pub fn note_read(&mut self, mut record: ReadRecord, file_text: &str) {
        if record.outline.is_none() && self.dedupes() {
            self.save_shown(&record.file_hash, file_text);
            let lines: Vec<&str> = file_text.lines().collect();
            let (mut known, changes) = self.carried_view(&record.path, file_text);
            for earlier in self.read_log.iter().filter(|r| r.path == record.path && r.file_hash == record.file_hash) {
                for (k, e) in known.iter_mut().zip(&earlier.known) {
                    *k |= *e;
                }
            }
            let (start, end) = (record.start.min(lines.len()), record.end.min(lines.len()));
            if !record.full {
                let note = changes_note(&changes, start, end);
                let body = if end > start && known[start..end].iter().all(|k| *k) {
                    Some(if record.whole {
                        UNCHANGED_NOTE.to_string()
                    } else {
                        let shown = self.display(&record.path);
                        format!(
                            "[lines {}–{end} of `{shown}`: already shown, unchanged; full:true shows them again]",
                            start + 1
                        )
                    })
                } else {
                    crate::seen::elide(&lines, start, end, &known, record.numbers)
                };
                record.display = match (body, note) {
                    (Some(body), Some(note)) => Some(format!("{note}\n{body}")),
                    (Some(body), None) => Some(body),
                    (None, Some(note)) => Some(format!("{note}\n{}", slice_lines(&lines, start, end, record.numbers))),
                    (None, None) => None,
                };
            }
            for k in known.iter_mut().take(end).skip(start) {
                *k = true;
            }
            record.known = known;
        }
        self.read_log.push(record);
    }

    /// Whether this session leaves out what the agent has already seen (not the shared default one).
    pub fn dedupes(&self) -> bool {
        self.session_id != "default"
    }

    pub fn dedupe_reads(&mut self, value: &mut Value) {
        if self.read_log.is_empty() {
            return;
        }
        match value {
            Value::Str(text) => self.dedupe_text(text),
            Value::List(items) => {
                for item in items {
                    if let Value::Record(record) = item
                        && let Some(Value::Str(content)) = record.get_mut("content")
                    {
                        self.dedupe_text(content);
                    }
                }
            }
            _ => {}
        }
    }

    /// Replace a read's text with what the agent hasn't seen of it, and remember what it now has.
    fn dedupe_text(&mut self, text: &mut String) {
        let hash = agentsh_platform::hash_bytes(text.as_bytes());
        let Some(record) = self.read_log.iter().find(|r| r.hash == hash).cloned() else { return };
        if let Some(outline) = record.outline.as_ref().filter(|_| !record.full) {
            *text = outline.clone();
            return;
        }
        if !self.dedupes() {
            return;
        }
        if let Some(display) = &record.display {
            *text = display.clone();
        }
        let Some(file_text) = self.load_shown(&record.file_hash) else { return };
        let lines: Vec<&str> = file_text.lines().collect();
        let (start, end) = (record.start.min(lines.len()), record.end.min(lines.len()));
        let mut view = self.state.views.remove(&record.path).unwrap_or_default();
        view.remember(lines[start..end].iter().copied());
        // If the view moved on after this read (the agent edited the file later in this run),
        // carry what this read showed forward instead of going back to the older version.
        let on_disk = hash_file(&record.path).unwrap_or_default();
        if !view.hash.is_empty()
            && view.hash != record.file_hash
            && view.hash == on_disk
            && let Some(newer) = self.load_shown(&view.hash)
        {
            let mut merged = crate::seen::carry(&file_text, &record.known, &newer);
            for (m, k) in merged.iter_mut().zip(view.known.flags(newer.lines().count())) {
                *m |= k;
            }
            let hash = view.hash.clone();
            self.set_view(&record.path, view, &hash, &merged);
            return;
        }
        self.set_view(&record.path, view, &record.file_hash, &record.known);
    }

    /// The agent's known lines of `path`, carried over to `current`, and how the lines it knew
    /// changed since it saw them: (known, (changed spans, removed-after spans)).
    pub fn carried_view(&self, path: &Path, current: &str) -> (Vec<bool>, crate::seen::Changes) {
        let len = current.lines().count();
        let Some(view) = self.state.views.get(path) else { return (vec![false; len], Default::default()) };
        if view.hash == agentsh_platform::hash_bytes(current.as_bytes()) {
            return (view.known.flags(len), Default::default());
        }
        let Some(old) = self.load_shown(&view.hash) else { return (vec![false; len], Default::default()) };
        let old_known = view.known.flags(old.lines().count());
        let changes = crate::seen::changes_to_known(&old, &old_known, current);
        (crate::seen::carry(&old, &old_known, current), changes)
    }

    /// The agent's view of `path`, if it has one (for commands that leave out what it has seen).
    pub fn view(&self, path: &Path) -> Option<&crate::seen::View> {
        if self.dedupes() { self.state.views.get(path) } else { None }
    }

    /// Record that the agent now knows `known` lines (and `view.ever`) of `path` as it is now.
    pub fn set_view(&mut self, path: &Path, mut view: crate::seen::View, file_hash: &str, known: &[bool]) {
        if !self.dedupes() {
            return;
        }
        if !view.hash.is_empty() && view.hash != file_hash && !self.read_log.iter().any(|r| r.file_hash == view.hash) {
            let _ = std::fs::remove_file(self.shown_dir().join(&view.hash));
        }
        view.hash = file_hash.to_string();
        view.known = crate::seen::Lines::from_flags(known);
        self.state.views.insert(path.to_path_buf(), view);
    }

    /// Like `set_view`, for a file version whose text isn't saved yet.
    pub fn set_view_text(&mut self, path: &Path, view: crate::seen::View, text: &str, known: &[bool]) {
        if !self.dedupes() || text.len() > MAX_SHOWN_BYTES {
            return;
        }
        let hash = agentsh_platform::hash_bytes(text.as_bytes());
        self.save_shown(&hash, text);
        self.set_view(path, view, &hash, known);
    }

    /// The agent wrote `new` over `old` through agentsh: it knows the lines it wrote.
    fn note_own_write(&mut self, path: &Path, old: &str, new: &str) {
        if !self.dedupes() || new.len() > MAX_SHOWN_BYTES {
            return;
        }
        let (old_known, _) = self.carried_view(path, old);
        let mut view = self.state.views.remove(path).unwrap_or_default();
        let known = crate::seen::carry_own_edit(old, &old_known, new, &mut view);
        let hash = agentsh_platform::hash_bytes(new.as_bytes());
        self.save_shown(&hash, new);
        self.set_view(path, view, &hash, &known);
    }

    /// Files the agent is up to date on, to tell it afterwards which ones a program changed.
    pub fn views_snapshot(&self) -> Vec<(PathBuf, String)> {
        if !self.dedupes() {
            return Vec::new();
        }
        self.state
            .views
            .iter()
            .filter_map(|(p, v)| hash_file(p).ok().filter(|h| *h == v.hash).map(|h| (p.clone(), h)))
            .collect()
    }

    pub fn report_changed(&mut self, before: Vec<(PathBuf, String)>, program: &str) {
        let mut changed: Vec<String> = before
            .iter()
            .filter(|(p, h)| hash_file(p).ok().as_ref() != Some(h))
            .map(|(p, _)| self.display(p))
            .collect();
        if changed.is_empty() {
            return;
        }
        changed.sort();
        let what = if changed.len() == 1 { "a file" } else { "files" };
        self.warn(format!(
            "`{program}` changed {what} you've read: {}; reading again shows only what changed",
            changed.join(", ")
        ));
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

fn slice_lines(lines: &[&str], start: usize, end: usize, numbers: bool) -> String {
    let body: Vec<String> =
        (start..end).map(|i| if numbers { format!("{}\t{}", i + 1, lines[i]) } else { lines[i].to_string() }).collect();
    body.join("\n") + "\n"
}

/// "[changed since you last saw it: ...]" for the changes that touch lines `start..end`.
fn changes_note(changes: &crate::seen::Changes, start: usize, end: usize) -> Option<String> {
    let (changed, removed) = (&changes.changed, &changes.removed);
    let changed: Vec<(usize, usize)> = changed.iter().copied().filter(|&(a, b)| a <= end && b > start).collect();
    let removed: Vec<(usize, usize)> = removed.iter().copied().filter(|&(at, _)| at >= start && at <= end).collect();
    if changed.is_empty() && removed.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    if !changed.is_empty() {
        let word = if changed.len() == 1 && changed[0].0 == changed[0].1 { "line" } else { "lines" };
        parts.push(format!("{word} {}", crate::seen::spans_text(&changed)));
    }
    for (at, n) in removed.iter().take(4) {
        let what = if *n == 1 { "1 line".to_string() } else { format!("{n} lines") };
        parts.push(if *at == 0 {
            format!("{what} removed at the top")
        } else {
            format!("{what} removed after line {at}")
        });
    }
    Some(format!("[changed since you last saw it: {}]", parts.join("; ")))
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
