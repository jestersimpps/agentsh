use agentsh_core::{Shell, ShellOptions};
use agentsh_platform::{Capability, Overrides};

const PIECES: &[&str] = &[
    "read \"a.txt\"",
    "ls",
    "find \".\"",
    "grep \"a\"",
    "count",
    "take 2",
    "skip 1",
    "sort",
    "uniq",
    "first",
    "last",
    "sum",
    "lines",
    "json",
    "pick a",
    "where a > 1",
    "group a",
    "map",
    "outline \"a.txt\"",
    "refs \"a\"",
    "env",
    "write \"b.txt\" \"x\"",
    "edit \"a.txt\" old:\"a\" new:\"b\"",
    "rm \"b.txt\"",
    "mkdir \"d\"",
    "cd \"d\"",
    "cd",
    "ago(9999999999999999d)",
    "ago(-5)",
    "now()",
    "len($x)",
    "int(\"99999999999999999999\")",
    "int(1e300)",
    "split(\"a,b\", \",\")",
    "glob(\"**/*\")",
    "exists(\"a.txt\")",
    "$x[-1]",
    "$x[99999999999]",
    "$x.a",
    "9223372036854775807 + 1",
    "-9223372036854775807 - 10",
    "5 / 0",
    "5 % 0",
    "1.5 * 1e308 * 10",
    "[1, 2] + [3]",
    "{a: 1}",
    "[{a: 1}, {a: 2}]",
    "\"x\" =~ \"(\"",
    "\"x\" =~ \"a*\"",
    "let x = [1, 2, 3]",
    "let x = \"abc\"",
    "for $i in [1, 2] { $i }",
    "if true { 1 } else { 2 }",
    "try { rm \"zz\" } catch $e { $e }",
    "print 1",
    "help",
    "help \"edit\"",
    "read \"a.txt\" from:99999999 to:1",
    "read \"a.txt\" tail:0",
    "take -1",
    "start \"sleep 1\"",
    "exec \"true\"",
    "check",
    " | ",
    "\n",
    "; ",
    "(",
    ")",
    "[",
    "]",
    "$_",
    "$r1",
];

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn random_programs_never_crash_the_shell() {
    let workspace = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("a.txt"), "a\nb\n{\"a\": 1}\n").unwrap();
    unsafe { std::env::set_var("AGENTSH_HOME", home.path()) };
    let options = ShellOptions {
        start_dir: workspace.path().to_path_buf(),
        root: Some(workspace.path().to_path_buf()),
        session: "fuzz".into(),
        overrides: Overrides { deny: vec![Capability::Exec, Capability::Net], ..Overrides::default() },
    };
    let shell = Shell::open(options, agentsh_builtins::registry()).unwrap();
    let iterations: usize = std::env::var("AGENTSH_FUZZ_ITERS").ok().and_then(|v| v.parse().ok()).unwrap_or(3_000);
    let seed: u64 =
        std::env::var("AGENTSH_FUZZ_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x9E37_79B9_7F4A_7C15);
    let mut rng = Rng(seed);
    for _ in 0..iterations {
        let len = 1 + (rng.next() % 5) as usize;
        let program: Vec<&str> = (0..len).map(|_| PIECES[(rng.next() % PIECES.len() as u64) as usize]).collect();
        let joiner = if rng.next().is_multiple_of(2) { " | " } else { "\n" };
        let source = program.join(joiner);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| shell.run(&source)));
        assert!(result.is_ok(), "panicked on program:\n{source}");
    }
}
