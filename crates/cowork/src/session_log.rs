//! One Markdown file holding everything that happened in Cowork threads, for diagnosing a session.
//!
//! The reader this is written for is usually another AI assistant, handed the file and asked what
//! worked, what failed and where the limits were. So it favours completeness and a structure that
//! can be navigated without guessing over looking good: every message in order, every tool call
//! with its full arguments, every result in full, and the thread's activity timeline around them.
//!
//! Two things are never written: what an attachment contains, and the file text a checkpoint keeps
//! for rewinds. Both can run to megabytes, neither explains a failure better than its name and size
//! do, and they are where a user's private data most likely sits. Credentials anywhere else in the
//! document are replaced before it is written, using the secret scan's own patterns.

use crate::{
    checkpoint::Before,
    cowork_settings::CoworkSettings,
    provider::{AttachmentKind, Role, ToolCall, ToolResult},
    thread::{ActivityEntry, ActivityKind, Thread, ThreadMetadata, now_seconds},
    thread_view::{permission_detail, permission_label},
    verify::{self, CheckReport, Severity},
};
use anyhow::{Context as _, Result};
use collections::{HashMap, HashSet};
use fs::Fs;
use gpui::{App, AppContext as _, AsyncApp, Task, WeakEntity};
use release_channel::AppVersion;
use settings::Settings as _;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Display,
    path::PathBuf,
    sync::Arc,
};
use time::{
    OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339,
    macros::format_description,
};
use util::ResultExt as _;
use workspace::{Toast, Workspace, notifications::NotificationId};

/// The most lines of Anna's own log an export carries. The newest are the ones kept: the failure a
/// log is exported to explain is usually just before the export.
const MAX_LOG_LINES: usize = 500;

/// One thread as a log sees it: what is stored, and what only a view that has it open knows.
#[derive(Clone, Debug)]
pub struct ThreadLog {
    pub thread: Thread,
    /// What the model worked through before each message, by message index. Reasoning is never
    /// stored, so only a thread open in a view has any.
    pub reasoning: Vec<String>,
    /// The failure on screen when the log was taken.
    pub current_error: Option<String>,
    /// The branch checked out in the thread's folder when the log was taken.
    pub branch: Option<String>,
    /// Whether this came from an open view, which decides what may legitimately be missing: check
    /// reports, reasoning and the error on screen are never stored.
    pub open_in_view: bool,
}

impl ThreadLog {
    /// A thread read back from the database, with nothing a view would add.
    pub fn stored(thread: Thread) -> Self {
        Self {
            thread,
            reasoning: Vec::new(),
            current_error: None,
            branch: None,
            open_in_view: false,
        }
    }
}

/// The machine and the moment, shared by every thread in one document.
#[derive(Clone, Debug)]
pub struct Environment {
    /// Unix seconds.
    pub exported_at: u64,
    /// The offset every time is written in, so times read the same as the clock the user saw.
    pub offset: UtcOffset,
    pub app_version: String,
    pub os: String,
    pub permission_level: String,
}

/// Anna's own log file, as it was read for the excerpt.
#[derive(Clone, Debug)]
pub struct WuLog {
    pub path: String,
    /// The text, or why it could not be read.
    pub contents: Result<String, String>,
}

pub struct SessionLogInput {
    pub thread: ThreadLog,
    pub environment: Environment,
    pub wu_log: WuLog,
}

/// A thread an export of many tried to include.
pub enum ExportedThread {
    Loaded(ThreadLog),
    /// Listed in the index, but its stored conversation could not be read.
    Failed {
        metadata: ThreadMetadata,
        error: String,
    },
}

pub struct AllThreadsLogInput {
    /// Which threads these are, in words.
    pub scope: String,
    pub threads: Vec<ExportedThread>,
    pub environment: Environment,
    pub wu_log: WuLog,
}

/// The log of one thread.
pub fn session_log(input: &SessionLogInput) -> String {
    let environment = &input.environment;
    let thread = render_thread(&input.thread, environment, ThreadHeading::Document);
    let excerpt = render_log_excerpt(
        &input.wu_log,
        Some(input.thread.thread.metadata.created_at),
        environment,
    );
    let mut redactions = thread.redactions;
    add_counts(&mut redactions, &excerpt.redactions);

    let mut document = String::new();
    push_heading(&mut document, 1, "Anna session log");
    document.push_str(&format!(
        "Everything recorded in one Anna thread, for diagnosing what worked and what did not. \
         Exported {} from Anna {} on {}. Attachment contents and the file text kept for rewinds are \
         never included. Credentials were replaced with `[REDACTED: <kind>]`: {} in this file.\n\n",
        format_time(environment.exported_at, environment.offset),
        environment.app_version,
        environment.os,
        describe_redactions(&redactions),
    ));
    document.push_str(&thread.text);
    document.push_str(&excerpt.text);
    document
}

/// The log of many threads in one file: what was exported, a line per thread to find it by, then
/// each thread exactly as its own export would show it.
pub fn all_threads_log(input: &AllThreadsLogInput) -> String {
    let environment = &input.environment;
    let mut totals = Summary::default();
    let mut redactions = BTreeMap::new();
    let mut contents = String::new();
    let mut sections = String::new();
    let mut failed = 0;

    for (index, exported) in input.threads.iter().enumerate() {
        let number = index + 1;
        match exported {
            ExportedThread::Loaded(log) => {
                let rendered = render_thread(log, environment, ThreadHeading::Numbered(number));
                let summary = &rendered.summary;
                let on_screen = if log.current_error.is_some() {
                    ", an error on screen"
                } else {
                    ""
                };
                contents.push_str(&format!(
                    "{number}. {} — {} messages — {} tool errors, {} failed turns{on_screen}\n",
                    contents_entry(number, &log.thread.metadata, environment),
                    log.thread.messages.len(),
                    summary.tool_errors,
                    summary.turns_failed,
                ));
                totals.add(summary);
                add_counts(&mut redactions, &rendered.redactions);
                sections.push_str(&rendered.text);
            }
            ExportedThread::Failed { metadata, error } => {
                failed += 1;
                contents.push_str(&format!(
                    "{number}. {} — could not be loaded\n",
                    contents_entry(number, metadata, environment)
                ));
                let rendered = verify::redact_secrets(&render_failed_thread(
                    number,
                    metadata,
                    error,
                    environment,
                ));
                add_counts(&mut redactions, &rendered.counts);
                sections.push_str(&rendered.text);
            }
        }
    }

    let contents = verify::redact_secrets(&contents);
    add_counts(&mut redactions, &contents.counts);
    let scope = verify::redact_secrets(&input.scope);
    add_counts(&mut redactions, &scope.counts);

    let since = input
        .threads
        .iter()
        .map(|exported| match exported {
            ExportedThread::Loaded(log) => log.thread.metadata.created_at,
            ExportedThread::Failed { metadata, .. } => metadata.created_at,
        })
        .min();
    let excerpt = render_log_excerpt(&input.wu_log, since, environment);
    add_counts(&mut redactions, &excerpt.redactions);

    let mut document = String::new();
    push_heading(&mut document, 1, "Anna log: all threads");
    document.push_str(
        "Everything recorded in each thread below, for diagnosing what worked and what did not. \
         Attachment contents and the file text kept for rewinds are never included, and \
         credentials were replaced with `[REDACTED: <kind>]`.\n\n",
    );
    push_heading(&mut document, 2, "Export");
    push_item(&mut document, "Scope", &scope.text);
    push_item(
        &mut document,
        "Exported",
        &format_time(environment.exported_at, environment.offset),
    );
    push_item(&mut document, "App version", &environment.app_version);
    push_item(&mut document, "OS", &environment.os);
    push_item(
        &mut document,
        "Permission level (at export)",
        &environment.permission_level,
    );
    push_item(
        &mut document,
        "Threads",
        &format!(
            "{} ({} loaded, {failed} could not be loaded)",
            input.threads.len(),
            input.threads.len() - failed
        ),
    );
    push_item(
        &mut document,
        "Credentials redacted",
        &describe_redactions(&redactions),
    );
    document.push('\n');
    totals.write(&mut document, 3, "Totals across threads", false);

    push_heading(&mut document, 2, "Contents");
    if contents.text.is_empty() {
        document.push_str("No threads.\n\n");
    } else {
        document.push_str(&contents.text);
        document.push('\n');
    }
    document.push_str(&sections);
    document.push_str(&excerpt.text);
    document
}

#[derive(Clone, Copy)]
enum ThreadHeading {
    /// The only thread in the document, whose sections are the document's own.
    Document,
    /// One of several, under a numbered heading of its own.
    Numbered(usize),
}

impl ThreadHeading {
    fn section_level(self) -> usize {
        match self {
            ThreadHeading::Document => 2,
            ThreadHeading::Numbered(_) => 3,
        }
    }
}

struct RenderedThread {
    text: String,
    redactions: BTreeMap<&'static str, usize>,
    summary: Summary,
}

fn render_thread(
    log: &ThreadLog,
    environment: &Environment,
    heading: ThreadHeading,
) -> RenderedThread {
    let thread = &log.thread;
    let metadata = &thread.metadata;
    let level = heading.section_level();
    let summary = Summary::of(thread);

    let mut session = String::new();
    if let ThreadHeading::Numbered(number) = heading {
        push_thread_heading(&mut session, number, &metadata.title);
    }
    push_heading(&mut session, level, "Session");
    push_metadata(&mut session, metadata, environment);
    let branch = match (&log.branch, log.open_in_view) {
        (Some(branch), _) => code(branch),
        (None, true) => "unknown: the folder is not in a git repository open in this project".to_owned(),
        (None, false) => "unknown: only a thread open in a view reports it".to_owned(),
    };
    push_item(&mut session, "Branch (at export)", &branch);
    push_item(
        &mut session,
        "Permission level (at export)",
        &environment.permission_level,
    );
    push_item(&mut session, "App version", &environment.app_version);
    push_item(&mut session, "OS", &environment.os);
    push_item(&mut session, "Messages", &thread.messages.len().to_string());
    push_item(
        &mut session,
        "Context tokens (last reported)",
        &match metadata.context_tokens {
            Some(tokens) => tokens.to_string(),
            None => "not reported by the provider".to_owned(),
        },
    );
    push_item(
        &mut session,
        "Forked",
        if metadata.forked { "yes" } else { "no" },
    );
    push_item(
        &mut session,
        "Source",
        if log.open_in_view {
            "open in a view, so reasoning, check reports and the error on screen are included"
        } else {
            "read from the database, where reasoning, check reports and the error on screen are \
             never stored"
        },
    );

    // A draft nobody has typed into has nothing to summarize, and empty sections would only be
    // something for the reader to rule out.
    let mut body = String::new();
    if !thread.messages.is_empty() || !thread.activity.is_empty() || log.current_error.is_some() {
        summary.write(&mut body, level, "Summary", true);
        write_transcript(&mut body, log, level);
        write_activity(&mut body, &thread.activity, environment.offset, level);
        if let Some(error) = &log.current_error {
            push_heading(&mut body, level, "Current error");
            push_fenced(&mut body, "text", error, "");
            body.push('\n');
        }
    }

    // Redacted before the count is written, so the count describes this very text.
    let session = verify::redact_secrets(&session);
    let body = verify::redact_secrets(&body);
    let mut redactions = session.counts;
    add_counts(&mut redactions, &body.counts);

    let mut text = session.text;
    push_item(
        &mut text,
        "Credentials redacted",
        &describe_redactions(&redactions),
    );
    text.push('\n');
    text.push_str(&body.text);

    RenderedThread {
        text,
        redactions,
        summary,
    }
}

fn render_failed_thread(
    number: usize,
    metadata: &ThreadMetadata,
    error: &str,
    environment: &Environment,
) -> String {
    let mut out = String::new();
    push_thread_heading(&mut out, number, &metadata.title);
    push_heading(&mut out, 3, "Session");
    push_metadata(&mut out, metadata, environment);
    push_item(
        &mut out,
        "Messages (according to the index)",
        &metadata.message_count.to_string(),
    );
    out.push('\n');
    push_heading(&mut out, 3, "Could not be loaded");
    out.push_str(
        "The thread is listed in the index, but its stored conversation could not be read, so \
         nothing more is known about it:\n\n",
    );
    push_fenced(&mut out, "text", error, "");
    out.push('\n');
    out
}

fn push_metadata(out: &mut String, metadata: &ThreadMetadata, environment: &Environment) {
    push_item(out, "Title", &single_line(&metadata.title));
    push_item(out, "Thread id", &code(metadata.id.as_str()));
    push_item(out, "Model", &code(&metadata.model.qualified()));
    push_item(out, "Folder", &folder(metadata));
    push_item(
        out,
        "Created",
        &format_time(metadata.created_at, environment.offset),
    );
    push_item(
        out,
        "Updated",
        &format_time(metadata.updated_at, environment.offset),
    );
}

fn contents_entry(number: usize, metadata: &ThreadMetadata, environment: &Environment) -> String {
    format!(
        "[{}](#{}) — {} — {} — created {}",
        single_line(&metadata.title)
            .replace('[', "\\[")
            .replace(']', "\\]"),
        anchor(number),
        code(&metadata.model.qualified()),
        folder(metadata),
        format_time(metadata.created_at, environment.offset),
    )
}

fn folder(metadata: &ThreadMetadata) -> String {
    metadata
        .project
        .as_deref()
        .map(code)
        .unwrap_or_else(|| "none recorded".to_owned())
}

/// Counts over one thread, or added up over several.
#[derive(Clone, Debug, Default)]
struct Summary {
    user_messages: usize,
    assistant_messages: usize,
    turns_finished: usize,
    turns_failed: usize,
    turns_stopped_repeating: usize,
    turns_cancelled: usize,
    tool_calls: BTreeMap<String, usize>,
    tool_errors: usize,
    permission_requests: usize,
    permissions_allowed: usize,
    permissions_denied: usize,
    rewinds: usize,
    attachments_refused: usize,
    attachments: BTreeMap<&'static str, usize>,
    /// Distinct, so a file edited in three threads is one file changed across them.
    files_changed: BTreeSet<String>,
}

impl Summary {
    fn of(thread: &Thread) -> Self {
        let mut summary = Self::default();

        for message in &thread.messages {
            match message.role {
                Role::User => summary.user_messages += 1,
                Role::Assistant => summary.assistant_messages += 1,
                Role::Tool => {}
            }
            for call in &message.tool_calls {
                *summary.tool_calls.entry(call.name.clone()).or_insert(0) += 1;
            }
            for result in &message.tool_results {
                if result.is_error {
                    summary.tool_errors += 1;
                }
                if let Some(checkpoint) = &result.checkpoint {
                    summary.files_changed.insert(checkpoint.abs_path.clone());
                } else if !result.diff.is_empty() && !result.path.is_empty() {
                    summary.files_changed.insert(result.path.clone());
                }
            }
            for attachment in &message.attachments {
                *summary
                    .attachments
                    .entry(attachment_kind_name(attachment.kind()))
                    .or_insert(0) += 1;
            }
        }

        for entry in &thread.activity {
            match entry.kind {
                ActivityKind::TurnFinished => summary.turns_finished += 1,
                ActivityKind::TurnFailed => summary.turns_failed += 1,
                ActivityKind::RepeatedCallsStopped => summary.turns_stopped_repeating += 1,
                ActivityKind::TurnCancelled => summary.turns_cancelled += 1,
                ActivityKind::PermissionRequested => summary.permission_requests += 1,
                ActivityKind::PermissionAllowed => summary.permissions_allowed += 1,
                ActivityKind::PermissionDenied => summary.permissions_denied += 1,
                ActivityKind::Rewound => summary.rewinds += 1,
                ActivityKind::AttachmentsRefused => summary.attachments_refused += 1,
                ActivityKind::MessageSent | ActivityKind::Forked => {}
            }
        }

        summary
    }

    fn add(&mut self, other: &Summary) {
        self.user_messages += other.user_messages;
        self.assistant_messages += other.assistant_messages;
        self.turns_finished += other.turns_finished;
        self.turns_failed += other.turns_failed;
        self.turns_stopped_repeating += other.turns_stopped_repeating;
        self.turns_cancelled += other.turns_cancelled;
        add_counts(&mut self.tool_calls, &other.tool_calls);
        self.tool_errors += other.tool_errors;
        self.permission_requests += other.permission_requests;
        self.permissions_allowed += other.permissions_allowed;
        self.permissions_denied += other.permissions_denied;
        self.rewinds += other.rewinds;
        self.attachments_refused += other.attachments_refused;
        add_counts(&mut self.attachments, &other.attachments);
        self.files_changed
            .extend(other.files_changed.iter().cloned());
    }

    fn write(&self, out: &mut String, level: usize, heading: &str, list_files: bool) {
        push_heading(out, level, heading);
        push_item(out, "User messages", &self.user_messages.to_string());
        push_item(
            out,
            "Assistant messages",
            &format!(
                "{} (one per model step; a turn that calls tools takes several)",
                self.assistant_messages
            ),
        );
        push_item(out, "Turns finished", &self.turns_finished.to_string());
        push_item(out, "Turns failed", &self.turns_failed.to_string());
        push_item(
            out,
            "Turns stopped for repeating the same call",
            &self.turns_stopped_repeating.to_string(),
        );
        push_item(out, "Turns cancelled", &self.turns_cancelled.to_string());
        push_item(out, "Tool calls", &with_breakdown(&self.tool_calls));
        push_item(out, "Tool errors", &self.tool_errors.to_string());
        push_item(
            out,
            "Permission requests (the user was asked)",
            &self.permission_requests.to_string(),
        );
        push_item(
            out,
            "Permissions allowed",
            &self.permissions_allowed.to_string(),
        );
        push_item(
            out,
            "Permissions denied",
            &self.permissions_denied.to_string(),
        );
        push_item(out, "Rewinds", &self.rewinds.to_string());
        push_item(
            out,
            "Attachments refused before sending",
            &self.attachments_refused.to_string(),
        );
        push_item(out, "Attachments sent", &with_breakdown(&self.attachments));
        push_item(out, "Files changed", &self.files_changed.len().to_string());
        if list_files {
            for path in &self.files_changed {
                out.push_str(&format!("  - {}\n", code(path)));
            }
        }
        out.push('\n');
    }
}

fn write_transcript(out: &mut String, log: &ThreadLog, level: usize) {
    let messages = &log.thread.messages;
    push_heading(out, level, "Transcript");
    if messages.is_empty() {
        out.push_str("No messages.\n\n");
        return;
    }

    let call_names = messages
        .iter()
        .flat_map(|message| &message.tool_calls)
        .map(|call| (call.id.as_str(), call.name.as_str()))
        .collect::<HashMap<_, _>>();
    let answered = messages
        .iter()
        .flat_map(|message| &message.tool_results)
        .map(|result| result.call_id.as_str())
        .collect::<HashSet<_>>();

    for (index, message) in messages.iter().enumerate() {
        let number = index + 1;
        match message.role {
            Role::User => {
                push_heading(out, level + 1, &format!("Message {number} · user"));
                push_text(out, "Text", "markdown", &message.text);
                if !message.attachments.is_empty() {
                    out.push_str("Attachments (contents not included):\n\n");
                    for attachment in &message.attachments {
                        out.push_str(&format!(
                            "- {} — {} — {}\n",
                            code(&attachment.name),
                            code(&attachment.media_type),
                            describe_size(decoded_size(&attachment.data)),
                        ));
                    }
                    out.push('\n');
                }
            }
            Role::Assistant => {
                push_heading(out, level + 1, &format!("Message {number} · assistant"));
                if let Some(reasoning) = log
                    .reasoning
                    .get(index)
                    .filter(|reasoning| !reasoning.trim().is_empty())
                {
                    push_text(out, "Reasoning", "text", reasoning);
                }
                push_text(out, "Text", "markdown", &message.text);
                for (call_index, call) in message.tool_calls.iter().enumerate() {
                    write_call(
                        out,
                        call,
                        call_index + 1,
                        answered.contains(call.id.as_str()),
                        level + 2,
                    );
                }
            }
            Role::Tool => {
                push_heading(out, level + 1, &format!("Message {number} · tool results"));
                if message.tool_results.is_empty() {
                    out.push_str("No results.\n\n");
                }
                for result in &message.tool_results {
                    write_result(
                        out,
                        result,
                        call_names.get(result.call_id.as_str()).copied(),
                        level + 2,
                    );
                }
            }
        }
    }
}

fn write_call(out: &mut String, call: &ToolCall, number: usize, answered: bool, level: usize) {
    push_heading(
        out,
        level,
        &format!(
            "Tool call {number} · {} · id {}",
            code(&call.name),
            code(&call.id)
        ),
    );

    let parsed = serde_json::from_str::<serde_json::Value>(&call.arguments);
    if let Ok(serde_json::Value::Object(fields)) = &parsed
        && let Some(intent) = fields
            .get("intent")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|intent| !intent.is_empty())
    {
        push_item(out, "Intent", &single_line(intent));
    }
    if !answered {
        push_item(
            out,
            "Result",
            "none recorded: the turn failed, was stopped or was cancelled before this call returned",
        );
    }

    if call.arguments.trim().is_empty() {
        push_item(out, "Arguments", "none");
        out.push('\n');
        return;
    }
    out.push('\n');
    match parsed.and_then(|value| serde_json::to_string_pretty(&value)) {
        Ok(pretty) => {
            out.push_str("Arguments:\n\n");
            push_fenced(out, "json", &pretty, "");
        }
        // Shown as they arrived: a malformed call is one of the things a log is read to find.
        Err(error) => {
            out.push_str(&format!("Arguments, which are not valid JSON ({error}):\n\n"));
            push_fenced(out, "text", &call.arguments, "");
        }
    }
    out.push('\n');
}

fn write_result(out: &mut String, result: &ToolResult, tool: Option<&str>, level: usize) {
    let status = if result.is_error { "error" } else { "ok" };
    let tool = tool
        .map(code)
        .unwrap_or_else(|| "no matching call".to_owned());
    push_heading(
        out,
        level,
        &format!("Result for {} · {tool} · {status}", code(&result.call_id)),
    );
    if !result.path.is_empty() {
        push_item(out, "File", &code(&result.path));
    }
    if let Some(checkpoint) = &result.checkpoint {
        // What kind of change it was, and never the text it replaced.
        let change = match &checkpoint.before {
            Before::Missing => "created",
            Before::Text(_) => "edited",
            Before::TooLarge => "too large to record",
        };
        push_item(
            out,
            "Checkpoint",
            &format!("{change} {}", code(&checkpoint.abs_path)),
        );
    }
    out.push('\n');

    push_text(out, "Content", "text", &result.content);
    if !result.diff.is_empty() {
        out.push_str("Diff:\n\n");
        push_fenced(out, "diff", &result.diff, "");
        out.push('\n');
    }
    if let Some(checks) = &result.checks {
        write_checks(out, checks);
    }
}

fn write_checks(out: &mut String, checks: &CheckReport) {
    out.push_str("Checks:\n\n");
    let attached = if checks.attached.is_empty() {
        "none".to_owned()
    } else {
        checks
            .attached
            .iter()
            .map(|name| name.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    push_item(out, "Language servers attached", &attached);
    push_item(
        out,
        "Changed by the formatter",
        if checks.formatted { "yes" } else { "no" },
    );
    push_item(out, "Findings", &checks.findings.len().to_string());
    for finding in &checks.findings {
        let severity = match finding.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        out.push_str(&format!(
            "  - {severity} [{}] line {}: {}\n",
            finding.check,
            finding.line,
            single_line(&finding.message)
        ));
    }
    out.push('\n');
}

fn write_activity(out: &mut String, activity: &[ActivityEntry], offset: UtcOffset, level: usize) {
    push_heading(out, level, "Activity");
    if activity.is_empty() {
        out.push_str(
            "Nothing recorded. A thread last saved before Anna kept an activity timeline has none.\n\n",
        );
        return;
    }

    for entry in activity {
        let mut lines = entry.detail.lines();
        let first = lines.next().unwrap_or("");
        let rest = lines.collect::<Vec<_>>();
        out.push_str(&format!(
            "- {} · {} · {first}\n",
            format_time(entry.at, offset),
            code(entry.kind.as_str()),
        ));
        if !rest.is_empty() {
            push_fenced(out, "text", &rest.join("\n"), "  ");
        }
    }
    out.push('\n');
}

struct RenderedExcerpt {
    text: String,
    redactions: BTreeMap<&'static str, usize>,
}

fn render_log_excerpt(
    wu_log: &WuLog,
    since: Option<u64>,
    environment: &Environment,
) -> RenderedExcerpt {
    let mut text = String::new();
    push_heading(&mut text, 2, "Anna log excerpt");
    push_item(&mut text, "File", &code(&wu_log.path));

    match &wu_log.contents {
        Err(error) => {
            text.push('\n');
            text.push_str("Anna's log file could not be read, so there is no excerpt:\n\n");
            push_fenced(&mut text, "text", error, "");
            text.push('\n');
        }
        Ok(log) => {
            let window = match since {
                Some(since) => format!(
                    "since {}, when the earliest thread in this file was created",
                    format_time(since, environment.offset)
                ),
                None => "at any time".to_owned(),
            };
            push_item(
                &mut text,
                "Kept",
                &format!(
                    "lines mentioning `cowork` (the agent's own), and every warning and error, \
                     logged {window}; a line without a timestamp continues the entry above it"
                ),
            );

            let (lines, matching) = log_excerpt(log, since);
            if lines.is_empty() {
                push_item(&mut text, "Lines", "none matched");
                text.push('\n');
            } else {
                let count = if matching > lines.len() {
                    format!("the last {} of {matching}", lines.len())
                } else {
                    lines.len().to_string()
                };
                push_item(&mut text, "Lines", &count);
                text.push('\n');
                push_fenced(&mut text, "text", &lines.join("\n"), "");
                text.push('\n');
            }
        }
    }

    let redacted = verify::redact_secrets(&text);
    RenderedExcerpt {
        text: redacted.text,
        redactions: redacted.counts,
    }
}

/// The lines of Anna's log worth reading beside a thread, and how many matched before only the newest
/// [`MAX_LOG_LINES`] were kept.
///
/// A line is kept when it mentions `cowork` or is a warning or an error, and was logged at or after
/// `since`. A line with no timestamp of its own continues the entry above it — a multi-line error —
/// and goes wherever that entry went. Timestamps are parsed rather than compared as text, because
/// the log writes the local offset, which moves with daylight saving and with the time zone.
fn log_excerpt(log: &str, since: Option<u64>) -> (Vec<&str>, usize) {
    let since = since.and_then(|since| i64::try_from(since).ok());
    let mut kept = Vec::new();
    let mut keeping = false;

    for line in log.lines() {
        if let Some(logged_at) = line_timestamp(line) {
            let recent = since.is_none_or(|since| logged_at >= since);
            keeping = recent && (mentions_cowork(line) || is_warning_or_error(line));
        }
        if keeping {
            kept.push(line);
        }
    }

    let matching = kept.len();
    let kept = kept.split_off(matching.saturating_sub(MAX_LOG_LINES));
    (kept, matching)
}

fn line_timestamp(line: &str) -> Option<i64> {
    let token = line.split(' ').next()?;
    OffsetDateTime::parse(token, &Rfc3339)
        .ok()
        .map(OffsetDateTime::unix_timestamp)
}

fn mentions_cowork(line: &str) -> bool {
    line.to_ascii_lowercase().contains("cowork")
}

fn is_warning_or_error(line: &str) -> bool {
    matches!(line.split_whitespace().nth(1), Some("ERROR" | "WARN"))
}

fn push_heading(out: &mut String, level: usize, text: &str) {
    out.push_str(&"#".repeat(level.clamp(1, 6)));
    out.push(' ');
    out.push_str(text);
    out.push_str("\n\n");
}

fn push_item(out: &mut String, label: &str, value: &str) {
    out.push_str(&format!("- {label}: {value}\n"));
}

fn push_thread_heading(out: &mut String, number: usize, title: &str) {
    out.push_str(&format!("<a id=\"{}\"></a>\n\n", anchor(number)));
    push_heading(
        out,
        2,
        &format!("Thread {number}: {}", single_line(title)),
    );
}

fn anchor(number: usize) -> String {
    format!("thread-{number}")
}

/// A label, then the text in a fenced block, or a note that there is none.
fn push_text(out: &mut String, label: &str, info: &str, text: &str) {
    if text.trim().is_empty() {
        out.push_str(&format!("{label}: none.\n\n"));
    } else {
        out.push_str(&format!("{label}:\n\n"));
        push_fenced(out, info, text, "");
        out.push('\n');
    }
}

/// A fenced block that nothing inside it can close.
///
/// The fence is one backtick longer than the longest run of backticks in the content, which is all
/// CommonMark needs: a closing fence has to be at least as long as the opening one. Without it a
/// tool that printed Markdown of its own, fences and all, would end the block early and have the
/// rest of its output read as the document's own headings.
fn push_fenced(out: &mut String, info: &str, content: &str, indent: &str) {
    let fence = "`".repeat(longest_backtick_run(content).max(2) + 1);
    out.push_str(indent);
    out.push_str(&fence);
    out.push_str(info);
    out.push('\n');
    for line in content.lines() {
        out.push_str(indent);
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(indent);
    out.push_str(&fence);
    out.push('\n');
}

fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for character in text.chars() {
        if character == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

/// Inline code that a backtick in the text cannot end early, on one line so it cannot end a list
/// item either.
fn code(text: &str) -> String {
    let text = single_line(text);
    let fence = "`".repeat(longest_backtick_run(&text) + 1);
    let padding = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{fence}{padding}{text}{padding}{fence}")
}

fn single_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

/// RFC 3339 in the export's offset, or the raw number for a time that cannot be represented.
fn format_time(unix_seconds: u64, offset: UtcOffset) -> String {
    i64::try_from(unix_seconds)
        .ok()
        .and_then(|seconds| OffsetDateTime::from_unix_timestamp(seconds).ok())
        .and_then(|time| time.checked_to_offset(offset))
        .and_then(|time| time.format(&Rfc3339).ok())
        .unwrap_or_else(|| format!("{unix_seconds} (unix seconds)"))
}

/// The size of the file base64 `data` encodes, without decoding it.
fn decoded_size(data: &str) -> usize {
    let padding = data
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'=')
        .take(2)
        .count();
    (data.len() / 4 * 3).saturating_sub(padding)
}

fn describe_size(bytes: usize) -> String {
    const KILOBYTE: usize = 1024;
    const MEGABYTE: usize = 1024 * 1024;
    match bytes {
        0..KILOBYTE => format!("{bytes} bytes"),
        KILOBYTE..MEGABYTE => format!("{:.1} KB ({bytes} bytes)", bytes as f64 / KILOBYTE as f64),
        _ => format!("{:.1} MB ({bytes} bytes)", bytes as f64 / MEGABYTE as f64),
    }
}

fn attachment_kind_name(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Image => "image",
        AttachmentKind::Pdf => "pdf",
        AttachmentKind::Text => "text",
    }
}

fn add_counts<Key: Ord + Clone>(into: &mut BTreeMap<Key, usize>, from: &BTreeMap<Key, usize>) {
    for (key, count) in from {
        *into.entry(key.clone()).or_insert(0) += count;
    }
}

/// `3 (edit: 1, read: 2)`, or `0`.
fn with_breakdown<Key: Display>(counts: &BTreeMap<Key, usize>) -> String {
    let total = counts.values().sum::<usize>();
    if total == 0 {
        return "0".to_owned();
    }
    let breakdown = counts
        .iter()
        .map(|(key, count)| format!("{key}: {count}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{total} ({breakdown})")
}

fn describe_redactions(counts: &BTreeMap<&'static str, usize>) -> String {
    if counts.values().sum::<usize>() == 0 {
        "none found".to_owned()
    } else {
        with_breakdown(counts)
    }
}

/// What a log is made of, once any stored threads have been read back.
pub enum LogContent {
    Session(ThreadLog),
    AllThreads {
        scope: String,
        threads: Vec<ExportedThread>,
    },
}

impl LogContent {
    fn render(self, environment: Environment, wu_log: WuLog) -> String {
        match self {
            LogContent::Session(thread) => session_log(&SessionLogInput {
                thread,
                environment,
                wu_log,
            }),
            LogContent::AllThreads { scope, threads } => all_threads_log(&AllThreadsLogInput {
                scope,
                threads,
                environment,
                wu_log,
            }),
        }
    }
}

/// Names the toast an export leaves, so a second export replaces it rather than stacking another.
struct ExportNotice;

/// Asks where to save a log, writes it there, and says how that went.
///
/// `content` may still be reading threads back from the database: the save dialog opens at once
/// regardless, and the document is put together once both are done. Everything that reads a file,
/// asks the operating system about itself or renders the document runs off the main thread.
pub fn export(
    kind: &str,
    subject: &str,
    content: Task<LogContent>,
    fs: Arc<dyn Fs>,
    workspace: WeakEntity<Workspace>,
    cx: &mut App,
) {
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);
    let exported_at = now_seconds();
    let suggested_name = suggested_file_name(kind, subject, exported_at, offset);
    let level = &CoworkSettings::get_global(cx).permission;
    let environment = Environment {
        exported_at,
        offset,
        app_version: AppVersion::global(cx).to_string(),
        // Asking the operating system for its version can block, so it is filled in off the main
        // thread, when the document is rendered.
        os: String::new(),
        permission_level: format!("{} ({})", permission_label(level), permission_detail(level)),
    };

    cx.spawn(async move |cx| {
        let outcome = save(suggested_name, content, environment, fs, cx).await;
        report(outcome, &workspace, cx);
    })
    .detach();
}

async fn save(
    suggested_name: String,
    content: Task<LogContent>,
    mut environment: Environment,
    fs: Arc<dyn Fs>,
    cx: &mut AsyncApp,
) -> Result<Option<PathBuf>> {
    let directory = default_directory(&fs, cx).await;
    let chosen = cx.update(|cx| cx.prompt_for_new_path(&directory, Some(suggested_name.as_str())));
    let path = match chosen.await {
        Ok(Ok(Some(path))) => path,
        // Cancelled, or the dialog went away with its window.
        Ok(Ok(None)) | Err(_) => return Ok(None),
        Ok(Err(error)) => return Err(error.context("could not open the save dialog")),
    };

    let content = content.await;
    let wu_log = read_wu_log(&fs).await;
    let document = cx
        .background_spawn(async move {
            environment.os = format!(
                "{} {}",
                client::os_info::os_name(),
                client::os_info::os_version()
            );
            content.render(environment, wu_log)
        })
        .await;

    fs.atomic_write(path.clone(), document)
        .await
        .with_context(|| format!("could not write {}", path.display()))?;
    Ok(Some(path))
}

/// The Downloads folder when there is one, since a file meant to be sent on is looked for there
/// first; the home folder otherwise.
async fn default_directory(fs: &Arc<dyn Fs>, cx: &mut AsyncApp) -> PathBuf {
    let downloads = cx.background_spawn(async { dirs::download_dir() }).await;
    if let Some(downloads) = downloads
        && fs.is_dir(&downloads).await
    {
        return downloads;
    }
    util::paths::home_dir().clone()
}

/// Anna's log, preceded by the file it rotated out when there is one: the log is cut when it grows
/// past its limit, and a long session can begin before the cut.
///
/// Read as bytes and decoded leniently, because one malformed line should cost that line, not the
/// whole excerpt.
async fn read_wu_log(fs: &Arc<dyn Fs>) -> WuLog {
    let path = paths::log_file();
    let contents = match fs.load_bytes(path).await {
        Ok(current) => {
            let current = String::from_utf8_lossy(&current);
            match fs.load_bytes(paths::old_log_file()).await {
                Ok(previous) => Ok(format!("{}\n{current}", String::from_utf8_lossy(&previous))),
                // No rotation has happened yet, which is the usual case.
                Err(_) => Ok(current.into_owned()),
            }
        }
        Err(error) => Err(format!("{error:#}")),
    };
    WuLog {
        path: path.display().to_string(),
        contents,
    }
}

fn report(outcome: Result<Option<PathBuf>>, workspace: &WeakEntity<Workspace>, cx: &mut AsyncApp) {
    let toast = match outcome {
        Ok(None) => return,
        Ok(Some(path)) => Toast::new(
            NotificationId::unique::<ExportNotice>(),
            format!("Saved the log to {}", path.display()),
        )
        .on_click("Show in folder", move |_, cx| cx.reveal_path(&path)),
        Err(error) => {
            log::warn!("cowork: could not export a log: {error:#}");
            Toast::new(
                NotificationId::unique::<ExportNotice>(),
                format!("Could not export the log: {error:#}"),
            )
        }
    };
    workspace
        .update(cx, |workspace, cx| workspace.show_toast(toast, cx))
        .log_err();
}

/// `<kind>-<subject>-<YYYY-MM-DD-HHMM>.md`, in local time.
fn suggested_file_name(kind: &str, subject: &str, at: u64, offset: UtcOffset) -> String {
    let stamp = i64::try_from(at)
        .ok()
        .and_then(|seconds| OffsetDateTime::from_unix_timestamp(seconds).ok())
        .and_then(|time| time.checked_to_offset(offset))
        .and_then(|time| {
            time.format(format_description!("[year]-[month]-[day]-[hour][minute]"))
                .ok()
        })
        .unwrap_or_else(|| at.to_string());
    format!("{kind}-{}-{stamp}.md", file_name_component(subject))
}

/// A title turned into something every file system accepts and a person can still read.
fn file_name_component(text: &str) -> String {
    const MAX_CHARACTERS: usize = 48;

    let mut component = String::new();
    let mut length = 0;
    for character in text.chars() {
        if length >= MAX_CHARACTERS {
            break;
        }
        if character.is_alphanumeric() {
            component.extend(character.to_lowercase());
            length += 1;
        } else if !component.is_empty() && !component.ends_with('-') {
            component.push('-');
            length += 1;
        }
    }

    let component = component.trim_end_matches('-');
    if component.is_empty() {
        "untitled".to_owned()
    } else {
        component.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        catalog::ModelRef,
        checkpoint::Checkpoint,
        provider::{Attachment, Message},
        thread::ThreadId,
        verify::Finding,
    };

    /// 2025-09-04T15:33:20Z.
    const CREATED_AT: u64 = 1_757_000_000;
    const ATTACHMENT_DATA: &str = "aGVyZSBpcyBhIHByaXZhdGUgcGljdHVyZSwgYnl0ZSBieSBieXRl";
    const BEFORE_TEXT: &str = "fn parse() { the file as it was before the edit }";
    const TOOL_OUTPUT_WITH_FENCES: &str = "Build output:\n```\n## Injected heading\n```\ndone";

    fn environment() -> Environment {
        Environment {
            exported_at: CREATED_AT + 3600,
            offset: UtcOffset::UTC,
            app_version: "1.0.6".to_owned(),
            os: "Windows 10.0.19045".to_owned(),
            permission_level: "Standard (Ask before anything that changes something)".to_owned(),
        }
    }

    fn metadata(id: &str, title: &str) -> ThreadMetadata {
        ThreadMetadata {
            id: serde_json::from_value::<ThreadId>(serde_json::Value::String(id.to_owned()))
                .expect("a thread id is a string"),
            title: title.to_owned(),
            model: ModelRef {
                provider_id: "anthropic".to_owned(),
                model_id: "claude-sonnet-4-5".to_owned(),
            },
            created_at: CREATED_AT,
            updated_at: CREATED_AT + 60,
            message_count: 0,
            preview: String::new(),
            context_tokens: Some(12_345),
            project: Some("/home/a/parser".to_owned()),
            forked: false,
        }
    }

    fn thread(id: &str, title: &str, messages: Vec<Message>) -> Thread {
        Thread {
            metadata: metadata(id, title),
            messages,
            activity: Vec::new(),
        }
    }

    fn tool_call(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
        }
    }

    fn tool_result(call_id: &str, content: &str) -> ToolResult {
        ToolResult {
            call_id: call_id.to_owned(),
            content: content.to_owned(),
            is_error: false,
            path: String::new(),
            diff: String::new(),
            checks: None,
            checkpoint: None,
        }
    }

    fn entry(offset: u64, kind: ActivityKind, detail: &str) -> ActivityEntry {
        ActivityEntry {
            at: CREATED_AT + offset,
            kind,
            detail: detail.to_owned(),
        }
    }

    fn wu_log(contents: &str) -> WuLog {
        WuLog {
            path: "/logs/Anna.log".to_owned(),
            contents: Ok(contents.to_owned()),
        }
    }

    /// A thread with one of everything the log has a section or a line for.
    fn eventful_session() -> ThreadLog {
        let mut user = Message::user("Fix the parser, the build fails");
        user.attachments = vec![Attachment {
            media_type: "image/png".to_owned(),
            data: ATTACHMENT_DATA.to_owned(),
            name: "screenshot.png".to_owned(),
        }];

        let mut assistant = Message::assistant("I will read the parser first.");
        assistant.tool_calls = vec![
            tool_call(
                "call-read",
                "read",
                r#"{"intent":"Reading the parser","path":"src/parser.rs"}"#,
            ),
            tool_call(
                "call-edit",
                "edit",
                r#"{"path":"src/parser.rs","old_text":"a","new_text":"b"}"#,
            ),
            tool_call("call-shell", "shell", r#"{"command":"cargo test"}"#),
            tool_call("call-never-ran", "read", "{not json"),
        ];

        let mut edited = tool_result("call-edit", "Edited src/parser.rs");
        edited.path = "src/parser.rs".to_owned();
        edited.diff = "--- a/src/parser.rs\n+++ b/src/parser.rs\n@@ -1 +1 @@\n-a\n+b\n".to_owned();
        edited.checkpoint = Some(Checkpoint {
            abs_path: "/home/a/parser/src/parser.rs".to_owned(),
            before: Before::Text(BEFORE_TEXT.to_owned()),
            after_digest: 7,
        });
        edited.checks = Some(CheckReport {
            attached: vec!["rust-analyzer".into()],
            findings: vec![Finding {
                check: "clippy".into(),
                severity: Severity::Warning,
                line: 3,
                message: "unused variable".to_owned(),
            }],
            formatted: true,
        });

        let mut created = tool_result("call-read", TOOL_OUTPUT_WITH_FENCES);
        created.checkpoint = Some(Checkpoint {
            abs_path: "/home/a/parser/src/new.rs".to_owned(),
            before: Before::Missing,
            after_digest: 9,
        });

        let mut declined = tool_result("call-shell", "the user declined to run this command");
        declined.is_error = true;

        let mut thread = thread(
            "thread-1",
            "Fix the parser",
            vec![
                user,
                assistant,
                Message::tool_results(vec![created, edited, declined]),
            ],
        );
        thread.activity = vec![
            entry(1, ActivityKind::MessageSent, "Message 1 sent"),
            entry(
                5,
                ActivityKind::PermissionRequested,
                "Run a command\ncargo test",
            ),
            entry(9, ActivityKind::PermissionDenied, "Denied by the user"),
            entry(12, ActivityKind::TurnFailed, "the provider answered 529"),
        ];

        ThreadLog {
            thread,
            reasoning: vec![
                String::new(),
                "The parser lives in src/parser.rs.".to_owned(),
            ],
            current_error: Some("the provider answered 529: overloaded".to_owned()),
            branch: Some("main".to_owned()),
            open_in_view: true,
        }
    }

    fn session_document(thread: ThreadLog, log: &str) -> String {
        session_log(&SessionLogInput {
            thread,
            environment: environment(),
            wu_log: wu_log(log),
        })
    }

    fn line_position(document: &str, line: &str) -> Option<usize> {
        let mut offset = 0;
        for candidate in document.split_inclusive('\n') {
            if candidate.trim_end_matches('\n') == line {
                return Some(offset);
            }
            offset += candidate.len();
        }
        None
    }

    fn assert_lines_in_order(document: &str, lines: &[&str]) {
        let mut previous: Option<(&str, usize)> = None;
        for line in lines {
            let position = line_position(document, line)
                .unwrap_or_else(|| panic!("missing the line `{line}` in:\n{document}"));
            if let Some((previous_line, previous_position)) = previous {
                assert!(
                    position > previous_position,
                    "`{line}` should come after `{previous_line}` in:\n{document}"
                );
            }
            previous = Some((line, position));
        }
    }

    /// The headings a Markdown reader would see: `#` lines outside every fenced block, read the way
    /// CommonMark reads fences.
    fn headings_outside_fences(document: &str) -> Vec<&str> {
        let mut open_fence: Option<usize> = None;
        let mut headings = Vec::new();
        for line in document.lines() {
            let trimmed = line.trim_start();
            let backticks = trimmed.chars().take_while(|character| *character == '`').count();
            match open_fence {
                Some(length) => {
                    if backticks >= length && trimmed.trim_end().chars().all(|c| c == '`') {
                        open_fence = None;
                    }
                }
                None if backticks >= 3 => open_fence = Some(backticks),
                None if line.starts_with('#') => headings.push(line),
                None => {}
            }
        }
        headings
    }

    #[test]
    fn a_session_log_has_every_section_in_order() {
        let log = format!(
            "{} ERROR [cowork::thread_view] cowork: completion failed: 529\n",
            format_time(CREATED_AT + 12, UtcOffset::UTC)
        );
        let document = session_document(eventful_session(), &log);

        assert_lines_in_order(
            &document,
            &[
                "# Anna session log",
                "## Session",
                "## Summary",
                "## Transcript",
                "### Message 1 · user",
                "### Message 2 · assistant",
                "### Message 3 · tool results",
                "## Activity",
                "## Current error",
                "## Anna log excerpt",
            ],
        );

        for line in [
            "- Thread id: `thread-1`",
            "- Model: `anthropic/claude-sonnet-4-5`",
            "- Folder: `/home/a/parser`",
            "- Branch (at export): `main`",
            "- Permission level (at export): Standard (Ask before anything that changes something)",
            "- App version: 1.0.6",
            "- OS: Windows 10.0.19045",
            "- Messages: 3",
            "- Context tokens (last reported): 12345",
            "- Forked: no",
            "- User messages: 1",
            "- Tool calls: 4 (edit: 1, read: 2, shell: 1)",
            "- Tool errors: 1",
            "- Permission requests (the user was asked): 1",
            "- Permissions denied: 1",
            "- Turns failed: 1",
            "- Attachments sent: 1 (image: 1)",
            "- Files changed: 2",
            "- Intent: Reading the parser",
            "The parser lives in src/parser.rs.",
            "- Language servers attached: rust-analyzer",
            "  - warning [clippy] line 3: unused variable",
            "```diff",
            "the provider answered 529: overloaded",
            "- Lines: 1",
        ] {
            assert!(
                line_position(&document, line).is_some(),
                "missing the line `{line}` in:\n{document}"
            );
        }
        assert!(
            document.contains("none recorded: the turn failed"),
            "a call that never returned says so"
        );
        assert!(document.contains("Arguments, which are not valid JSON"));

        let created = document
            .lines()
            .find_map(|line| line.strip_prefix("- Created: "))
            .expect("the session says when the thread was created");
        OffsetDateTime::parse(created, &Rfc3339).expect("times are RFC 3339");
        assert!(document.contains(&format!(
            "- {} · `turn_failed` · the provider answered 529",
            format_time(CREATED_AT + 12, UtcOffset::UTC)
        )));
    }

    #[test]
    fn a_credential_in_a_message_is_redacted_and_counted() {
        let log = ThreadLog::stored(thread(
            "thread-1",
            "Upload fails",
            vec![Message::user(
                "My key is AKIAIOSFODNN7EXAMPLE and the upload still fails",
            )],
        ));
        let document = session_document(log, "");

        assert!(
            !document.contains("AKIAIOSFODNN7EXAMPLE"),
            "the key reached the file:\n{document}"
        );
        assert!(document.contains("My key is [REDACTED: aws] and the upload still fails"));
        assert!(line_position(&document, "- Credentials redacted: 1 (aws: 1)").is_some());
    }

    #[test]
    fn attachment_contents_and_checkpoint_text_never_appear() {
        let document = session_document(eventful_session(), "");

        assert!(!document.contains(ATTACHMENT_DATA), "attachment data leaked");
        assert!(!document.contains(BEFORE_TEXT), "a checkpoint's previous text leaked");
        assert!(
            line_position(&document, "- `screenshot.png` — `image/png` — 39 bytes").is_some(),
            "the attachment is described by name, type and size:\n{document}"
        );
        assert!(
            line_position(
                &document,
                "- Checkpoint: edited `/home/a/parser/src/parser.rs`"
            )
            .is_some()
        );
        assert!(
            line_position(&document, "- Checkpoint: created `/home/a/parser/src/new.rs`").is_some()
        );
    }

    #[test]
    fn tool_output_with_fences_of_its_own_stays_inside_its_block() {
        let document = session_document(eventful_session(), "");
        let headings = headings_outside_fences(&document);

        assert!(document.contains("## Injected heading"), "the output is included");
        assert!(
            !headings.contains(&"## Injected heading"),
            "tool output broke out of its fence: {headings:#?}"
        );
        assert!(
            headings.contains(&"## Activity") && headings.contains(&"## Anna log excerpt"),
            "the document after the output must still be read as the document: {headings:#?}"
        );
    }

    #[test]
    fn a_draft_nobody_has_typed_into_exports_only_its_session() {
        let draft = ThreadLog {
            open_in_view: true,
            ..ThreadLog::stored(thread("draft", "New thread", Vec::new()))
        };
        let document = session_document(draft, "");
        let headings = headings_outside_fences(&document);

        assert_eq!(
            headings,
            vec!["# Anna session log", "## Session", "## Anna log excerpt"]
        );
    }

    #[test]
    fn the_log_excerpt_keeps_cowork_lines_and_problems_from_when_the_thread_began() {
        let before = format_time(CREATED_AT - 100, UtcOffset::UTC);
        let after = format_time(CREATED_AT + 10, UtcOffset::UTC);
        let log = [
            format!("{before} ERROR [cowork] from an earlier session"),
            // 14:00 UTC, before the thread, though it reads later than 15:33 as text.
            "2025-09-04T16:00:00+02:00 WARN  [cowork] also earlier, in another offset".to_owned(),
            format!("{after} INFO  [project] unrelated"),
            format!("{after} INFO  [cowork::thread_view] cowork: recovered 1 tool call"),
            format!("{after} ERROR [gpui] something broke"),
            "  a continuation of that error".to_owned(),
            format!("{after} DEBUG [editor] noise"),
            "  a continuation of the noise".to_owned(),
            // 16:00 UTC, after the thread.
            "2025-09-04T18:00:00+02:00 WARN  [http] slow".to_owned(),
        ]
        .join("\n");

        let (kept, matching) = log_excerpt(&log, Some(CREATED_AT));

        assert_eq!(
            kept,
            vec![
                format!("{after} INFO  [cowork::thread_view] cowork: recovered 1 tool call"),
                format!("{after} ERROR [gpui] something broke"),
                "  a continuation of that error".to_owned(),
                "2025-09-04T18:00:00+02:00 WARN  [http] slow".to_owned(),
            ]
        );
        assert_eq!(matching, 4);
    }

    #[test]
    fn the_log_excerpt_keeps_only_the_newest_lines() {
        let at = format_time(CREATED_AT, UtcOffset::UTC);
        let log = (0..MAX_LOG_LINES + 20)
            .map(|index| format!("{at} INFO  [cowork] line {index}"))
            .collect::<Vec<_>>()
            .join("\n");

        let (kept, matching) = log_excerpt(&log, None);

        assert_eq!(matching, MAX_LOG_LINES + 20);
        assert_eq!(kept.len(), MAX_LOG_LINES);
        assert_eq!(kept.first().copied(), Some(format!("{at} INFO  [cowork] line 20").as_str()));
    }

    #[test]
    fn an_unreadable_wu_log_is_reported_rather_than_left_out() {
        let document = session_log(&SessionLogInput {
            thread: eventful_session(),
            environment: environment(),
            wu_log: WuLog {
                path: "/logs/Anna.log".to_owned(),
                contents: Err("The system cannot find the file specified.".to_owned()),
            },
        });

        assert!(document.contains("Anna's log file could not be read"));
        assert!(document.contains("The system cannot find the file specified."));
    }

    fn quiet_thread(id: &str, title: &str) -> ThreadLog {
        let mut assistant = Message::assistant("Done.");
        assistant.tool_calls = vec![tool_call("call-list", "list", r#"{"path":"."}"#)];
        ThreadLog::stored(thread(
            id,
            title,
            vec![
                Message::user("List the files"),
                assistant,
                Message::tool_results(vec![tool_result("call-list", "src/\nCargo.toml")]),
            ],
        ))
    }

    fn all_threads_document(threads: Vec<ExportedThread>) -> String {
        all_threads_log(&AllThreadsLogInput {
            scope: "the threads of `parser`".to_owned(),
            threads,
            environment: environment(),
            wu_log: wu_log(""),
        })
    }

    fn contents_lines(document: &str) -> Vec<&str> {
        document
            .lines()
            .filter(|line| {
                line.split_once(". [")
                    .is_some_and(|(number, _)| number.parse::<usize>().is_ok())
            })
            .collect()
    }

    #[test]
    fn a_log_of_every_thread_has_totals_contents_and_each_thread() {
        let document = all_threads_document(vec![
            ExportedThread::Loaded(eventful_session()),
            ExportedThread::Loaded(quiet_thread("thread-2", "List the files")),
        ]);

        assert_lines_in_order(
            &document,
            &[
                "# Anna log: all threads",
                "## Export",
                "### Totals across threads",
                "## Contents",
                "## Thread 1: Fix the parser",
                "### Session",
                "### Transcript",
                "## Thread 2: List the files",
                "## Anna log excerpt",
            ],
        );
        assert!(line_position(&document, "- Threads: 2 (2 loaded, 0 could not be loaded)").is_some());
        assert!(
            line_position(&document, "- Tool calls: 5 (edit: 1, list: 1, read: 2, shell: 1)")
                .is_some(),
            "the totals add up both threads:\n{document}"
        );

        let contents = contents_lines(&document);
        assert_eq!(contents.len(), 2, "one line per thread: {contents:#?}");
        assert!(contents[0].starts_with("1. [Fix the parser](#thread-1)"));
        assert!(contents[0].ends_with("1 tool errors, 1 failed turns, an error on screen"));
        assert!(contents[1].starts_with("2. [List the files](#thread-2)"));
        assert_eq!(
            document.matches("### Session").count(),
            2,
            "each thread has the same sections as its own export"
        );
    }

    #[test]
    fn a_thread_that_could_not_be_loaded_does_not_stop_the_export() {
        let document = all_threads_document(vec![
            ExportedThread::Failed {
                metadata: metadata("thread-broken", "Broken"),
                error: "parsing a cowork thread: expected value at line 1 column 1".to_owned(),
            },
            ExportedThread::Loaded(quiet_thread("thread-2", "List the files")),
        ]);

        assert!(line_position(&document, "- Threads: 2 (1 loaded, 1 could not be loaded)").is_some());
        assert_lines_in_order(
            &document,
            &[
                "## Thread 1: Broken",
                "### Could not be loaded",
                "parsing a cowork thread: expected value at line 1 column 1",
                "## Thread 2: List the files",
                "### Transcript",
            ],
        );
        let contents = contents_lines(&document);
        assert_eq!(contents.len(), 2);
        assert!(contents[0].ends_with("could not be loaded"));
    }

    #[test]
    fn a_suggested_file_name_is_readable_and_safe() {
        assert_eq!(
            suggested_file_name("anna-session", "Fix: the parser / ação?", CREATED_AT, UtcOffset::UTC),
            "anna-session-fix-the-parser-ação-2025-09-04-1533.md"
        );
        assert_eq!(file_name_component("???"), "untitled");
    }

    #[test]
    fn an_attachment_size_is_read_from_its_encoding() {
        assert_eq!(decoded_size("aGVsbG8="), 5);
        assert_eq!(decoded_size("aGVsbG8h"), 6);
        assert_eq!(decoded_size(""), 0);
    }
}
