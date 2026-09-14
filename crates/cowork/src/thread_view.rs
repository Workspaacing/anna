use crate::{
    Cancel, ExportSessionLog, SelectModel, Submit,
    catalog::ModelRef,
    checkpoint,
    cowork_settings::CoworkSettings,
    instructions::{self, Instructions},
    model_selector::ModelSelector,
    provider::{
        self, Attachment, AttachmentKind, CompletionEvent, CompletionRequest, Message, Role,
        Served, StepRecord, StopReason, ToolCall, ToolResult,
    },
    permission::{Decision, DecidedBy, PermissionBroker, PermissionEvent, PermissionRequest},
    session_log::{self, LogContent, ThreadLog},
    thread::{ActivityKind, CoworkStore, Thread, ThreadId, now_seconds},
    tool::{ToolContext, ToolKind, ToolRegistry},
    verify::{CheckReport, Finding, Severity},
};
use anyhow::{Context as _, Result, anyhow};
use editor::Editor;
use futures::StreamExt as _;
use gpui::{
    AnyElement, App, ClipboardEntry, ClipboardItem, Entity, EventEmitter, FocusHandle, Focusable,
    ScrollHandle, SharedString, Task, WeakEntity, relative,
};
use language::LanguageRegistry;
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use project::{
    Project,
    git_store::{GitStoreEvent, RepositoryEvent},
};
use settings::Settings as _;
use std::{
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};
use crate::waiting::{OpenPrompt, WaitingOnYou};
use crate::image_preview::ImagePreview;
use gpui::StyledText;
use release_channel::AppVersion;
use settings::AgentPermission;
use ui::{
    Button, ButtonStyle, ContextMenu, ContextMenuEntry, CopyButton, Divider, PopoverMenu, Tooltip,
    prelude::*,
};
use util::ResultExt as _;
use workspace::{
    Toast, Workspace,
    item::{Item, ItemEvent},
    notifications::NotificationId,
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
    /// What was sent along with this message.
    attachments: Vec<Attachment>,
    /// `attachments` decoded for drawing, one entry each, `None` for anything that is not a picture
    /// gpui can draw. Filled in by `render` the first time the message is shown, so a thread with
    /// many pictures decodes each one once rather than on every frame.
    thumbnails: Vec<Option<AttachmentPreview>>,
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
    /// Pictures and files chosen but not yet sent.
    pending_attachments: PendingAttachments,
    /// The user's own instructions, read again at the start of each turn.
    user_instructions: Vec<Instructions>,
    /// Each project folder's rules file, read again at the start of each turn.
    project_rules: Vec<Instructions>,
    error: Option<SharedString>,
    completion: Option<Task<()>>,
    _permissions: gpui::Subscription,
    /// Repaints the branch chip when a repository changes branch. The branch itself is read from
    /// the git store's state, never by running git.
    _git_store: gpui::Subscription,
    /// Not stored yet. See [`CoworkStore::draft_thread`].
    is_draft: bool,
    /// What is waiting on the user in this project's repository, for the home of a thread with no
    /// messages yet. `None` for a thread opened with a history: it never shows the home, and asking
    /// GitHub on its behalf would be a request nobody sees the answer to.
    waiting: Option<Entity<WaitingOnYou>>,
    /// Redraws the home when the answer arrives, which is always after the first frame.
    _waiting: Option<gpui::Subscription>,
}

/// The side of a picture's tile above the composer: large enough to recognise the picture, small
/// enough that several share a row without pushing the composer down.
const PENDING_PICTURE_SIZE: f32 = 96.;

/// A file chosen for the next message.
struct PendingAttachment {
    attachment: Attachment,
    /// `None` for anything that is not a picture gpui can draw, which is shown as a chip instead.
    preview: Option<AttachmentPreview>,
}

/// What has been chosen to go with the next message.
///
/// Each picture is decoded once, when it is added, rather than on every frame. Its preview is kept
/// in the same entry as the attachment so that removing one can never leave a picture drawn under
/// another file's name.
#[derive(Default)]
struct PendingAttachments(Vec<PendingAttachment>);

impl PendingAttachments {
    fn new(attachments: Vec<Attachment>) -> Self {
        let mut pending = Self::default();
        pending.extend(attachments);
        pending
    }

    fn extend(&mut self, attachments: impl IntoIterator<Item = Attachment>) {
        self.0
            .extend(attachments.into_iter().map(|attachment| PendingAttachment {
                preview: preview(&attachment),
                attachment,
            }));
    }

    fn remove(&mut self, index: usize) {
        if index < self.0.len() {
            self.0.remove(index);
        }
    }

    fn take(&mut self) -> Vec<Attachment> {
        std::mem::take(&mut self.0)
            .into_iter()
            .map(|pending| pending.attachment)
            .collect()
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn iter(&self) -> impl Iterator<Item = &PendingAttachment> {
        self.0.iter()
    }
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
                attachments: message.attachments.clone(),
                thumbnails: Vec::new(),
                rendered: (message.role == Role::Assistant)
                    .then(|| render_markdown(&message.text, language_registry.clone(), cx)),
            })
            .collect();

        let permissions = cx.new(|cx| PermissionBroker::new(thread.metadata.project.clone(), cx));
        // A question the agent is waiting on has to reach the screen, and the question and its
        // answer the thread's activity; it is the broker that knows when either happens.
        let permissions_subscription =
            cx.subscribe(&permissions, |this, _, event: &PermissionEvent, cx| {
                this.record_permission(event, cx);
                cx.notify();
            });

        // Only the events that can change the branch. Statuses update with every file the agent
        // writes, and repainting the whole thread for each of those would be for nothing.
        let git_store = project.read(cx).git_store().clone();
        let git_store_subscription =
            cx.subscribe(&git_store, |_, _, event: &GitStoreEvent, cx| match event {
                GitStoreEvent::RepositoryUpdated(
                    _,
                    RepositoryEvent::HeadChanged | RepositoryEvent::BranchListChanged,
                    _,
                )
                | GitStoreEvent::RepositoryAdded
                | GitStoreEvent::RepositoryRemoved(_) => cx.notify(),
                _ => {}
            });

        let (waiting, waiting_subscription) = if thread.messages.is_empty() {
            let waiting = cx.new(|cx| WaitingOnYou::new(project.clone(), cx));
            let subscription = cx.observe(&waiting, |_, _, cx| cx.notify());
            (Some(waiting), Some(subscription))
        } else {
            (None, None)
        };

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
            pending_attachments: PendingAttachments::default(),
            user_instructions: Vec::new(),
            project_rules: Vec::new(),
            error: None,
            completion: None,
            _permissions: permissions_subscription,
            _git_store: git_store_subscription,
            is_draft: false,
            waiting,
            _waiting: waiting_subscription,
        };

        // Once the view exists, because the warm-up stores its handle back onto it.
        view.warm_up_toolchain(cx);
        view.load_instructions(cx);
        view
    }

    /// Reads the user's instructions and the project's rules as soon as the thread opens, so a
    /// session log exported before the next message shows the system prompt it would be sent with.
    /// Each turn reads them again, so an edited rules file still applies from the next message.
    fn load_instructions(&mut self, cx: &mut Context<Self>) {
        let fs = self.fs.clone();
        let folders = project_folders(&self.project, cx);
        let setting = CoworkSettings::get_global(cx).instructions.clone();
        cx.spawn(async move |this, cx| {
            let user_instructions =
                instructions::load_user_instructions(fs.as_ref(), &setting, paths::agents_file())
                    .await;
            let project_rules = instructions::load_rules(fs.as_ref(), &folders).await;
            this.update(cx, |this, _| {
                this.user_instructions = user_instructions;
                this.project_rules = project_rules;
            })
        })
        .detach_and_log_err(cx);
    }

    /// A view over a thread the store has not saved, which saves it when its first message is sent.
    pub fn draft(
        thread: Thread,
        store: Entity<CoworkStore>,
        workspace: WeakEntity<Workspace>,
        project: Entity<Project>,
        fs: Arc<dyn fs::Fs>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self::new(thread, store, workspace, project, fs, window, cx);
        view.is_draft = true;
        view
    }

    pub fn thread_id(&self) -> &ThreadId {
        &self.thread.metadata.id
    }

    pub fn is_draft(&self) -> bool {
        self.is_draft
    }

    pub fn focus_composer(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.input.focus_handle(cx), cx);
    }

    fn is_streaming(&self) -> bool {
        self.completion.is_some()
    }

    fn submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_streaming() {
            return;
        }

        let prompt = self.input.read(cx).text(cx).trim().to_owned();
        // A picture on its own is a question — "what is wrong with this?" — so an empty box with
        // something attached still sends.
        if prompt.is_empty() && self.pending_attachments.is_empty() {
            return;
        }

        // Checked against the model the thread has now, not the one it had when the files were
        // picked: the model can be changed afterwards, and a rewind, fork or prefill brings back
        // attachments chosen for another one. The provider would reject the whole request.
        if let Some(refusal) = self.refuse_unreadable_attachments(cx) {
            self.thread
                .record(ActivityKind::AttachmentsRefused, refusal.clone());
            self.error = Some(refusal.into());
            cx.notify();
            return;
        }

        self.input.update(cx, |editor, cx| editor.clear(window, cx));
        self.error = None;
        let attachments = self.pending_attachments.take();
        // The model is named because it can change between messages, and which model a message
        // went to is the first thing to check when one of them misbehaves.
        let sent = format!(
            "Message {} sent to {}: {} characters, {} attachment(s){}",
            self.thread.messages.len() + 1,
            self.thread.metadata.model.qualified(),
            prompt.chars().count(),
            attachments.len(),
            attachments
                .iter()
                .map(|attachment| format!(" {} ({})", attachment.name, attachment.media_type))
                .collect::<Vec<_>>()
                .join(","),
        );
        self.push_message(Role::User, prompt, cx);
        self.thread.record(ActivityKind::MessageSent, sent);
        if !attachments.is_empty() {
            if let Some(stored) = self.thread.messages.last_mut() {
                stored.attachments = attachments.clone();
            }
            if let Some(shown) = self.messages.last_mut() {
                shown.attachments = attachments;
            }
        }
        // Written now rather than when the reply ends, so the conversation reaches the panel the
        // moment it starts and survives a window closed before the reply lands.
        if self.is_draft {
            self.is_draft = false;
            self.persist(cx);
            cx.emit(CoworkThreadEvent::TitleChanged);
        }
        self.start_completion(cx);
        cx.notify();
    }

    /// Sends a message the user did not type, which is how work arrives from elsewhere.
    ///
    /// Goes through the same path as pressing enter rather than a shortcut of its own, so a thread
    /// opened from GitHub is in every way an ordinary thread from the moment it exists.
    pub fn send_now(&mut self, message: String, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |editor, cx| {
            editor.set_text(message, window, cx);
        });
        self.submit(&Submit, window, cx);
    }

    fn cancel(&mut self, _: &Cancel, _window: &mut Window, cx: &mut Context<Self>) {
        // An outstanding question belongs to the turn being interrupted, so it goes with it rather
        // than being left on screen asking about work nobody is waiting for any more.
        self.permissions
            .update(cx, |permissions, cx| permissions.cancel(cx));

        // Dropping the task cancels the request; whatever streamed so far is kept.
        if self.completion.take().is_some() {
            self.thread.record(
                ActivityKind::TurnCancelled,
                "Stopped by the user; whatever streamed so far is kept",
            );
            self.persist(cx);
            cx.notify();
        }
    }

    /// Puts a message in the composer without sending it, with the pictures it carried.
    fn prefill(
        &mut self,
        text: String,
        attachments: Vec<Attachment>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_attachments = PendingAttachments::new(attachments);
        self.input
            .update(cx, |editor, cx| editor.set_text(text, window, cx));
        window.focus(&self.input.focus_handle(cx), cx);
        cx.notify();
    }

    /// Goes back to one of the user's messages, as Claude Code's rewind does.
    ///
    /// The message itself is removed and put back in the composer, because the usual reason to go
    /// back is to ask the same thing differently. Files are restored only as far as the agent's own
    /// `write` and `edit` calls reach: what a shell command changed was never recorded, and a file
    /// the user has touched since is left alone rather than overwritten.
    fn rewind(
        &mut self,
        index: usize,
        scope: RewindScope,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Never while a turn runs. The agent is still changing the files and the transcript, so a
        // rewind underneath it would undo work it is about to build on. The button is disabled for
        // the same reason; this holds even for a menu that was opened before the turn started.
        if self.is_streaming() {
            return;
        }

        let Some(message) = self
            .thread
            .messages
            .get(index)
            .filter(|message| message.role == Role::User)
        else {
            return;
        };
        let text = message.text.clone();
        let attachments = message.attachments.clone();
        let checkpoints = checkpoints_from(self.thread.messages.get(index..).unwrap_or_default());

        if scope.restores_code() {
            let plan = checkpoint::plan(checkpoints);
            let project = self.project.clone();
            let workspace = self.workspace.clone();
            cx.spawn(async move |_, cx| {
                let outcome = checkpoint::restore(project, plan, cx).await;
                let notice = describe_restore(&outcome);
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.show_toast(
                            Toast::new(NotificationId::unique::<RewindNotice>(), notice),
                            cx,
                        );
                    })
                    .log_err();
            })
            .detach();
        }

        if scope.rewinds_conversation() {
            self.thread.messages.truncate(index);
            self.messages.truncate(index);
            // Keyed by message index, so an entry past the cut would open the diff of whatever
            // message takes that place next.
            self.expanded_diffs
                .retain(|(message_index, _)| *message_index < index);
            // It described the longer conversation; the next reply reports the real figure.
            self.thread.metadata.context_tokens = None;
            self.error = None;
            self.prefill(text, attachments, window, cx);
            self.persist(cx);
            cx.emit(CoworkThreadEvent::TitleChanged);
        }
        cx.notify();
        self.thread.record(ActivityKind::Rewound, format!("Rewound to message {} ({})", index + 1, scope.describe()));
    }

    /// Opens the picture at `index` among the ones waiting to be sent.
    fn preview_pending_picture(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let pictures = self
            .pending_attachments
            .iter()
            .enumerate()
            .filter_map(|(position, pending)| {
                pending.preview.clone().map(|preview| {
                    (position, SharedString::from(pending.attachment.name.clone()), preview)
                })
            })
            .collect();
        self.open_preview(pictures, index, window, cx);
    }

    /// Opens a picture from a sent message, with the message's other pictures a step away.
    fn preview_sent_picture(
        &mut self,
        message_index: usize,
        position: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(message) = self.messages.get(message_index) else {
            return;
        };
        let pictures = message
            .attachments
            .iter()
            .zip(message.thumbnails.iter())
            .enumerate()
            .filter_map(|(at, (attachment, thumbnail))| {
                thumbnail
                    .clone()
                    .map(|preview| (at, SharedString::from(attachment.name.clone()), preview))
            })
            .collect();
        self.open_preview(pictures, position, window, cx);
    }

    /// Shows `pictures` in the preview, starting at the one whose attachment position is `clicked`.
    ///
    /// Positions are among all attachments, pictures or not, because that is what the click knows;
    /// the preview steps through pictures only, so a PDF between two screenshots is not a blank page.
    fn open_preview(
        &self,
        pictures: Vec<(usize, SharedString, AttachmentPreview)>,
        clicked: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(start) = pictures.iter().position(|(position, _, _)| *position == clicked) else {
            return;
        };
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let pictures = pictures
            .into_iter()
            .map(|(_, name, preview)| (name, preview))
            .collect::<Vec<_>>();

        workspace.update(cx, |workspace, cx| {
            workspace.toggle_modal(window, cx, move |_window, cx| {
                ImagePreview::new(pictures, start, cx)
            });
        });
    }

    /// Starts a new thread from everything before one of the user's messages, with that message
    /// waiting in its composer.
    ///
    /// The thread it came from is left exactly as it was, and so are the files: both threads work in
    /// the same folder, so a fork is a second line of conversation, not a second copy of the code.
    fn fork(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(message) = self
            .thread
            .messages
            .get(index)
            .filter(|message| message.role == Role::User)
        else {
            return;
        };
        let text = message.text.clone();
        let attachments = message.attachments.clone();
        let earlier = self
            .thread
            .messages
            .get(..index)
            .unwrap_or_default()
            .to_vec();
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        let source = self.thread.metadata.clone();
        let thread = self
            .store
            .update(cx, |store, cx| store.fork_thread(&source, earlier, cx));
        let store = self.store.clone();
        let workspace_handle = self.workspace.clone();
        let project = self.project.clone();
        let fs = self.fs.clone();

        workspace.update(cx, |workspace, cx| {
            let view = cx.new(|cx| {
                CoworkThreadView::new(thread, store, workspace_handle, project, fs, window, cx)
            });
            workspace.add_item_to_active_pane(Box::new(view.clone()), None, true, window, cx);
            // After the item is added, which focuses the item itself; the composer is where the
            // user is about to type.
            view.update(cx, |view, cx| view.prefill(text, attachments, window, cx));
        });
        self.thread.record(ActivityKind::Forked, format!("Forked at message {} into a new thread", index + 1));
    }

    /// Copy, rewind and fork, under one of the user's messages.
    ///
    /// Shown on hover, so a long conversation is not a column of buttons. Each button hides itself,
    /// the way the debugger's session list does, rather than a hidden row wrapping them: the rewind
    /// menu is drawn deferred, apart from the button that opened it, so it stays open when the
    /// pointer leaves the message to reach it.
    fn render_user_message_actions(
        &self,
        index: usize,
        text: &str,
        group: SharedString,
        has_file_changes: bool,
        is_streaming: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let view = cx.weak_entity();

        h_flex()
            .gap_0p5()
            .child(
                CopyButton::new(("cowork-copy-message", index), text.to_owned())
                    .icon_size(IconSize::XSmall)
                    .tooltip_label("Copy message")
                    .visible_on_hover(group.clone()),
            )
            .child(if is_streaming {
                IconButton::new(("cowork-rewind-trigger", index), IconName::RotateCcw)
                    .icon_size(IconSize::XSmall)
                    .icon_color(Color::Muted)
                    .disabled(true)
                    .tooltip(Tooltip::text("Rewind is available once the agent has stopped"))
                    .visible_on_hover(group.clone())
                    .into_any_element()
            } else {
                PopoverMenu::new(("cowork-rewind", index))
                    .trigger(
                        IconButton::new(("cowork-rewind-trigger", index), IconName::RotateCcw)
                            .icon_size(IconSize::XSmall)
                            .icon_color(Color::Muted)
                            .tooltip(Tooltip::text("Rewind to this message"))
                            .visible_on_hover(group.clone()),
                    )
                    .menu(move |window, cx| {
                        let view = view.clone();
                        Some(ContextMenu::build(window, cx, move |menu, _, _| {
                            let rewind = |scope: RewindScope| {
                                let view = view.clone();
                                move |window: &mut Window, cx: &mut App| {
                                    view.update(cx, |view, cx| {
                                        view.rewind(index, scope, window, cx)
                                    })
                                    .log_err();
                                }
                            };

                            menu.header("Rewind to this message")
                                .item(
                                    ContextMenuEntry::new("Conversation and code")
                                        .icon(IconName::RotateCcw)
                                        .disabled(!has_file_changes)
                                        .handler(rewind(RewindScope::ConversationAndCode)),
                                )
                                .item(
                                    ContextMenuEntry::new("Conversation only")
                                        .icon(IconName::Return)
                                        .handler(rewind(RewindScope::Conversation)),
                                )
                                .item(
                                    ContextMenuEntry::new("Code only")
                                        .icon(IconName::Undo)
                                        .disabled(!has_file_changes)
                                        .handler(rewind(RewindScope::Code)),
                                )
                                .separator()
                                .label("Files changed by shell commands are not restored.")
                        }))
                    })
                    .anchor(gpui::Anchor::TopRight)
                    .into_any_element()
            })
            .child(
                IconButton::new(("cowork-fork", index), IconName::GitBranch)
                    .icon_size(IconSize::XSmall)
                    .icon_color(Color::Muted)
                    .visible_on_hover(group)
                    .tooltip(Tooltip::text(
                        "Fork: a new thread with everything before this message",
                    ))
                    .on_click(cx.listener(move |this, _, window, cx| this.fork(index, window, cx))),
            )
    }

    fn answer_permission(&mut self, decision: Decision, cx: &mut Context<Self>) {
        self.permissions
            .update(cx, |permissions, cx| permissions.resolve(decision, cx));
    }

    /// Writes a permission question, and what settled it, into the thread's activity: the only
    /// place an exported log can learn what the agent wanted to run and why it did or did not.
    fn record_permission(&mut self, event: &PermissionEvent, cx: &App) {
        match event {
            PermissionEvent::Changed => {}
            PermissionEvent::Asked(request) => self.thread.record(
                ActivityKind::PermissionRequested,
                format!("Asked the user: {}", describe_permission_request(request)),
            ),
            PermissionEvent::Decided {
                request,
                decision,
                by,
            } => {
                let outcome = match (*decision, *by) {
                    (Decision::Once, DecidedBy::User) => "Allowed once by the user".to_owned(),
                    (Decision::Always, DecidedBy::User) => format!(
                        "Allowed by the user for every `{}` command from now on",
                        request.scope
                    ),
                    (Decision::Reject, DecidedBy::User) => "Denied by the user".to_owned(),
                    (_, DecidedBy::Level) => format!(
                        "Allowed without asking: the {} permission level does not ask about this",
                        permission_label(&CoworkSettings::get_global(cx).permission)
                    ),
                    (_, DecidedBy::Grant) => format!(
                        "Allowed without asking: `{}` was allowed earlier",
                        request.scope
                    ),
                    (_, DecidedBy::Displaced) => {
                        "Denied: another request replaced it before it was answered".to_owned()
                    }
                    (_, DecidedBy::Cancelled) => {
                        "Denied: the turn was stopped while it was open".to_owned()
                    }
                };
                let kind = if decision.is_allowed() {
                    ActivityKind::PermissionAllowed
                } else {
                    ActivityKind::PermissionDenied
                };
                self.thread.record(
                    kind,
                    format!("{outcome}: {}", describe_permission_request(request)),
                );
            }
        }
    }

    /// This thread as an exported log sees it, with what only an open view knows: the reasoning,
    /// the checks on each result, the error on screen and the branch.
    pub(crate) fn thread_log(&self, cx: &App) -> ThreadLog {
        ThreadLog {
            thread: self.thread.clone(),
            reasoning: self
                .messages
                .iter()
                .map(|message| message.reasoning.clone())
                .collect(),
            current_error: self.error.as_ref().map(|error| error.to_string()),
            branch: self.branch_name(cx).map(|branch| branch.to_string()),
            open_in_view: true,
            system_prompt: Some(self.current_system_prompt(cx)),
        }
    }

    fn export_session_log(
        &mut self,
        _: &ExportSessionLog,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let log = self.thread_log(cx);
        session_log::export(
            "anna-session",
            &self.thread.metadata.title,
            Task::ready(LogContent::Session(log)),
            self.fs.clone(),
            self.workspace.clone(),
            cx,
        );
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
                        // Not offered for a request that is always asked, such as a command that
                        // leaves the project: the broker asks about those whatever was granted, so
                        // the button would promise to stop asking and then not stop.
                        .when(!request.always_ask, |this| {
                            this.child(
                                Button::new(
                                    "cowork-permission-always",
                                    format!("Always allow {}", request.scope),
                                )
                                .tooltip(Tooltip::text(
                                    "Stop asking about this program until Anna restarts",
                                ))
                                .on_click(cx.listener(|this, _, _window, cx| {
                                    this.answer_permission(Decision::Always, cx)
                                })),
                            )
                        })
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

    /// Keeps the newest content in view, but only for a reader who was already there.
    ///
    /// The transcript used to scroll to the bottom on every chunk of every token. For someone
    /// watching a reply arrive that is exactly right, and for someone who has scrolled up to check
    /// what the agent did four steps ago it is unusable: the view is snatched back to the end
    /// several times a second, and the harder the agent is working the worse it gets.
    ///
    /// So the end is followed only while the reader is at the end. Scrolling up is treated as what
    /// it is — a decision to read something — and is left alone until they come back down.
    fn follow_the_end(&mut self) {
        if self.is_at_the_end() {
            self.scroll_handle.scroll_to_bottom();
        }
    }

    /// Whether the transcript is scrolled to its end, give or take a line.
    ///
    /// The offset runs *negative* as the view moves down, so the end is where it reaches the
    /// negation of the maximum. The tolerance matters: content grows while a reply streams, and a
    /// reader sitting at the bottom would otherwise be judged to have scrolled up simply because
    /// a new line arrived between the last frame and this one.
    fn is_at_the_end(&self) -> bool {
        const SLACK: gpui::Pixels = px(48.);
        let offset = self.scroll_handle.offset().y;
        let max = self.scroll_handle.max_offset().y;
        (offset + max).abs() <= SLACK
    }

    /// Adds to what the model is working through, which is not part of its answer.
    fn extend_reasoning(&mut self, chunk: &str, cx: &mut Context<Self>) {
        let registry = self.language_registry.clone();
        let Some(message) = self.messages.last_mut() else {
            return;
        };
        message.reasoning.push_str(chunk);
        // Marked from the whole text rather than chunk by chunk, because a chunk can end halfway
        // through a word that only reads as code once it is complete.
        let source = crate::code_spans::mark_code(&message.reasoning);

        match message.reasoning_rendered.clone() {
            Some(markdown) => markdown.update(cx, |markdown, cx| markdown.replace(source, cx)),
            None => {
                let source = message.reasoning.clone();
                let markdown = render_markdown(&source, registry, cx);
                if let Some(message) = self.messages.last_mut() {
                    message.reasoning_rendered = Some(markdown);
                }
            }
        }

        self.follow_the_end();
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
        if let Some(step) = self.current_step() {
            // Anthropic reports input when the message starts and output when it ends, each time
            // with the other at zero, so a zero never replaces a count already known.
            if input > 0 {
                step.input_tokens = Some(input);
            }
            if output > 0 {
                step.output_tokens = Some(output);
            }
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

    fn set_working_folder(&mut self, folder: String, cx: &mut Context<Self>) {
        if self.thread.metadata.project.as_deref() == Some(folder.as_str()) {
            return;
        }
        self.thread.metadata.project = Some(folder);
        self.persist(cx);
        cx.notify();
    }

    /// Adds a folder to the project and makes it this thread's.
    ///
    /// Adding it to the project is the point: a folder the agent is told to work in but which the
    /// project cannot see would leave every path it tries unresolvable. Once it is a worktree,
    /// everything inside is readable, searchable and editable like the rest of the project.
    fn add_folder(&mut self, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add to project".into()),
        });

        let project = self.project.clone();
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };

            let added = project.update(cx, |project, cx| project.create_worktree(&path, true, cx));
            if let Err(error) = added.await {
                this.update(cx, |this, cx| {
                    this.error = Some(
                        format!(
                            "{} could not be added to the project: {error:#}",
                            path.display()
                        )
                        .into(),
                    );
                    cx.notify();
                })
                .log_err();
                return;
            }

            this.update(cx, |this, cx| {
                // The project's own spelling of the path, because that is what the folder menu
                // compares the thread's folder against.
                let folder = project_folders(&this.project, cx)
                    .into_iter()
                    .map(|(_, candidate)| candidate)
                    .find(|candidate| Path::new(candidate) == path)
                    .unwrap_or_else(|| path.to_string_lossy().into_owned());
                this.set_working_folder(folder, cx);
            })
            .log_err();
        })
        .detach();
    }

    /// Where the agent runs, and the way to change it.
    ///
    /// A menu rather than a prompt so that adding a folder sits beside choosing one, and so that a
    /// thread showing "No folder" — created before the folder was recorded, or in another window —
    /// can always be pointed at one.
    fn render_folder_menu(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let view = cx.weak_entity();
        let project = self.project.clone();
        let current = self.thread.metadata.project.clone();

        PopoverMenu::new("cowork-folder")
            .trigger(
                Button::new("cowork-folder-trigger", self.working_folder_label())
                    .start_icon(Icon::new(IconName::Folder).size(IconSize::Small))
                    .label_size(LabelSize::Small)
                    .tooltip(Tooltip::text("Choose or add the folder this thread works in")),
            )
            .menu(move |window, cx| {
                // Read when the menu opens, so a folder added to the project since is offered.
                let folders = project_folders(&project, cx);
                let view = view.clone();
                let current = current.clone();
                Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                    let has_folders = !folders.is_empty();
                    for (name, path) in folders {
                        let selected = current.as_deref() == Some(path.as_str());
                        let view = view.clone();
                        menu = menu.toggleable_entry(
                            name,
                            selected,
                            ui::IconPosition::Start,
                            None,
                            move |_window, cx| {
                                let path = path.clone();
                                view.update(cx, |view, cx| view.set_working_folder(path, cx))
                                    .log_err();
                            },
                        );
                    }
                    if has_folders {
                        menu = menu.separator();
                    }
                    menu.item(
                        ContextMenuEntry::new("Add folder…")
                            .icon(IconName::Plus)
                            .handler(move |_window, cx| {
                                view.update(cx, |view, cx| view.add_folder(cx)).log_err();
                            }),
                    )
                }))
            })
            .anchor(gpui::Anchor::TopLeft)
    }

    /// The branch checked out where this thread works, as a label: nothing here changes it.
    fn render_branch_chip(&self, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        let branch = self.branch_name(cx)?;

        Some(
            h_flex()
                .id("cowork-branch")
                .gap_1()
                .px_1()
                .tooltip(Tooltip::text("The git branch checked out in this thread's folder"))
                .child(
                    Icon::new(IconName::GitBranch)
                        .size(IconSize::Small)
                        .color(Color::Muted),
                )
                .child(Label::new(branch).size(LabelSize::Small).color(Color::Muted)),
        )
    }

    /// The branch of the repository this thread's folder is in, from the git store's own state.
    ///
    /// The innermost repository is the one that counts: a folder inside a nested checkout is on
    /// that checkout's branch, and the project's active repository may be another folder entirely.
    fn branch_name(&self, cx: &App) -> Option<SharedString> {
        let folder = Path::new(self.thread.metadata.project.as_deref()?);
        let repositories = self.project.read(cx).repositories(cx);
        let repository = containing_repository(
            folder,
            repositories
                .values()
                .map(|repository| (&*repository.read(cx).work_directory_abs_path, repository)),
        )?;
        let branch = repository.read(cx).branch.as_ref()?;
        Some(SharedString::from(branch.name().to_owned()))
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
            attachments: Vec::new(),
            thumbnails: Vec::new(),
            rendered,
        });
        self.follow_the_end();
    }

    /// Starts the record of the step whose assistant message was just pushed.
    fn begin_step(&mut self, cx: &App) {
        let app_version = AppVersion::global(cx).to_string();
        if let Some(message) = self.thread.messages.last_mut() {
            message.step = Some(StepRecord {
                started_at: now_seconds(),
                app_version,
                ..StepRecord::default()
            });
        }
    }

    /// The record of the step being streamed, on the stored copy of its assistant message.
    fn current_step(&mut self) -> Option<&mut StepRecord> {
        self.thread
            .messages
            .last_mut()
            .filter(|message| message.role == Role::Assistant)
            .and_then(|message| message.step.as_mut())
    }

    fn record_served(&mut self, served: Served) {
        let Some(step) = self.current_step() else {
            return;
        };
        // Logged only when it is news: Gemini names the model on every chunk, and a line per chunk
        // pushes the warnings before a reply out of the session log's excerpt.
        if let Some(model) = served
            .model
            .as_ref()
            .filter(|model| step.model.as_ref() != Some(*model))
        {
            match &served.provider {
                Some(provider) => {
                    log::info!("cowork: step answered by {model} through {provider}")
                }
                None => log::info!("cowork: step answered by {model}"),
            }
        }
        if served.model.is_some() {
            step.model = served.model;
        }
        if served.provider.is_some() {
            step.provider = served.provider;
        }
        if served.response_id.is_some() {
            step.response_id = served.response_id;
        }
    }

    fn record_stop(&mut self, reason: StopReason) {
        if let Some(step) = self.current_step() {
            step.stop_reason = Some(reason.as_str().to_owned());
        }
    }

    fn end_step(&mut self, took: std::time::Duration) {
        if let Some(step) = self.current_step() {
            step.duration_ms = Some(u64::try_from(took.as_millis()).unwrap_or(u64::MAX));
        }
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

        self.follow_the_end();
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
        let fs = self.fs.clone();
        let folders = project_folders(&self.project, cx);
        let setting = CoworkSettings::get_global(cx).instructions.clone();

        self.completion = Some(cx.spawn(async move |this, cx| {
            // Read at the start of every turn, so an edited rules file applies from the next message
            // without reopening the thread.
            let user_instructions =
                instructions::load_user_instructions(fs.as_ref(), &setting, paths::agents_file())
                    .await;
            let project_rules = instructions::load_rules(fs.as_ref(), &folders).await;
            if this
                .update(cx, |this, _| {
                    this.user_instructions = user_instructions;
                    this.project_rules = project_rules;
                })
                .is_err()
            {
                return;
            }

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
                        this.push_message(Role::Assistant, String::new(), cx);
                        this.begin_step(cx);
                    })
                    .is_err()
                {
                    return;
                }

                let started = std::time::Instant::now();
                let outcome = Self::stream_one_step(&this, http_client.clone(), request, cx).await;
                this.update(cx, |this, _| this.end_step(started.elapsed()))
                    .log_err();
                let (calls, stop) = match outcome {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        this.update(cx, |this, cx| this.finish_with_error(error, cx))
                            .log_err();
                        return;
                    }
                };

                // Calls are run whatever the stop reason says: some OpenAI-compatible servers send
                // them with `stop`, and judging by the reason dropped them and ended the turn.
                if calls.is_empty() {
                    let unfinished = match stop {
                        Some(StopReason::MaxTokens) => Some(
                            "The model reached its output limit before finishing its answer. Ask \
                             it to continue, or pick a model with a larger output limit.",
                        ),
                        Some(StopReason::Other) => Some(
                            "The provider ended the model's answer before it finished, for \
                             example with a content filter or an error of its own.",
                        ),
                        Some(StopReason::EndTurn | StopReason::ToolUse) | None => None,
                    };
                    this.update(cx, |this, cx| match unfinished {
                        Some(reason) => this.finish_with_error(anyhow!(reason), cx),
                        None => this.finish(cx),
                    })
                    .log_err();
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
                        this.thread.record(
                            ActivityKind::RepeatedCallsStopped,
                            format!(
                                "The model asked to {repeated} {} times in a row",
                                Self::MAX_IDENTICAL_CALLS
                            ),
                        );
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
    ) -> Result<(Vec<ToolCall>, Option<StopReason>)> {
        let mut stream = provider::stream_completion(http_client, request).await?;
        let mut calls: Vec<ToolCall> = Vec::new();
        let mut stop = None;

        while let Some(event) = stream.next().await {
            match event? {
                CompletionEvent::Text(chunk) => {
                    if this
                        .update(cx, |this, cx| this.extend_last_message(&chunk, cx))
                        .is_err()
                    {
                        return Ok((Vec::new(), None));
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
                        return Ok((Vec::new(), None));
                    }
                }
                CompletionEvent::Usage { input, output } => {
                    if this
                        .update(cx, |this, cx| this.record_usage(input, output, cx))
                        .is_err()
                    {
                        return Ok((Vec::new(), None));
                    }
                }
                CompletionEvent::CachedInput(tokens) => {
                    if this
                        .update(cx, |this, _| {
                            if let Some(step) = this.current_step() {
                                step.cached_input_tokens = Some(tokens);
                            }
                        })
                        .is_err()
                    {
                        return Ok((Vec::new(), None));
                    }
                }
                CompletionEvent::Served(served) => {
                    if this
                        .update(cx, |this, _| this.record_served(served))
                        .is_err()
                    {
                        return Ok((Vec::new(), None));
                    }
                }
                CompletionEvent::Stop(reason) => {
                    // Recorded rather than breaking on. OpenAI-compatible providers send the
                    // usage chunk *after* the one carrying `finish_reason`, so stopping here
                    // meant the token counts were never read and the context meter stayed
                    // empty. The stream ends on its own at `[DONE]`.
                    // The first reason given is kept. A later one only repeats the end of the
                    // stream, and letting it overwrite `ToolUse` dropped the calls.
                    if stop.is_none() {
                        stop = Some(reason);
                        this.update(cx, |this, _| this.record_stop(reason))
                            .log_err();
                    }
                }
            }
        }

        if calls.is_empty() {
            let text = this
                .update(cx, |this, _| {
                    this.messages
                        .last()
                        .map(|message| message.text.clone())
                        .unwrap_or_default()
                })
                .unwrap_or_default();
            // Checked before looking for calls in it: whatever a model writes after its output
            // has fallen apart is not something to run.
            if crate::inline_calls::degenerate(&text) {
                return Err(anyhow!(
                    "The model's reply fell apart into `<unk>` tokens, so nothing in it was run. \
                     Send the message again, or pick another model."
                ));
            }

            // A model that emitted no structured call may still have asked for one, in the prose.
            // The text is the only place left to look, and looking costs nothing when it is not
            // there.
            let salvaged = this
                .update(cx, |this, cx| this.salvage_inline_calls(cx))
                .unwrap_or_default();
            if !salvaged.is_empty() {
                return Ok((salvaged, Some(StopReason::ToolUse)));
            }
            if crate::inline_calls::unread_call(&text) {
                return Err(anyhow!(
                    "The model wrote a tool call as text, in a form Anna cannot read, instead of \
                     calling the tool, so it was not run. Send the message again, or pick a model \
                     that supports tool calling."
                ));
            }
        }

        Ok((calls, stop))
    }

    /// Promotes tool calls the model wrote into its answer to real ones.
    ///
    /// Returns what it found, and takes the text of the calls out of the message, because leaving
    /// it would show the user a JSON blob beside the card for the very same call.
    fn salvage_inline_calls(&mut self, cx: &mut Context<Self>) -> Vec<ToolCall> {
        let Some(message) = self.messages.last_mut() else {
            return Vec::new();
        };
        let found = crate::inline_calls::find(&message.text);
        if found.is_empty() {
            return Vec::new();
        }

        log::info!(
            "cowork: recovered {} tool call(s) the model wrote as text",
            found.len()
        );

        let calls = found
            .iter()
            .map(|inline| inline.call.clone())
            .collect::<Vec<_>>();
        let cleaned = crate::inline_calls::strip(&message.text, &found);

        message.text = cleaned.clone();
        message.tool_calls = calls.clone();
        message.rendered = (!cleaned.is_empty())
            .then(|| render_markdown(&cleaned, self.language_registry.clone(), cx));

        // The stored copy has to agree, or the next request sends the raw text back to the model
        // and it reads its own miswritten call as history.
        if let Some(stored) = self.thread.messages.last_mut() {
            stored.text = cleaned;
            stored.tool_calls = calls.clone();
        }

        cx.notify();
        calls
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
            checks: None,
            checkpoint: None,
            duration_ms: None,
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

        let started = std::time::Instant::now();
        let run = cx.update(|cx| tool.run(arguments, context, cx));
        let outcome = run.await;
        // Around the whole run, so a command that waited for the user's approval counts the wait.
        let duration_ms = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        match outcome {
            Ok(output) => ToolResult {
                call_id: call.id.clone(),
                content: output.content,
                is_error: false,
                path: output.path,
                diff: output.diff,
                checks: output.checks,
                checkpoint: output.checkpoint,
                duration_ms,
            },
            Err(failure) => ToolResult {
                duration_ms,
                ..error(format!("{failure:#}"))
            },
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
            attachments: Vec::new(),
            thumbnails: Vec::new(),
        });
        self.follow_the_end();
        cx.notify();
    }

    /// The whole system prompt this thread sends, as the session log shows it.
    fn current_system_prompt(&self, cx: &App) -> String {
        format!(
            "{}\n\n{}",
            instructions::stable(&self.user_instructions, &self.project_rules),
            instructions::environment(&self.agent_environment(cx))
        )
    }

    /// Where and when the agent is working, as the system prompt's changing part describes it.
    fn agent_environment(&self, cx: &App) -> instructions::Environment {
        let level = &CoworkSettings::get_global(cx).permission;
        let knowledge = self
            .store
            .read(cx)
            .catalog()
            .model(&self.thread.metadata.model)
            .and_then(|(_, model)| model.knowledge.clone());
        instructions::Environment {
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            date: today(),
            working_folder: self.thread.metadata.project.clone(),
            folders: project_folders(&self.project, cx),
            branch: self.branch_name(cx).map(|branch| branch.to_string()),
            permission: format!("{} ({})", permission_label(level), permission_detail(level)),
            model: self.thread.metadata.model.qualified(),
            knowledge,
        }
    }

    fn build_request(&self, cx: &App) -> Result<CompletionRequest> {
        let model = self.thread.metadata.model.clone();
        let store = self.store.read(cx);
        let (catalog_provider, catalog_model) = store.catalog().model(&model).with_context(|| {
            format!(
                "{} is not in the models.dev catalog. Refresh the catalog from the Anna panel, \
                 or pick another model.",
                model.qualified()
            )
        })?;

        let api_key = store.api_key(&model.provider_id).ok_or_else(|| {
            anyhow!(
                "{} has no API key. Add one from Settings → Anna → Providers, or set {} in the \
                 environment.",
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
            system: Some(instructions::stable(
                &self.user_instructions,
                &self.project_rules,
            )),
            system_context: Some(instructions::environment(&self.agent_environment(cx))),
            messages: self.thread.messages.clone(),
            tools: self.tools.definitions(),
            // Every model publishes its own ceiling, so asking for less would be leaving the
            // model's capability on the table for no reason.
            max_output_tokens: catalog_model.limit.and_then(|limit| limit.output),
        })
    }

    fn fail(&mut self, error: anyhow::Error, cx: &mut Context<Self>) {
        self.fail_after_step(error, None, cx);
    }

    /// A failure, with the step it ended when that step's message is gone: the model and response
    /// id are what to look up in the provider's own logs.
    fn fail_after_step(
        &mut self,
        error: anyhow::Error,
        step: Option<&StepRecord>,
        cx: &mut Context<Self>,
    ) {
        let message = format!("{error:#}");
        let detail = match step.and_then(describe_failed_step) {
            Some(step) => format!("{message} ({step})"),
            None => message.clone(),
        };
        log::warn!("cowork: completion failed: {detail}");
        self.thread.record(ActivityKind::TurnFailed, detail);
        self.error = Some(message.into());
        self.completion = None;
        // Written at once, so the record of the failure outlives the window it happened in. Some
        // failures end a turn nothing else would save: a missing key, or the repeated-call guard.
        self.persist(cx);
        cx.notify();
    }

    /// A failure once streaming was under way. An assistant turn that never received a chunk is
    /// dropped so the thread does not keep a blank message, but partial output is preserved.
    fn finish_with_error(&mut self, error: anyhow::Error, cx: &mut Context<Self>) {
        let mut dropped_step = None;
        if self
            .messages
            .last()
            .is_some_and(|message| message.role == Role::Assistant && message.text.is_empty())
        {
            self.messages.pop();
            dropped_step = self.thread.messages.pop().and_then(|message| message.step);
        }
        self.fail_after_step(error, dropped_step.as_ref(), cx);
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        self.completion = None;
        self.thread.record(
            ActivityKind::TurnFinished,
            match self.thread.metadata.context_tokens {
                Some(tokens) => format!(
                    "The model ended its turn at message {}; the provider reported {tokens} tokens \
                     of context",
                    self.thread.messages.len()
                ),
                None => format!(
                    "The model ended its turn at message {}",
                    self.thread.messages.len()
                ),
            },
        );
        self.persist(cx);
        cx.emit(CoworkThreadEvent::TitleChanged);
        cx.notify();
    }

    fn persist(&mut self, cx: &mut Context<Self>) {
        // A draft lives only in this view until `submit` sends its first message, so changing its
        // model or folder must not store it early.
        if self.is_draft {
            return;
        }
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
                    .child(self.render_folder_menu(cx))
                    .children(self.render_branch_chip(cx)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .children(self.render_context_meter(cx))
                    .child(self.render_changes_button(cx))
                    .child(
                        IconButton::new("cowork-export-session-log", IconName::Download)
                            .icon_size(IconSize::Small)
                            .icon_color(Color::Muted)
                            .tooltip(Tooltip::text("Export session log"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.export_session_log(&ExportSessionLog, window, cx)
                            })),
                    )
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
    /// Deliberately not a diff viewer of its own. Anna already has one — a multibuffer with staging,
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

    /// How the current permission level is shown, and changed.
    ///
    /// A menu rather than a switch because there are four positions and they are a ladder, not an
    /// on/off: each rung asks about a subset of what the one above it asks about.
    fn render_permission_toggle(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let level = CoworkSettings::get_global(cx).permission;
        let open = level == AgentPermission::Open;

        PopoverMenu::new("cowork-permission")
            .trigger(
                Button::new("cowork-permission-trigger", permission_label(&level))
                    .start_icon(
                        Icon::new(if open {
                            IconName::Warning
                        } else {
                            IconName::Lock
                        })
                        .size(IconSize::Small),
                    )
                    .label_size(LabelSize::Small)
                    .style(if open {
                        ButtonStyle::Tinted(ui::TintColor::Warning)
                    } else {
                        ButtonStyle::Subtle
                    })
                    .tooltip(Tooltip::text(permission_detail(&level))),
            )
            .menu({
                let fs = self.fs.clone();
                move |window, cx| {
                    let fs = fs.clone();
                    let current = level;
                    Some(ContextMenu::build(window, cx, move |mut menu, _, _| {
                        for candidate in [
                            AgentPermission::Ask,
                            AgentPermission::Standard,
                            AgentPermission::Trusted,
                            AgentPermission::Open,
                        ] {
                            let chosen = candidate;
                            let fs = fs.clone();
                            let selected = candidate == current;
                            menu = menu.toggleable_entry(
                                permission_detail(&candidate),
                                selected,
                                ui::IconPosition::Start,
                                None,
                                move |window, cx| choose_permission(fs.clone(), chosen, window, cx),
                            );
                        }
                        menu
                    }))
                }
            })
            .anchor(gpui::Anchor::BottomRight)
    }

    /// The home a new thread opens on. Quiet on purpose: the composer below is the thing to use.
    fn render_empty_state(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_1()
            .child(Label::new("What should we work on?").size(LabelSize::Large))
            .child(
                Label::new(
                    self.thread
                        .metadata
                        .project
                        .clone()
                        .unwrap_or_else(|| "No folder open".to_owned()),
                )
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            // The work that needs the user is the likeliest answer to the question above. A row
            // sends its hand-off prompt in this thread rather than opening another, which would
            // leave this home standing empty beside the thread that took its place.
            .children(self.waiting.as_ref().and_then(|waiting| {
                let view = cx.weak_entity();
                let on_open: OpenPrompt = Rc::new(move |prompt, window, cx| {
                    view.update(cx, |view, cx| view.send_now(prompt, window, cx))
                        .log_err();
                });
                waiting
                    .read(cx)
                    .render(on_open)
                    .map(|section| div().w_full().max_w(rems(40.)).pt_4().child(section))
            }))
    }

    /// Why the pending attachments cannot go to this thread's model, when any of them cannot.
    ///
    /// A model missing from the catalog is not judged here: `build_request` reports that it is
    /// missing, which is the real problem, and "cannot read images" would only hide it.
    fn refuse_unreadable_attachments(&self, cx: &App) -> Option<String> {
        let model_ref = &self.thread.metadata.model;
        let store = self.store.read(cx);
        let (_, model) = store.catalog().model(model_ref)?;
        let unreadable = unreadable_attachments(
            self.pending_attachments
                .iter()
                .map(|pending| &pending.attachment),
            model.accepts_images(),
            model.accepts_pdf(),
        );
        describe_unreadable(&model_ref.model_id, &unreadable)
    }

    /// Whether the chosen model can actually look at a picture.
    ///
    /// Read from the catalog's `modalities.input` rather than its `attachment` flag, which
    /// disagrees on hundreds of models. Offering an attachment a model will reject is worse than
    /// not offering one.
    fn model_accepts_images(&self, cx: &App) -> bool {
        self.store
            .read(cx)
            .catalog()
            .model(&self.thread.metadata.model)
            .is_some_and(|(_, model)| model.accepts_images())
    }

    /// Whether the chosen model reads PDFs, read from the same `modalities.input` as
    /// `model_accepts_images` and for the same reason.
    fn model_accepts_pdf(&self, cx: &App) -> bool {
        self.store
            .read(cx)
            .catalog()
            .model(&self.thread.metadata.model)
            .is_some_and(|(_, model)| model.accepts_pdf())
    }

    fn choose_images(&mut self, cx: &mut Context<Self>) {
        self.choose_attachments(crate::image::attach, cx);
    }

    /// Text files for any model, and PDFs for a model that reads them.
    ///
    /// Whether PDFs are allowed is settled when the picker opens, so the answer is the one the
    /// button's tooltip gave.
    fn choose_files(&mut self, cx: &mut Context<Self>) {
        let accepts_pdf = self.model_accepts_pdf(cx);
        self.choose_attachments(
            move |path, bytes| crate::document::attach(path, bytes, accepts_pdf),
            cx,
        );
    }

    /// Asks for files and attaches the ones `prepare` accepts.
    fn choose_attachments(
        &mut self,
        prepare: impl Fn(&Path, Vec<u8>) -> Result<Attachment> + Send + Sync + 'static,
        cx: &mut Context<Self>,
    ) {
        let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Attach".into()),
        });

        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            this.update(cx, |this, cx| this.attach_paths(paths, prepare, cx))
                .log_err();
        })
        .detach();
    }

    /// Reads `paths` and attaches the ones `prepare` accepts.
    ///
    /// Whatever the user picked or pasted is read and checked here rather than at send time, so a
    /// file that cannot be sent is refused while they are still looking at it — not after the
    /// message has gone. Preparing runs off the main thread: a bitmap is converted to PNG, which on
    /// a large screenshot takes long enough to be felt as a stall.
    fn attach_paths(
        &mut self,
        paths: Vec<PathBuf>,
        prepare: impl Fn(&Path, Vec<u8>) -> Result<Attachment> + Send + Sync + 'static,
        cx: &mut Context<Self>,
    ) {
        let fs = self.fs.clone();
        let prepare = Arc::new(prepare);

        cx.spawn(async move |this, cx| {
            let mut attached = Vec::new();
            let mut refused: Option<String> = None;
            for path in paths {
                match fs.load_bytes(&path).await {
                    Ok(bytes) => {
                        let prepared = cx
                            .background_spawn({
                                let prepare = prepare.clone();
                                async move { prepare(&path, bytes) }
                            })
                            .await;
                        match prepared {
                            Ok(attachment) => attached.push(attachment),
                            Err(error) => {
                                // Only the first refusal is reported: picking ten files and being
                                // told ten times about the same mistake is not ten times as useful.
                                refused.get_or_insert_with(|| format!("{error}"));
                            }
                        }
                    }
                    Err(error) => {
                        refused
                            .get_or_insert_with(|| format!("{path:?} could not be read: {error}"));
                    }
                }
            }

            this.update(cx, |this, cx| {
                this.pending_attachments.extend(attached);
                if let Some(refused) = refused {
                    this.error = Some(refused.into());
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    /// Pasting into the message field attaches what text cannot carry.
    ///
    /// Copied files become attachments, checked the way the buttons check them, and so does a
    /// picture copied on its own: a screenshot, or a browser's "Copy image". Anything else is left
    /// to the editor, which pastes text as it always has.
    fn paste(&mut self, _: &editor::actions::Paste, window: &mut Window, cx: &mut Context<Self>) {
        if !self.input.focus_handle(cx).is_focused(window) {
            return;
        }
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };

        match pasted(&item) {
            Pasted::Text => {}
            Pasted::Files(paths) => {
                cx.stop_propagation();
                let accepts_images = self.model_accepts_images(cx);
                let accepts_pdf = self.model_accepts_pdf(cx);
                self.attach_paths(
                    paths,
                    move |path, bytes| attach_file(path, bytes, accepts_images, accepts_pdf),
                    cx,
                );
            }
            Pasted::Image(bytes) => {
                cx.stop_propagation();
                self.attach_pasted_image(bytes, cx);
            }
        }
    }

    /// Attaches a picture that was pasted without a file, such as a screenshot.
    fn attach_pasted_image(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if !self.model_accepts_images(cx) {
            self.error = Some(
                format!(
                    "{} cannot read images, so the pasted picture was not attached. Pick a model \
                     that reads images and paste it again.",
                    self.thread.metadata.model.model_id
                )
                .into(),
            );
            cx.notify();
            return;
        }

        cx.spawn(async move |this, cx| {
            let prepared = cx
                .background_spawn(async move {
                    crate::image::attach_named(PASTED_IMAGE_NAME.to_owned(), bytes)
                })
                .await;
            this.update(cx, |this, cx| {
                match prepared {
                    Ok(attachment) => this.pending_attachments.extend([attachment]),
                    Err(error) => this.error = Some(format!("{error}").into()),
                }
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    /// What is waiting to be sent, each with a way to take it back off.
    ///
    /// A picture is a tile showing the picture, the way Claude Code draws it: seeing it is how the
    /// user knows they picked the right one. Its remove button appears only on hover, so it is not
    /// covering a corner of the picture the rest of the time. Anything else is a chip with its name.
    fn render_pending_attachments(&self, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        if self.pending_attachments.is_empty() {
            return None;
        }
        let colors = cx.theme().colors();

        Some(
            h_flex()
                .w_full()
                .px_3()
                .pt_2()
                .gap_1p5()
                .flex_wrap()
                .items_end()
                .children(self.pending_attachments.iter().enumerate().map(|(index, pending)| {
                    let name = SharedString::from(pending.attachment.name.clone());
                    let remove = IconButton::new(("cowork-unattach", index), IconName::Close)
                        .icon_size(IconSize::XSmall)
                        .tooltip(Tooltip::text("Remove"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // The button sits on the picture's tile, which opens the preview when
                            // clicked; removing a picture must not also open it.
                            cx.stop_propagation();
                            this.pending_attachments.remove(index);
                            cx.notify();
                        }));

                    match &pending.preview {
                        Some(preview) => {
                            let group =
                                SharedString::from(format!("cowork-pending-picture-{index}"));
                            div()
                                .id(("cowork-pending-picture", index))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.preview_pending_picture(index, window, cx)
                                }))
                                .group(group.clone())
                                .relative()
                                .flex_none()
                                .w(px(PENDING_PICTURE_SIZE))
                                .h(px(PENDING_PICTURE_SIZE))
                                .rounded_md()
                                .overflow_hidden()
                                .bg(colors.editor_background)
                                .tooltip(Tooltip::text(name))
                                .child(
                                    gpui::img(preview.image.clone())
                                        .size_full()
                                        .object_fit(gpui::ObjectFit::Contain),
                                )
                                .child(
                                    div().absolute().top_1().right_1().child(
                                        // Filled, so the cross stays visible over a light picture.
                                        remove
                                            .style(ButtonStyle::Filled)
                                            .visible_on_hover(group),
                                    ),
                                )
                                .into_any_element()
                        }
                        None => h_flex()
                            .px_1p5()
                            .py_0p5()
                            .gap_1()
                            .rounded_sm()
                            .border_1()
                            .border_color(colors.border)
                            .bg(colors.element_background)
                            .child(Icon::new(IconName::File).size(IconSize::XSmall))
                            .child(Label::new(name).size(LabelSize::Small))
                            .child(remove)
                            .into_any_element(),
                    }
                })),
        )
    }

    /// What the project's own tooling made of the files the agent just wrote.
    ///
    /// A tool that ran and found nothing and a tool that never started both report no findings, so
    /// a strip assembled from findings alone would be blank in both cases and misleading in one of
    /// them. `attached` is the only thing that tells them apart, which is why every server that
    /// looked gets a chip even when it has nothing to say, and why nothing having looked is stated
    /// outright: until now "is Biome actually working?" could not be answered by looking.
    ///
    /// Only the most recent exchange is shown. Anything older describes a file the agent may have
    /// changed again since, and a strip that accumulated them would push the composer it sits
    /// above off the screen.
    fn render_check_strip(&self, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        let reported = self
            .messages
            .iter()
            .rev()
            .find(|message| message.role == Role::Tool)?
            .tool_results
            .iter()
            .filter_map(|result| {
                let report = result.checks.as_ref()?;
                Some((SharedString::from(result.path.clone()), check_chips(report)))
            })
            .collect::<Vec<_>>();

        if reported.is_empty() {
            return None;
        }

        let colors = cx.theme().colors();

        Some(
            v_flex()
                .w_full()
                .px_3()
                .pt_2()
                .gap_1()
                .children(reported.into_iter().map(|(path, chips)| {
                    h_flex()
                        .w_full()
                        .gap_1p5()
                        .flex_wrap()
                        .child(Label::new(path).size(LabelSize::Small).color(Color::Muted))
                        .children(chips.into_iter().map(|(text, color)| {
                            h_flex()
                                .px_1p5()
                                .py_0p5()
                                .gap_1()
                                .rounded_sm()
                                .border_1()
                                .border_color(colors.border)
                                .bg(colors.element_background)
                                .child(Label::new(text).size(LabelSize::Small).color(color))
                        }))
                })),
        )
    }

    fn render_composer(&self, is_streaming: bool, cx: &Context<Self>) -> impl IntoElement {
        let accepts_images = self.model_accepts_images(cx);
        let accepts_pdf = self.model_accepts_pdf(cx);

        v_flex()
            .w_full()
            .child(Divider::horizontal())
            .children(self.render_check_strip(cx))
            .children(self.render_pending_attachments(cx))
            .child(
                h_flex()
                    .w_full()
                    .p_3()
                    .gap_2()
                    .items_end()
                    .bg(cx.theme().colors().panel_background)
                    .child(
                        IconButton::new("cowork-attach", IconName::Image)
                            .icon_size(IconSize::Small)
                            .disabled(!accepts_images)
                            .tooltip(Tooltip::text(if accepts_images {
                                "Attach an image"
                            } else {
                                "This model does not accept images"
                            }))
                            .on_click(cx.listener(|this, _, _, cx| this.choose_images(cx))),
                    )
                    // Never disabled, unlike the image button: every model reads text.
                    .child(
                        IconButton::new("cowork-attach-file", IconName::Attach)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text(if accepts_pdf {
                                "Attach text files or PDFs"
                            } else {
                                "Attach text files"
                            }))
                            .on_click(cx.listener(|this, _, _, cx| this.choose_files(cx))),
                    )
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

/// One chip per tool, in the order the file met them: the language servers that were attached,
/// then the checks that reported without one — the secret scan and the dependency audit are
/// compiled in and so never appear in `attached`.
fn check_chips(report: &CheckReport) -> Vec<(SharedString, Color)> {
    let mut chips = Vec::new();

    // The one state the per-tool chips cannot express, and the state most worth knowing: no
    // language server so much as opened the file.
    if report.attached.is_empty() {
        chips.push((SharedString::new_static("no checks ran"), Color::Muted));
    }

    for name in &report.attached {
        let findings = report
            .findings
            .iter()
            .filter(|finding| finding.check.eq_ignore_ascii_case(name))
            .collect::<Vec<_>>();
        chips.push(check_chip(name.clone(), &findings));
    }

    // Shown even when nothing was attached: a leaked credential is not made less true by the
    // absence of a language server.
    let mut unattached = Vec::new();
    for finding in &report.findings {
        let covered = report
            .attached
            .iter()
            .any(|name| finding.check.eq_ignore_ascii_case(name));
        if !covered && !unattached.contains(&finding.check) {
            unattached.push(finding.check.clone());
        }
    }
    for check in unattached {
        let findings = report
            .findings
            .iter()
            .filter(|finding| finding.check == check)
            .collect::<Vec<_>>();
        chips.push(check_chip(check.clone(), &findings));
    }

    if report.formatted {
        chips.push((SharedString::new_static("formatted"), Color::Muted));
    }

    chips
}

/// A tool's own chip: a tick when it was satisfied, otherwise how much it had to say, coloured by
/// the worst of it.
fn check_chip(name: SharedString, findings: &[&Finding]) -> (SharedString, Color) {
    if findings.is_empty() {
        return (format!("{name} \u{2713}").into(), Color::Success);
    }

    let worst = if findings
        .iter()
        .any(|finding| finding.severity == Severity::Error)
    {
        Color::Error
    } else {
        Color::Warning
    };

    (format!("{name} {}", findings.len()).into(), worst)
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

/// The two or three words on the button.
pub(crate) fn permission_label(level: &AgentPermission) -> &'static str {
    match level {
        AgentPermission::Ask => "Ask",
        AgentPermission::Standard => "Standard",
        AgentPermission::Trusted => "Trusted",
        AgentPermission::Open => "Open",
    }
}

/// The sentence in the menu and the tooltip, which says what the level actually does rather than
/// what it is called — a name alone does not tell anyone where the line is.
pub(crate) fn permission_detail(level: &AgentPermission) -> &'static str {
    match level {
        AgentPermission::Ask => "Ask before every command",
        AgentPermission::Standard => "Ask before anything that changes something",
        AgentPermission::Trusted => "Ask only before what cannot be undone",
        AgentPermission::Open => "Never ask, except outside this project",
    }
}

/// Moves to a level, confirming first when the move is the one that can cost something.
///
/// Only the last rung asks. Going from "ask me" to "run anything" is the direction with an
/// irreversible failure at the end of it, and it is a setting rather than a per-turn choice, so it
/// is not something to arrive at by a stray click. Every other move is undone by another click.
fn choose_permission(
    fs: Arc<dyn fs::Fs>,
    level: AgentPermission,
    window: &mut Window,
    cx: &mut App,
) {
    if level != AgentPermission::Open {
        write_permission(fs, level, cx);
        return;
    }

    let answer = window.prompt(
        gpui::PromptLevel::Warning,
        "Let the agent run commands without asking?",
        Some(
            "It will be able to run any command in this project — including ones that delete \
             files, push to a remote, or publish — with no further confirmation. Nothing in the \
             editor can undo those. Commands that reach outside this project will still ask.",
        ),
        &["Allow everything", "Cancel"],
        cx,
    );

    cx.spawn(async move |cx| {
        if answer.await.ok() != Some(0) {
            return;
        }
        cx.update(|cx| write_permission(fs, AgentPermission::Open, cx));
    })
    .detach();
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

fn write_permission(fs: Arc<dyn fs::Fs>, level: AgentPermission, cx: &mut App) {
    settings::update_settings_file(fs, cx, move |settings, _| {
        settings.cowork.get_or_insert_default().permission = Some(level);
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

/// The candidate whose working directory holds `folder`, the innermost when several do.
fn containing_repository<'a, T>(
    folder: &Path,
    repositories: impl IntoIterator<Item = (&'a Path, T)>,
) -> Option<T> {
    repositories
        .into_iter()
        // By component rather than by string, so `/work/app` does not claim `/work/application`.
        .filter(|(work_directory, _)| folder.starts_with(work_directory))
        .max_by_key(|(work_directory, _)| work_directory.components().count())
        .map(|(_, repository)| repository)
}

/// The local date, for the environment the agent is told about.
fn today() -> String {
    let offset = time::UtcOffset::current_local_offset().unwrap_or(time::UtcOffset::UTC);
    time::OffsetDateTime::now_utc()
        .to_offset(offset)
        .date()
        .to_string()
}

/// The name a picture pasted without a file is attached under.
const PASTED_IMAGE_NAME: &str = "Pasted image";

/// What a paste into the message field does with what the clipboard holds.
#[derive(Debug, PartialEq, Eq)]
enum Pasted {
    /// Left to the editor.
    Text,
    Files(Vec<PathBuf>),
    Image(Vec<u8>),
}

/// Files come first: macOS puts the names of files copied in Finder on the clipboard as text too,
/// and pasting them should attach the files rather than type their names. Text comes before a
/// picture: Excel puts a picture of copied cells beside their text, and pasting cells should give
/// their values rather than a screenshot of them.
fn pasted(item: &ClipboardItem) -> Pasted {
    let mut picture = None;
    let mut has_text = false;
    for entry in item.entries() {
        match entry {
            ClipboardEntry::ExternalPaths(paths) if !paths.paths().is_empty() => {
                return Pasted::Files(paths.paths().to_vec());
            }
            ClipboardEntry::String(text) if !text.text().is_empty() => has_text = true,
            ClipboardEntry::Image(image) if !image.bytes.is_empty() => {
                picture.get_or_insert(image);
            }
            _ => {}
        }
    }
    match picture {
        Some(image) if !has_text => Pasted::Image(image.bytes.clone()),
        _ => Pasted::Text,
    }
}

/// Prepares a pasted file, whatever kind it is.
///
/// Each button asks for one kind, but one paste can bring several, so the kind is read from each
/// file's bytes. The name settles only a bitmap, whose two-byte signature is too short to trust
/// alone.
fn attach_file(
    path: &Path,
    bytes: Vec<u8>,
    accepts_images: bool,
    accepts_pdf: bool,
) -> Result<Attachment> {
    let is_picture = crate::image::sniff(&bytes).is_some()
        || path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("bmp"));
    if !is_picture {
        return crate::document::attach(path, bytes, accepts_pdf);
    }
    if !accepts_images {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "image".to_owned());
        anyhow::bail!(
            "{name} is an image, and this model does not read images. Pick a model that does, or \
             paste the other files on their own."
        );
    }
    crate::image::attach(path, bytes)
}

/// The names of the attachments a model cannot read, by what they are.
#[derive(Debug, Default, PartialEq, Eq)]
struct UnreadableAttachments {
    images: Vec<String>,
    pdfs: Vec<String>,
}

/// Which attachments a model that reads images, PDFs, both or neither would refuse.
///
/// Text files never are: they are sent as ordinary text, which every model reads.
fn unreadable_attachments<'a>(
    attachments: impl IntoIterator<Item = &'a Attachment>,
    accepts_images: bool,
    accepts_pdf: bool,
) -> UnreadableAttachments {
    let mut unreadable = UnreadableAttachments::default();
    for attachment in attachments {
        match attachment.kind() {
            AttachmentKind::Image if !accepts_images => {
                unreadable.images.push(attachment.name.clone())
            }
            AttachmentKind::Pdf if !accepts_pdf => unreadable.pdfs.push(attachment.name.clone()),
            AttachmentKind::Image | AttachmentKind::Pdf | AttachmentKind::Text => {}
        }
    }
    unreadable
}

/// The error shown instead of sending: which files, what the model cannot read, and the two ways
/// out of it.
fn describe_unreadable(model: &str, unreadable: &UnreadableAttachments) -> Option<String> {
    let mut sentences = Vec::new();
    let mut kinds = Vec::new();
    for (kind, names) in [("images", &unreadable.images), ("PDFs", &unreadable.pdfs)] {
        if !names.is_empty() {
            sentences.push(format!("{model} cannot read {kind}: {}.", names.join(", ")));
            kinds.push(kind);
        }
    }
    if kinds.is_empty() {
        return None;
    }

    let refused = unreadable.images.len() + unreadable.pdfs.len();
    let pronoun = if refused == 1 { "it" } else { "them" };
    sentences.push(format!(
        "Remove {pronoun} or pick a model that reads {}.",
        kinds.join(" and ")
    ));
    Some(sentences.join(" "))
}

/// The project's open folders, as the name the user knows and the path a command runs in.
/// Who answered a step that failed, for a failure whose message is dropped with the step on it.
fn describe_failed_step(step: &StepRecord) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(model) = &step.model {
        parts.push(format!("answered by {model}"));
    }
    if let Some(provider) = &step.provider {
        parts.push(format!("through {provider}"));
    }
    if let Some(response_id) = &step.response_id {
        parts.push(format!("response id {response_id}"));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

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

/// How far a rewind reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RewindScope {
    ConversationAndCode,
    Conversation,
    Code,
}

impl RewindScope {
    /// As the menu names it, for the activity timeline.
    fn describe(self) -> &'static str {
        match self {
            RewindScope::ConversationAndCode => "conversation and code",
            RewindScope::Conversation => "conversation only",
            RewindScope::Code => "code only",
        }
    }

    fn rewinds_conversation(self) -> bool {
        self != RewindScope::Code
    }

    fn restores_code(self) -> bool {
        self != RewindScope::Conversation
    }
}

/// Names the toast a rewind leaves, so a second rewind replaces it rather than stacking another.
struct RewindNotice;

/// A permission request in full, for the activity timeline: what was wanted, how far it reaches and
/// what "always" would cover, then the specifics — for a command, the command — on lines of their own.
fn describe_permission_request(request: &PermissionRequest) -> String {
    let boundary = if request.always_ask {
        ", reaches outside the project"
    } else {
        ""
    };
    format!(
        "{} (tool `{}`, {:?}, scope `{}`{boundary})\n{}",
        request.title, request.tool, request.consequence, request.scope, request.detail
    )
}

/// Every file checkpoint recorded in these messages, oldest first.
fn checkpoints_from(messages: &[Message]) -> Vec<checkpoint::Checkpoint> {
    messages
        .iter()
        .flat_map(|message| &message.tool_results)
        .filter_map(|result| result.checkpoint.clone())
        .collect()
}

/// The toast after a rewind that restored code: the outcome's sentence and, when several files were
/// left alone, which ones and why, which the sentence alone has no room for.
fn describe_restore(outcome: &checkpoint::RestoreOutcome) -> String {
    let summary = outcome.summary();
    if outcome.skipped.len() < 2 {
        return summary;
    }

    let details = outcome
        .skipped
        .iter()
        .map(|(path, reason)| {
            let name = std::path::Path::new(path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            format!("{name} {reason}")
        })
        .collect::<Vec<_>>()
        .join("; ");
    // The summary is a finished sentence; the details belong inside it, before its full stop.
    format!("{} ({details}).", summary.trim_end_matches('.'))
}

impl CoworkThreadView {
    /// Decodes the pictures of any message whose attachments have not been decoded yet.
    ///
    /// Compared by count rather than tracked with a flag, because the attachments of the message
    /// just sent are filled in after its view was pushed.
    fn decode_thumbnails(&mut self) {
        for message in &mut self.messages {
            if message.thumbnails.len() != message.attachments.len() {
                message.thumbnails = message.attachments.iter().map(preview).collect();
            }
        }
    }
}

/// The tallest a row of sent pictures is drawn. A picture on its own grows to this; several share
/// a row and shrink together so the row still fits.
const PICTURE_ROW_HEIGHT: f32 = 360.;

/// How many pictures share a row before another starts. Past this each one is too small to make out.
const PICTURES_PER_ROW: usize = 4;

/// The space between pictures in a row, which the row's width has to account for.
const PICTURE_GAP: f32 = 6.;

/// A picture ready to draw, with the shape it was taken in.
#[derive(Clone)]
pub(crate) struct AttachmentPreview {
    pub(crate) image: Arc<gpui::Image>,
    /// Width over height, read from the file's own header. Laying pictures out by their real shape
    /// is what lets a row of them share one height without cropping any of them.
    pub(crate) aspect_ratio: f32,
    /// The picture's own width in pixels, so a small one is never blown up past it.
    pub(crate) natural_width: f32,
}

impl AttachmentPreview {
    /// The widest this picture is drawn in a row: its own width, or the width at which it would
    /// reach the row's height, whichever comes first.
    fn widest(&self) -> f32 {
        self.natural_width.min(PICTURE_ROW_HEIGHT * self.aspect_ratio)
    }
}

/// An attachment as something gpui can draw, when it is a picture in a format gpui knows.
pub(crate) fn preview(attachment: &Attachment) -> Option<AttachmentPreview> {
    use base64::Engine as _;

    let format = gpui::ImageFormat::from_mime_type(&attachment.media_type)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&attachment.data)
        .ok()?;

    // Only the header is read, not the pixels. A format the `image` crate is not built with, such
    // as SVG, still draws; it is laid out square, which is a guess about its shape, not a failure.
    let (natural_width, aspect_ratio) = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        .filter(|(width, height)| *width > 0 && *height > 0)
        .map_or((PICTURE_ROW_HEIGHT, 1.), |(width, height)| {
            (width as f32, width as f32 / height as f32)
        });

    Some(AttachmentPreview {
        image: Arc::new(gpui::Image::from_bytes(format, bytes)),
        aspect_ratio,
        natural_width,
    })
}

/// How wide a row of pictures wants to be when nothing constrains it.
///
/// Given to the row as its width, so the bubble around it grows to fit the pictures rather than to
/// fit whatever text came with them; the bubble's own cap still applies, and the row shrinks to it.
fn picture_row_width(row: &[AttachmentPreview]) -> f32 {
    let gaps = PICTURE_GAP * row.len().saturating_sub(1) as f32;
    row.iter().map(AttachmentPreview::widest).sum::<f32>() + gaps
}

/// What the user sent with a message, inside its bubble, the way Claude Code shows it.
///
/// Pictures are shown rather than named, in their own shape. Every picture in a row gets a share of
/// the width in proportion to its aspect ratio, which gives them all the same height: one picture
/// fills the row, several shrink together to fit it. Anything that is not a picture is a chip.
fn render_sent_attachments(
    index: usize,
    message: &MessageView,
    view: WeakEntity<CoworkThreadView>,
    cx: &App,
) -> impl IntoElement {
    let colors = cx.theme().colors();

    let mut pictures = Vec::new();
    let mut others = Vec::new();
    for (position, attachment) in message.attachments.iter().enumerate() {
        match message.thumbnails.get(position).cloned().flatten() {
            Some(preview) => pictures.push((position, attachment.name.clone(), preview)),
            None => others.push((position, attachment.name.clone())),
        }
    }
    let rows = pictures.chunks(PICTURES_PER_ROW).collect::<Vec<_>>();
    let width = rows
        .iter()
        .map(|row| {
            let previews = row
                .iter()
                .map(|(_, _, preview)| preview.clone())
                .collect::<Vec<_>>();
            picture_row_width(&previews)
        })
        .fold(0., f32::max);

    v_flex()
        .gap(px(PICTURE_GAP))
        .when(!rows.is_empty(), |this| this.w(px(width)).max_w_full())
        .children(rows.into_iter().map(|row| {
            h_flex()
                .w_full()
                .items_start()
                .gap(px(PICTURE_GAP))
                .children(row.iter().map(|(position, name, preview)| {
                    div()
                        .id(SharedString::from(format!(
                            "cowork-sent-picture-{index}-{position}"
                        )))
                        .cursor_pointer()
                        .on_click({
                            let view = view.clone();
                            let position = *position;
                            move |_, window, cx| {
                                view.update(cx, |view, cx| {
                                    view.preview_sent_picture(index, position, window, cx)
                                })
                                .log_err();
                            }
                        })
                        .flex_grow(preview.aspect_ratio)
                        .flex_basis(px(0.))
                        .min_w_0()
                        .max_w(px(preview.widest()))
                        .aspect_ratio(preview.aspect_ratio)
                        .rounded_md()
                        .overflow_hidden()
                        .bg(colors.editor_background)
                        .tooltip(Tooltip::text(name.clone()))
                        .child(
                            gpui::img(preview.image.clone())
                                .size_full()
                                .object_fit(gpui::ObjectFit::Contain),
                        )
                }))
        }))
        .when(!others.is_empty(), |this| {
            this.child(h_flex().gap_1p5().flex_wrap().children(others.into_iter().map(
                |(position, name)| {
                    h_flex()
                        .id(SharedString::from(format!(
                            "cowork-sent-attachment-{index}-{position}"
                        )))
                        .px_1p5()
                        .py_0p5()
                        .gap_1()
                        .rounded_sm()
                        .border_1()
                        .border_color(colors.border)
                        .bg(colors.editor_background)
                        .child(Icon::new(IconName::File).size(IconSize::XSmall))
                        .child(Label::new(name).size(LabelSize::Small))
                },
            )))
        })
}

impl Render for CoworkThreadView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Before anything borrows the theme, because asking for a language needs `cx` mutably.
        self.request_diff_languages(cx);
        self.decode_thumbnails();

        let colors = cx.theme().colors();
        let is_streaming = self.is_streaming();
        let markdown_style = MarkdownStyle::themed(MarkdownFont::Preview, window, cx);
        let model_label = self.thread.metadata.model.model_id.clone();

        // For each message, whether it or anything after it changed a file: what rewinding the code
        // to that message would have to undo. One pass from the end rather than a scan per message
        // per frame.
        let mut changes_after = vec![false; self.thread.messages.len()];
        let mut seen_change = false;
        for (index, message) in self.thread.messages.iter().enumerate().rev() {
            seen_change |= message
                .tool_results
                .iter()
                .any(|result| result.checkpoint.is_some());
            if let Some(slot) = changes_after.get_mut(index) {
                *slot = seen_change;
            }
        }

        let messages = self
            .messages
            .iter()
            .enumerate()
            .map(|(index, message)| match message.role {
                // The user's own turns are right-aligned and the model's are full width, so the
                // two are told apart by position before a single word is read.
                Role::User => {
                    let group = SharedString::from(format!("cowork-user-message-{index}"));
                    let has_file_changes = changes_after.get(index).copied().unwrap_or(false);

                    // Each row is right-aligned by its own `justify_end`, the layout the bubble had
                    // before these actions existed. Aligning the column's children with `items_end`
                    // instead drew the bubble on the first frame and lost it on a later repaint.
                    v_flex()
                        .id(("cowork-user-message", index))
                        .group(group.clone())
                        .w_full()
                        .gap_0p5()
                        .child(
                            h_flex().w_full().justify_end().child(
                                v_flex()
                                    .max_w(relative(0.75))
                                    .p_3()
                                    .gap_1()
                                    .rounded_md()
                                    .bg(colors.element_background)
                                    .child(
                                        Label::new("You")
                                            .size(LabelSize::XSmall)
                                            .color(Color::Muted),
                                    )
                                    // Above the words, as they were sent: the picture is usually what
                                    // the words are about.
                                    .when(!message.attachments.is_empty(), |this| {
                                        this.child(render_sent_attachments(index, message, cx.weak_entity(), cx))
                                    })
                                    // A message that was only a picture has no text to show.
                                    .when(!message.text.is_empty(), |this| {
                                        this.child(div().child(message.text.clone()))
                                    }),
                            ),
                        )
                        .child(h_flex().w_full().justify_end().child(
                            self.render_user_message_actions(
                                index,
                                &message.text,
                                group,
                                has_file_changes,
                                is_streaming,
                                cx,
                            ),
                        ))
                        .into_any_element()
                }
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
            .on_action(cx.listener(Self::export_session_log))
            // Before the editor's own paste, which would take only the text.
            .capture_action(cx.listener(Self::paste))
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
                    .when(is_empty, |this| this.child(self.render_empty_state(cx)))
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

    #[test]
    fn a_failed_step_is_described_by_what_the_provider_reported() {
        assert_eq!(describe_failed_step(&StepRecord::default()), None);

        let step = StepRecord {
            model: Some("claude-opus-5".to_owned()),
            response_id: Some("msg_01".to_owned()),
            ..StepRecord::default()
        };
        assert_eq!(
            describe_failed_step(&step).as_deref(),
            Some("answered by claude-opus-5, response id msg_01")
        );
    }

    fn attachment(media_type: &str, data: &str) -> Attachment {
        Attachment {
            media_type: media_type.to_owned(),
            data: data.to_owned(),
            name: "sent".to_owned(),
        }
    }

    fn png(width: u32, height: u32) -> String {
        use base64::Engine as _;

        let mut buffer = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::new(width, height))
            .write_to(&mut buffer, image::ImageFormat::Png)
            .expect("a PNG encodes");
        base64::engine::general_purpose::STANDARD.encode(buffer.into_inner())
    }

    #[test]
    fn a_sent_picture_keeps_the_shape_it_was_taken_in() {
        let wide = preview(&attachment("image/png", &png(400, 200))).expect("a PNG is drawable");
        assert_eq!(wide.image.format, gpui::ImageFormat::Png);
        assert_eq!(wide.aspect_ratio, 2.);
        assert_eq!(wide.natural_width, 400.);
    }

    #[test]
    fn a_picture_whose_header_cannot_be_read_is_still_drawn_square() {
        // Only the signature of a PNG: drawable bytes, no dimensions to read.
        let unknown = preview(&attachment("image/png", "iVBORw0KGgo=")).expect("still drawable");
        assert_eq!(unknown.aspect_ratio, 1.);
        assert_eq!(unknown.image.bytes, b"\x89PNG\r\n\x1a\n".to_vec());
    }

    #[test]
    fn anything_that_is_not_a_drawable_picture_falls_back_to_a_chip() {
        assert!(preview(&attachment("text/plain", "aGVsbG8=")).is_none());
        assert!(preview(&attachment("application/pdf", "JVBERi0=")).is_none());
        assert!(preview(&attachment("image/png", "not base64 at all!")).is_none());
    }

    #[test]
    fn pasted_files_are_attached_even_when_their_names_came_as_text() {
        let item = ClipboardItem {
            entries: vec![
                ClipboardEntry::String(gpui::ClipboardString::new("notes.txt".to_owned())),
                ClipboardEntry::ExternalPaths(gpui::ExternalPaths(
                    vec![PathBuf::from("/work/notes.txt")].into(),
                )),
            ],
        };
        assert_eq!(
            pasted(&item),
            Pasted::Files(vec![PathBuf::from("/work/notes.txt")])
        );
    }

    #[test]
    fn copied_text_stays_text_and_only_a_picture_on_its_own_is_attached() {
        let signature = b"\x89PNG\r\n\x1a\n".to_vec();
        let picture = gpui::Image::from_bytes(gpui::ImageFormat::Png, signature.clone());

        let cells = ClipboardItem {
            entries: vec![
                ClipboardEntry::String(gpui::ClipboardString::new("A1\tB1".to_owned())),
                ClipboardEntry::Image(picture.clone()),
            ],
        };
        assert_eq!(pasted(&cells), Pasted::Text);

        let screenshot = ClipboardItem {
            entries: vec![ClipboardEntry::Image(picture)],
        };
        assert_eq!(pasted(&screenshot), Pasted::Image(signature));
    }

    #[test]
    fn a_pasted_image_file_needs_a_model_that_reads_images() {
        let png = b"\x89PNG\r\n\x1a\n".to_vec();
        let error = attach_file(Path::new("shot.png"), png.clone(), false, true)
            .expect_err("a model without images refuses it");
        assert!(error.to_string().contains("shot.png"), "{error}");

        let attachment = attach_file(Path::new("shot.png"), png, true, false).unwrap();
        assert_eq!(attachment.media_type, "image/png");
    }

    #[test]
    fn pasted_text_and_pdf_files_are_attached_as_the_file_button_would() {
        let text = attach_file(
            Path::new("main.rs"),
            b"fn main() {}\n".to_vec(),
            false,
            false,
        )
        .unwrap();
        assert_eq!(text.media_type, crate::document::TEXT_MEDIA_TYPE);

        let pdf = attach_file(Path::new("paper.pdf"), b"%PDF-1.7\n".to_vec(), false, true).unwrap();
        assert_eq!(pdf.media_type, crate::document::PDF_MEDIA_TYPE);
    }

    #[test]
    fn a_picture_is_never_drawn_taller_than_the_row_or_wider_than_itself() {
        let screenshot = AttachmentPreview {
            image: Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, Vec::new())),
            aspect_ratio: 2.,
            natural_width: 1600.,
        };
        // At the row's height it is twice as wide as tall, long before its own 1600 pixels.
        assert_eq!(screenshot.widest(), PICTURE_ROW_HEIGHT * 2.);

        let icon = AttachmentPreview {
            natural_width: 48.,
            aspect_ratio: 1.,
            ..screenshot.clone()
        };
        assert_eq!(icon.widest(), 48.);

        // Two of them side by side want both widths and the gap between.
        assert_eq!(
            picture_row_width(&[screenshot, icon]),
            PICTURE_ROW_HEIGHT * 2. + 48. + PICTURE_GAP
        );
    }

    fn written(abs_path: &str, before: checkpoint::Before) -> ToolResult {
        ToolResult {
            call_id: abs_path.to_owned(),
            content: String::new(),
            is_error: false,
            path: String::new(),
            diff: String::new(),
            checks: None,
            checkpoint: Some(checkpoint::Checkpoint {
                abs_path: abs_path.to_owned(),
                before,
                after_digest: 0,
            }),
            duration_ms: None,
        }
    }

    #[test]
    fn each_rewind_scope_touches_only_what_it_names() {
        assert!(RewindScope::ConversationAndCode.rewinds_conversation());
        assert!(RewindScope::ConversationAndCode.restores_code());
        assert!(RewindScope::Conversation.rewinds_conversation());
        assert!(!RewindScope::Conversation.restores_code());
        assert!(!RewindScope::Code.rewinds_conversation());
        assert!(RewindScope::Code.restores_code());
    }

    #[test]
    fn a_rewind_collects_the_checkpoints_after_the_message_in_order() {
        let messages = vec![
            Message::user("first"),
            Message::tool_results(vec![written("/p/a.ts", checkpoint::Before::Missing)]),
            Message::user("second"),
            Message::tool_results(vec![
                written("/p/b.ts", checkpoint::Before::Text("b0".into())),
                written("/p/a.ts", checkpoint::Before::Text("a1".into())),
            ]),
        ];

        let from_second = checkpoints_from(messages.get(2..).unwrap_or_default());
        let paths = from_second
            .iter()
            .map(|checkpoint| checkpoint.abs_path.as_str())
            .collect::<Vec<_>>();
        assert_eq!(paths, ["/p/b.ts", "/p/a.ts"]);
        assert_eq!(checkpoints_from(&messages).len(), 3);
    }

    #[test]
    fn a_restore_that_skipped_several_files_says_which_and_why() {
        let outcome = checkpoint::RestoreOutcome {
            restored: vec!["/p/a.ts".into()],
            removed: Vec::new(),
            skipped: vec![
                ("/p/b.ts".into(), "has unsaved edits".into()),
                ("/p/c.ts".into(), "changed since the agent wrote it".into()),
            ],
        };
        assert_eq!(
            describe_restore(&outcome),
            "Restored 1 file, left 2 files alone \
             (b.ts has unsaved edits; c.ts changed since the agent wrote it)."
        );
    }

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

    #[test]
    fn removing_a_pending_attachment_takes_its_preview_with_it() {
        let named = |name: &str, media_type: &str, data: &str| Attachment {
            name: name.to_owned(),
            ..attachment(media_type, data)
        };
        let mut pending = PendingAttachments::new(vec![
            named("first.png", "image/png", "iVBORw0KGgo="),
            named("notes.txt", "text/plain", "aGk="),
            named("second.png", "image/png", "iVBORw0KGgo="),
        ]);

        pending.remove(0);

        // Each file that is left must still have its own preview: the text file a chip, the second
        // picture a tile — not the first picture's preview drawn under the text file's name.
        let remaining = pending
            .iter()
            .map(|entry| (entry.attachment.name.as_str(), entry.preview.is_some()))
            .collect::<Vec<_>>();
        assert_eq!(remaining, vec![("notes.txt", false), ("second.png", true)]);

        // A click on a tile that was already removed must not take the app down.
        pending.remove(5);

        let sent = pending.take();
        assert_eq!(
            sent.iter()
                .map(|attachment| attachment.name.as_str())
                .collect::<Vec<_>>(),
            vec!["notes.txt", "second.png"]
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn the_innermost_repository_holding_the_folder_is_the_one_whose_branch_is_shown() {
        let outer = Path::new(util::path!("/work/monorepo"));
        let inner = Path::new(util::path!("/work/monorepo/vendor/library"));
        let sibling = Path::new(util::path!("/work/monorepo-tools"));
        let repositories = [(outer, "outer"), (inner, "inner"), (sibling, "sibling")];

        assert_eq!(
            containing_repository(
                Path::new(util::path!("/work/monorepo/vendor/library/src")),
                repositories
            ),
            Some("inner"),
            "a nested checkout is on its own branch, not the outer repository's"
        );
        assert_eq!(
            containing_repository(Path::new(util::path!("/work/monorepo/app")), repositories),
            Some("outer")
        );
        assert_eq!(
            containing_repository(Path::new(util::path!("/work/monorepo-old")), repositories),
            None,
            "a shared prefix of characters is not a parent folder"
        );
        assert_eq!(
            containing_repository(Path::new(util::path!("/elsewhere")), repositories),
            None
        );
    }

    #[test]
    fn a_model_refuses_only_the_kinds_of_attachment_it_cannot_read() {
        let named = |name: &str, media_type: &str| Attachment {
            name: name.to_owned(),
            ..attachment(media_type, "")
        };
        let attachments = vec![
            named("screenshot.png", "image/png"),
            named("report.pdf", crate::document::PDF_MEDIA_TYPE),
            named("notes.txt", "text/plain"),
        ];

        let reads_text_only = unreadable_attachments(&attachments, false, false);
        assert_eq!(
            reads_text_only,
            UnreadableAttachments {
                images: vec!["screenshot.png".to_owned()],
                pdfs: vec!["report.pdf".to_owned()],
            },
            "text files are always readable"
        );
        assert_eq!(
            describe_unreadable("gpt-x", &reads_text_only).as_deref(),
            Some(
                "gpt-x cannot read images: screenshot.png. gpt-x cannot read PDFs: report.pdf. \
                 Remove them or pick a model that reads images and PDFs."
            )
        );

        let sees_pictures = unreadable_attachments(&attachments, true, false);
        assert!(sees_pictures.images.is_empty());
        assert_eq!(
            describe_unreadable("gpt-x", &sees_pictures).as_deref(),
            Some("gpt-x cannot read PDFs: report.pdf. Remove it or pick a model that reads PDFs.")
        );

        let reads_everything = unreadable_attachments(&attachments, true, true);
        assert_eq!(reads_everything, UnreadableAttachments::default());
        assert_eq!(describe_unreadable("claude", &reads_everything), None);
    }
}
