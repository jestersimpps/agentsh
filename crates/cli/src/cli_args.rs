use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "agentsh",
    version,
    disable_help_subcommand = true,
    about = "A shell for AI agents. Every call prints one result: readable text, or a JSON envelope with --format json."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    #[arg(long, short, global = true, env = "AGENTSH_SESSION", default_value = "default")]
    pub session: String,

    #[arg(
        long,
        global = true,
        env = "AGENTSH_ROOT",
        help = "workspace root (default: nearest folder with .agentsh.toml or .git)"
    )]
    pub root: Option<PathBuf>,

    #[arg(long, global = true, value_name = "CAPS", help = "grant capabilities, e.g. exec,net")]
    pub allow: Vec<String>,

    #[arg(long, global = true, value_name = "CAPS", help = "remove capabilities, e.g. delete")]
    pub deny: Vec<String>,

    #[arg(long, global = true, value_name = "BACKEND", help = "auto, none, seatbelt or landlock")]
    pub sandbox: Option<String>,

    #[arg(long, global = true, value_name = "FILE", help = "append every call to this JSONL trace")]
    pub trace: Option<PathBuf>,

    #[arg(long, global = true, help = "pretty-print the JSON envelope")]
    pub pretty: bool,

    #[arg(
        long,
        global = true,
        value_enum,
        env = "AGENTSH_FORMAT",
        default_value = "text",
        help = "text: compact and readable (default); json: the envelope for programs"
    )]
    pub format: Format,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Json,
    Text,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(about = "run a program (argument, --file, or stdin)")]
    Run {
        program: Option<String>,
        #[arg(long, short)]
        file: Option<PathBuf>,
    },
    #[command(about = "print the cheat sheet, or details for one command")]
    Help { topic: Option<String> },
    #[command(about = "revert a run's file changes (default: the latest run)")]
    Undo {
        token: Option<String>,
        #[arg(long)]
        force: bool,
    },
    #[command(about = "list undoable runs and checkpoints")]
    Changes,
    #[command(about = "keep all changes and free undo storage")]
    Commit,
    #[command(about = "undo every uncommitted change in the session")]
    Discard {
        #[arg(long)]
        force: bool,
    },
    #[command(about = "name the current point so you can roll back to it")]
    Checkpoint { name: String },
    #[command(about = "undo everything after a checkpoint")]
    Rollback {
        name: String,
        #[arg(long)]
        force: bool,
    },
    #[command(about = "list, install or remove packages (a human's tool; agents can't run it)")]
    Pkg {
        #[command(subcommand)]
        action: PkgAction,
    },
    #[command(about = "print the JSON Schema of the envelope")]
    Schema,
    #[command(about = "summarize a trace: failures, costly calls, failed-then-fixed pairs")]
    Trace {
        #[arg(help = "trace file (default: the configured trace)")]
        file: Option<PathBuf>,
        #[arg(long, default_value_t = 10, help = "rows per section")]
        top: usize,
        #[arg(long, value_name = "FILE", help = "write failed-then-fixed pairs as JSONL (for training or evals)")]
        pairs: Option<PathBuf>,
    },
    #[command(name = "__sandbox", hide = true)]
    Sandbox {
        policy: String,
        #[arg(last = true)]
        argv: Vec<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum PkgAction {
    #[command(about = "list every package: built-in, configured and installed (with where it came from)")]
    Ls,
    #[command(about = "install a package from a folder or a git URL (private repos use your git credentials)")]
    Add {
        #[arg(help = "package folder, or a git URL (https://, git@, ssh://, file://, *.git)")]
        source: String,
        #[arg(long, value_name = "DIR", help = "subfolder that holds package.toml, e.g. packages/net")]
        path: Option<PathBuf>,
        #[arg(long, value_name = "REF", help = "git branch or tag to install (default: the default branch)")]
        rev: Option<String>,
        #[arg(long, help = "replace an installed package with the same name")]
        force: bool,
    },
    #[command(about = "reinstall installed packages from where they came from (all, or the ones named)")]
    Update { names: Vec<String> },
    #[command(about = "remove an installed package")]
    Rm { name: String },
    #[command(about = "create a new package folder with an example command and a test")]
    New {
        name: String,
        #[arg(long, value_name = "DIR", help = "parent folder (default: current folder)")]
        dir: Option<PathBuf>,
    },
    #[command(about = "check a package folder and run its tests/*.json")]
    Test {
        #[arg(default_value = ".")]
        dir: PathBuf,
    },
}
