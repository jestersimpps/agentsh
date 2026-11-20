mod check;
mod code;
mod data;
mod env;
mod exec;
mod fs;
mod help;
mod jobs;
mod map;
mod text;
mod util;

use agentsh_core::Registry;

pub use help::{brief, command_text, compact};

pub fn registry() -> Registry {
    let mut registry = Registry::default();
    for builtin in [
        env::ENV,
        fs::LS,
        fs::FIND,
        fs::READ,
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
        exec::EXEC,
        check::CHECK,
        jobs::START,
        jobs::JOBS,
        jobs::LOGS,
        jobs::STOP,
        help::HELP,
    ] {
        registry.add(builtin);
    }
    registry
}
