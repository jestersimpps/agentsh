use crate::SandboxKind;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxPolicy {
    pub writable: Vec<PathBuf>,
    pub protected: Vec<PathBuf>,
    pub network: bool,
}

pub trait Sandbox {
    fn name(&self) -> &'static str;
    fn command(&self, program: &str, args: &[String], policy: &SandboxPolicy) -> Command;
    fn enforced(&self) -> bool {
        true
    }
}

pub const SANDBOX_MARKER: &str = "AGENTSH_SANDBOX_ACTIVE";

pub fn sandbox_for(kind: SandboxKind) -> Result<Box<dyn Sandbox>, String> {
    if kind != SandboxKind::None && std::env::var_os(SANDBOX_MARKER).is_some() {
        return Ok(Box::new(Inherited));
    }
    match kind {
        SandboxKind::None => Ok(Box::new(NoSandbox { label: "none" })),
        SandboxKind::Auto => Ok(auto_sandbox()),
        SandboxKind::Seatbelt => seatbelt(),
        SandboxKind::Landlock => landlock(),
    }
}

struct Inherited;

impl Sandbox for Inherited {
    fn name(&self) -> &'static str {
        "inherited (already inside an agentsh sandbox)"
    }

    fn command(&self, program: &str, args: &[String], _policy: &SandboxPolicy) -> Command {
        let mut cmd = Command::new(program);
        cmd.args(args);
        cmd
    }
}

struct NoSandbox {
    label: &'static str,
}

impl Sandbox for NoSandbox {
    fn name(&self) -> &'static str {
        self.label
    }

    fn enforced(&self) -> bool {
        false
    }

    fn command(&self, program: &str, args: &[String], _policy: &SandboxPolicy) -> Command {
        let mut cmd = Command::new(program);
        cmd.args(args);
        cmd
    }
}

#[cfg(target_os = "macos")]
fn auto_sandbox() -> Box<dyn Sandbox> {
    seatbelt().unwrap_or_else(|_| Box::new(NoSandbox { label: "none (sandbox-exec unavailable)" }))
}

#[cfg(target_os = "linux")]
fn auto_sandbox() -> Box<dyn Sandbox> {
    landlock().unwrap_or_else(|_| Box::new(NoSandbox { label: "none (landlock unavailable)" }))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn auto_sandbox() -> Box<dyn Sandbox> {
    Box::new(NoSandbox { label: "none (unsupported OS)" })
}

#[cfg(target_os = "macos")]
fn seatbelt() -> Result<Box<dyn Sandbox>, String> {
    if std::path::Path::new(macos::SANDBOX_EXEC).exists() {
        Ok(Box::new(macos::Seatbelt))
    } else {
        Err(format!("{} not found", macos::SANDBOX_EXEC))
    }
}

#[cfg(not(target_os = "macos"))]
fn seatbelt() -> Result<Box<dyn Sandbox>, String> {
    Err("the seatbelt sandbox is only available on macOS".into())
}

#[cfg(target_os = "linux")]
fn landlock() -> Result<Box<dyn Sandbox>, String> {
    match linux::landlock_abi() {
        abi if abi >= 1 => Ok(Box::new(linux::Landlock)),
        _ => {
            Err("landlock is not available (kernel older than 5.13, landlock not enabled, or blocked by seccomp)"
                .into())
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn landlock() -> Result<Box<dyn Sandbox>, String> {
    Err("the landlock sandbox is only available on Linux".into())
}

#[cfg(target_os = "linux")]
pub use linux::landlock_restrict_self;

#[cfg(target_os = "macos")]
mod macos {
    use super::{Sandbox, SandboxPolicy};
    use std::ffi::{CString, c_char, c_int};
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::Command;

    pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

    pub struct Seatbelt;

    impl Sandbox for Seatbelt {
        fn name(&self) -> &'static str {
            "seatbelt"
        }

        fn command(&self, program: &str, args: &[String], policy: &SandboxPolicy) -> Command {
            let mut cmd = Command::new(program);
            cmd.args(args);
            let Ok(profile) = CString::new(profile(policy)) else {
                let mut fallback = Command::new(SANDBOX_EXEC);
                fallback.arg("-p").arg("(version 1)(deny default)").arg(program);
                return fallback;
            };
            unsafe {
                cmd.pre_exec(move || {
                    let mut error: *mut c_char = std::ptr::null_mut();
                    if sandbox_init(profile.as_ptr(), 0, &mut error) == 0 {
                        Ok(())
                    } else {
                        Err(std::io::Error::other("sandbox_init failed"))
                    }
                });
            }
            cmd
        }
    }

    unsafe extern "C" {
        fn sandbox_init(profile: *const c_char, flags: u64, errorbuf: *mut *mut c_char) -> c_int;
    }

    fn profile(policy: &SandboxPolicy) -> String {
        let writable: String = policy.writable.iter().map(|p| format!(" (subpath {})", quote(p))).collect();
        let protected: String = policy.protected.iter().map(|p| format!(" (subpath {})", quote(p))).collect();
        let mut out = String::from("(version 1)\n(allow default)\n(deny file-write*)\n");
        out.push_str(&format!(
            "(allow file-write* (literal \"/dev/null\") (literal \"/dev/zero\") (literal \"/dev/dtracehelper\") (regex #\"^/dev/tty\") (regex #\"^/dev/fd/\"){writable})\n"
        ));
        if !protected.is_empty() {
            out.push_str(&format!("(deny file-write*{protected})\n"));
        }
        if !policy.network {
            out.push_str("(deny network-outbound (remote ip))\n(deny network-inbound (local ip))\n");
            out.push_str("(allow network-outbound (remote ip \"localhost:*\"))\n");
            out.push_str(
                "(allow network-inbound (local ip \"localhost:*\"))\n(allow network-bind (local ip \"localhost:*\"))\n",
            );
        }
        out
    }

    fn quote(path: &Path) -> String {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let text = canonical.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");
        format!("\"{text}\"")
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{Sandbox, SandboxPolicy};
    use landlock::{
        ABI, Access, AccessFs, AccessNet, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    };
    use std::process::Command;

    pub struct Landlock;

    pub fn landlock_abi() -> i64 {
        const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;
        unsafe {
            libc::syscall(
                libc::SYS_landlock_create_ruleset,
                std::ptr::null::<libc::c_void>(),
                0usize,
                LANDLOCK_CREATE_RULESET_VERSION,
            )
        }
    }

    impl Sandbox for Landlock {
        fn name(&self) -> &'static str {
            "landlock"
        }

        fn command(&self, program: &str, args: &[String], policy: &SandboxPolicy) -> Command {
            let exe = std::env::current_exe().unwrap_or_else(|_| "agentsh".into());
            let mut cmd = Command::new(exe);
            cmd.arg("__sandbox")
                .arg(serde_json::to_string(policy).unwrap_or_default())
                .arg("--")
                .arg(program)
                .args(args);
            cmd
        }
    }

    pub fn landlock_restrict_self(policy: &SandboxPolicy) -> Result<bool, String> {
        let abi = ABI::V4;
        let read = AccessFs::from_read(abi);
        let all = AccessFs::from_all(abi);
        let mut ruleset = Ruleset::default().handle_access(all).map_err(|e| e.to_string())?;
        if !policy.network {
            ruleset = ruleset.handle_access(AccessNet::BindTcp | AccessNet::ConnectTcp).map_err(|e| e.to_string())?;
        }
        let mut created = ruleset.create().map_err(|e| e.to_string())?;
        created = created
            .add_rule(PathBeneath::new(PathFd::new("/").map_err(|e| e.to_string())?, read))
            .map_err(|e| e.to_string())?;
        let dev = std::path::PathBuf::from("/dev");
        for path in policy.writable.iter().chain(std::iter::once(&dev)) {
            if let Ok(fd) = PathFd::new(path) {
                created = created.add_rule(PathBeneath::new(fd, all)).map_err(|e| e.to_string())?;
            }
        }
        let status = created.restrict_self().map_err(|e| e.to_string())?;
        Ok(status.ruleset != RulesetStatus::NotEnforced)
    }
}
