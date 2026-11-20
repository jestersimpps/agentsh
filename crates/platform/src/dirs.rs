use directories::{BaseDirs, ProjectDirs};
use std::path::{Path, PathBuf};

fn agentsh_home() -> Option<PathBuf> {
    std::env::var_os("AGENTSH_HOME").map(PathBuf::from)
}

fn project_dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("dev", "agentsh", "agentsh")
}

pub fn global_config_path() -> Option<PathBuf> {
    agentsh_home().or_else(|| project_dirs().map(|d| d.config_dir().to_path_buf())).map(|dir| dir.join("config.toml"))
}

pub fn global_data_dir() -> Option<PathBuf> {
    agentsh_home().or_else(|| project_dirs().map(|d| d.data_dir().to_path_buf()))
}

pub fn find_project_root(start: &Path) -> PathBuf {
    start
        .ancestors()
        .find(|dir| dir.join(".agentsh.toml").is_file() || dir.join(".git").exists())
        .unwrap_or(start)
        .to_path_buf()
}

pub fn expand_home(path: &Path) -> PathBuf {
    let Ok(rest) = path.strip_prefix("~") else { return path.to_path_buf() };
    match BaseDirs::new() {
        Some(base) => base.home_dir().join(rest),
        None => path.to_path_buf(),
    }
}
