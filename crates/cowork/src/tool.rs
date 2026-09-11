//! What the agent can do, and the registry that describes it to a model.
//!
//! A tool is deliberately not a shell utility reimplemented in Rust. Each one is routed through
//! machinery the editor already has — `Project` for paths and buffers, the worktree snapshot for
//! listings, the language servers for diagnostics — so the agent sees the same state the user does,
//! including unsaved edits, and so remote projects work without a second code path.

use anyhow::{Context as _, Result, anyhow};
use gpui::{App, AppContext as _, Entity, Task, WeakEntity};
use project::Project;
use serde_json::{Value, json};
use std::sync::Arc;
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
}

/// The result of one tool call.
///
/// `content` is what the model sees. `summary` is what the transcript shows before the card is
/// expanded, because a model-facing payload is usually too long to read at a glance.
#[derive(Clone, Debug)]
pub struct ToolOutput {
    pub content: String,
    pub summary: String,
}

impl ToolOutput {
    pub fn new(content: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            summary: summary.into(),
        }
    }
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

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// The tools available to a thread today.
    ///
    /// Read-only for now: nothing here can change a file or run a command, so no permission broker
    /// is needed yet. Anything that mutates the user's project or shell waits for one.
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
                parameters: tool.parameters(),
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
    Ok(ToolOutput::new(entries.join("
"), summary))
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

    #[test]
    fn missing_arguments_are_reported_rather_than_defaulted() {
        let error = string_argument(&json!({}), "path")
            .expect_err("a missing argument should not silently become empty");

        assert!(error.to_string().contains("path"), "got: {error}");
    }
}
