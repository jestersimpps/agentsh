mod check;
mod code;
mod data;
mod env;
mod exec;
mod fs;
mod help;
mod jobs;
mod map;
mod packages;
mod text;
mod util;

use agentsh_core::Registry;

pub use help::{brief, command_text, compact, package_text, packages_text};
pub use packages::EMBEDDED;

pub fn registry() -> Registry {
    let mut registry = Registry::default();
    for builtin in [
        env::ENV,
        fs::LS,
        fs::FIND,
        fs::READ,
        fs::EXCERPT,
        fs::STAT,
        map::MAP,
        code::OUTLINE,
        code::REFS,
        text::GREP,
        fs::WRITE,
        fs::EDIT,
        text::REPLACE,
        fs::MKDIR,
        fs::CP,
        fs::MV,
        fs::RM,
        fs::CHMOD,
        fs::CD,
        fs::PWD,
        data::WHERE,
        data::SORT,
        data::TAKE,
        data::SKIP,
        data::FIRST,
        data::LAST,
        data::PICK,
        data::UNIQ,
        data::GROUP,
        data::SUM,
        text::COUNT,
        text::LINES,
        data::JSON,
        data::PRINT,
        data::FAIL,
        exec::EXEC,
        check::CHECK,
        jobs::START,
        jobs::JOBS,
        jobs::LOGS,
        jobs::STOP,
        help::HELP,
    ] {
        match builtin.spec.name {
            "map" | "outline" | "refs" | "check" => registry.add_to("code", builtin),
            _ => registry.add(builtin),
        }
    }
    registry.embed(packages::EMBEDDED);
    registry
}
