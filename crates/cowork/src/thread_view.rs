use crate::{
    Cancel, SelectModel, Submit,
    catalog::ModelRef,
    model_selector::ModelSelector,
    provider::{
        self, CompletionEvent, CompletionRequest, Message, Role, StopReason, ToolCall, ToolResult,
    },
    permission::{Decision, PermissionBroker, PermissionEvent},
    thread::{CoworkStore, Thread, ThreadId},
    tool::{ToolContext, ToolRegistry},
};
use anyhow::{Context as _, Result, anyhow};
use editor::Editor;
use futures::StreamExt as _;
use gpui::{
    AnyElement, Entity, EventEmitter, FocusHandle, Focusable, ScrollHandle, SharedString, Task,
    WeakEntity, relative,
};
use language::LanguageRegistry;
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use project::Project;
use std::sync::Arc;
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
    tools: ToolRegistry,
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

        Self {
            thread,
            messages,
            input,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            language_registry,
            store,
            workspace,
            project,
            tools: ToolRegistry::default_tools(),
            permissions,
            error: None,
            completion: None,
            _permissions: permissions_subscription,
        }
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
                this.update(cx, |this, cx| {
                    this.thread.metadata.model = model;
                    this.persist(cx);
                    cx.notify();
                })
                .log_err();
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

    /// The number of model round-trips one user message may cause.
    ///
    /// A loop that cannot end is the default failure mode of a tool-calling agent: a model that
    /// keeps re-reading the same file will otherwise spend the user's money until they notice.
    const MAX_STEPS: usize = 12;

    fn start_completion(&mut self, cx: &mut Context<Self>) {
        let http_client = cx.http_client();
        let tools = self.tools.clone();
        let project = self.project.clone();
        let workspace = self.workspace.clone();
        let permissions = self.permissions.clone();

        self.completion = Some(cx.spawn(async move |this, cx| {
            for step in 0..Self::MAX_STEPS {
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

                let mut results = Vec::with_capacity(calls.len());
                for call in &calls {
                    let result = Self::run_tool(
                        &tools,
                        call,
                        ToolContext {
                            project: project.clone(),
                            workspace: workspace.clone(),
                            permissions: permissions.clone(),
                        },
                        cx,
                    )
                    .await;
                    results.push(result);
                }

                if this
                    .update(cx, |this, cx| this.push_tool_results(results, cx))
                    .is_err()
                {
                    return;
                }

                if step + 1 == Self::MAX_STEPS {
                    this.update(cx, |this, cx| {
                        this.fail(
                            anyhow!(
                                "stopped after {} tool calls in one turn. Ask again, more \
                                 narrowly, to continue.",
                                Self::MAX_STEPS
                            ),
                            cx,
                        )
                    })
                    .log_err();
                    return;
                }
            }
        }));
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
                CompletionEvent::Stop(reason) => {
                    stopped_for_tools = reason == StopReason::ToolUse;
                    break;
                }
            }
        }

        Ok((calls, stopped_for_tools))
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
                Button::new("cowork-model", self.thread.metadata.model.qualified())
                    .start_icon(Icon::new(IconName::Sparkle).size(IconSize::Small))
                    .label_size(LabelSize::Small)
                    .tooltip(Tooltip::text("Change the model for this thread"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_model_selector(window, cx)),
                    ),
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
fn summarize_arguments(arguments: &str) -> String {
    let Ok(serde_json::Value::Object(fields)) = serde_json::from_str(arguments) else {
        return String::new();
    };
    fields
        .values()
        .map(|value| match value {
            serde_json::Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
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
                    .when_some(message.rendered.clone(), |this, markdown| {
                        this.child(MarkdownElement::new(markdown, markdown_style.clone()))
                    })
                    .children(message.tool_calls.iter().enumerate().map(|(position, call)| {
                        h_flex()
                            .id(("cowork-tool-call", position))
                            .w_full()
                            .gap_1p5()
                            .child(
                                Icon::new(IconName::PlayOutlined)
                                    .size(IconSize::XSmall)
                                    .color(Color::Accent),
                            )
                            .child(Label::new(call.name.clone()).size(LabelSize::XSmall))
                            .child(
                                Label::new(summarize_arguments(&call.arguments))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted)
                                    .truncate_middle(),
                            )
                    }))
                    .into_any_element(),
                Role::Tool => v_flex()
                    .id(("cowork-tool-results", index))
                    .w_full()
                    .px_3()
                    .gap_1()
                    .children(message.tool_results.iter().enumerate().map(
                        |(position, result)| {
                            let (icon, color) = if result.is_error {
                                (IconName::XCircle, Color::Error)
                            } else {
                                (IconName::Check, Color::Success)
                            };
                            h_flex()
                                .id(("cowork-tool-result", position))
                                .w_full()
                                .gap_1p5()
                                .child(Icon::new(icon).size(IconSize::XSmall).color(color))
                                .child(
                                    Label::new(first_line(&result.content))
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted)
                                        .truncate_middle(),
                                )
                        },
                    ))
                    .into_any_element(),
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
            .children(self.render_permission(cx))
            .when_some(self.error.clone(), |this, error| {
                this.child(render_error(error, cx))
            })
            .child(self.render_composer(is_streaming, cx))
    }
}
