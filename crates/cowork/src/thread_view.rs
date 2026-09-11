use crate::{
    Cancel, SelectModel, Submit,
    catalog::ModelRef,
    cowork_settings::CoworkSettings,
    model_selector::ModelSelector,
    provider::{self, CompletionEvent, CompletionRequest, Message, Role},
    thread::{CoworkStore, Thread, ThreadId},
};
use anyhow::{Context as _, Result, anyhow};
use editor::Editor;
use futures::StreamExt as _;
use gpui::{Entity, EventEmitter, FocusHandle, Focusable, ScrollHandle, Task, WeakEntity};
use language::LanguageRegistry;
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use project::Project;
use settings::Settings as _;
use std::sync::Arc;
use ui::{Divider, Tooltip, prelude::*};
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
    error: Option<SharedString>,
    completion: Option<Task<()>>,
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
                rendered: (message.role == Role::Assistant)
                    .then(|| render_markdown(&message.text, language_registry.clone(), cx)),
            })
            .collect();

        Self {
            thread,
            messages,
            input,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            language_registry,
            store,
            workspace,
            error: None,
            completion: None,
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
        // Dropping the task cancels the request; whatever streamed so far is kept.
        if self.completion.take().is_some() {
            self.persist(cx);
            cx.notify();
        }
    }

    fn select_model(&mut self, _: &SelectModel, window: &mut Window, cx: &mut Context<Self>) {
        self.open_model_selector(window, cx);
    }

    fn open_model_selector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        let this = cx.entity().downgrade();
        let selected = Some(self.thread.metadata.model.clone());
        let on_confirm: Arc<dyn Fn(ModelRef, &mut Window, &mut App) + Send + Sync> =
            Arc::new(move |model, _window, cx| {
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

        self.thread.messages.push(Message {
            role,
            text: text.clone(),
        });
        self.messages.push(MessageView {
            role,
            text,
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

    fn start_completion(&mut self, cx: &mut Context<Self>) {
        let request = match self.build_request(cx) {
            Ok(request) => request,
            Err(error) => {
                self.fail(error, cx);
                return;
            }
        };

        self.push_message(Role::Assistant, String::new(), cx);

        let http_client = cx.http_client();
        self.completion = Some(cx.spawn(async move |this, cx| {
            let mut stream = match provider::stream_completion(http_client, request).await {
                Ok(stream) => stream,
                Err(error) => {
                    this.update(cx, |this, cx| this.finish_with_error(error, cx))
                        .log_err();
                    return;
                }
            };

            while let Some(event) = stream.next().await {
                match event {
                    Ok(CompletionEvent::Text(chunk)) => {
                        if this
                            .update(cx, |this, cx| this.extend_last_message(&chunk, cx))
                            .is_err()
                        {
                            return;
                        }
                    }
                    Ok(CompletionEvent::Stop) => break,
                    Err(error) => {
                        this.update(cx, |this, cx| this.finish_with_error(error, cx))
                            .log_err();
                        return;
                    }
                }
            }

            this.update(cx, |this, cx| this.finish(cx)).log_err();
        }));
    }

    fn build_request(&self, cx: &App) -> Result<CompletionRequest> {
        let model = self.thread.metadata.model.clone();
        let store = self.store.read(cx);
        let (catalog_provider, _) = store.catalog().model(&model).with_context(|| {
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
            messages: self.thread.messages.clone(),
            max_output_tokens: CoworkSettings::get_global(cx).max_output_tokens,
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
                Role::User => v_flex()
                    .id(("cowork-user-message", index))
                    .w_full()
                    .p_3()
                    .gap_1()
                    .rounded_md()
                    .bg(colors.element_background)
                    .child(Label::new("You").size(LabelSize::XSmall).color(Color::Muted))
                    .child(div().child(message.text.clone()))
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
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    h_flex()
                        .w_full()
                        .px_4()
                        .py_2()
                        .gap_2()
                        .bg(colors.element_background)
                        .child(Icon::new(IconName::Warning).color(Color::Error))
                        .child(Label::new(error).color(Color::Error)),
                )
            })
            .child(self.render_composer(is_streaming, cx))
    }
}
