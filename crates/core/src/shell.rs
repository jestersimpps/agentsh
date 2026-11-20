use crate::journal::{Journal, JournalOp, UndoAction};
use crate::limits::{estimate_tokens, shape};
use crate::redact::Redactor;
use crate::session::SessionStore;
use crate::{Ctx, Registry, Result, ShellError, trace};
use agentsh_platform::{Config, Overrides, Sandbox, Secrets, find_project_root, sandbox_for};
use agentsh_spec::{Effect, EffectOp, Envelope, ErrorCode, Output, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

const MAX_HANDLE_BYTES: usize = 5 * 1024 * 1024;
const UNSHORTENED_BYTES: usize = 400_000;

pub struct ShellOptions {
    pub start_dir: PathBuf,
    pub root: Option<PathBuf>,
    pub session: String,
    pub overrides: Overrides,
}

pub struct Shell {
    root: PathBuf,
    start_dir: PathBuf,
    extra_roots: Vec<PathBuf>,
    config: Config,
    registry: Registry,
    sandbox: Box<dyn Sandbox>,
    secrets: Secrets,
    session: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Run,
    Control,
}

impl Shell {
    pub fn open(options: ShellOptions, registry: Registry) -> std::result::Result<Shell, String> {
        let start_dir =
            options.start_dir.canonicalize().map_err(|e| format!("can't open {}: {e}", options.start_dir.display()))?;
        let root = match options.root {
            Some(root) => root.canonicalize().map_err(|e| format!("can't open root {}: {e}", root.display()))?,
            None => find_project_root(&start_dir),
        };
        let config = Config::load(&root, &options.overrides)?;
        let extra_roots = config.extra_roots.iter().filter_map(|p| p.canonicalize().ok()).collect();
        let sandbox = sandbox_for(config.sandbox)?;
        let secrets = Secrets::new(&config);
        Ok(Shell { root, start_dir, extra_roots, config, registry, sandbox, secrets, session: options.session })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn run(&self, source: &str) -> Envelope {
        self.execute(source, Kind::Run, |ctx| {
            let program =
                agentsh_lang::parse(source).map_err(|e| external_program_hint(ctx.registry, source, e.into()))?;
            let value = ctx.run_top_level(&program, source).map_err(|e| match e.code {
                ErrorCode::UnknownCommand => external_program_hint(ctx.registry, source, e),
                _ => e,
            })?;
            match program.stmts.last() {
                Some(agentsh_lang::Stmt::Let { name, .. }) => {
                    let bound = ctx.state.vars.get(name).cloned().unwrap_or_default();
                    Ok(Value::Str(crate::interp::describe_binding(name, &bound)))
                }
                _ => Ok(value),
            }
        })
    }

    pub fn undo(&self, token: Option<&str>, force: bool) -> Envelope {
        self.execute("undo", Kind::Control, |ctx| {
            let run = match token {
                Some(t) => parse_undo_token(t)?,
                None => ctx.journal.last_run().ok_or_else(nothing_to_undo)?,
            };
            if !ctx.journal.entries().iter().any(|e| e.run == run) {
                return Err(nothing_to_undo().with_value(format!("u_{run}")));
            }
            let actions = ctx.journal.undo_where(|_, e| e.run == run, force)?;
            Ok(ctx.report_actions(format!("u_{run}"), actions))
        })
    }

    pub fn changes(&self) -> Envelope {
        self.execute("changes", Kind::Control, |ctx| Ok(ctx.describe_journal()))
    }

    pub fn commit(&self) -> Envelope {
        self.execute("commit", Kind::Control, |ctx| {
            let entries = ctx.journal.len();
            let freed = ctx.journal.blob_bytes();
            ctx.journal.clear().map_err(|e| ShellError::io(&e, "journal"))?;
            ctx.state.checkpoints.clear();
            Ok(Value::record([("committed", Value::Int(entries as i64)), ("freed_bytes", Value::Int(freed as i64))]))
        })
    }

    pub fn discard(&self, force: bool) -> Envelope {
        self.execute("discard", Kind::Control, |ctx| {
            if ctx.journal.is_empty() {
                return Err(nothing_to_undo());
            }
            let actions = ctx.journal.undo_where(|_, _| true, force)?;
            ctx.state.checkpoints.clear();
            Ok(ctx.report_actions("all".into(), actions))
        })
    }

    pub fn checkpoint(&self, name: &str) -> Envelope {
        self.execute("checkpoint", Kind::Control, |ctx| {
            let at = ctx.state.run_counter;
            ctx.state.checkpoints.insert(name.to_string(), at);
            Ok(Value::record([("checkpoint", Value::str(name)), ("after_run", Value::Int(at as i64))]))
        })
    }

    pub fn rollback(&self, name: &str, force: bool) -> Envelope {
        self.execute("rollback", Kind::Control, |ctx| {
            let Some(&at) = ctx.state.checkpoints.get(name) else {
                let known: Vec<&str> = ctx.state.checkpoints.keys().map(String::as_str).collect();
                return Err(ShellError::new(ErrorCode::NotFound, format!("no checkpoint named `{name}`"))
                    .with_value(name)
                    .with_hint(if known.is_empty() {
                        "no checkpoints yet; create one with: agentsh checkpoint <name>".into()
                    } else {
                        format!("checkpoints: {}", known.join(", "))
                    }));
            };
            let actions = ctx.journal.undo_where(|_, e| e.run > at, force)?;
            ctx.state.checkpoints.retain(|_, run| *run <= at);
            Ok(ctx.report_actions(format!("checkpoint:{name}"), actions))
        })
    }

    fn execute(&self, label: &str, kind: Kind, action: impl FnOnce(&mut Ctx) -> Result<Value>) -> Envelope {
        let start = Instant::now();
        match self.execute_inner(label, kind, action, start) {
            Ok(envelope) => envelope,
            Err(err) => self.finish(Envelope { error: Some(err.to_info(false)), ..Envelope::default() }, start, label),
        }
    }

    fn execute_inner(
        &self,
        label: &str,
        kind: Kind,
        action: impl FnOnce(&mut Ctx) -> Result<Value>,
        start: Instant,
    ) -> Result<Envelope> {
        let store = SessionStore::open(&self.root, &self.session)?;
        let mut state = store.load_state()?;
        let caller_cwd =
            self.start_dir.strip_prefix(&self.root).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
        if state.caller_cwd.as_deref() != Some(caller_cwd.as_str()) {
            state.cwd = caller_cwd.clone();
            state.caller_cwd = Some(caller_cwd);
        }
        if kind == Kind::Run {
            state.run_counter += 1;
        }
        let run_id = state.run_counter;
        let journal = Journal::load(&store.dir)?;
        let mut ctx = Ctx {
            root: self.root.clone(),
            extra_roots: self.extra_roots.clone(),
            config: &self.config,
            registry: &self.registry,
            sandbox: self.sandbox.as_ref(),
            secrets: &self.secrets,
            session_id: &self.session,
            handles_dir: store.dir.join("handles"),
            state,
            journal,
            run_id,
            effects: Vec::new(),
            printed: Vec::new(),
            warnings: Vec::new(),
            redactor: Redactor::new(self.secrets.sensitive_env()),
            exec_used: false,
            exec_completed: false,
            keep_on_failure: false,
            read_log: Vec::new(),
            outputs: Vec::new(),
            last_label: None,
            started_jobs: Vec::new(),
            unshortened: false,
            regex_cache: HashMap::new(),
        };
        let journal_mark = ctx.journal.len();
        let result = action(&mut ctx);
        let mut envelope = Envelope::default();
        match result {
            Ok(value) => {
                self.attach_value(&mut ctx, &mut envelope, value, label);
                if kind == Kind::Run && !ctx.effects.is_empty() {
                    envelope.undo = Some(format!("u_{run_id}"));
                }
                envelope.ok = true;
            }
            Err(err) => {
                let changed = kind == Kind::Run && ctx.journal.len() > journal_mark;
                let kept = changed && ctx.keep_on_failure;
                let rolled_back = changed && !kept;
                if kept {
                    envelope.undo = Some(format!("u_{run_id}"));
                }
                let mut restored = Vec::new();
                if rolled_back {
                    match ctx.rollback_since(journal_mark) {
                        Ok(paths) => restored = paths,
                        Err(rb) => ctx.warn(format!("rollback failed: {}", rb.message)),
                    }
                    ctx.effects.clear();
                }
                if kind == Kind::Run && !kept {
                    for (name, pid) in std::mem::take(&mut ctx.started_jobs) {
                        agentsh_platform::stop_group(pid, std::time::Duration::from_secs(1));
                        ctx.warn(format!("stopped job `{name}` because this run failed"));
                    }
                }
                if rolled_back && ctx.exec_completed {
                    ctx.warn("files written by exec'd programs themselves were not rolled back");
                }
                let repeats = ctx.state.repeated_failures(label, err.code);
                if repeats >= 2 {
                    ctx.warn(format!(
                        "this exact program has now failed {} times with {}; change the approach instead of retrying",
                        repeats + 1,
                        err.code.as_str()
                    ));
                }
                let mut info = err.to_info(rolled_back);
                info.kept = kept;
                info.restored = restored;
                if rolled_back && err.code == ErrorCode::ExecFailed {
                    let tip = "keep:true keeps this run's changes for a small fix";
                    info.hint = Some(info.hint.map(|h| format!("{h}; {tip}")).unwrap_or_else(|| tip.into()));
                }
                envelope.error = Some(info);
            }
        }
        if kind == Kind::Run {
            ctx.state.record_history(label, envelope.error.as_ref().map(|e| e.code));
        }
        if self.session != "default"
            && let Some(error) = envelope.error.as_mut()
        {
            ctx.state.forget_seen_hints(error);
        }
        self.attach_outputs(&mut ctx, &mut envelope);
        envelope.printed = std::mem::take(&mut ctx.printed);
        envelope.effects = std::mem::take(&mut ctx.effects);
        envelope.warnings = std::mem::take(&mut ctx.warnings);
        self.redact(&ctx.redactor, &mut envelope);
        for value in ctx.state.vars.values_mut() {
            ctx.redactor.redact_value(value);
        }
        ctx.journal.save()?;
        store.save_state(&ctx.state)?;
        Ok(self.finish(envelope, start, label))
    }

    fn attach_value(&self, ctx: &mut Ctx, envelope: &mut Envelope, mut value: Value, label: &str) {
        if value != Value::Null {
            ctx.store_handle("_", &value);
        }
        ctx.dedupe_reads(&mut value);
        ctx.dedupe_repeat(label, &mut value);
        let limits = if ctx.unshortened {
            agentsh_platform::Limits { max_string_bytes: UNSHORTENED_BYTES, ..self.config.limits.clone() }
        } else {
            self.config.limits.clone()
        };
        let shaped = shape(value.clone(), &limits);
        if shaped.truncated {
            envelope.handle = ctx.new_handle(&value, self.config.limits.max_handles);
        }
        envelope.value = Some(shaped.value).filter(|v| *v != Value::Null);
        envelope.shown = shaped.shown;
        envelope.total = shaped.total;
        envelope.truncated = shaped.truncated;
    }

    fn attach_outputs(&self, ctx: &mut Ctx, envelope: &mut Envelope) {
        let outputs = std::mem::take(&mut ctx.outputs);
        if outputs.is_empty() {
            return;
        }
        let share = outputs.len() + usize::from(envelope.value.is_some());
        let limits = agentsh_platform::Limits {
            max_string_bytes: (self.config.limits.max_string_bytes / share.max(1)).max(1_000),
            ..self.config.limits.clone()
        };
        for (statement, mut value) in outputs {
            ctx.dedupe_reads(&mut value);
            let shaped = shape(value, &limits);
            envelope.outputs.push(Output { statement, value: shaped.value, truncated: shaped.truncated });
        }
        envelope.label = ctx.last_label.take();
    }

    fn redact(&self, redactor: &Redactor, envelope: &mut Envelope) {
        if let Some(v) = envelope.value.as_mut() {
            redactor.redact_value(v);
        }
        for output in envelope.outputs.iter_mut() {
            redactor.redact_value(&mut output.value);
        }
        envelope.printed.iter_mut().for_each(|v| redactor.redact_value(v));
        envelope.warnings = envelope.warnings.iter().map(|w| redactor.redact_str(w)).collect();
        if let Some(err) = envelope.error.as_mut() {
            err.message = redactor.redact_str(&err.message);
            err.hint = err.hint.as_ref().map(|h| redactor.redact_str(h));
            err.value = err.value.as_ref().map(|v| redactor.redact_str(v));
        }
    }

    fn finish(&self, mut envelope: Envelope, start: Instant, label: &str) -> Envelope {
        envelope.session = self.session.clone();
        envelope.ms = start.elapsed().as_millis() as u64;
        envelope.tokens_est = estimate_tokens(serde_json::to_string(&envelope).map(|s| s.len()).unwrap_or(0));
        if let Some(path) = &self.config.trace_path {
            trace::append(path, &self.session, &self.root, label, &envelope);
        }
        envelope
    }
}

impl Ctx<'_> {
    pub fn store_handle(&mut self, name: &str, value: &Value) -> bool {
        let Ok(bytes) = serde_json::to_vec(value) else { return false };
        if bytes.len() > MAX_HANDLE_BYTES || std::fs::create_dir_all(&self.handles_dir).is_err() {
            return false;
        }
        std::fs::write(self.handles_dir.join(format!("{name}.json")), bytes).is_ok()
    }

    pub fn load_handle(&self, name: &str) -> Option<Value> {
        let bytes = std::fs::read(self.handles_dir.join(format!("{name}.json"))).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    fn new_handle(&mut self, value: &Value, max_handles: usize) -> Option<String> {
        self.state.next_handle += 1;
        let name = format!("r{}", self.state.next_handle);
        if !self.store_handle(&name, value) {
            self.warn("the full result was too large to keep as a handle; narrow the query");
            return None;
        }
        self.state.handle_order.push_back(name.clone());
        while self.state.handle_order.len() > max_handles {
            if let Some(old) = self.state.handle_order.pop_front() {
                let _ = std::fs::remove_file(self.handles_dir.join(format!("{old}.json")));
            }
        }
        Some(format!("${name}"))
    }

    fn report_actions(&mut self, undone: String, actions: Vec<UndoAction>) -> Value {
        for action in &actions {
            if self.state.seen.contains_key(&action.path) {
                self.mark_seen(&action.path.clone());
            }
            self.effects.push(Effect {
                op: action.op,
                path: self.display(&action.path),
                to: action.to.as_ref().map(|t| self.display(t)),
                bytes: None,
            });
        }
        Value::record([("undone", Value::Str(undone)), ("actions", Value::Int(actions.len() as i64))])
    }

    fn describe_journal(&self) -> Value {
        let mut runs: Vec<(u64, Vec<Value>)> = Vec::new();
        for entry in self.journal.entries() {
            let (op, path, to) = match &entry.op {
                JournalOp::Create { path, .. } => (EffectOp::Create, path, None),
                JournalOp::Modify { path, .. } => (EffectOp::Modify, path, None),
                JournalOp::Delete { path, .. } => (EffectOp::Delete, path, None),
                JournalOp::Move { from, to } => (EffectOp::Move, from, Some(to)),
                JournalOp::Mkdir { path } => (EffectOp::Mkdir, path, None),
                JournalOp::Chmod { path, .. } => (EffectOp::Modify, path, None),
            };
            let mut record = vec![
                ("op", serde_json::to_value(op).map(Value::from).unwrap_or_default()),
                ("path", Value::Str(self.display(path))),
            ];
            if let Some(to) = to {
                record.push(("to", Value::Str(self.display(to))));
            }
            match runs.last_mut() {
                Some((run, items)) if *run == entry.run => items.push(Value::record(record)),
                _ => runs.push((entry.run, vec![Value::record(record)])),
            }
        }
        let runs: Vec<Value> = runs
            .into_iter()
            .map(|(run, effects)| {
                Value::record([("undo", Value::Str(format!("u_{run}"))), ("effects", Value::List(effects))])
            })
            .collect();
        let checkpoints =
            self.state.checkpoints.iter().map(|(name, run)| (name.clone(), Value::Int(*run as i64))).collect();
        Value::record([
            ("runs", Value::List(runs)),
            ("checkpoints", Value::Record(checkpoints)),
            ("backup_bytes", Value::Int(self.journal.blob_bytes() as i64)),
        ])
    }
}

fn external_program_hint(registry: &Registry, source: &str, err: ShellError) -> ShellError {
    let Some(line) = err.span.and_then(|s| source.lines().nth(s.line.saturating_sub(1))) else { return err };
    let words: Vec<&str> = line.split_whitespace().collect();
    let Some(first) = words.first().copied() else { return err };
    let looks_external = registry.get(first).is_none()
        && crate::suggest::bash_equivalent(first).is_none()
        && first.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && crate::interp::on_path(first);
    if !looks_external {
        return err;
    }
    let line = line.trim();
    let hint = if line.contains('\'') {
        format!("`{first}` is an external program; run it with exec: exec \"{first}\" \"arg\" ...")
    } else if line.contains(['|', '$', '>', '<', '&', ';', '*', '`']) {
        format!(
            "`{first}` is an external program and this line uses shell features; run it with: exec \"sh\" \"-c\" '{line}'"
        )
    } else {
        format!("`{first}` is an external program; run it with: exec '{line}'")
    };
    err.with_hint(hint)
}

fn parse_undo_token(token: &str) -> Result<u64> {
    token.strip_prefix("u_").and_then(|n| n.parse().ok()).ok_or_else(|| {
        ShellError::new(ErrorCode::InvalidValue, format!("`{token}` is not an undo token"))
            .with_hint("undo tokens look like u_12")
    })
}

fn nothing_to_undo() -> ShellError {
    ShellError::new(ErrorCode::NothingToUndo, "nothing to undo").with_hint("changes lists what can be undone")
}
