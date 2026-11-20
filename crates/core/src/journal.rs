use crate::session::{load_json, save_json};
use crate::{Result, ShellError};
use agentsh_platform::{hash_bytes, hash_file};
use agentsh_spec::{EffectOp, ErrorCode};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum JournalOp {
    Create { path: PathBuf, after: Option<String>, is_dir: bool },
    Modify { path: PathBuf, backup: String, after: String },
    Delete { path: PathBuf, backup: String },
    Move { from: PathBuf, to: PathBuf },
    Mkdir { path: PathBuf },
    Chmod { path: PathBuf, before: u32, after: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    pub run: u64,
    #[serde(flatten)]
    pub op: JournalOp,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct JournalFile {
    entries: Vec<JournalEntry>,
    next_blob: u64,
}

#[derive(Debug, Clone)]
pub struct UndoAction {
    pub op: EffectOp,
    pub path: PathBuf,
    pub to: Option<PathBuf>,
}

enum Expect {
    Absent,
    Present,
    Hash(String),
    Mode(u32),
}

pub struct Journal {
    dir: PathBuf,
    data: JournalFile,
}

impl Journal {
    pub fn load(session_dir: &Path) -> Result<Journal> {
        let data = load_json(&session_dir.join("journal.json"))?;
        Ok(Journal { dir: session_dir.to_path_buf(), data })
    }

    pub fn save(&self) -> Result<()> {
        save_json(&self.dir.join("journal.json"), &self.data)
    }

    pub fn len(&self) -> usize {
        self.data.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.entries.is_empty()
    }

    pub fn entries(&self) -> &[JournalEntry] {
        &self.data.entries
    }

    pub fn last_run(&self) -> Option<u64> {
        self.data.entries.last().map(|e| e.run)
    }

    pub fn blob_bytes(&self) -> u64 {
        dir_size(&self.dir.join("blobs"))
    }

    fn push(&mut self, run: u64, op: JournalOp) {
        self.data.entries.push(JournalEntry { run, op });
    }

    fn new_blob(&mut self) -> io::Result<(String, PathBuf)> {
        let blobs = self.dir.join("blobs");
        std::fs::create_dir_all(&blobs)?;
        self.data.next_blob += 1;
        let id = self.data.next_blob.to_string();
        Ok((id.clone(), blobs.join(id)))
    }

    fn blob_path(&self, id: &str) -> PathBuf {
        self.dir.join("blobs").join(id)
    }

    pub fn write_file(&mut self, run: u64, path: &Path, bytes: &[u8]) -> io::Result<EffectOp> {
        if path.exists() {
            let (backup, blob) = self.new_blob()?;
            std::fs::copy(path, &blob)?;
            std::fs::write(path, bytes)?;
            self.push(run, JournalOp::Modify { path: path.into(), backup, after: hash_bytes(bytes) });
            return Ok(EffectOp::Modify);
        }
        if let Some(parent) = path.parent() {
            self.mkdir_all(run, parent)?;
        }
        std::fs::write(path, bytes)?;
        self.push(run, JournalOp::Create { path: path.into(), after: Some(hash_bytes(bytes)), is_dir: false });
        Ok(EffectOp::Create)
    }

    pub fn append_file(&mut self, run: u64, path: &Path, bytes: &[u8]) -> io::Result<EffectOp> {
        if !path.exists() {
            return self.write_file(run, path, bytes);
        }
        let (backup, blob) = self.new_blob()?;
        std::fs::copy(path, &blob)?;
        std::fs::OpenOptions::new().append(true).open(path)?.write_all(bytes)?;
        self.push(run, JournalOp::Modify { path: path.into(), backup, after: hash_file(path)? });
        Ok(EffectOp::Modify)
    }

    pub fn mkdir_all(&mut self, run: u64, path: &Path) -> io::Result<bool> {
        let missing: Vec<&Path> = path.ancestors().take_while(|p| !p.exists()).collect();
        for dir in missing.iter().rev() {
            std::fs::create_dir(dir)?;
            self.push(run, JournalOp::Mkdir { path: dir.to_path_buf() });
        }
        Ok(!missing.is_empty())
    }

    pub fn remove(&mut self, run: u64, path: &Path) -> io::Result<()> {
        let (backup, blob) = self.new_blob()?;
        if std::fs::rename(path, &blob).is_err() {
            copy_recursive(path, &blob)?;
            remove_any(path)?;
        }
        self.push(run, JournalOp::Delete { path: path.into(), backup });
        Ok(())
    }

    pub fn rename(&mut self, run: u64, from: &Path, to: &Path) -> io::Result<()> {
        if let Some(parent) = to.parent() {
            self.mkdir_all(run, parent)?;
        }
        std::fs::rename(from, to)?;
        self.push(run, JournalOp::Move { from: from.into(), to: to.into() });
        Ok(())
    }

    pub fn copy(&mut self, run: u64, from: &Path, to: &Path) -> io::Result<()> {
        if let Some(parent) = to.parent() {
            self.mkdir_all(run, parent)?;
        }
        copy_recursive(from, to)?;
        let is_dir = to.is_dir();
        let after = if is_dir { None } else { Some(hash_file(to)?) };
        self.push(run, JournalOp::Create { path: to.into(), after, is_dir });
        Ok(())
    }

    pub fn chmod(&mut self, run: u64, path: &Path, mode: u32) -> io::Result<u32> {
        let before = file_mode(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
        self.push(run, JournalOp::Chmod { path: path.into(), before, after: mode });
        Ok(before)
    }

    pub fn undo_where(
        &mut self,
        select: impl Fn(usize, &JournalEntry) -> bool,
        force: bool,
    ) -> Result<Vec<UndoAction>> {
        let selected: Vec<usize> =
            self.data.entries.iter().enumerate().filter(|(i, e)| select(*i, e)).map(|(i, _)| i).collect();
        if !force {
            self.check_conflicts(&selected)?;
        }
        let mut actions = Vec::new();
        for &i in selected.iter().rev() {
            let entry = self.data.entries[i].clone();
            if let Some(action) = self.revert(&entry.op).map_err(|e| ShellError::io(&e, "undo"))? {
                actions.push(action);
            }
        }
        let selected: HashSet<usize> = selected.into_iter().collect();
        let mut index = 0;
        self.data.entries.retain(|_| {
            let keep = !selected.contains(&index);
            index += 1;
            keep
        });
        Ok(actions)
    }

    pub fn clear(&mut self) -> io::Result<()> {
        self.data.entries.clear();
        let blobs = self.dir.join("blobs");
        if blobs.exists() {
            std::fs::remove_dir_all(blobs)?;
        }
        Ok(())
    }

    fn check_conflicts(&self, selected: &[usize]) -> Result<()> {
        let mut touched = HashSet::new();
        let mut conflicts = Vec::new();
        for &i in selected.iter().rev() {
            for (path, expect) in expectations(&self.data.entries[i].op) {
                let covered = touched.iter().any(|t: &PathBuf| path.starts_with(t));
                if !covered && !holds(&path, &expect) {
                    conflicts.push(path.display().to_string());
                }
                touched.insert(path);
            }
        }
        if conflicts.is_empty() {
            return Ok(());
        }
        Err(ShellError::new(ErrorCode::Conflict, format!("changed since the action being undone: {}", conflicts.join(", ")))
            .with_value(conflicts.join(", "))
            .with_hint("something else modified these paths afterwards; undo the later changes first, or force the undo to overwrite them"))
    }

    fn revert(&self, op: &JournalOp) -> io::Result<Option<UndoAction>> {
        Ok(Some(match op {
            JournalOp::Create { path, .. } => {
                if path.exists() || path.is_symlink() {
                    remove_any(path)?;
                }
                UndoAction { op: EffectOp::Delete, path: path.clone(), to: None }
            }
            JournalOp::Modify { path, backup, .. } => {
                let blob = self.blob_path(backup);
                std::fs::copy(&blob, path)?;
                std::fs::remove_file(blob)?;
                UndoAction { op: EffectOp::Modify, path: path.clone(), to: None }
            }
            JournalOp::Delete { path, backup } => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if path.exists() {
                    remove_any(path)?;
                }
                std::fs::rename(self.blob_path(backup), path)?;
                UndoAction { op: EffectOp::Create, path: path.clone(), to: None }
            }
            JournalOp::Move { from, to } => {
                if let Some(parent) = from.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::rename(to, from)?;
                UndoAction { op: EffectOp::Move, path: to.clone(), to: Some(from.clone()) }
            }
            JournalOp::Mkdir { path } => {
                if std::fs::remove_dir(path).is_err() {
                    return Ok(None);
                }
                UndoAction { op: EffectOp::Delete, path: path.clone(), to: None }
            }
            JournalOp::Chmod { path, before, .. } => {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(*before))?;
                UndoAction { op: EffectOp::Modify, path: path.clone(), to: None }
            }
        }))
    }
}

fn expectations(op: &JournalOp) -> Vec<(PathBuf, Expect)> {
    match op {
        JournalOp::Create { path, after, .. } => {
            vec![(path.clone(), after.clone().map(Expect::Hash).unwrap_or(Expect::Present))]
        }
        JournalOp::Modify { path, after, .. } => vec![(path.clone(), Expect::Hash(after.clone()))],
        JournalOp::Delete { path, .. } => vec![(path.clone(), Expect::Absent)],
        JournalOp::Move { from, to } => vec![(to.clone(), Expect::Present), (from.clone(), Expect::Absent)],
        JournalOp::Mkdir { path } => vec![(path.clone(), Expect::Present)],
        JournalOp::Chmod { path, after, .. } => vec![(path.clone(), Expect::Mode(*after))],
    }
}

fn holds(path: &Path, expect: &Expect) -> bool {
    match expect {
        Expect::Absent => !path.exists() && !path.is_symlink(),
        Expect::Present => path.exists(),
        Expect::Hash(h) => hash_file(path).is_ok_and(|actual| &actual == h),
        Expect::Mode(m) => file_mode(path).is_ok_and(|actual| actual == *m),
    }
}

pub fn file_mode(path: &Path) -> io::Result<u32> {
    Ok(std::fs::metadata(path)?.permissions().mode() & 0o7777)
}

pub fn remove_any(path: &Path) -> io::Result<()> {
    if path.is_dir() && !path.is_symlink() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) }
}

pub fn copy_recursive(from: &Path, to: &Path) -> io::Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.is_symlink() {
        let target = std::fs::read_link(from)?;
        return std::os::unix::fs::symlink(target, to);
    }
    if !meta.is_dir() {
        return std::fs::copy(from, to).map(|_| ());
    }
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
    }
    Ok(())
}

fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path).map(|entries| entries.flatten().map(|e| dir_size(&e.path())).sum()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(root: &Path) -> Vec<(String, Vec<u8>, u32)> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let p = entry.path();
                let rel = p.strip_prefix(root).unwrap().display().to_string();
                let mode = file_mode(&p).unwrap();
                if p.is_dir() {
                    out.push((rel + "/", Vec::new(), mode));
                    stack.push(p);
                } else {
                    out.push((rel, std::fs::read(&p).unwrap(), mode));
                }
            }
        }
        out.sort();
        out
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
            &items[(self.next() % items.len() as u64) as usize]
        }
    }

    #[test]
    fn random_operations_undo_to_original_tree() {
        for seed in 1..=300u64 {
            let ws = tempfile::tempdir().unwrap();
            let session = tempfile::tempdir().unwrap();
            let root = ws.path();
            std::fs::create_dir_all(root.join("a/b")).unwrap();
            std::fs::write(root.join("a/one.txt"), "one").unwrap();
            std::fs::write(root.join("a/b/two.txt"), "two").unwrap();
            std::fs::write(root.join("three.txt"), "three").unwrap();
            let before = snapshot(root);

            let mut journal = Journal::load(session.path()).unwrap();
            let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            let names = ["a/one.txt", "a/b/two.txt", "three.txt", "new.txt", "a/c/new.txt", "a/b", "a", "x/y/z.txt"];
            for step in 0..12u64 {
                let run = step / 3 + 1;
                let target = root.join(rng.pick(&names));
                let other = root.join(rng.pick(&names));
                let _ = match rng.next() % 7 {
                    0 if !target.is_dir() => {
                        journal.write_file(run, &target, format!("w{step}").as_bytes()).map(|_| ())
                    }
                    1 if target.is_file() => journal.append_file(run, &target, b"+").map(|_| ()),
                    2 if target.exists() => journal.remove(run, &target),
                    3 if target.exists() && !other.exists() && !other.starts_with(&target) => {
                        journal.rename(run, &target, &other)
                    }
                    4 if target.exists() && !other.exists() && !other.starts_with(&target) => {
                        journal.copy(run, &target, &other)
                    }
                    5 => journal.mkdir_all(run, &target.with_extension("d")).map(|_| ()),
                    6 if target.exists() => {
                        journal.chmod(run, &target, if step % 2 == 0 { 0o700 } else { 0o755 }).map(|_| ())
                    }
                    _ => Ok(()),
                };
            }
            journal.undo_where(|_, _| true, false).unwrap();
            assert_eq!(snapshot(root), before, "seed {seed}");
            assert!(journal.is_empty());
        }
    }

    #[test]
    fn conflict_detected_when_file_changed_later() {
        let ws = tempfile::tempdir().unwrap();
        let session = tempfile::tempdir().unwrap();
        let file = ws.path().join("f.txt");
        std::fs::write(&file, "v1").unwrap();
        let mut journal = Journal::load(session.path()).unwrap();
        journal.write_file(1, &file, b"v2").unwrap();
        std::fs::write(&file, "edited by someone else").unwrap();
        let err = journal.undo_where(|_, e| e.run == 1, false).unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
        journal.undo_where(|_, e| e.run == 1, true).unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "v1");
    }
}
