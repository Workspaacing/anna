use crate::{
    Cancel, SelectModel, Submit,
    catalog::ModelRef,
    cowork_settings::CoworkSettings,
    model_selector::ModelSelector,
    provider::{
        self, CompletionEvent, CompletionRequest, Message, Role, StopReason, ToolCall, ToolResult,
    },
    permission::{Decision, PermissionBroker, PermissionEvent},
    thread::{CoworkStore, Thread, ThreadId},
    tool::{ToolContext, ToolKind, ToolRegistry},
};
use anyhow::{Context as _, Result, anyhow};
use editor::Editor;
use futures::StreamExt as _;
use gpui::{
    AnyElement, App, Entity, EventEmitter, FocusHandle, Focusable, ScrollHandle, SharedString, Task,
    WeakEntity, relative,
};
use language::LanguageRegistry;
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use project::Project;
use settings::Settings as _;
use std::sync::Arc;
use gpui::StyledText;
use ui::{Button, ButtonStyle, CopyButton, Divider, Tooltip, prelude::*};
use util::ResultExt as _;
use workspace::{
    Workspace,
    item::{Item, ItemEvent},
};

pub enum CoworkThreadEvent {
    TitleChanged,
}

struct MessageView {
    role: Role,
    text: String,
    /// What the model worked through before answering, when it reports any.
    reasoning: String,
    /// The same, parsed. Rebuilt as it streams, the way the answer is.
    reasoning_rendered: Option<Entity<Markdown>>,
    tool_calls: Vec<ToolCall>,
    tool_results: Vec<ToolResult>,
    /// Assistant prose is rendered as markdown. The entity is built when the message is created or
    /// extended, never during `render`, because updating an entity while rendering panics.
    rendered: Option<Entity<Markdown>>,
}

pub struct CoworkThreadView {
    thread: Thread,
    messages: Vec<MessageView>,
    input: Entity<Editor>,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    language_registry: Arc<LanguageRegistry>,
    store: Entity<CoworkStore>,
    workspace: WeakEntity<Workspace>,
    project: Entity<Project>,
    /// Taken at construction: the toggle writes to `settings.json`, and reading the workspace for
    /// it later would risk the "already being updated" panic the panel hit.
    fs: Arc<dyn fs::Fs>,
    tools: ToolRegistry,
    /// `(message index, result index)` for each diff the user has opened.
    expanded_diffs: collections::HashSet<(usize, usize)>,
    /// Languages already resolved for diffs, keyed by file extension.
    ///
    /// Loading one is asynchronous, so the first frame of a diff is unhighlighted and repaints.
    /// `None` records a language that was looked for and not found, so it is not looked for again.
    diff_languages: collections::HashMap<SharedString, Option<Arc<language::Language>>>,
    /// Keeps the JavaScript toolchain's servers running while this thread is open.
    ///
    /// Dropping it unregisters the buffer that started them, so it is held rather than discarded.
    _warm_toolchain: Option<project::lsp_store::OpenLspBufferHandle>,
    _warm_up: Task<()>,
    permissions: Entity<PermissionBroker>,
    error: Option<SharedString>,
    completion: Option<Task<()>>,
    _permissions: gpui::Subscription,
}

impl CoworkThreadView {
    pub fn new(
        thread: Thread,
        store: Entity<CoworkStore>,
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        fs: Arc<dyn fs::Fs>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let language_registry = project.read(cx).languages().clone();

        let input = cx.new(|cx| {
            let mut editor = Editor::auto_height(1, 12, window, cx);
            editor.set_placeholder_text(
                "Ask anything. Enter to send, shift-enter for a newline.",
                window,
                cx,
            );
            editor
        });

        let messages = thread
            .messages
            .iter()
            .map(|message| MessageView {
                role: message.role,
                text: message.text.clone(),
                reasoning: String::new(),
                reasoning_rendered: None,
                tool_calls: message.tool_calls.clone(),
                tool_results: message.tool_results.clone(),
                rendered: (message.role == Role::Assistant)
                    .then(|| render_markdown(&message.text, language_registry.clone(), cx)),
            })
            .collect();

        let permissions = cx.new(|_| PermissionBroker::new());
        // A question the agent is waiting on has to reach the screen, and it is the broker that
        // knows when one arrives.
        let permissions_subscription =
            cx.subscribe(&permissions, |_, _, _: &PermissionEvent, cx| cx.notify());

        let mut view = Self {
            thread,
            messages,
            input,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            language_registry,
            store,
            workspace,
            project,
            fs,
            tools: ToolRegistry::default_tools(),
            expanded_diffs: collections::HashSet::default(),
            diff_languages: collections::HashMap::default(),
            _warm_toolchain: None,
            _warm_up: Task::ready(()),
            permissions,
            error: None,
            completion: None,
            _permissions: permissions_subscription,
        };

        // Once the view exists, because the warm-up stores its handle back onto it.
        view.warm_up_toolchain(cx);
        view
    }

    pub fn thread_id(&self) -> &ThreadId {
        &self.thread.metadata.id
    }

    fn is_streaming(&self) -> bool {
        self.completion.is_some()
    }

    fn submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_streaming() {
            return;
        }

        let prompt = self.input.read(cx).text(cx).trim().to_owned();
        if prompt.is_empty() {
            return;
        }

        self.input.update(cx, |editor, cx| editor.clear(window, cx));
        self.error = None;
        self.push_message(Role::User, prompt, cx);
        self.start_completion(cx);
        cx.notify();
    }

    fn cancel(&mut self, _: &Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        // An outstanding question belongs to the turn being interrupted, so it goes with it rather
        // than being left on screen asking about work nobody is waiting for any more.
        self.permissions
            .update(cx, |permissions, cx| permissions.cancel(cx));

        // Dropping the task cancels the request; whatever streamed so far is kept.
        if self.completion.take().is_some() {
            self.persist(cx);
            cx.notify();
        }
    }

    fn answer_permission(&mut self, decision: Decision, cx: &mut Context<Self>) {
        self.permissions
            .update(cx, |permissions, cx| permissions.resolve(decision, cx));
    }

    /// What the agent is doing right now, while it is still doing it.
    ///
    /// Only shown when nothing else already says so: a running tool has its own card describing
    /// itself, and repeating "Executing tools…" underneath would be noise. What is left is the
    /// gap before the first token arrives, which is otherwise a blank screen.
    fn render_status(&self, is_streaming: bool, _cx: &Context<Self>) -> Option<AnyElement> {
        if !is_streaming {
            return None;
        }

        let last = self.messages.last()?;
        let waiting_on_a_tool = last.role == Role::Assistant && !last.tool_calls.is_empty();
        let has_text = last.role == Role::Assistant && !last.text.is_empty();
        if waiting_on_a_tool || has_text {
            return None;
        }

        Some(
            h_flex()
                .w_full()
                .px_4()
                .py_1p5()
                .gap_1p5()
                .child(
                    Icon::new(IconName::Sparkle)
                        .size(IconSize::XSmall)
                        .color(Color::Accent),
                )
                .child(
                    Label::new("Thinking…")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
                .into_any_element(),
        )
    }

    /// The card that asks before the agent runs a command.
    ///
    /// Deliberately shown between the transcript and the composer rather than as a modal: the user
    /// needs the conversation above it to judge the request, and a dialog would hide exactly that.
    fn render_permission(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let request = self.permissions.read(cx).pending()?.clone();
        let colors = cx.theme().colors();

        Some(
            v_flex()
                .w_full()
                .px_4()
                .py_3()
                .gap_2()
                .bg(colors.element_background)
                .border_t_1()
                .border_color(colors.border_variant)
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Icon::new(IconName::Warning)
                                .size(IconSize::Small)
                                .color(Color::Warning),
                        )
                        .child(Label::new(request.title.clone()).size(LabelSize::Small)),
                )
                .child(
                    div()
                        .id("cowork-permission-detail")
                        .w_full()
                        .min_w_0()
                        .max_h(px(120.))
                        .overflow_y_scroll()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .bg(colors.editor_background)
                        .font_buffer(cx)
                        .text_ui_sm(cx)
                        .child(request.detail.clone()),
                )
                .child(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_1p5()
                        // Declining comes first, and is the plain button: approving is the choice
                        // that cannot be taken back, so it should not be the one hit by reflex.
                        .child(
                            Button::new("cowork-permission-reject", "Don't run")
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.answer_permission(Decision::Reject, cx)
                                })),
                        )
                        .child(
                            Button::new(
                                "cowork-permission-always",
                                format!("Always allow {}", request.scope),
                            )
                            .tooltip(Tooltip::text(
                                "Stop asking about this program until Wu restarts",
                            ))
                            .on_click(cx.listener(|this, _, _window, cx| {
                                this.answer_permission(Decision::Always, cx)
                            })),
                        )
                        .child(
                            Button::new("cowork-permission-once", "Run once")
                                .style(ButtonStyle::Tinted(ui::TintColor::Accent))
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.answer_permission(Decision::Once, cx)
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    fn select_model(&mut self, _: &SelectModel, window: &mut Window, cx: &mut Context<Self>) {
        self.open_model_selector(window, cx);
    }

    /// Starts the JavaScript toolchain before the agent needs it.
    ///
    /// Biome, ESLint and the TypeScript server are what make Cowork's formatting and checks work
    /// on a JavaScript project, and they install themselves on first use. Left to the agent's
    /// first edit, that means its opening move waits minutes on an npm install nobody asked for —
    /// a poor showing for the feature this is meant to be good at.
    ///
    /// So a file is opened for them the moment a thread does, which is the same thing that happens
    /// when a person opens one in the editor: the real installer runs, through the real adapters,
    /// with no duplicated path logic to drift.
    ///
    /// Nothing at all happens in a project with no JavaScript in it. A Rust-only user should not
    /// spend 300 MB and several minutes on a toolchain they will never run — which is why this
    /// looks for a file rather than installing unconditionally.
    fn warm_up_toolchain(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.first_javascript_file(cx) else {
            return;
        };

        let project = self.project.clone();
        self._warm_up = cx.spawn(async move |this, cx| {
            let opened = project.update(cx, |project, cx| project.open_buffer(path, cx));
            let Ok(buffer) = opened.await else {
                return;
            };

            let handle = project.update(cx, |project, cx| {
                project.register_buffer_with_language_servers(&buffer, cx)
            });
            this.update(cx, |this, _| this._warm_toolchain = Some(handle))
                .log_err();
        });
    }

    /// Any file in the project that the JavaScript toolchain would act on.
    ///
    /// The first one found is enough: the servers are started per worktree, not per file.
    fn first_javascript_file(&self, cx: &App) -> Option<project::ProjectPath> {
        const EXTENSIONS: [&str; 8] = ["ts", "tsx", "js", "jsx", "mjs", "cjs", "json", "css"];

        let project = self.project.read(cx);
        for worktree in project.visible_worktrees(cx) {
            let worktree = worktree.read(cx);
            let found = worktree.files(false, 0).find(|entry| {
                entry
                    .path
                    .extension()
                    .is_some_and(|extension| EXTENSIONS.contains(&extension))
            });

            if let Some(entry) = found {
                return Some(project::ProjectPath {
                    worktree_id: worktree.id(),
                    path: entry.path.clone(),
                });
            }
        }
        None
    }

    /// Asks for the languages the open diffs need, before the render borrows `self`.
    ///
    /// Loading one is asynchronous and rendering is not, so a diff appears immediately and gains
    /// its colours a frame later. An extension recorded as pending is not asked for twice, which
    /// matters because this runs on every frame.
    fn request_diff_languages(&mut self, cx: &mut Context<Self>) {
        let wanted = self
            .messages
            .iter()
            .flat_map(|message| message.tool_results.iter())
            .filter(|result| !result.diff.is_empty())
            .filter_map(|result| extension_of(&result.path))
            .filter(|extension| !self.diff_languages.contains_key(extension))
            .collect::<Vec<_>>();

        for extension in wanted {
            self.diff_languages.insert(extension.clone(), None);

            let registry = self.language_registry.clone();
            let file_name = format!("a.{extension}");
            cx.spawn(async move |this, cx| {
                let loaded = registry
                    .load_language_for_file_path(std::path::Path::new(&file_name))
                    .await
                    .ok();
                this.update(cx, |this, cx| {
                    this.diff_languages.insert(extension, loaded);
                    cx.notify();
                })
                .log_err();
            })
            .detach();
        }
    }

    /// Adds to what the model is working through, which is not part of its answer.
    fn extend_reasoning(&mut self, chunk: &str, cx: &mut Context<Self>) {
        let registry = self.language_registry.clone();
        let Some(message) = self.messages.last_mut() else {
            return;
        };
        message.reasoning.push_str(chunk);

        match message.reasoning_rendered.clone() {
            Some(markdown) => markdown.update(cx, |markdown, cx| markdown.append(chunk, cx)),
            None => {
                let source = message.reasoning.clone();
                let markdown = render_markdown(&source, registry, cx);
                if let Some(message) = self.messages.last_mut() {
                    message.reasoning_rendered = Some(markdown);
                }
            }
        }

        self.scroll_handle.scroll_to_bottom();
        cx.notify();
    }

    /// Records what the provider said the exchange cost.
    ///
    /// Input is the whole conversation as the provider saw it, so it replaces rather than adds to
    /// the previous figure; output is what was just produced and rides along with it.
    fn record_usage(&mut self, input: u64, output: u64, cx: &mut Context<Self>) {
        if input == 0 && output == 0 {
            return;
        }
        self.thread.metadata.context_tokens = Some(input.saturating_add(output));
        cx.notify();
    }

    /// How full the context is, when both halves of the answer are known.
    ///
    /// `None` until a provider has reported usage: an estimate would be wrong in a way the user
    /// could not see, and a meter that lies is worse than no meter.
    fn context_usage(&self, cx: &App) -> Option<(u64, u64)> {
        let used = self.thread.metadata.context_tokens?;
        let (_, model) = self
            .store
            .read(cx)
            .catalog()
            .model(&self.thread.metadata.model)?;
        let limit = model.limit.and_then(|limit| limit.context)?;
        (limit > 0).then_some((used, limit))
    }

    /// Changes the model this conversation runs on.
    ///
    /// Public because the panel's own picker sets it too: the two must never name different
    /// models for the same open thread.
    pub fn set_model(&mut self, model: ModelRef, cx: &mut Context<Self>) {
        if self.thread.metadata.model == model {
            return;
        }
        self.thread.metadata.model = model;
        self.persist(cx);
        cx.notify();
    }

    /// The folder's own name, which is what identifies it to the user — the full path is too long
    /// for a header and its last component is what they called the project.
    fn working_folder_label(&self) -> SharedString {
        self.thread
            .metadata
            .project
            .as_deref()
            .and_then(|path| {
                std::path::Path::new(path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .map(SharedString::from)
            .unwrap_or_else(|| SharedString::new_static("No folder"))
    }

    /// Sets which of the project's folders this thread works in.
    ///
    /// With several folders it asks. With one it simply adopts it, which is not a no-op: a thread
    /// created before the folder was recorded — or in another window — shows "No folder" and needs
    /// exactly this to repair it. Doing nothing at all was the bug: the button offered an action
    /// and then declined to perform it.
    fn choose_working_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let folders = project_folders(&self.project, cx);

        match folders.len() {
            0 => {}
            1 => {
                let (_, path) = &folders[0];
                self.set_working_folder(path.clone(), cx);
            }
            _ => {
                let labels = folders
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .collect::<Vec<_>>();
                let answer = window.prompt(
                    gpui::PromptLevel::Info,
                    "Which folder should this thread work in?",
                    Some("Commands run here, and paths the agent gives are resolved from here."),
                    &labels,
                    cx,
                );

                cx.spawn(async move |this, cx| {
                    let Ok(chosen) = answer.await else {
                        return;
                    };
                    let Some((_, path)) = folders.get(chosen) else {
                        return;
                    };
                    let path = path.clone();
                    this.update(cx, |this, cx| this.set_working_folder(path, cx))
                        .log_err();
                })
                .detach();
            }
        }
    }

    fn set_working_folder(&mut self, folder: String, cx: &mut Context<Self>) {
        if self.thread.metadata.project.as_deref() == Some(folder.as_str()) {
            return;
        }
        self.thread.metadata.project = Some(folder);
        self.persist(cx);
        cx.notify();
    }

    /// Whether there is anything for the folder button to do.
    ///
    /// A button that cannot change anything is disabled rather than silently inert, and its
    /// tooltip says which case it is in.
    fn can_change_working_folder(&self, cx: &App) -> bool {
        let folders = project_folders(&self.project, cx);
        match folders.len() {
            0 => false,
            1 => self.thread.metadata.project.as_deref() != Some(folders[0].1.as_str()),
            _ => true,
        }
    }

    fn open_model_selector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        let this = cx.entity().downgrade();
        let store = self.store.downgrade();
        let selected = Some(self.thread.metadata.model.clone());
        let on_confirm: Arc<dyn Fn(ModelRef, &mut Window, &mut App) + Send + Sync> =
            Arc::new(move |model, _window, cx| {
                // Also what the next new thread will start on: choosing a model here is the
                // clearest statement of preference the user can make.
                store
                    .update(cx, |store, cx| store.remember_model(model.clone(), cx))
                    .log_err();
                this.update(cx, |this, cx| this.set_model(model, cx)).log_err();
            });

        workspace.update(cx, |workspace, cx| {
            workspace.toggle_modal(window, cx, move |window, cx| {
                ModelSelector::new(selected, on_confirm, window, cx)
            });
        });
    }

    fn push_message(&mut self, role: Role, text: String, cx: &mut Context<Self>) {
        let rendered = (role == Role::Assistant)
            .then(|| render_markdown(&text, self.language_registry.clone(), cx));

        self.thread.messages.push(match role {
            Role::User => Message::user(text.clone()),
            Role::Assistant => Message::assistant(text.clone()),
            Role::Tool => Message::tool_results(Vec::new()),
        });
        self.messages.push(MessageView {
            role,
            text,
            reasoning: String::new(),
            reasoning_rendered: None,
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            rendered,
        });
        self.scroll_handle.scroll_to_bottom();
    }

    fn extend_last_message(&mut self, chunk: &str, cx: &mut Context<Self>) {
        let Some(message) = self.messages.last_mut() else {
            return;
        };

        message.text.push_str(chunk);
        let rendered = message.rendered.clone();

        if let Some(stored) = self.thread.messages.last_mut() {
            stored.text.push_str(chunk);
        }

        if let Some(rendered) = rendered {
            rendered.update(cx, |markdown, cx| markdown.append(chunk, cx));
        }

        self.scroll_handle.scroll_to_bottom();
        cx.notify();
    }

    /// How many times the same call may repeat before the turn is stopped.
    ///
    /// There is deliberately no limit on *steps*. A turn runs until the model says it is done or
    /// the user stops it, because any number chosen here is a guess that will one day cut real
    /// work in half — reading four files, writing three, running a build and reacting to it is an
    /// ordinary task that spends a dozen steps before it has started. And the loop is visible: a
    /// reader watching the transcript sees repetition at once, and Stop is right there.
    ///
    /// What is worth catching is not length but the absence of progress. A model asking for the
    /// identical thing over and over is not working, and unlike a long turn it will never end on
    /// its own. Two identical calls can be a retry; three is a circle.
    const MAX_IDENTICAL_CALLS: usize = 3;

    fn start_completion(&mut self, cx: &mut Context<Self>) {
        let http_client = cx.http_client();
        let tools = self.tools.clone();
        let project = self.project.clone();
        let workspace = self.workspace.clone();
        let permissions = self.permissions.clone();
        let working_folder = self
            .thread
            .metadata
            .project
            .as_ref()
            .map(std::path::PathBuf::from);

        self.completion = Some(cx.spawn(async move |this, cx| {
            let mut recent: Vec<String> = Vec::new();
            loop {
                let request = match this.update(cx, |this, cx| this.build_request(cx)) {
                    Ok(Ok(request)) => request,
                    Ok(Err(error)) => {
                        this.update(cx, |this, cx| this.fail(error, cx)).log_err();
                        return;
                    }
                    Err(_) => return,
                };

                if this
                    .update(cx, |this, cx| {
                        this.push_message(Role::Assistant, String::new(), cx)
                    })
                    .is_err()
                {
                    return;
                }

                let outcome = Self::stream_one_step(&this, http_client.clone(), request, cx).await;
                let (calls, stopped_for_tools) = match outcome {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        this.update(cx, |this, cx| this.finish_with_error(error, cx))
                            .log_err();
                        return;
                    }
                };

                if !stopped_for_tools || calls.is_empty() {
                    this.update(cx, |this, cx| this.finish(cx)).log_err();
                    return;
                }

                // Record the calls on the assistant message before running them, so the
                // conversation stays well-formed even if a tool panics or the turn is cancelled.
                if this
                    .update(cx, |this, cx| {
                        this.attach_tool_calls(calls.clone(), cx);
                    })
                    .is_err()
                {
                    return;
                }

                let context = || ToolContext {
                    project: project.clone(),
                    workspace: workspace.clone(),
                    permissions: permissions.clone(),
                    working_folder: working_folder.clone(),
                };
                if let Some(repeated) = Self::repeating(&mut recent, &calls) {
                    this.update(cx, |this, cx| {
                        this.fail(
                            anyhow!(
                                "Stopped: the model asked to {repeated} {} times in a row without \
                                 using the answer. Whatever it finished is kept — ask again, or \
                                 tell it what to do differently.",
                                Self::MAX_IDENTICAL_CALLS
                            ),
                            cx,
                        )
                    })
                    .log_err();
                    return;
                }

                let results = Self::run_tools(&tools, &calls, context, cx).await;

                if this
                    .update(cx, |this, cx| this.push_tool_results(results, cx))
                    .is_err()
                {
                    return;
                }

            }
        }));
    }

    /// Whether the model is asking for the same thing again instead of making progress.
    ///
    /// Compares the whole call — name and arguments — because the same tool on a different file is
    /// progress, and the same tool on the same file is not. Anything different resets the count: a
    /// model that reads a file, writes it, then reads it again is working.
    fn repeating(recent: &mut Vec<String>, calls: &[ToolCall]) -> Option<String> {
        let signature = calls
            .iter()
            .map(|call| format!("{}({})", call.name, call.arguments))
            .collect::<Vec<_>>()
            .join(", ");

        if recent.last() != Some(&signature) {
            recent.clear();
        }
        recent.push(signature);

        (recent.len() >= Self::MAX_IDENTICAL_CALLS).then(|| {
            calls
                .first()
                .map(|call| {
                    let subject = summarize_arguments(&call.arguments);
                    if subject.is_empty() {
                        format!("run `{}`", call.name)
                    } else {
                        format!("run `{}` on {subject}", call.name)
                    }
                })
                .unwrap_or_else(|| "do the same thing".to_owned())
        })
    }

    /// Streams one assistant message, returning the tool calls it asked for and whether it stopped
    /// in order to make them.
    async fn stream_one_step(
        this: &WeakEntity<Self>,
        http_client: Arc<dyn http_client::HttpClient>,
        request: CompletionRequest,
        cx: &mut gpui::AsyncApp,
    ) -> Result<(Vec<ToolCall>, bool)> {
        let mut stream = provider::stream_completion(http_client, request).await?;
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut stopped_for_tools = false;

        while let Some(event) = stream.next().await {
            match event? {
                CompletionEvent::Text(chunk) => {
                    if this
                        .update(cx, |this, cx| this.extend_last_message(&chunk, cx))
                        .is_err()
                    {
                        return Ok((Vec::new(), false));
                    }
                }
                CompletionEvent::ToolCallStart { id, name } => calls.push(ToolCall {
                    id,
                    name,
                    arguments: String::new(),
                }),
                CompletionEvent::ToolCallDelta { id, arguments } => {
                    // Anthropic scopes argument deltas to the open content block rather than
                    // naming the call, and sends an empty id; those belong to the most recent one.
                    let call = if id.is_empty() {
                        calls.last_mut()
                    } else {
                        calls.iter_mut().find(|call| call.id == id)
                    };
                    if let Some(call) = call {
                        call.arguments.push_str(&arguments);
                    }
                }
                CompletionEvent::Reasoning(chunk) => {
                    if this
                        .update(cx, |this, cx| this.extend_reasoning(&chunk, cx))
                        .is_err()
                    {
                        return Ok((Vec::new(), false));
                    }
                }
                CompletionEvent::Usage { input, output } => {
                    if this
                        .update(cx, |this, cx| this.record_usage(input, output, cx))
                        .is_err()
                    {
                        return Ok((Vec::new(), false));
                    }
                }
                CompletionEvent::Stop(reason) => {
                    // Recorded rather than breaking on. OpenAI-compatible providers send the
                    // usage chunk *after* the one carrying `finish_reason`, so stopping here
                    // meant the token counts were never read and the context meter stayed
                    // empty. The stream ends on its own at `[DONE]`.
                    stopped_for_tools = reason == StopReason::ToolUse;
                }
            }
        }

        Ok((calls, stopped_for_tools))
    }

    /// Runs the calls a model asked for, concurrently where that is safe.
    ///
    /// A model that wants four files reads them one after another otherwise, paying a full round
    /// trip of latency for each, when nothing about them depends on the others.
    ///
    /// Only reads and searches go in parallel, and the reason is not caution for its own sake:
    ///
    /// - Two edits can touch the same file. Running them together means the second reads a buffer
    ///   the first has already changed, and whichever finishes last wins — silently.
    /// - Commands have order-dependent effects. `npm install` then `npm test` is not the same as
    ///   both at once.
    /// - The permission broker holds one question at a time, and a second request displaces the
    ///   first, which is refused. Two commands asking together would mean one denied for no reason
    ///   the user could see.
    ///
    /// Results come back in the order the model asked for them, whatever order they finished in,
    /// because a tool result is matched to its call by position in some formats.
    async fn run_tools(
        tools: &ToolRegistry,
        calls: &[ToolCall],
        context: impl Fn() -> ToolContext,
        cx: &mut gpui::AsyncApp,
    ) -> Vec<ToolResult> {
        let concurrent = |call: &ToolCall| {
            tools
                .get(&call.name)
                .is_some_and(|tool| matches!(tool.kind(), ToolKind::Read | ToolKind::Search))
        };

        let mut results: Vec<Option<ToolResult>> = (0..calls.len()).map(|_| None).collect();

        // Everything that only reads, at once.
        let reads = calls
            .iter()
            .enumerate()
            .filter(|(_, call)| concurrent(call))
            .map(|(index, call)| {
                let context = context();
                // `AsyncApp` is a handle, so each concurrent read gets its own rather than
                // sharing one mutable borrow.
                let mut cx = cx.clone();
                async move { (index, Self::run_tool(tools, call, context, &mut cx).await) }
            })
            .collect::<Vec<_>>();

        for (index, result) in futures::future::join_all(reads).await {
            results[index] = Some(result);
        }

        // Everything else in the order it was asked for.
        for (index, call) in calls.iter().enumerate() {
            if results[index].is_some() {
                continue;
            }
            results[index] = Some(Self::run_tool(tools, call, context(), cx).await);
        }

        results.into_iter().flatten().collect()
    }

    /// Runs one tool call, turning every failure into a result the model can read and correct.
    ///
    /// A tool that errors must not end the turn: the model asked for something it could not have,
    /// and telling it so is more useful than a dead conversation.
    async fn run_tool(
        tools: &ToolRegistry,
        call: &ToolCall,
        context: ToolContext,
        cx: &mut gpui::AsyncApp,
    ) -> ToolResult {
        let error = |message: String| ToolResult {
            call_id: call.id.clone(),
            content: message,
            is_error: true,
            path: String::new(),
            diff: String::new(),
        };

        let Some(tool) = tools.get(&call.name) else {
            return error(format!("there is no tool named `{}`", call.name));
        };

        let arguments = if call.arguments.trim().is_empty() {
            serde_json::json!({})
        } else {
            match serde_json::from_str(&call.arguments) {
                Ok(arguments) => arguments,
                Err(parse_error) => {
                    return error(format!("the arguments were not valid JSON: {parse_error}"));
                }
            }
        };

        let run = cx.update(|cx| tool.run(arguments, context, cx));
        match run.await {
            Ok(output) => ToolResult {
                call_id: call.id.clone(),
                content: output.content,
                is_error: false,
                path: output.path,
                diff: output.diff,
            },
            Err(failure) => error(format!("{failure:#}")),
        }
    }

    fn attach_tool_calls(&mut self, calls: Vec<ToolCall>, cx: &mut Context<Self>) {
        if let Some(message) = self.thread.messages.last_mut() {
            message.tool_calls = calls.clone();
        }
        if let Some(view) = self.messages.last_mut() {
            view.tool_calls = calls;
        }
        cx.notify();
    }

    fn push_tool_results(&mut self, results: Vec<ToolResult>, cx: &mut Context<Self>) {
        self.thread
            .messages
            .push(Message::tool_results(results.clone()));
        self.messages.push(MessageView {
            role: Role::Tool,
            text: String::new(),
            reasoning: String::new(),
            reasoning_rendered: None,
            rendered: None,
            tool_calls: Vec::new(),
            tool_results: results,
        });
        self.scroll_handle.scroll_to_bottom();
        cx.notify();
    }

    fn build_request(&self, cx: &App) -> Result<CompletionRequest> {
        let model = self.thread.metadata.model.clone();
        let store = self.store.read(cx);
        let (catalog_provider, catalog_model) = store.catalog().model(&model).with_context(|| {
            format!(
                "{} is not in the models.dev catalog. Refresh the catalog from the Cowork panel, \
                 or pick another model.",
                model.qualified()
            )
        })?;

        let api_key = store.api_key(&model.provider_id).ok_or_else(|| {
            anyhow!(
                "{} has no API key. Add one from Settings → Cowork → Providers, or set {} in the                  environment.",
                model.provider_id,
                catalog_provider.primary_env_var().unwrap_or("its API key variable"),
            )
        })?;

        Ok(CompletionRequest {
            provider_id: model.provider_id.clone(),
            provider: catalog_provider.clone(),
            model: catalog_model.clone(),
            model_id: model.model_id.clone(),
            api_key,
            system: None,
            messages: self.thread.messages.clone(),
            tools: self.tools.definitions(),
            // Every model publishes its own ceiling, so asking for less would be leaving the
            // model's capability on the table for no reason.
            max_output_tokens: catalog_model.limit.and_then(|limit| limit.output),
        })
    }

    fn fail(&mut self, error: anyhow::Error, cx: &mut Context<Self>) {
        log::warn!("cowork: completion failed: {error:#}");
        self.error = Some(format!("{error:#}").into());
        self.completion = None;
        cx.notify();
    }

    /// A failure once streaming was under way. An assistant turn that never received a chunk is
    /// dropped so the thread does not keep a blank message, but partial output is preserved.
    fn finish_with_error(&mut self, error: anyhow::Error, cx: &mut Context<Self>) {
        if self
            .messages
            .last()
            .is_some_and(|message| message.role == Role::Assistant && message.text.is_empty())
        {
            self.messages.pop();
            self.thread.messages.pop();
        }
        self.fail(error, cx);
        self.persist(cx);
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        self.completion = None;
        self.persist(cx);
        cx.emit(CoworkThreadEvent::TitleChanged);
        cx.notify();
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
        self.thread.refresh_metadata();
        let thread = self.thread.clone();
        self.store
            .update(cx, |store, cx| store.save_thread(thread, cx));
    }

    fn render_header(&self, is_streaming: bool, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .px_3()
            .py_1p5()
            .gap_2()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().colors().border_variant)
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("cowork-model", self.thread.metadata.model.qualified())
                            .start_icon(Icon::new(IconName::Sparkle).size(IconSize::Small))
                            .label_size(LabelSize::Small)
                            .tooltip(Tooltip::text("Change the model for this thread"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_model_selector(window, cx)
                            })),
                    )
                    // Where the agent runs. Always visible, because "which folder is this editing"
                    // is not something the user should have to infer from the output.
                    .child({
                        let changeable = self.can_change_working_folder(cx);
                        Button::new("cowork-folder", self.working_folder_label())
                            .start_icon(Icon::new(IconName::Folder).size(IconSize::Small))
                            .label_size(LabelSize::Small)
                            .disabled(!changeable)
                            .tooltip(Tooltip::text(if changeable {
                                "Change the folder this thread works in"
                            } else {
                                "The only folder open in this project"
                            }))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.choose_working_folder(window, cx)
                            }))
                    }),
            )
            .child(
                h_flex()
                    .gap_1()
                    .children(self.render_context_meter(cx))
                    .child(self.render_changes_button(cx))
                    .child(self.render_permission_toggle(cx)),
            )
            .when(is_streaming, |this| {
                this.child(
                    Button::new("cowork-stop", "Stop")
                        .start_icon(Icon::new(IconName::Stop).size(IconSize::Small))
                        .label_size(LabelSize::Small)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.cancel(&Cancel, window, cx)),
                        ),
                )
            })
    }

    /// How much of the model's context the conversation is using.
    ///
    /// Only shown once a provider has actually reported usage. Every provider tokenizes
    /// differently, so a locally computed estimate would be wrong in a way the user could not see
    /// — and a meter that lies about how close you are to the limit is worse than no meter.
    fn render_context_meter(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let (used, limit) = self.context_usage(cx)?;
        let fraction = (used as f32 / limit as f32).clamp(0.0, 1.0);
        let percent = (fraction * 100.0).round() as u32;

        // Amber past three quarters, red once the next turn may not fit.
        let colors = cx.theme().colors();
        let status = cx.theme().status();
        let fill = match percent {
            0..=74 => colors.text_accent,
            75..=89 => status.warning,
            _ => status.error,
        };

        Some(
            h_flex()
                .id("cowork-context")
                .gap_1p5()
                .px_1p5()
                .py_0p5()
                .rounded_sm()
                .tooltip(Tooltip::text(format!(
                    "{used} of {limit} tokens used in this conversation"
                )))
                .child(
                    div()
                        .w(px(48.))
                        .h(px(4.))
                        .rounded_full()
                        .bg(colors.element_background)
                        .child(
                            div()
                                .w(relative(fraction))
                                .h_full()
                                .rounded_full()
                                .bg(fill),
                        ),
                )
                .child(
                    Label::new(format!("{} / {}", compact(used), compact(limit)))
                        .size(LabelSize::XSmall)
                        .color(Color::Muted),
                )
                .into_any_element(),
        )
    }

    /// Opens the editor's own diff view, showing every uncommitted change in the project.
    ///
    /// Deliberately not a diff viewer of its own. Wu already has one — a multibuffer with staging,
    /// per-hunk revert and the whole editor behind it — and the agent's edits land in the same
    /// buffers as the user's, so they show up there without anything being tracked twice.
    ///
    /// Dispatched by name rather than by type. `git_ui` is a large crate that the workspace
    /// deliberately keeps out of feature crates — `tooling/xtask` bans five separate edges into it
    /// to stop the build serializing — and an action name costs no dependency at all.
    fn render_changes_button(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        Button::new("cowork-changes", "Changes")
            .start_icon(Icon::new(IconName::Diff).size(IconSize::Small))
            .label_size(LabelSize::Small)
            .style(ButtonStyle::Subtle)
            .tooltip(Tooltip::text(
                "Show every uncommitted change in this project, including the agent's",
            ))
            .on_click(cx.listener(|_, _, window, cx| {
                match cx.build_action("git::Diff", None) {
                    Ok(action) => window.dispatch_action(action, cx),
                    Err(error) => log::warn!("cowork: could not open the diff view: {error}"),
                }
            }))
    }

    /// Everything a turn did with tools, as one table.
    ///
    /// A card each looked like several unrelated events rather than one turn's work, and each
    /// carried a line of the result — for `read` that is the first line of the file, which says
    /// nothing. The outcome is in the tick, what changed is in the `+12 -3`, and neither needs a
    /// sentence of its own.
    fn render_tool_calls(
        &self,
        message_index: usize,
        calls: &[ToolCall],
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();

        v_flex()
            .w_full()
            .min_w_0()
            .rounded_md()
            .border_1()
            .border_color(colors.border_variant)
            .bg(colors.element_background)
            .children(calls.iter().enumerate().map(|(position, call)| {
                self.render_tool_row(message_index, position, call, position > 0, cx)
            }))
            .into_any_element()
    }

    /// One row of that table: what was done, whether it worked, and what it changed.
    fn render_tool_row(
        &self,
        message_index: usize,
        position: usize,
        call: &ToolCall,
        divided: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let colors = cx.theme().colors();
        let result = self
            .messages
            .get(message_index + 1)
            .filter(|next| next.role == Role::Tool)
            .and_then(|next| next.tool_results.get(position));

        let (icon, icon_color) = match result {
            None => (IconName::ArrowCircle, Color::Accent),
            Some(result) if result.is_error => (IconName::XCircle, Color::Error),
            Some(_) => (IconName::Check, Color::Success),
        };

        let diff = result
            .map(|result| result.diff.as_str())
            .filter(|diff| !diff.is_empty());
        let open = self.expanded_diffs.contains(&(message_index, position));
        // Already resolved by `request_diff_languages`, which runs before the render borrows
        // `self` immutably.
        let language = result
            .map(|result| result.path.as_str())
            .and_then(extension_of)
            .and_then(|extension| self.diff_languages.get(&extension).cloned())
            .flatten();

        v_flex()
            .w_full()
            .min_w_0()
            .when(divided, |this| {
                this.border_t_1().border_color(colors.border_variant)
            })
            .child(
                h_flex()
                    .id(("cowork-tool-row", message_index * 64 + position))
                    .w_full()
                    .min_w_0()
                    .px_2p5()
                    .py_1p5()
                    .gap_1p5()
                    .when(diff.is_some(), |this| this.cursor_pointer())
                    .child(Icon::new(icon).size(IconSize::XSmall).color(icon_color))
                    .child(
                        div().flex_1().min_w_0().child(
                            Label::new(describe_call(
                                &call.name,
                                &call.arguments,
                                result.is_some(),
                            ))
                            .size(LabelSize::Small)
                            .truncate_middle(),
                        ),
                    )
                    // An error has no diff to show, so its message goes where the badge would be —
                    // it is the one outcome a tick cannot express.
                    .when_some(
                        result.filter(|result| result.is_error),
                        |this, result| {
                            this.child(
                                Label::new(first_line(&result.content))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Error)
                                    .truncate_middle(),
                            )
                        },
                    )
                    .when_some(diff, |this, diff| {
                        this.child(render_diff_counts(diff, cx)).child(
                            Icon::new(if open {
                                IconName::ChevronDown
                            } else {
                                IconName::ChevronRight
                            })
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        let key = (message_index, position);
                        if !this.expanded_diffs.remove(&key) {
                            this.expanded_diffs.insert(key);
                        }
                        cx.notify();
                    })),
            )
            .when(open, |this| {
                this.when_some(diff, |this, diff| {
                    this.child(div().px_2p5().pb_2().child(render_diff(diff, language.as_ref(), cx)))
                })
            })
            .into_any_element()
    }

    /// The switch that stops the agent asking before it runs commands.
    fn render_permission_toggle(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let approving = CoworkSettings::get_global(cx).auto_approve;

        Button::new("cowork-auto-approve", if approving { "Auto" } else { "Ask" })
            .start_icon(
                Icon::new(if approving {
                    IconName::Warning
                } else {
                    IconName::Lock
                })
                .size(IconSize::Small),
            )
            .label_size(LabelSize::Small)
            .style(if approving {
                ButtonStyle::Tinted(ui::TintColor::Warning)
            } else {
                ButtonStyle::Subtle
            })
            .tooltip(Tooltip::text(if approving {
                "Commands run without asking. Click to require approval again."
            } else {
                "You are asked before each command. Click to approve everything automatically."
            }))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_auto_approve(window, cx)))
    }

    /// Turning approval off asks first; turning it back on does not.
    ///
    /// The asymmetry is the point. Going from "ask me" to "run anything" is the direction that can
    /// cost something irreversible, and it is a setting rather than a per-turn choice, so it stays
    /// off until the user says so in as many words.
    fn toggle_auto_approve(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let approving = CoworkSettings::get_global(cx).auto_approve;
        let fs = self.fs.clone();

        if !approving {
            let answer = window.prompt(
                gpui::PromptLevel::Warning,
                "Let the agent run commands without asking?",
                Some(
                    "It will be able to run any command in this project — including ones that \
                     delete files, push to a remote, or publish — with no further confirmation. \
                     Nothing in the editor can undo those.",
                ),
                &["Allow everything", "Cancel"],
                cx,
            );

            cx.spawn(async move |_, cx| {
                if answer.await.ok() != Some(0) {
                    return;
                }
                cx.update(|cx| write_auto_approve(fs, true, cx));
            })
            .detach();
            return;
        }

        write_auto_approve(fs, false, cx);
    }

    fn render_empty_state(&self) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_1()
            .child(Icon::new(IconName::Sparkle).color(Color::Muted))
            .child(Label::new("Start a conversation").color(Color::Muted))
            .child(
                Label::new(
                    "Cowork reads its model list from models.dev and provider keys from your environment.",
                )
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
    }

    fn render_composer(&self, is_streaming: bool, cx: &Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .child(Divider::horizontal())
            .child(
                h_flex()
                    .w_full()
                    .p_3()
                    .gap_2()
                    .items_end()
                    .bg(cx.theme().colors().panel_background)
                    .child(div().flex_1().child(self.input.clone()))
                    .child(
                        IconButton::new(
                            "cowork-submit",
                            if is_streaming {
                                IconName::Stop
                            } else {
                                IconName::Send
                            },
                        )
                        .icon_size(IconSize::Small)
                        .tooltip(Tooltip::text(if is_streaming {
                            "Stop generating"
                        } else {
                            "Send"
                        }))
                        .on_click(cx.listener(|this, _, window, cx| {
                            if this.is_streaming() {
                                this.cancel(&Cancel, window, cx);
                            } else {
                                this.submit(&Submit, window, cx);
                            }
                        })),
                    ),
            )
    }
}

/// The first non-empty line of a tool's output, for the collapsed row.
fn first_line(content: &str) -> String {
    let line = content.lines().find(|line| !line.trim().is_empty()).unwrap_or("");
    if line.chars().count() > 120 {
        format!("{}…", line.chars().take(120).collect::<String>())
    } else {
        line.to_owned()
    }
}

/// Tool arguments are shown as the values alone: the model already named the tool, and the keys
/// are noise at this size.
/// What the agent is doing, as a sentence.
///
/// `write src/index.ts` is the wire form of a tool call, not a description of anything. It tells
/// the reader to know what `write` means and to infer that the path is its argument. A sentence
/// asks nothing of them, and reads the same whether they are watching it happen or scrolling past
/// it afterwards — which is why the only thing the tense changes is the verb.
fn describe_call(tool: &str, arguments: &str, finished: bool) -> String {
    // The model's own account of the step, when it gave one. It knows why it made the call; a
    // sentence assembled from the tool's name only ever describes the mechanism.
    if let Ok(serde_json::Value::Object(fields)) = serde_json::from_str::<serde_json::Value>(arguments)
        && let Some(intent) = fields
            .get("intent")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|intent| !intent.is_empty())
    {
        return first_line(intent);
    }

    let subject = summarize_arguments(arguments);

    let verb = match (tool, finished) {
        ("read", false) => "Reading",
        ("read", true) => "Read",
        ("list", false) => "Listing",
        ("list", true) => "Listed",
        ("write", false) => "Writing",
        ("write", true) => "Wrote",
        ("edit", false) => "Editing",
        ("edit", true) => "Edited",
        ("shell", false) => "Running",
        ("shell", true) => "Ran",
        // A tool nobody has written a phrase for still reads as a sentence rather than as a
        // fragment of JSON.
        (_, false) => return format!("Running {tool}…"),
        (_, true) => return format!("Ran {tool}"),
    };

    match (subject.is_empty(), finished) {
        (true, false) => format!("{verb}…"),
        (true, true) => verb.to_owned(),
        (false, false) => format!("{verb} {subject}…"),
        (false, true) => format!("{verb} {subject}"),
    }
}

/// What a tool call is *about*, in one line.
///
/// Deliberately a named subset rather than every argument. Joining all of them put the entire
/// contents of a written file into the transcript — the argument that matters for `write` is the
/// path, and the file itself is shown afterwards as a diff.
fn summarize_arguments(arguments: &str) -> String {
    const SUBJECT: [&str; 3] = ["path", "command", "old_text"];

    let Ok(serde_json::Value::Object(fields)) = serde_json::from_str(arguments) else {
        return String::new();
    };

    let subject = SUBJECT.iter().find_map(|name| {
        fields
            .get(*name)
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
    });

    match subject {
        Some(subject) => first_line(subject),
        // An unrecognised tool still says something rather than nothing, but never more than the
        // first field and never more than one line.
        None => fields
            .values()
            .find_map(serde_json::Value::as_str)
            .map(first_line)
            .unwrap_or_default(),
    }
}

fn render_markdown(
    source: &str,
    language_registry: Arc<LanguageRegistry>,
    cx: &mut Context<CoworkThreadView>,
) -> Entity<Markdown> {
    let source = SharedString::from(source.to_owned());
    cx.new(|cx| Markdown::new(source, Some(language_registry), None, cx))
}

impl Focusable for CoworkThreadView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<CoworkThreadEvent> for CoworkThreadView {}

impl Item for CoworkThreadView {
    type Event = CoworkThreadEvent;

    fn tab_icon(&self, _window: &Window, _cx: &App) -> Option<Icon> {
        Some(Icon::new(IconName::Sparkle))
    }

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        self.thread.metadata.title.clone().into()
    }

    fn tab_tooltip_text(&self, _cx: &App) -> Option<SharedString> {
        Some(self.thread.metadata.model.qualified().into())
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(ItemEvent)) {
        match event {
            CoworkThreadEvent::TitleChanged => f(ItemEvent::UpdateTab),
        }
    }

    fn show_toolbar(&self) -> bool {
        false
    }
}

/// How many diff lines are rendered before the rest is summarised away.
const MAX_DIFF_LINES: usize = 400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DiffKind {
    Added,
    Removed,
    Context,
    /// A `@@` hunk header — where in the file the next lines are.
    Header,
}

/// One line of a diff, with the line numbers a reader needs to place it.
struct DiffLine {
    kind: DiffKind,
    old_number: Option<u32>,
    new_number: Option<u32>,
    text: String,
}

/// Reads a unified diff back into numbered lines.
///
/// The line numbers come from the `@@ -old,count +new,count @@` headers and are then advanced per
/// line: a removed line advances only the old side, an added line only the new, and context both.
/// That is the whole of what a unified diff encodes, and reconstructing it is what lets the gutter
/// show real file positions instead of an offset from the top of the hunk.
fn parse_unified_diff(diff: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    let mut old_number = 0u32;
    let mut new_number = 0u32;

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            (old_number, new_number) = hunk_start(rest).unwrap_or((old_number, new_number));
            lines.push(DiffLine {
                kind: DiffKind::Header,
                old_number: None,
                new_number: None,
                text: line.to_owned(),
            });
            continue;
        }

        // `---` and `+++` file headers carry no line of their own.
        if line.starts_with("---") || line.starts_with("+++") {
            continue;
        }

        let (kind, text) = match line.as_bytes().first() {
            Some(b'+') => (DiffKind::Added, &line[1..]),
            Some(b'-') => (DiffKind::Removed, &line[1..]),
            Some(b' ') => (DiffKind::Context, &line[1..]),
            _ => (DiffKind::Context, line),
        };

        let (old, new) = match kind {
            DiffKind::Added => {
                new_number += 1;
                (None, Some(new_number))
            }
            DiffKind::Removed => {
                old_number += 1;
                (Some(old_number), None)
            }
            _ => {
                old_number += 1;
                new_number += 1;
                (Some(old_number), Some(new_number))
            }
        };

        lines.push(DiffLine {
            kind,
            old_number: old,
            new_number: new,
            text: text.to_owned(),
        });
    }

    lines
}

/// The two starting line numbers out of `-12,7 +12,9 @@`.
fn hunk_start(header: &str) -> Option<(u32, u32)> {
    let mut old = None;
    let mut new = None;

    for token in header.split_whitespace() {
        let (sign, rest) = token.split_at(token.char_indices().nth(1).map_or(0, |(i, _)| i));
        let first = rest.split(',').next()?.parse::<u32>().ok();
        match sign {
            "-" => old = first,
            "+" => new = first,
            _ => {}
        }
    }

    // One before, because the counters are advanced before use.
    Some((old?.saturating_sub(1), new?.saturating_sub(1)))
}

/// How many lines a diff adds and removes.
fn diff_counts(diff: &str) -> (usize, usize) {
    diff.lines()
        .filter(|line| !line.starts_with("+++") && !line.starts_with("---"))
        .fold((0, 0), |(added, removed), line| match line.as_bytes().first() {
            Some(b'+') => (added + 1, removed),
            Some(b'-') => (added, removed + 1),
            _ => (added, removed),
        })
}

/// The `+12 -3` badge.
fn render_diff_counts(diff: &str, cx: &App) -> impl IntoElement + use<> {
    let (added, removed) = diff_counts(diff);
    let colors = cx.theme().colors();

    h_flex()
        .gap_1()
        .when(added > 0, |this| {
            this.child(
                div()
                    .text_color(colors.version_control_added)
                    .text_ui_sm(cx)
                    .child(format!("+{added}")),
            )
        })
        .when(removed > 0, |this| {
            this.child(
                div()
                    .text_color(colors.version_control_deleted)
                    .text_ui_sm(cx)
                    .child(format!("-{removed}")),
            )
        })
}

/// What the model worked through before answering.
///
/// Rendered as markdown, like the answer, because it *is* prose: models fence code inside it and
/// write lists in it, and showing it raw left the backticks on screen. Set apart rather than mixed
/// in — this is the working, not the conclusion — but at reading size, because it is meant to be
/// read.
fn render_reasoning(
    markdown: Entity<Markdown>,
    style: MarkdownStyle,
    cx: &App,
) -> impl IntoElement + use<> {
    let colors = cx.theme().colors();

    div()
        .w_full()
        .min_w_0()
        .my_1()
        .pl_3()
        .border_l_2()
        .border_color(colors.border_variant)
        .child(MarkdownElement::new(markdown, style))
}

/// A path's extension, which is the key the language cache uses.
fn extension_of(path: &str) -> Option<SharedString> {
    std::path::Path::new(path)
        .extension()
        .map(|extension| SharedString::from(extension.to_string_lossy().into_owned()))
}

/// The diff itself: the code in its own syntax colours, the change in the background.
///
/// Colouring the characters green and red throws away everything the syntax already told you — a
/// string stops looking like a string, a keyword like a keyword — to say something the background
/// is already saying. So the text keeps the language's own highlighting and only the row behind it
/// carries the add/remove colour.
fn render_diff(
    diff: &str,
    language: Option<&Arc<language::Language>>,
    cx: &App,
) -> impl IntoElement + use<> {
    let colors = cx.theme().colors();
    let syntax = cx.theme().syntax();
    let parsed = parse_unified_diff(diff);
    let shown = parsed.len().min(MAX_DIFF_LINES);
    let truncated = parsed.len().saturating_sub(shown);

    let rows = parsed
        .into_iter()
        .take(shown)
        .map(|line| {
            let (background, marker, marker_color) = match line.kind {
                DiffKind::Added => (
                    colors.version_control_added.opacity(0.15),
                    "+",
                    colors.version_control_added,
                ),
                DiffKind::Removed => (
                    colors.version_control_deleted.opacity(0.15),
                    "-",
                    colors.version_control_deleted,
                ),
                DiffKind::Header => (colors.element_background, " ", colors.text_accent),
                DiffKind::Context => (colors.editor_background, " ", colors.text_muted),
            };

            // A hunk header is not source, so it is left as plain accent text.
            let text: AnyElement = match (line.kind, language) {
                (DiffKind::Header, _) | (_, None) => div()
                    .text_color(if line.kind == DiffKind::Header {
                        colors.text_accent
                    } else {
                        colors.text
                    })
                    .child(line.text.clone())
                    .into_any_element(),
                (_, Some(language)) => {
                    let highlights = language
                        .highlight_text(&language::Rope::from(line.text.as_str()), 0..line.text.len())
                        .into_iter()
                        .filter_map(|(range, id)| syntax.get(id).cloned().map(|style| (range, style)))
                        .collect::<Vec<_>>();

                    StyledText::new(line.text.clone())
                        .with_highlights(highlights)
                        .into_any_element()
                }
            };

            h_flex()
                .w_full()
                .bg(background)
                .child(
                    div()
                        .w(px(40.))
                        .px_1()
                        .flex_shrink_0()
                        .text_color(colors.text_muted)
                        .child(
                            line.new_number
                                .or(line.old_number)
                                .map(|number| number.to_string())
                                .unwrap_or_default(),
                        ),
                )
                .child(
                    div()
                        .w(px(10.))
                        .flex_shrink_0()
                        .text_color(marker_color)
                        .child(marker),
                )
                .child(div().flex_1().min_w_0().child(text))
        })
        .collect::<Vec<_>>();

    v_flex()
        .id("cowork-diff")
        .w_full()
        .min_w_0()
        .mt_1()
        .max_h(px(360.))
        .overflow_y_scroll()
        .rounded_sm()
        .border_1()
        .border_color(colors.border_variant)
        .bg(colors.editor_background)
        .font_buffer(cx)
        .text_ui_sm(cx)
        .children(rows)
        .when(truncated > 0, |this| {
            this.child(
                div()
                    .px_2()
                    .text_color(colors.text_muted)
                    .child(format!("… {truncated} more lines")),
            )
        })
}

fn write_auto_approve(fs: Arc<dyn fs::Fs>, approve: bool, cx: &mut App) {
    settings::update_settings_file(fs, cx, move |settings, _| {
        settings.cowork.get_or_insert_default().auto_approve = Some(approve);
    });
}

/// Token counts as a person reads them: `706k`, not `706123`.
fn compact(tokens: u64) -> String {
    match tokens {
        0..=999 => tokens.to_string(),
        1_000..=999_999 => format!("{}k", tokens / 1_000),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.0),
    }
}

/// The project's open folders, as the name the user knows and the path a command runs in.
pub fn project_folders(project: &Entity<Project>, cx: &App) -> Vec<(String, String)> {
    project
        .read(cx)
        .visible_worktrees(cx)
        .map(|worktree| {
            let path = worktree.read(cx).abs_path();
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            (name, path.to_string_lossy().into_owned())
        })
        .collect()
}

/// The whole failure, wrapped, with a button that takes it away.
///
/// A provider's error is often a long JSON body whose useful part — the model name, the status, the
/// reason — sits at the end. Rendering it as a `Label` put it on one unwrappable line that ran off
/// the right edge of the window, which hid exactly the part worth reading, and left no way to get
/// at the text to report it.
fn render_error(error: SharedString, cx: &App) -> impl IntoElement {
    let colors = cx.theme().colors();
    let status = cx.theme().status();

    h_flex()
        .w_full()
        .items_start()
        .px_4()
        .py_2()
        .gap_2()
        .bg(colors.element_background)
        .border_t_1()
        .border_color(colors.border_variant)
        .child(
            Icon::new(IconName::Warning)
                .size(IconSize::Small)
                .color(Color::Error),
        )
        .child(
            div()
                .id("cowork-error-text")
                // `min_w_0` is what allows the text to wrap: without it this child keeps its
                // natural single-line width and pushes the row past the edge of the window.
                .flex_1()
                .min_w_0()
                // A provider can return a very long body. Wrapping it is right; letting it push
                // the composer off the bottom of the window is not.
                .max_h(px(160.))
                .overflow_y_scroll()
                .text_ui(cx)
                .text_color(status.error)
                .child(error.clone()),
        )
        .child(
            CopyButton::new("cowork-copy-error", error)
                .icon_size(IconSize::Small)
                .tooltip_label("Copy error"),
        )
}

impl Render for CoworkThreadView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Before anything borrows the theme, because asking for a language needs `cx` mutably.
        self.request_diff_languages(cx);

        let colors = cx.theme().colors();
        let is_streaming = self.is_streaming();
        let markdown_style = MarkdownStyle::themed(MarkdownFont::Preview, window, cx);
        let model_label = self.thread.metadata.model.model_id.clone();

        let messages = self
            .messages
            .iter()
            .enumerate()
            .map(|(index, message)| match message.role {
                // The user's own turns are right-aligned and the model's are full width, so the
                // two are told apart by position before a single word is read.
                Role::User => h_flex()
                    .id(("cowork-user-message", index))
                    .w_full()
                    .justify_end()
                    .child(
                        v_flex()
                            .max_w(relative(0.75))
                            .p_3()
                            .gap_1()
                            .rounded_md()
                            .bg(colors.element_background)
                            .child(Label::new("You").size(LabelSize::XSmall).color(Color::Muted))
                            .child(div().child(message.text.clone())),
                    )
                    .into_any_element(),
                Role::Assistant => v_flex()
                    .id(("cowork-assistant-message", index))
                    .w_full()
                    .px_3()
                    .py_2()
                    .gap_1()
                    .child(
                        Label::new(model_label.clone())
                            .size(LabelSize::XSmall)
                            .color(Color::Muted),
                    )
                    .when_some(message.reasoning_rendered.clone(), |this, markdown| {
                        this.child(render_reasoning(markdown, markdown_style.clone(), cx))
                    })
                    .when_some(message.rendered.clone(), |this, markdown| {
                        this.child(MarkdownElement::new(markdown, markdown_style.clone()))
                    })
                    .when(!message.tool_calls.is_empty(), |this| {
                        this.child(self.render_tool_calls(index, &message.tool_calls, cx))
                    })
                    .into_any_element(),
                // Results are drawn inside the card of the call they answer, so this message
                // contributes nothing of its own.
                Role::Tool => div().into_any_element(),
            })
            .collect::<Vec<_>>();

        let is_empty = messages.is_empty();

        v_flex()
            .key_context("CoworkThread")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::select_model))
            .size_full()
            .bg(colors.editor_background)
            .child(self.render_header(is_streaming, cx))
            .child(
                v_flex()
                    .id("cowork-messages")
                    .flex_1()
                    .w_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll_handle)
                    .p_4()
                    .gap_3()
                    .when(is_empty, |this| this.child(self.render_empty_state()))
                    .children(messages),
            )
            .children(self.render_status(is_streaming, cx))
            .children(self.render_permission(cx))
            .when_some(self.error.clone(), |this, error| {
                this.child(render_error(error, cx))
            })
            .child(self.render_composer(is_streaming, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = concat!(
        "@@ -3,4 +3,5 @@\n",
        " fn main() {\n",
        "-    println!(\"old\");\n",
        "+    println!(\"new\");\n",
        "+    println!(\"extra\");\n",
        " }\n",
    );

    fn call(name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: name.to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
        }
    }

    #[test]
    fn three_identical_calls_in_a_row_is_a_circle() {
        let mut recent = Vec::new();
        let same = [call("read", r#"{"path":"a.rs"}"#)];

        assert_eq!(CoworkThreadView::repeating(&mut recent, &same), None);
        assert_eq!(CoworkThreadView::repeating(&mut recent, &same), None,
            "twice can be a retry");
        assert!(
            CoworkThreadView::repeating(&mut recent, &same).is_some(),
            "three times is not work"
        );
    }

    #[test]
    fn the_same_tool_on_a_different_file_is_progress() {
        let mut recent = Vec::new();

        for path in ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"] {
            let calls = [call("read", &format!(r#"{{"path":"{path}"}}"#))];
            assert_eq!(
                CoworkThreadView::repeating(&mut recent, &calls),
                None,
                "reading {path} is not a repeat of the file before it"
            );
        }
    }

    #[test]
    fn anything_different_resets_the_count() {
        // A model that reads a file, writes it, then reads it again is working, and must not be
        // stopped because two of those three happened to match.
        let mut recent = Vec::new();
        let read = [call("read", r#"{"path":"a.rs"}"#)];
        let write = [call("write", r#"{"path":"a.rs"}"#)];

        CoworkThreadView::repeating(&mut recent, &read);
        CoworkThreadView::repeating(&mut recent, &read);
        assert_eq!(CoworkThreadView::repeating(&mut recent, &write), None);
        assert_eq!(
            CoworkThreadView::repeating(&mut recent, &read),
            None,
            "the streak restarted"
        );
    }

    #[test]
    fn the_message_names_what_is_being_repeated() {
        // "Stopped" with no subject leaves the user to guess which of ten calls was the problem.
        let mut recent = Vec::new();
        let same = [call("shell", r#"{"command":"npm test"}"#)];

        CoworkThreadView::repeating(&mut recent, &same);
        CoworkThreadView::repeating(&mut recent, &same);
        let reported = CoworkThreadView::repeating(&mut recent, &same).expect("should stop");

        assert!(reported.contains("shell"), "got: {reported}");
        assert!(reported.contains("npm test"), "got: {reported}");
    }

    #[test]
    fn the_models_own_account_of_a_step_wins() {
        // A transcript built from tool names reads as mechanics and leaves the reader to
        // reconstruct why. The model knows why; it is asked, and its sentence is what shows.
        let described = describe_call(
            "read",
            r#"{"path":"src/picker.rs","intent":"Checked how the picker is wired"}"#,
            true,
        );

        assert_eq!(described, "Checked how the picker is wired");
    }

    #[test]
    fn without_one_the_step_is_still_a_sentence() {
        assert_eq!(
            describe_call("write", r#"{"path":"src/index.ts"}"#, true),
            "Wrote src/index.ts"
        );
        assert_eq!(
            describe_call("shell", r#"{"command":"npm install"}"#, false),
            "Running npm install…"
        );
    }

    #[test]
    fn an_empty_account_falls_back_rather_than_showing_nothing() {
        assert_eq!(
            describe_call("read", r#"{"path":"a.rs","intent":"   "}"#, true),
            "Read a.rs"
        );
    }

    #[test]
    fn an_unknown_tool_still_reads_as_a_sentence() {
        assert_eq!(describe_call("teleport", "{}", false), "Running teleport…");
        assert_eq!(describe_call("teleport", "{}", true), "Ran teleport");
    }

    #[test]
    fn counts_what_changed() {
        assert_eq!(diff_counts(SAMPLE), (2, 1));
        assert_eq!(diff_counts(""), (0, 0));
    }

    #[test]
    fn file_headers_are_not_counted_as_changes() {
        // `+++ b/file` and `--- a/file` start with the same characters as a changed line, and
        // counting them would report two phantom edits on every diff.
        let with_headers = concat!("--- a/x.rs\n", "+++ b/x.rs\n", "@@ -1,1 +1,1 @@\n", "+one\n");

        assert_eq!(diff_counts(with_headers), (1, 0));
    }

    #[test]
    fn line_numbers_come_from_the_hunk_header() {
        let lines = parse_unified_diff(SAMPLE);

        // The header itself, then the context line at 3, the removal at 4, two additions at 4 and
        // 5, and the closing context.
        assert_eq!(lines[0].kind, DiffKind::Header);
        assert_eq!(lines[1].new_number, Some(3));
        assert_eq!(lines[2].old_number, Some(4), "a removal numbers the old side");
        assert_eq!(lines[2].new_number, None);
        assert_eq!(lines[3].new_number, Some(4), "an addition numbers the new side");
        assert_eq!(lines[4].new_number, Some(5));
    }

    #[test]
    fn each_side_advances_only_on_its_own_lines() {
        // This is the whole of what a unified diff encodes; getting it wrong makes the gutter show
        // positions that exist in neither version of the file.
        let lines = parse_unified_diff(SAMPLE);
        let removal = &lines[2];
        let addition = &lines[3];

        assert_eq!((removal.old_number, removal.new_number), (Some(4), None));
        assert_eq!((addition.old_number, addition.new_number), (None, Some(4)));
    }

    #[test]
    fn the_marker_is_stripped_from_the_text() {
        let lines = parse_unified_diff(SAMPLE);

        assert_eq!(lines[3].text, "    println!(\"new\");");
        assert!(!lines[3].text.starts_with('+'));
    }

    #[test]
    fn token_counts_read_the_way_a_person_says_them() {
        assert_eq!(compact(842), "842");
        assert_eq!(compact(706_123), "706k");
        assert_eq!(compact(1_048_576), "1.0M");
    }
}
