use crate::time::iso_now;
use agentsh_spec::Envelope;
use serde::Serialize;
use std::io::Write;
use std::path::Path;

#[derive(Serialize)]
struct TraceLine<'a> {
    ts: String,
    session: &'a str,
    root: String,
    program: &'a str,
    envelope: &'a Envelope,
}

pub fn append(path: &Path, session: &str, root: &Path, program: &str, envelope: &Envelope) {
    let line = TraceLine { ts: iso_now(), session, root: root.display().to_string(), program, envelope };
    let Ok(mut json) = serde_json::to_string(&line) else { return };
    json.push('\n');
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(json.as_bytes());
    }
}
