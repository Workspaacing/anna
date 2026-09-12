//! What the agent can do, and the registry that describes it to a model.
//!
//! A tool is deliberately not a shell utility reimplemented in Rust. Each one is routed through
//! machinery the editor already has — `Project` for paths and buffers, the worktree snapshot for
//! listings, the language servers for diagnostics — so the agent sees the same state the user does,
//! including unsaved edits, and so remote projects work without a second code path.

use crate::{
    cowork_settings::CoworkSettings,
    permission::{Decision, PermissionBroker, PermissionRequest, command_scope},
    verify,
};
use anyhow::{Context as _, Result, anyhow, bail};
use futures::FutureExt as _;
use gpui::{App, AppContext as _, AsyncApp, Entity, Task, WeakEntity};
use language::Buffer;
use collections::HashSet;
use project::{
    Project, ProjectPath,
    lsp_store::{FormatTrigger, LspFormatTarget},
};
use serde_json::{Value, json};
use settings::Settings as _;
use task::Shell;
use terminal::terminal_settings::TerminalSettings;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};
use task::ShellBuilder;
use util::{paths::PathStyle, rel_path::RelPath};
use workspace::Workspace;

/// How a tool call reads to the user, and how the permission broker will classify it.
///
/// Named after the Agent Client Protocol's tool-call kinds so the panel's event model stays a
/// superset of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Read,
    Edit,
    Execute,
    Search,
    Other,
}

pub struct ToolContext {
    pub project: Entity<Project>,
    pub workspace: WeakEntity<Workspace>,
    /// How a tool asks before doing something the editor cannot undo.
    pub permissions: Entity<PermissionBroker>,
    /// Where the thread runs commands, when the model names no directory of its own.
    ///
    /// `None` falls back to the project's first folder, which is what a thread created before the
    /// choice existed has.
    pub working_folder: Option<PathBuf>,
}

/// The result of one tool call.
///
/// `content` is what the model sees. `summary` is what the transcript shows before the card is
/// expanded, because a model-facing payload is usually too long to read at a glance.
#[derive(Clone, Debug)]
pub struct ToolOutput {
    pub content: String,
    pub summary: String,
    /// A unified diff, when this tool changed a file.
    pub diff: String,
    /// The file it changed, for highlighting that diff.
    pub path: String,
}

impl ToolOutput {
    pub fn new(content: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            summary: summary.into(),
            diff: String::new(),
            path: String::new(),
        }
    }

    pub fn with_diff(mut self, diff: String, path: String) -> Self {
        self.diff = diff;
        self.path = path;
        self
    }
}

/// The parameter every tool takes, asking the model to say what the call is for.
///
/// A transcript built from tool names reads as mechanics — `read`, `read`, `edit` — and leaves the
/// reader to reconstruct the intent. The model already knows why it is making each call, so it is
/// asked, and its own sentence is what the transcript shows.
fn intent_parameter() -> (&'static str, Value) {
    (
        "intent",
        json!({
            "type": "string",
            "description": "A short sentence, in the past tense, saying what this call is for — \
                            \"Checked how the picker is wired\", \"Added the missing import\". \
                            Shown to the user in place of the tool's name.",
        }),
    )
}

/// Adds the shared `intent` parameter to a tool's own schema.
fn with_intent(mut parameters: Value) -> Value {
    let (name, schema) = intent_parameter();
    if let Some(properties) = parameters
        .get_mut("properties")
        .and_then(Value::as_object_mut)
    {
        properties.insert(name.to_owned(), schema);
    }
    parameters
}

pub trait Tool: Send + Sync + 'static {
    fn name(&self) -> &'static str;

    fn kind(&self) -> ToolKind;

    /// Shown to the model. This is the whole interface: a tool the model misuses is usually a tool
    /// whose description did not say enough.
    fn description(&self) -> &'static str;

    /// A JSON Schema object describing the arguments.
    fn parameters(&self) -> Value;

    fn run(&self, input: Value, context: ToolContext, cx: &mut App) -> Task<Result<ToolOutput>>;
}

#[derive(Clone)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::default_tools()
    }
}

impl ToolRegistry {
    /// The tools available to a thread.
    ///
    /// `write` and `edit` change the user's files without asking, because every change lands in a
    /// buffer the editor owns: it shows up in the open editor, in the git gutter, and in the undo
    /// history, so nothing happens that the user cannot see and reverse.
    ///
    /// `shell` is different — nothing here can undo `rm -rf`, a push, or a publish — so it asks
    /// first, through the permission broker.
    pub fn default_tools() -> Self {
        Self {
            tools: vec![
                Arc::new(ReadTool),
                Arc::new(ListTool),
                Arc::new(WriteTool),
                Arc::new(EditTool),
                Arc::new(ShellTool),
            ],
        }
    }

    /// Only the tools that cannot change anything.
    pub fn read_only() -> Self {
        Self {
            tools: vec![Arc::new(ReadTool), Arc::new(ListTool)],
        }
    }

    pub fn definitions(&self) -> Vec<crate::provider::ToolDefinition> {
        self.tools
            .iter()
            .map(|tool| crate::provider::ToolDefinition {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                parameters: with_intent(tool.parameters()),
            })
            .collect()
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .cloned()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

/// Resolves a path the model supplied against the project.
///
/// `find_project_path` accepts absolute paths, worktree-root-prefixed paths and bare relative ones,
/// which matters because models mix all three freely. `None` means the path is outside every
/// worktree — the boundary a future `external_directory` permission will guard.
fn resolve(project: &Project, path: &str, cx: &App) -> Result<project::ProjectPath> {
    project
        .find_project_path(path, cx)
        .with_context(|| format!("`{path}` is not inside any folder open in this project"))
}

fn string_argument(input: &Value, name: &str) -> Result<String> {
    input
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("missing required string argument `{name}`"))
}

/// What a tool wants to do to a file's text.
enum Change {
    /// Replace everything.
    Whole(String),
    /// Replace one exact occurrence, or every occurrence.
    Splice {
        old: String,
        new: String,
        all: bool,
    },
}

impl Change {
    /// The text the agent is introducing, which is what the secret scan judges.
    ///
    /// Deliberately not the resulting file: scanning that would flag a credential the user put
    /// there themselves and lock the agent out of the file entirely. The agent is answerable for
    /// what it writes, not for what was already there.
    fn added_text(&self) -> &str {
        match self {
            Change::Whole(text) => text,
            Change::Splice { new, .. } => new,
        }
    }
}

/// Resolves a path that may not exist yet.
///
/// `find_project_path` only invents a path for something that is absent when the path carries a
/// worktree root name, because with several folders open a bare `src/main.rs` is genuinely
/// ambiguous. With exactly one folder open there is nothing to be ambiguous about, so resolve it.
fn resolve_for_create(project: &Project, path: &str, cx: &App) -> Result<ProjectPath> {
    if let Some(project_path) = project.find_project_path(path, cx) {
        return Ok(project_path);
    }

    let path_style = project.path_style(cx);
    let mut worktrees = project.visible_worktrees(cx);
    let (Some(worktree), None) = (worktrees.next(), worktrees.next()) else {
        bail!(
            "`{path}` did not resolve to a file in any open folder. With more than one folder open, \
             prefix the path with the folder's name."
        );
    };

    if matches!(path_style, PathStyle::Windows) && path.contains(':') || path.starts_with('/') {
        bail!("`{path}` is outside every folder open in this project");
    }

    let relative = RelPath::new(path.as_ref(), path_style)
        .with_context(|| format!("`{path}` is not a usable relative path"))?;
    Ok(ProjectPath {
        worktree_id: worktree.read(cx).id(),
        path: relative.into_arc(),
    })
}

/// Applies a change to a file, saves it, and reports what the project's own checks make of it.
///
/// Everything goes through a `Buffer` rather than the filesystem, so the change appears in an open
/// editor immediately, joins the undo history, and works the same on a remote project.
async fn apply(
    project: Entity<Project>,
    path: String,
    change: Change,
    cx: &mut AsyncApp,
) -> Result<ToolOutput> {
    let (settings, http) = cx.update(|cx| {
        (
            CoworkSettings::get_global(cx).verification,
            cx.http_client(),
        )
    });

    // Refuse before anything is written. A credential that reaches the buffer is already in the
    // undo history and moments away from a commit.
    let blocked = verify::gate(&settings, change.added_text());
    if !blocked.is_empty() {
        let report = verify::VerificationReport { findings: blocked };
        bail!("refused to write to {path}.\n{}", report.to_model(&path));
    }

    let project_path = project.update(cx, |project, cx| resolve_for_create(project, &path, cx))?;
    let existed = project.read_with(cx, |project, cx| {
        project
            .worktree_for_id(project_path.worktree_id, cx)
            .and_then(|worktree| worktree.read(cx).entry_for_path(&project_path.path).cloned())
            .is_some()
    });

    if !existed {
        // `Fs::write` creates the parent directories, so a nested path needs no preparation.
        project
            .update(cx, |project, cx| {
                project.create_entry(project_path.clone(), false, cx)
            })
            .await
            .with_context(|| format!("creating {path}"))?;
    }

    let buffer = project
        .update(cx, |project, cx| {
            project.open_buffer(project_path.clone(), cx)
        })
        .await
        .with_context(|| format!("opening {path}"))?;

    // Opening a buffer does not start the language servers for it — `open_buffer_with_lsp` does
    // that, and it is gated behind `test-support`. Without this the agent edited files that no
    // server had ever seen: Biome and ESLint never started, so there was nothing for the formatter
    // chain to call and no diagnostics for the checks to report. The handle is held until the end
    // of the operation, because dropping it unregisters the buffer again.
    let _language_servers = project.update(cx, |project, cx| {
        project.register_buffer_with_language_servers(&buffer, cx)
    });

    let before = buffer_text(&buffer, cx).await;
    let summary = edit_buffer(&buffer, &change, cx).await?;

    // Before saving, so the file lands formatted and auto-fixed rather than being rewritten a
    // moment later.
    if settings.format {
        format_with_project(&project, &buffer, cx).await;
    }

    let mut findings = Vec::new();

    project
        .update(cx, |project, cx| project.save_buffer(buffer.clone(), cx))
        .await
        .with_context(|| format!("saving {path}"))?;

    let file_name = project_path
        .path
        .file_name()
        .unwrap_or_default()
        .to_owned();
    let saved = buffer_text(&buffer, cx).await;

    // Diffed before the buffer is handed to the checks, which consume it. Three lines of context
    // either side: enough to see where a change landed without pasting the file back into the
    // transcript, which is what made an earlier version unreadable.
    let diff = language::unified_diff_with_context(&before, &saved, 1, 1, 3);

    findings.extend(
        verify::inspect(settings, http, buffer, file_name, saved, cx)
            .await
            .findings,
    );

    let mut content = format!("{summary} in {path}.");
    let report = verify::VerificationReport { findings };
    if !report.is_empty() {
        content.push('\n');
        content.push_str(&report.to_model(&path));
    }

    Ok(ToolOutput::new(content, format!("{path} · {summary}")).with_diff(diff, path.clone()))
}

/// Hands the change to the project's own formatter chain.
///
/// Whatever is configured for this language — Biome's `source.fixAll`, an ESLint fix-all action,
/// Prettier, the language server, or several of them in order — is what the agent's edits get, so
/// the agent's work is treated exactly like the user's own edits on save.
///
/// This deliberately replaced a direct `biome --stdin-file-path` call, for two reasons. A second
/// formatter can only ever disagree with the one the editor runs, and each would undo the other
/// every turn. And that CLI mode turns out to be unusable for checking anything: Biome silently
/// disables `--reporter` when reading from stdin, so it returns the fixed source and nothing else —
/// including for a file that does not parse, which it will happily rewrite and still call clean.
///
/// Nothing here can fail the write. A formatter that breaks a turn is worse than an unformatted
/// file, and whatever it could not fix is reported by the diagnostics check a moment later.
async fn format_with_project(
    project: &Entity<Project>,
    buffer: &Entity<Buffer>,
    cx: &mut AsyncApp,
) {
    let formatted = project.update(cx, |project, cx| {
        project.format(
            HashSet::from_iter([buffer.clone()]),
            LspFormatTarget::Buffers,
            // Into the undo history, so one ctrl-z takes back the formatting as well as the edit.
            true,
            FormatTrigger::Manual,
            cx,
        )
    });

    if let Err(error) = formatted.await {
        log::warn!("cowork: could not format the agent's change: {error:#}");
    }
}

/// Turning a rope into a `String` is real work; it does not belong on the foreground thread.
async fn buffer_text(buffer: &Entity<Buffer>, cx: &mut AsyncApp) -> String {
    let snapshot = buffer.read_with(cx, |buffer, _| buffer.snapshot());
    cx.background_spawn(async move { snapshot.text() }).await
}

/// Applies the change, returning a description of what it did.
async fn edit_buffer(
    buffer: &Entity<Buffer>,
    change: &Change,
    cx: &mut AsyncApp,
) -> Result<String> {
    match change {
        Change::Whole(text) => {
            let lines = text.lines().count();
            let text = text.clone();
            buffer.update(cx, |buffer, cx| buffer.set_text(text, cx));
            Ok(format!("wrote {lines} lines"))
        }
        Change::Splice { old, new, all } => {
            let text = buffer_text(buffer, cx).await;
            let ranges = occurrences(&text, old);

            // An edit tool is only trustworthy if it refuses to guess. Both of these mean the model
            // must look again, and saying which is which is what lets it recover in one step.
            match ranges.len() {
                0 => bail!(
                    "the text to replace does not appear in the file. Read it again; it may have \
                     changed, or the whitespace may differ."
                ),
                count if count > 1 && !all => bail!(
                    "the text to replace appears {count} times. Include enough surrounding lines \
                     to make it unique, or pass `replace_all`."
                ),
                _ => {}
            }

            let replaced = ranges.len();
            let edits = ranges
                .into_iter()
                .map(|range| (range, new.clone()))
                .collect::<Vec<_>>();
            buffer.update(cx, |buffer, cx| buffer.edit(edits, None, cx));

            Ok(match replaced {
                1 => "replaced 1 occurrence".to_owned(),
                count => format!("replaced {count} occurrences"),
            })
        }
    }
}

/// Every byte range in `text` holding `needle`, without overlaps.
fn occurrences(text: &str, needle: &str) -> Vec<std::ops::Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    text.match_indices(needle)
        .map(|(start, matched)| start..start + matched.len())
        .collect()
}

/// How long a command may run before it is killed, when the model names no limit.
const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

/// The longest a command may be given, so a model cannot ask for an hour.
const MAX_COMMAND_TIMEOUT: Duration = Duration::from_secs(600);

/// How much of a command's output the model is shown.
///
/// A build log can run to megabytes, and sending it would cost more than the command saved. The
/// tail is kept rather than the head: the error is almost always at the end.
const MAX_OUTPUT_BYTES: usize = 16 * 1024;

struct ShellTool;

impl Tool for ShellTool {
    fn name(&self) -> &'static str {
        "shell"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Execute
    }

    fn description(&self) -> &'static str {
        "Run a command in the project's shell and return its output. The user is asked to approve \
         it first, and may decline. Runs in the project's root directory unless `cwd` says \
         otherwise. Output is truncated to the last few thousand characters, so prefer commands \
         that report concisely — `cargo test --quiet` over `cargo test`. Not for editing files: \
         use `edit` and `write`, whose changes the user can see and undo."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The command line to run, as you would type it in a terminal.",
                },
                "cwd": {
                    "type": "string",
                    "description": "Directory to run in, relative to a project folder. Defaults to \
                                    the project root.",
                },
                "timeout_seconds": {
                    "type": "number",
                    "description": "How long to wait before giving up. Defaults to 60.",
                },
            },
            "required": ["command"],
        })
    }

    fn run(&self, input: Value, context: ToolContext, cx: &mut App) -> Task<Result<ToolOutput>> {
        let command = match string_argument(&input, "command") {
            Ok(command) => command,
            Err(error) => return Task::ready(Err(error)),
        };
        if command.trim().is_empty() {
            return Task::ready(Err(anyhow!("the command was empty")));
        }

        let requested = input
            .get("timeout_seconds")
            .and_then(Value::as_f64)
            .filter(|seconds| *seconds > 0.0)
            .map(Duration::from_secs_f64)
            .unwrap_or(DEFAULT_COMMAND_TIMEOUT);
        let timeout = requested.min(MAX_COMMAND_TIMEOUT);
        let relative_cwd = input
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::to_owned);

        cx.spawn(async move |cx| {
            let decision = context
                .permissions
                .update(cx, |permissions, cx| {
                    permissions.request(
                        PermissionRequest {
                            tool: "shell",
                            title: "Run a command".into(),
                            detail: command.clone().into(),
                            scope: command_scope(&command),
                        },
                        cx,
                    )
                })
                .await;

            // A dropped sender means the thread went away mid-question, which is a refusal.
            if !decision.map(Decision::is_allowed).unwrap_or(false) {
                bail!("the user declined to run this command");
            }

            let (shell, directory) = cx.update(|cx| {
                let shell = agent_shell(cx);
                let directory = working_directory(
                    &context.project,
                    context.working_folder.as_deref(),
                    relative_cwd.as_deref(),
                    cx,
                );
                (shell, directory)
            });
            let directory = directory?;

            let executor = cx.background_executor().clone();
            run_command(command.clone(), shell, directory, timeout, &executor).await
        })
    }
}

/// The shell the agent runs commands in.
///
/// `cowork.shell` when it is set, and otherwise whatever the terminal is configured to use — so the
/// agent runs commands in the same shell the user gets when they open a terminal, which is the only
/// behaviour that will not surprise them.
fn agent_shell(cx: &App) -> Shell {
    CoworkSettings::get_global(cx)
        .shell
        .clone()
        .unwrap_or_else(|| TerminalSettings::get_global(cx).shell.clone())
}

/// Where to run, which must be inside the project.
fn working_directory(
    project: &Entity<Project>,
    thread_folder: Option<&Path>,
    relative: Option<&str>,
    cx: &App,
) -> Result<PathBuf> {
    let project = project.read(cx);

    let Some(relative) = relative else {
        // The thread's own folder, when it still names one of the project's — a folder closed
        // since the thread was created must not send the agent somewhere outside the project.
        if let Some(folder) = thread_folder
            && project
                .visible_worktrees(cx)
                .any(|worktree| worktree.read(cx).abs_path().as_ref() == folder)
        {
            return Ok(folder.to_path_buf());
        }

        let worktree = project
            .visible_worktrees(cx)
            .next()
            .context("this project has no folder to run a command in")?;
        return Ok(worktree.read(cx).abs_path().to_path_buf());
    };

    // Resolving through the project is what keeps `cwd` inside it: a path that names no worktree
    // is refused rather than run from wherever it happens to point.
    let project_path = project
        .find_project_path(relative, cx)
        .with_context(|| format!("`{relative}` is not inside any folder open in this project"))?;
    let worktree = project
        .worktree_for_id(project_path.worktree_id, cx)
        .context("that folder is no longer open")?;

    Ok(worktree
        .read(cx)
        .abs_path()
        .join(project_path.path.as_std_path()))
}

/// Runs one command and collects what it said.
///
/// The child is spawned through `util::process::Child` rather than plainly, so that on Windows it
/// joins a job object and on Unix a process group: killing it on timeout then takes the whole tree
/// with it, instead of leaving a build running forever with nobody watching.
async fn run_command(
    command: String,
    shell: Shell,
    directory: PathBuf,
    timeout: Duration,
    executor: &gpui::BackgroundExecutor,
) -> Result<ToolOutput> {
    let label = summarize_command(&command);

    // With no arguments the command line is passed through verbatim, and the builder handles the
    // difference between `sh -c` and `cmd /C` quoting.
    let mut process = ShellBuilder::new(&shell, cfg!(windows))
        .non_interactive()
        .redirect_stdin_to_dev_null()
        .build_std_command(Some(command), &[]);
    process.current_dir(&directory);

    let mut child = util::process::Child::spawn(process, Stdio::null(), Stdio::piped(), Stdio::piped())
        .context("starting the command")?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let mut reader = Box::pin(
        futures::future::join(read_pipe(stdout), read_pipe(stderr)).fuse(),
    );
    let mut timer = Box::pin(executor.timer(timeout).fuse());

    let mut timed_out = false;
    let collected = futures::select_biased! {
        collected = reader => Some(collected),
        _ = timer => {
            timed_out = true;
            None
        }
    };

    // Killing closes the pipes, so the readers finish with whatever the command managed to say
    // before it was stopped — which is usually where it got stuck.
    let (out, err) = match collected {
        Some(collected) => collected,
        None => {
            let _ = child.kill();
            reader.await
        }
    };

    let status = if timed_out {
        None
    } else {
        child.status().await.ok()
    };

    Ok(ToolOutput::new(
        describe_run(&out, &err, status, timed_out, timeout),
        match status.map(|status| status.success()) {
            Some(true) => label,
            Some(false) => format!("{label} · failed"),
            None => format!("{label} · timed out"),
        },
    ))
}

async fn read_pipe<R: futures::AsyncRead + Unpin>(pipe: Option<R>) -> String {
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut text = String::new();
    // A command that writes bytes which are not UTF-8 is not a failure of the command.
    let mut bytes = Vec::new();
    if futures::AsyncReadExt::read_to_end(&mut pipe, &mut bytes)
        .await
        .is_ok()
    {
        text = String::from_utf8_lossy(&bytes).into_owned();
    }
    text
}

/// What the model is told, which has to be enough to act on and no more.
fn describe_run(
    out: &str,
    err: &str,
    status: Option<std::process::ExitStatus>,
    timed_out: bool,
    timeout: Duration,
) -> String {
    let mut described = String::new();

    match (timed_out, status.map(|status| status.code())) {
        (true, _) => described.push_str(&format!(
            "The command was still running after {} seconds and was stopped. Its output so far:\n",
            timeout.as_secs()
        )),
        (false, Some(Some(0))) | (false, None) => {}
        (false, Some(Some(code))) => described.push_str(&format!("Exited with status {code}.\n")),
        (false, Some(None)) => described.push_str("The command was killed by a signal.\n"),
    }

    if !out.trim().is_empty() {
        described.push_str(&tail(out));
    }
    if !err.trim().is_empty() {
        if !described.is_empty() && !described.ends_with('\n') {
            described.push('\n');
        }
        described.push_str("stderr:\n");
        described.push_str(&tail(err));
    }

    if described.trim().is_empty() {
        described.push_str("The command produced no output.");
    }
    described
}

/// The last of a long output, on a character boundary.
fn tail(text: &str) -> String {
    if text.len() <= MAX_OUTPUT_BYTES {
        return text.to_owned();
    }

    let mut start = text.len() - MAX_OUTPUT_BYTES;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    format!(
        "[… {} earlier characters omitted …]\n{}",
        start,
        &text[start..]
    )
}

/// A one-line label for the transcript.
fn summarize_command(command: &str) -> String {
    let single_line = command.split('\n').next().unwrap_or(command).trim();
    if single_line.chars().count() <= 60 {
        return single_line.to_owned();
    }
    let truncated: String = single_line.chars().take(57).collect();
    format!("{truncated}…")
}

struct WriteTool;

impl Tool for WriteTool {
    fn name(&self) -> &'static str {
        "write"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    fn description(&self) -> &'static str {
        "Write a file, creating it if it does not exist and replacing its entire contents if it \
         does. Parent directories are created as needed. Prefer `edit` for changing part of an \
         existing file; this replaces everything. The change appears in the editor and can be \
         undone by the user."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, absolute or relative to a project folder.",
                },
                "contents": {
                    "type": "string",
                    "description": "The complete new contents of the file.",
                },
            },
            "required": ["path", "contents"],
        })
    }

    fn run(&self, input: Value, context: ToolContext, cx: &mut App) -> Task<Result<ToolOutput>> {
        let arguments = (
            string_argument(&input, "path"),
            string_argument(&input, "contents"),
        );
        let (path, contents) = match arguments {
            (Ok(path), Ok(contents)) => (path, contents),
            (Err(error), _) | (_, Err(error)) => return Task::ready(Err(error)),
        };

        let project = context.project;
        cx.spawn(async move |cx| apply(project, path, Change::Whole(contents), cx).await)
    }
}

struct EditTool;

impl Tool for EditTool {
    fn name(&self) -> &'static str {
        "edit"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Edit
    }

    fn description(&self) -> &'static str {
        "Replace an exact run of text in a file. `old_text` must match the file byte for byte, \
         including indentation, and must appear exactly once unless `replace_all` is set — include \
         enough surrounding lines to make it unique. Read the file first; editing against \
         remembered contents is how these edits go wrong."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, absolute or relative to a project folder.",
                },
                "old_text": {
                    "type": "string",
                    "description": "The exact text to replace, copied from the file.",
                },
                "new_text": {
                    "type": "string",
                    "description": "The text to put in its place. Empty deletes the old text.",
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace every occurrence rather than requiring exactly one.",
                },
            },
            "required": ["path", "old_text", "new_text"],
        })
    }

    fn run(&self, input: Value, context: ToolContext, cx: &mut App) -> Task<Result<ToolOutput>> {
        let arguments = (
            string_argument(&input, "path"),
            string_argument(&input, "old_text"),
            string_argument(&input, "new_text"),
        );
        let (path, old, new) = match arguments {
            (Ok(path), Ok(old), Ok(new)) => (path, old, new),
            (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => {
                return Task::ready(Err(error));
            }
        };
        let all = input
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let project = context.project;
        cx.spawn(async move |cx| {
            apply(project, path, Change::Splice { old, new, all }, cx).await
        })
    }
}

struct ReadTool;

impl Tool for ReadTool {
    fn name(&self) -> &'static str {
        "read"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn description(&self) -> &'static str {
        "Read a file from the project. Returns the file's current contents, including edits the \
         user has made but not yet saved. The path may be absolute or relative to a project folder."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, absolute or relative to a project folder.",
                },
            },
            "required": ["path"],
        })
    }

    fn run(&self, input: Value, context: ToolContext, cx: &mut App) -> Task<Result<ToolOutput>> {
        let path = match string_argument(&input, "path") {
            Ok(path) => path,
            Err(error) => return Task::ready(Err(error)),
        };

        let project = context.project;
        cx.spawn(async move |cx| {
            let open = project.update(cx, |project, cx| {
                let project_path = resolve(project, &path, cx)?;
                anyhow::Ok(project.open_buffer(project_path, cx))
            })?;
            let buffer = open.await?;

            // Snapshot on the foreground, stringify on the background: turning a large rope into a
            // String is real work and must not block the UI.
            let snapshot = buffer.read_with(cx, |buffer, _| buffer.snapshot());
            let text = cx.background_spawn(async move { snapshot.text() }).await;

            let lines = text.lines().count();
            Ok(ToolOutput::new(text, format!("{path} · {lines} lines")))
        })
    }
}

struct ListTool;

impl Tool for ListTool {
    fn name(&self) -> &'static str {
        "list"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn description(&self) -> &'static str {
        "List the immediate contents of a directory in the project. Files ignored by version \
         control are omitted. Use an empty path to list a project folder's root."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory path, absolute or relative to a project folder.",
                },
            },
            "required": ["path"],
        })
    }

    fn run(&self, input: Value, context: ToolContext, cx: &mut App) -> Task<Result<ToolOutput>> {
        let path = match string_argument(&input, "path") {
            Ok(path) => path,
            Err(error) => return Task::ready(Err(error)),
        };

        // Listing reads an already-loaded worktree snapshot, so there is nothing to await.
        let project = context.project.read(cx);
        Task::ready(list_directory(project, &path, cx))
    }
}

fn list_directory(project: &Project, path: &str, cx: &App) -> Result<ToolOutput> {
    let project_path = resolve(project, path, cx)?;
    let worktree = project
        .worktree_for_id(project_path.worktree_id, cx)
        .context("that folder is no longer open")?;
    let worktree = worktree.read(cx);
    let path_style = worktree.path_style();

    let options = worktree::ChildEntriesOptions {
        include_files: true,
        include_dirs: true,
        include_ignored: false,
    };

    let mut entries = worktree
        .child_entries_with_options(&project_path.path, options)
        .map(|entry| {
            let name = entry.path.file_name().unwrap_or_default().to_owned();
            if entry.is_dir() {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect::<Vec<_>>();
    entries.sort();

    let shown = project_path.path.display(path_style).to_string();
    let summary = format!("{shown} · {} entries", entries.len());
    Ok(ToolOutput::new(entries.join("\n"), summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_describes_every_tool_to_the_model() {
        let registry = ToolRegistry::read_only();
        let definitions = registry.definitions();

        assert!(!definitions.is_empty());
        for definition in &definitions {
            assert!(!definition.name.is_empty());
            assert!(
                definition.description.len() > 20,
                "`{}` needs a description the model can act on",
                definition.name
            );
            assert_eq!(
                definition.parameters["type"], "object",
                "`{}` parameters must be a JSON Schema object",
                definition.name
            );
        }
    }

    #[test]
    fn tools_are_addressable_by_the_name_the_model_sends() {
        let registry = ToolRegistry::read_only();

        assert!(registry.get("read").is_some());
        assert!(registry.get("list").is_some());
        assert!(registry.get("no-such-tool").is_none());
    }

    /// The rule the turn loop uses to decide what may run at the same time.
    ///
    /// Mirrored here rather than imported so that a tool whose kind changes trips this test as
    /// well as changing behaviour.
    fn runs_concurrently(kind: ToolKind) -> bool {
        matches!(kind, ToolKind::Read | ToolKind::Search)
    }

    #[test]
    fn only_tools_that_change_nothing_run_at_the_same_time() {
        let registry = ToolRegistry::default_tools();

        for name in ["read", "list"] {
            let tool = registry.get(name).expect("should exist");
            assert!(
                runs_concurrently(tool.kind()),
                "`{name}` only reads, so waiting for it in turn is latency for nothing"
            );
        }

        for name in ["write", "edit", "shell"] {
            let tool = registry.get(name).expect("should exist");
            assert!(
                !runs_concurrently(tool.kind()),
                "`{name}` must stay sequential"
            );
        }
    }

    #[test]
    fn editing_tools_are_never_concurrent_because_they_can_collide() {
        // Two edits to the same file run together means the second reads a buffer the first has
        // already changed, and whichever finishes last wins — silently.
        assert!(!runs_concurrently(ToolKind::Edit));
    }

    #[test]
    fn commands_are_never_concurrent_because_order_is_their_meaning() {
        // `npm install` then `npm test` is not the same as both at once. And the permission broker
        // holds one question at a time, so two commands asking together would mean one denied for
        // no reason the user could see.
        assert!(!runs_concurrently(ToolKind::Execute));
    }

    #[test]
    fn the_default_registry_can_change_files_and_the_read_only_one_cannot() {
        let default = ToolRegistry::default_tools();
        assert!(default.get("write").is_some());
        assert!(default.get("edit").is_some());

        let read_only = ToolRegistry::read_only();
        assert!(read_only.get("write").is_none());
        assert!(read_only.get("edit").is_none());
        assert!(read_only.get("read").is_some());
    }

    #[test]
    fn a_change_is_judged_on_what_it_adds_not_on_the_whole_file() {
        // The user's own credentials are not the agent's to be blamed for; scanning the result
        // would lock the agent out of any file that ever contained one.
        let splice = Change::Splice {
            old: "KEY = \"AKIAIOSFODNN7EXAMPLE\"".into(),
            new: "KEY = env(\"AWS_KEY\")".into(),
            all: false,
        };

        assert_eq!(splice.added_text(), "KEY = env(\"AWS_KEY\")");
        assert_eq!(Change::Whole("all of it".into()).added_text(), "all of it");
    }

    #[test]
    fn occurrences_finds_every_non_overlapping_match() {
        assert_eq!(occurrences("a b a", "a"), vec![0..1, 4..5]);
        assert!(occurrences("abc", "z").is_empty());
        assert!(
            occurrences("hello", "").is_empty(),
            "an empty needle matches everywhere and means nothing"
        );
    }

    #[test]
    fn occurrences_are_byte_ranges_that_survive_multibyte_text() {
        let text = "let a = \"café\";\nlet b = \"café\";\n";
        let found = occurrences(text, "café");

        assert_eq!(found.len(), 2);
        for range in found {
            assert_eq!(&text[range], "café", "a range must land on a char boundary");
        }
    }

    #[test]
    fn the_edit_tool_tells_the_model_how_to_make_a_match_unique() {
        // The wording is the interface: a model that is told only "failed" retries the same edit.
        let description = EditTool.description();

        assert!(description.contains("exactly once"), "{description}");
        assert!(description.contains("Read the file first"), "{description}");
    }

    #[test]
    fn missing_arguments_are_reported_rather_than_defaulted() {
        let error = string_argument(&json!({}), "path")
            .expect_err("a missing argument should not silently become empty");

        assert!(error.to_string().contains("path"), "got: {error}");
    }
}
