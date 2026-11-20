use crate::{SANDBOX_MARKER, Sandbox, SandboxPolicy};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const NONINTERACTIVE_ENV: &[(&str, &str)] = &[
    ("TERM", "dumb"),
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("MANPAGER", "cat"),
    ("EDITOR", "false"),
    ("VISUAL", "false"),
    ("GIT_EDITOR", "false"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("CI", "1"),
    ("NO_COLOR", "1"),
    ("CLICOLOR", "0"),
    ("DEBIAN_FRONTEND", "noninteractive"),
    ("PIP_NO_INPUT", "1"),
    ("PYTHONUNBUFFERED", "1"),
    ("HOMEBREW_NO_AUTO_UPDATE", "1"),
];

const DRAIN_GRACE: Duration = Duration::from_secs(1);

pub struct ExecRequest<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub cwd: &'a Path,
    pub stdin: Option<Vec<u8>>,
    pub env: &'a [(String, String)],
    pub timeout: Duration,
    pub capture_limit: usize,
    pub policy: &'a SandboxPolicy,
}

#[derive(Debug, Default)]
pub struct ExecOutput {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_total: u64,
    pub stderr_total: u64,
    pub timed_out: bool,
    pub elapsed: Duration,
}

#[derive(Default)]
struct Capture {
    bytes: Vec<u8>,
    total: u64,
}

pub fn run_exec(sandbox: &dyn Sandbox, req: ExecRequest) -> std::io::Result<ExecOutput> {
    let mut cmd = base_command(sandbox, req.program, req.args, req.cwd, req.env, req.policy);
    cmd.stdin(if req.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);

    let start = Instant::now();
    let mut child = cmd.spawn()?;
    let pgid = child.id() as i32;

    if let (Some(bytes), Some(mut stdin)) = (req.stdin, child.stdin.take()) {
        thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }
    let (done_tx, done_rx) = mpsc::channel();
    let stdout = spawn_reader(child.stdout.take(), req.capture_limit, done_tx.clone());
    let stderr = spawn_reader(child.stderr.take(), req.capture_limit, done_tx);

    let (exit_tx, exit_rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = exit_tx.send(child.wait());
    });
    let mut timed_out = false;
    let status = match exit_rx.recv_timeout(req.timeout) {
        Ok(status) => status?,
        Err(_) => {
            timed_out = true;
            kill_group(pgid);
            exit_rx.recv().map_err(|_| std::io::Error::other("lost the child process"))??
        }
    };
    kill_group(pgid);
    let drain_deadline = Instant::now() + DRAIN_GRACE;
    for _ in 0..2 {
        let remaining = drain_deadline.saturating_duration_since(Instant::now());
        if done_rx.recv_timeout(remaining).is_err() {
            break;
        }
    }

    let (stdout, stdout_total) = take_capture(&stdout);
    let (stderr, stderr_total) = take_capture(&stderr);
    Ok(ExecOutput {
        exit_code: status.code(),
        signal: status.signal(),
        stdout,
        stderr,
        stdout_total,
        stderr_total,
        timed_out,
        elapsed: start.elapsed(),
    })
}

fn spawn_reader<R: Read + Send + 'static>(
    source: Option<R>,
    limit: usize,
    done: mpsc::Sender<()>,
) -> Arc<Mutex<Capture>> {
    let capture = Arc::new(Mutex::new(Capture::default()));
    let shared = Arc::clone(&capture);
    thread::spawn(move || {
        if let Some(mut source) = source {
            let mut buf = [0u8; 16 * 1024];
            while let Ok(n) = source.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let mut c = shared.lock().unwrap_or_else(|e| e.into_inner());
                c.total += n as u64;
                let room = limit.saturating_sub(c.bytes.len());
                c.bytes.extend_from_slice(&buf[..n.min(room)]);
            }
        }
        let _ = done.send(());
    });
    capture
}

fn take_capture(capture: &Arc<Mutex<Capture>>) -> (Vec<u8>, u64) {
    let mut c = capture.lock().unwrap_or_else(|e| e.into_inner());
    (std::mem::take(&mut c.bytes), c.total)
}

fn kill_group(pgid: i32) {
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

fn base_command(
    sandbox: &dyn Sandbox,
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    policy: &SandboxPolicy,
) -> Command {
    let mut cmd = sandbox.command(program, args, policy);
    cmd.current_dir(cwd)
        .envs(NONINTERACTIVE_ENV.iter().copied())
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    if sandbox.enforced() {
        cmd.env(SANDBOX_MARKER, sandbox.name());
    }
    cmd
}

pub struct SpawnRequest<'a> {
    pub program: &'a str,
    pub args: &'a [String],
    pub cwd: &'a Path,
    pub env: &'a [(String, String)],
    pub log: File,
    pub policy: &'a SandboxPolicy,
}

pub fn spawn_detached(sandbox: &dyn Sandbox, req: SpawnRequest) -> std::io::Result<Child> {
    let mut cmd = base_command(sandbox, req.program, req.args, req.cwd, req.env, req.policy);
    cmd.stdin(Stdio::null()).stdout(req.log.try_clone()?).stderr(req.log);
    unsafe {
        cmd.pre_exec(|| if libc::setsid() == -1 { Err(std::io::Error::last_os_error()) } else { Ok(()) });
    }
    cmd.spawn()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    NotRunning,
    Terminated,
    Killed,
}

impl StopOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            StopOutcome::NotRunning => "already exited",
            StopOutcome::Terminated => "terminated",
            StopOutcome::Killed => "killed",
        }
    }
}

const STOP_POLL: Duration = Duration::from_millis(50);

pub fn process_alive(pid: u32) -> bool {
    signal_alive(pid as i32)
}

pub fn stop_group(pgid: u32, grace: Duration) -> StopOutcome {
    let group = -(pgid as i32);
    if !signal_alive(group) {
        return StopOutcome::NotRunning;
    }
    unsafe {
        libc::kill(group, libc::SIGTERM);
    }
    if wait_gone(group, grace) {
        return StopOutcome::Terminated;
    }
    unsafe {
        libc::kill(group, libc::SIGKILL);
    }
    wait_gone(group, grace);
    StopOutcome::Killed
}

fn wait_gone(target: i32, grace: Duration) -> bool {
    let deadline = Instant::now() + grace;
    while signal_alive(target) {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(STOP_POLL);
    }
    true
}

fn signal_alive(target: i32) -> bool {
    unsafe {
        libc::waitpid(target, std::ptr::null_mut(), libc::WNOHANG);
        libc::kill(target, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SandboxKind, sandbox_for};

    fn run(program: &str, args: &[&str], timeout_ms: u64) -> ExecOutput {
        let sandbox = sandbox_for(SandboxKind::None).unwrap();
        let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        run_exec(
            sandbox.as_ref(),
            ExecRequest {
                program,
                args: &args,
                cwd: Path::new("/"),
                stdin: None,
                env: &[],
                timeout: Duration::from_millis(timeout_ms),
                capture_limit: 1024,
                policy: &SandboxPolicy::default(),
            },
        )
        .unwrap()
    }

    #[test]
    fn captures_output_and_exit_code() {
        let out = run("/bin/sh", &["-c", "echo hi; echo err >&2; exit 3"], 5_000);
        assert_eq!(out.exit_code, Some(3));
        assert_eq!(out.stdout, b"hi\n");
        assert_eq!(out.stderr, b"err\n");
    }

    #[test]
    fn times_out_and_kills_children() {
        let out = run("/bin/sh", &["-c", "sleep 30 & sleep 30"], 200);
        assert!(out.timed_out);
        assert!(out.elapsed < Duration::from_secs(3));
    }

    #[test]
    fn caps_capture_but_counts_total() {
        let out = run("/bin/sh", &["-c", "head -c 5000 /dev/zero"], 5_000);
        assert_eq!(out.stdout.len(), 1024);
        assert_eq!(out.stdout_total, 5000);
    }

    #[test]
    fn background_child_does_not_hang() {
        let out = run("/bin/sh", &["-c", "sleep 30 & echo started"], 5_000);
        assert_eq!(out.stdout, b"started\n");
        assert!(out.elapsed < Duration::from_secs(3));
    }

    #[test]
    fn detached_job_runs_until_its_group_is_stopped() {
        let sandbox = sandbox_for(SandboxKind::None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let args = vec!["-c".to_string(), "sleep 30 & sleep 30".to_string()];
        let request = SpawnRequest {
            program: "/bin/sh",
            args: &args,
            cwd: Path::new("/"),
            env: &[],
            log: File::create(dir.path().join("job.log")).unwrap(),
            policy: &SandboxPolicy::default(),
        };
        let pid = spawn_detached(sandbox.as_ref(), request).unwrap().id();
        assert!(process_alive(pid));
        assert_eq!(stop_group(pid, Duration::from_secs(3)), StopOutcome::Terminated);
        assert!(!process_alive(pid));
        assert_eq!(stop_group(pid, Duration::from_secs(3)), StopOutcome::NotRunning);
    }
}
