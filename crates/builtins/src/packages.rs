//! First-party packages from `packages/`, compiled into the binary so they work offline and always
//! match this build. Add a folder there and a line here to ship a new one.

use agentsh_core::package::EmbeddedPackage;

macro_rules! embed {
    ($name:literal) => {
        EmbeddedPackage {
            manifest: include_str!(concat!("../../../packages/", $name, "/package.toml")),
            brief: include_str!(concat!("../../../packages/", $name, "/brief.md")),
        }
    };
}

/// Load order matters: `check` picks the first detected package that can run a task.
pub const EMBEDDED: &[EmbeddedPackage] = &[
    embed!("code"),
    embed!("rust"),
    embed!("js"),
    embed!("go"),
    embed!("swift"),
    embed!("python"),
    embed!("git"),
    embed!("gh"),
    embed!("http"),
    embed!("sys"),
    embed!("docs"),
];
