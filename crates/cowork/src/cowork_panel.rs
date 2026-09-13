use crate::{
    ExportAllSessionLogs, NewThread, OpenSettings, SelectModel, ToggleFocus,
    catalog::ModelRef,
    cowork_settings::CoworkSettings,
    model_selector::ModelSelector,
    session_log::{self, ExportedThread, LogContent, ThreadLog},
    thread::{
        CatalogState, CoworkStore, CoworkStoreEvent, Thread, ThreadId, ThreadMetadata, format_age,
    },
    thread_view::CoworkThreadView,
};
use editor::{Editor, EditorEvent};
use fs::Fs;
use project::Project;
use gpui::{
    Action, AsyncWindowContext, App, Entity, EventEmitter, FocusHandle, Focusable, Pixels,
    PromptLevel, Subscription, Task, WeakEntity, actions, uniform_list,
};
use settings::{DockSide, Settings as _};
use std::{path::Path, sync::Arc};
use ui::{
    ContextMenu, ContextMenuEntry, ListItem, ListItemSpacing, PopoverMenu, Tooltip, prelude::*,
};
use util::ResultExt as _;
use workspace::{
    Workspace,
    dock::{DockPosition, Panel, PanelEvent},
};

const COWORK_PANEL_KEY: &str = "CoworkPanel";

actions!(
    cowork,
    [
        /// Deletes the thread selected in the Cowork panel.
        DeleteSelectedThread,
        /// Moves the selection to the next thread in the Cowork panel.
        SelectNextThread,
        /// Moves the selection to the previous thread in the Cowork panel.
        SelectPreviousThread,
        /// Opens the thread selected in the Cowork panel.
        OpenSelectedThread,
    ]
);

/// Which threads an export of many covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ThreadSelection {
    /// The threads the panel lists for this project.
    ThisProject,
    /// Every stored thread, whatever project it was about.
    AllProjects,
}

/// A thread on its way into an export: already in hand from its view, or still being read back.
enum PendingThread {
    Open(ThreadLog),
    Stored {
        metadata: ThreadMetadata,
        load: Task<anyhow::Result<Thread>>,
    },
}

pub struct CoworkPanel {
    store: Entity<CoworkStore>,
    workspace: WeakEntity<Workspace>,
    /// Held directly rather than reached through the workspace.
    ///
    /// The panel is built inside `workspace.update_in`, so reading the `Workspace` entity from the
    /// constructor — or from anything the constructor calls — panics with "already being updated".
    /// The project handle is taken once, from the `&mut Workspace` we are handed.
    project: Entity<Project>,
    fs: Arc<dyn Fs>,
    focus_handle: FocusHandle,
    search_editor: Entity<Editor>,
    query: String,
    visible_threads: Vec<ThreadMetadata>,
    selected_index: usize,
    /// Whether the panel is showing in its dock, which is when an empty center becomes the home.
    active: bool,
    /// The home was wanted while the catalog was still loading, and opens once it has loaded.
    home_pending: bool,
    _subscriptions: Vec<Subscription>,
}

impl CoworkPanel {
    pub async fn load(
        workspace: WeakEntity<Workspace>,
        mut cx: AsyncWindowContext,
    ) -> anyhow::Result<Entity<Self>> {
        workspace.update_in(&mut cx, |workspace, window, cx| Self::new(workspace, window, cx))
    }

    fn new(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let fs = workspace.app_state().fs.clone();
        let project = workspace.project().clone();
        let workspace_handle = cx.entity().downgrade();
        let store = CoworkStore::global(cx).unwrap_or_else(|| {
            let store = cx.new(CoworkStore::new);
            CoworkStore::set_global(store.clone(), cx);
            store
        });

        cx.new(|cx| {
            let search_editor = cx.new(|cx| {
                let mut editor = Editor::single_line(window, cx);
                editor.set_placeholder_text("Search threads…", window, cx);
                editor
            });

            let mut subscriptions = Vec::new();
            subscriptions.push(cx.subscribe_in(
                &store,
                window,
                |this: &mut Self, _, event, window, cx| match event {
                    CoworkStoreEvent::ThreadsChanged => {
                        this.refresh_visible_threads(cx);
                        cx.notify();
                    }
                    CoworkStoreEvent::CatalogChanged => {
                        if this.home_pending {
                            this.show_home_later(window, cx);
                        }
                        cx.notify();
                    }
                },
            ));
            subscriptions.push(cx.subscribe(
                &search_editor,
                |this: &mut Self, editor, event: &EditorEvent, cx| {
                    if matches!(event, EditorEvent::BufferEdited) {
                        this.query = editor.read(cx).text(cx);
                        this.refresh_visible_threads(cx);
                        cx.notify();
                    }
                },
            ));

            store.update(cx, |store, cx| store.load_catalog(false, cx));

            let mut this = Self {
                store,
                workspace: workspace_handle,
                project,
                fs,
                focus_handle: cx.focus_handle(),
                search_editor,
                query: String::new(),
                visible_threads: Vec::new(),
                selected_index: 0,
                active: false,
                home_pending: false,
                _subscriptions: subscriptions,
            };
            this.refresh_visible_threads(cx);
            this
        })
    }

    /// The absolute path of the project's first folder, which is what a thread is scoped to.
    /// Every folder of this project, which is what a thread is matched against.
    ///
    /// A thread carries the one folder it was started in, but it has to stay visible when the
    /// project grows another one — see `ThreadMetadata::belongs_to`.
    fn project_folders(&self, cx: &App) -> Vec<String> {
        self.project
            .read(cx)
            .visible_worktrees(cx)
            .map(|worktree| worktree.read(cx).abs_path().to_string_lossy().into_owned())
            .collect()
    }

    /// Threads are matched on their title and their preview so a search finds a conversation by
    /// what was said in it, not only by the prompt that named it.
    ///
    /// Only this project's threads are listed. The index is shared by every window, so without
    /// that filter a panel would list every conversation the user has ever had, about any project.
    fn refresh_visible_threads(&mut self, cx: &mut Context<Self>) {
        let query = self.query.trim().to_lowercase();
        let folders = self.project_folders(cx);
        let threads = self.store.read(cx).threads();

        self.visible_threads = threads
            .iter()
            .filter(|thread| thread.belongs_to(&folders))
            .filter(|thread| {
                query.is_empty()
                    || thread.title.to_lowercase().contains(&query)
                    || thread.preview.to_lowercase().contains(&query)
                    || thread.model.qualified().to_lowercase().contains(&query)
            })
            .cloned()
            .collect();

        self.selected_index = self
            .selected_index
            .min(self.visible_threads.len().saturating_sub(1));
    }

    fn new_thread(&mut self, _: &NewThread, window: &mut Window, cx: &mut Context<Self>) {
        self.start_new_thread(window, cx);
    }

    /// Opens the home once the current update is over, if it should open at all.
    ///
    /// Deferred because the dock tells a panel it became active from inside its own update, and
    /// opening the home reads and updates the workspace that update belongs to.
    fn show_home_later(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        window.defer(cx, move |window, cx| {
            this.update(cx, |this, cx| this.show_home(window, cx))
                .log_err();
        });
    }

    fn show_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };
        let (thread_open, center_empty) = {
            let workspace = workspace.read(cx);
            (
                workspace
                    .items_of_type::<CoworkThreadView>(cx)
                    .next()
                    .is_some(),
                workspace.active_pane().read(cx).items_len() == 0,
            )
        };

        let store = self.store.read(cx);
        let decision = home_decision(HomeCircumstances {
            active: self.active,
            thread_open,
            center_empty,
            model_available: store.model_for_new_thread().is_some(),
            catalog_loading: matches!(
                store.catalog_state(),
                CatalogState::Idle | CatalogState::Loading
            ),
        });

        self.home_pending = decision == HomeDecision::WaitForCatalog;
        if decision == HomeDecision::Open {
            self.start_new_thread(window, cx);
        }
    }

    /// Opens a thread that already has something to work on, and starts it.
    ///
    /// Stored at once rather than drafted: the caller is handing over a specific piece of work,
    /// which is sent as the first message straight away, on the obvious model and folder — the
    /// model last used, and the project this window has open.
    pub fn start_thread_with(
        &mut self,
        prompt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.store.read(cx).model_for_new_thread() else {
            self.report_no_model(cx);
            return;
        };
        let folder = self.project_folders(cx).into_iter().next();
        self.open_thread_in_with(model, folder, Some(prompt), window, cx);
    }

    /// Opens a composer ready to type into, as Claude Code's home screen does.
    ///
    /// The thread behind it is a draft, stored only once its first message is sent, so a "+"
    /// pressed and closed again leaves nothing in the list. It starts on the model last used and
    /// the project's first folder, and both stay changeable from the thread's header. An open
    /// draft is brought forward instead of a second one being opened beside it.
    pub fn start_new_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        let existing = workspace
            .read(cx)
            .items_of_type::<CoworkThreadView>(cx)
            .find(|view| view.read(cx).is_draft());
        if let Some(existing) = existing {
            workspace.update(cx, |workspace, cx| {
                workspace.activate_item(&existing, true, true, window, cx);
            });
            existing.update(cx, |view, cx| view.focus_composer(window, cx));
            return;
        }

        // A key may have been added since the catalog was last read, and a model whose provider is
        // not connected is never offered.
        self.store
            .update(cx, |store, cx| store.refresh_connections(cx));
        let Some(model) = self.store.read(cx).model_for_new_thread() else {
            self.report_no_model(cx);
            return;
        };
        let folder = self.project_folders(cx).into_iter().next();
        let thread = self
            .store
            .update(cx, |store, _| store.draft_thread(model, folder));
        let store = self.store.clone();
        let workspace_handle = self.workspace.clone();

        workspace.update(cx, |workspace, cx| {
            let project = workspace.project().clone();
            let fs = workspace.app_state().fs.clone();
            let view = cx.new(|cx| {
                CoworkThreadView::draft(thread, store, workspace_handle, project, fs, window, cx)
            });
            workspace.add_item_to_active_pane(Box::new(view.clone()), None, true, window, cx);
            // After the item is added, which focuses the item itself rather than its composer.
            view.update(cx, |view, cx| view.focus_composer(window, cx));
        });
    }

    /// Opens a thread, optionally with its first message already written and sent.
    ///
    /// The message is handed to the view before the item is added to the pane, so the first thing
    /// the user sees is a thread already working — not an empty box that fills in a frame later.
    fn open_thread_in_with(
        &mut self,
        model: ModelRef,
        project: Option<String>,
        opening_message: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        self.store
            .update(cx, |store, cx| store.remember_model(model.clone(), cx));
        let thread = self
            .store
            .update(cx, |store, cx| store.create_thread(model, project, cx));
        let store = self.store.clone();
        let workspace_handle = self.workspace.clone();

        workspace.update(cx, |workspace, cx| {
            let project = workspace.project().clone();
            let fs = workspace.app_state().fs.clone();
            let view = cx.new(|cx| {
                CoworkThreadView::new(thread, store, workspace_handle, project, fs, window, cx)
            });
            if let Some(message) = opening_message {
                view.update(cx, |view, cx| view.send_now(message, window, cx));
            }
            workspace.add_item_to_active_pane(Box::new(view), None, true, window, cx);
        });
    }

    fn open_thread(&mut self, id: ThreadId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        // An already-open thread must be re-activated rather than opened twice; two views over one
        // thread would race each other when persisting.
        let existing = workspace.read(cx).items_of_type::<CoworkThreadView>(cx).find(
            |view| view.read(cx).thread_id() == &id,
        );
        if let Some(existing) = existing {
            workspace.update(cx, |workspace, cx| {
                workspace.activate_item(&existing, true, true, window, cx);
            });
            return;
        }

        let store = self.store.clone();
        let workspace_handle = self.workspace.clone();
        let load = self.store.read(cx).load_thread(id, cx);

        cx.spawn_in(window, async move |this, cx| {
            let thread = match load.await {
                Ok(thread) => thread,
                Err(error) => {
                    log::warn!("cowork: could not open a thread: {error:#}");
                    this.update(cx, |this, cx| {
                        this.report_error(format!("{error:#}"), cx);
                    })
                    .log_err();
                    return;
                }
            };

            workspace
                .update_in(cx, |workspace, window, cx| {
                    let project = workspace.project().clone();
                    let fs = workspace.app_state().fs.clone();
                    let view = cx.new(|cx| {
                        CoworkThreadView::new(
                            thread,
                            store,
                            workspace_handle,
                            project,
                            fs,
                            window,
                            cx,
                        )
                    });
                    workspace.add_item_to_active_pane(Box::new(view), None, true, window, cx);
                })
                .log_err();
        })
        .detach();
    }

    fn delete_selected_thread(
        &mut self,
        _: &DeleteSelectedThread,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.visible_threads.get(self.selected_index).cloned() else {
            return;
        };
        self.confirm_delete(thread, window, cx);
    }

    /// Asks before deleting, because nothing here can put a conversation back.
    ///
    /// Threads are not in the editor's undo history and are removed from the key-value store
    /// outright, so the prompt is the only thing standing between a mis-aimed keystroke and losing
    /// a conversation.
    fn confirm_delete(
        &mut self,
        thread: ThreadMetadata,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Delete “{}”?", thread.title),
            Some("This conversation cannot be recovered."),
            &["Delete", "Cancel"],
            cx,
        );

        let store = self.store.downgrade();
        let id = thread.id;
        cx.spawn(async move |_, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            store
                .update(cx, |store, cx| store.delete_thread(id, cx))
                .log_err();
        })
        .detach();
    }

    fn select_next_thread(
        &mut self,
        _: &SelectNextThread,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.visible_threads.is_empty() {
            self.selected_index = (self.selected_index + 1).min(self.visible_threads.len() - 1);
            cx.notify();
        }
    }

    fn select_previous_thread(
        &mut self,
        _: &SelectPreviousThread,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_index = self.selected_index.saturating_sub(1);
        cx.notify();
    }

    fn open_selected_thread(
        &mut self,
        _: &OpenSelectedThread,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(thread) = self.visible_threads.get(self.selected_index) {
            let id = thread.id.clone();
            self.open_thread(id, window, cx);
        }
    }

    fn select_model(&mut self, _: &SelectModel, window: &mut Window, cx: &mut Context<Self>) {
        self.open_model_selector(window, cx);
    }

    /// Choosing here changes what new threads start on **and** the thread being looked at.
    ///
    /// The footer and the thread's own header used to disagree: changing the model in the header
    /// updated the footer, but not the other way round, so the panel could name one model while
    /// the open conversation ran on another.
    fn open_model_selector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected = self.store.read(cx).model_for_new_thread();
        let store = self.store.downgrade();
        let workspace = self.workspace.clone();

        self.pick_model(
            selected,
            Arc::new(move |model, _window, cx| {
                store
                    .update(cx, |store, cx| store.remember_model(model.clone(), cx))
                    .log_err();
                workspace
                    .update(cx, |workspace, cx| {
                        if let Some(thread) = workspace.active_item_as::<CoworkThreadView>(cx) {
                            thread.update(cx, |thread, cx| thread.set_model(model, cx));
                        }
                    })
                    .log_err();
            }),
            window,
            cx,
        );
    }

    /// Opens the model picker and hands the choice to `on_chosen`.
    fn pick_model(
        &mut self,
        selected: Option<ModelRef>,
        on_chosen: Arc<dyn Fn(ModelRef, &mut Window, &mut App) + Send + Sync>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        // A key may have been added since the catalog was last read, and a model whose provider is
        // not connected is never offered.
        self.store
            .update(cx, |store, cx| store.refresh_connections(cx));

        workspace.update(cx, |workspace, cx| {
            workspace.toggle_modal(window, cx, move |window, cx| {
                ModelSelector::new(selected, on_chosen, window, cx)
            });
        });
    }

    fn open_settings(&mut self, _: &OpenSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.store
            .update(cx, |store, cx| store.refresh_connections(cx));
        window.dispatch_action(
            Box::new(wu_actions::OpenSettingsPage {
                page: "Cowork".to_owned(),
                target: None,
            }),
            cx,
        );
    }

    fn report_no_model(&mut self, cx: &mut Context<Self>) {
        self.report_error(
            "No model is available yet. Connect a provider in Settings > Cowork > Providers, or \
             refresh the models.dev catalog."
                .to_owned(),
            cx,
        );
    }

    fn report_error(&mut self, message: String, cx: &mut Context<Self>) {
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.show_error(message, cx);
            });
        }
    }

    fn export_all_session_logs(
        &mut self,
        _: &ExportAllSessionLogs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.export_threads(ThreadSelection::ThisProject, window, cx);
    }

    /// Saves many threads to one log file, each with everything its own export would show.
    ///
    /// A thread open in this window is taken from its view, which knows what is never stored:
    /// reasoning, check reports, the error on screen. The rest are read back from the database,
    /// and one that cannot be read is reported in its place rather than ending the export — the
    /// file is wanted most exactly when something is broken.
    pub(crate) fn export_threads(
        &mut self,
        selection: ThreadSelection,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let folders = self.project_folders(cx);
        let mut threads = self
            .store
            .read(cx)
            .threads()
            .iter()
            .filter(|thread| {
                selection == ThreadSelection::AllProjects || thread.belongs_to(&folders)
            })
            .cloned()
            .collect::<Vec<_>>();
        if threads.is_empty() {
            self.report_error("There are no Cowork threads to export yet.".to_owned(), cx);
            return;
        }
        // Oldest first, so the file reads in the order the work happened.
        threads.sort_by_key(|thread| thread.created_at);

        let folder_names = folders
            .iter()
            .map(|folder| {
                Path::new(folder)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| folder.clone())
            })
            .collect::<Vec<_>>();
        let (scope, subject) = match (selection, folder_names.first()) {
            (ThreadSelection::ThisProject, Some(first)) => (
                format!("the threads of this project ({})", folder_names.join(", ")),
                first.clone(),
            ),
            (ThreadSelection::ThisProject, None) => (
                "every stored thread: no folder is open in this window, so none are filtered out"
                    .to_owned(),
                "all-projects".to_owned(),
            ),
            (ThreadSelection::AllProjects, _) => (
                "every stored thread, from every project".to_owned(),
                "all-projects".to_owned(),
            ),
        };

        let open_views = self
            .workspace
            .upgrade()
            .map(|workspace| {
                workspace
                    .read(cx)
                    .items_of_type::<CoworkThreadView>(cx)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let store = self.store.read(cx);
        let mut pending = Vec::new();
        for metadata in threads {
            let open = open_views
                .iter()
                .find(|view| view.read(cx).thread_id() == &metadata.id);
            pending.push(match open {
                Some(view) => PendingThread::Open(view.read(cx).thread_log(cx)),
                None => PendingThread::Stored {
                    load: store.load_thread(metadata.id.clone(), cx),
                    metadata,
                },
            });
        }

        let content = cx.spawn(async move |_, _| {
            let mut exported = Vec::new();
            for thread in pending {
                exported.push(match thread {
                    PendingThread::Open(log) => ExportedThread::Loaded(log),
                    PendingThread::Stored { metadata, load } => match load.await {
                        Ok(thread) => ExportedThread::Loaded(ThreadLog::stored(thread)),
                        Err(error) => {
                            log::warn!("cowork: could not read a thread to export: {error:#}");
                            ExportedThread::Failed {
                                metadata,
                                error: format!("{error:#}"),
                            }
                        }
                    },
                });
            }
            LogContent::AllThreads {
                scope,
                threads: exported,
            }
        });

        session_log::export(
            "cowork-all-threads",
            &subject,
            content,
            self.fs.clone(),
            self.workspace.clone(),
            cx,
        );
    }

    /// A menu rather than a button: this project's threads are the usual want, but a problem that
    /// spans projects needs the other choice, and both belong in the same place.
    fn render_export_menu(&self, cx: &Context<Self>) -> impl IntoElement + use<> {
        let panel = cx.weak_entity();

        PopoverMenu::new("cowork-export-threads")
            .trigger(
                IconButton::new("cowork-export-threads-trigger", IconName::Download)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Export all threads")),
            )
            .menu(move |window, cx| {
                let panel = panel.clone();
                Some(ContextMenu::build(window, cx, move |menu, _, _| {
                    let export = |selection: ThreadSelection| {
                        let panel = panel.clone();
                        move |window: &mut Window, cx: &mut App| {
                            panel
                                .update(cx, |panel, cx| {
                                    panel.export_threads(selection, window, cx)
                                })
                                .log_err();
                        }
                    };

                    menu.header("Export all threads to one log file")
                        .item(
                            ContextMenuEntry::new("This project's threads")
                                .handler(export(ThreadSelection::ThisProject)),
                        )
                        .item(
                            ContextMenuEntry::new("All projects")
                                .handler(export(ThreadSelection::AllProjects)),
                        )
                }))
            })
            .anchor(gpui::Anchor::TopRight)
    }

    fn render_header(&self, cx: &Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .px_2()
            .py_1()
            .gap_1()
            .justify_between()
            .child(
                Label::new("Cowork")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .child(
                h_flex()
                    .gap_0p5()
                    .child(
                        IconButton::new("cowork-new-thread", IconName::Plus)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("New thread"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.new_thread(&NewThread, window, cx)
                            })),
                    )
                    .child(self.render_export_menu(cx))
                    .child(
                        IconButton::new("cowork-settings", IconName::Settings)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Cowork settings"))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_settings(&OpenSettings, window, cx)
                            })),
                    ),
            )
    }

    fn render_search(&self) -> impl IntoElement {
        h_flex()
            .w_full()
            .px_2()
            .pb_1()
            .gap_1p5()
            .child(
                Icon::new(IconName::MagnifyingGlass)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .child(div().flex_1().child(self.search_editor.clone()))
    }

    fn render_thread_list(&self, cx: &Context<Self>) -> impl IntoElement {
        let selected_index = self.selected_index;
        let count = self.visible_threads.len();

        uniform_list(
            "cowork-threads",
            count,
            cx.processor(move |this, range: std::ops::Range<usize>, _window, cx| {
                range
                    .filter_map(|index| {
                        let thread = this.visible_threads.get(index)?;
                        Some(this.render_thread(index, thread, index == selected_index, cx))
                    })
                    .collect()
            }),
        )
        .flex_1()
    }

    fn render_thread(
        &self,
        index: usize,
        thread: &ThreadMetadata,
        selected: bool,
        cx: &Context<Self>,
    ) -> ListItem {
        let id = thread.id.clone();
        // The whole record, because the confirmation names the thread being deleted.
        let to_delete = thread.clone();

        ListItem::new(("cowork-thread", index))
            .inset(true)
            .spacing(ListItemSpacing::Sparse)
            .toggle_state(selected)
            .start_slot(
                Icon::new(IconName::Chat)
                    .size(IconSize::Small)
                    .color(Color::Muted),
            )
            .child(
                v_flex()
                    .w_full()
                    .child(
                        h_flex()
                            .w_full()
                            .gap_1()
                            .justify_between()
                            .child(Label::new(thread.title.clone()).size(LabelSize::Small))
                            .child(
                                Label::new(format_age(thread.updated_at))
                                    .size(LabelSize::XSmall)
                                    .color(Color::Muted),
                            ),
                    )
                    .when(!thread.preview.is_empty(), |this| {
                        this.child(
                            Label::new(thread.preview.clone())
                                .size(LabelSize::XSmall)
                                .color(Color::Muted)
                                .truncate_middle(),
                        )
                    }),
            )
            .end_slot_on_hover(
                IconButton::new(("cowork-delete-thread", index), IconName::Trash)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Delete thread"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.confirm_delete(to_delete.clone(), window, cx);
                    })),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.selected_index = index;
                this.open_thread(id.clone(), window, cx);
            }))
    }

    /// A footer rather than a header entry: it is the least-used control on the panel, and putting
    /// it at the bottom keeps the thread list starting at the top.
    fn render_model_footer(&self, cx: &Context<Self>) -> impl IntoElement {
        let store = self.store.read(cx);
        let connected = store.connected_count();

        let (label, detail) = match store.model_for_new_thread() {
            Some(model) => (model.model_id, model.provider_id),
            None if connected == 0 => (
                "No provider connected".to_owned(),
                "Add an API key in Settings > Cowork > Providers".to_owned(),
            ),
            None => (
                "No model available".to_owned(),
                "Every model of your connected providers is hidden".to_owned(),
            ),
        };

        v_flex()
            .w_full()
            .border_t_1()
            .border_color(cx.theme().colors().border_variant)
            .child(
                h_flex()
                    .id("cowork-model-footer")
                    .w_full()
                    .px_2()
                    .py_1p5()
                    .gap_2()
                    .justify_between()
                    .cursor_pointer()
                    .hover(|style| style.bg(cx.theme().colors().element_hover))
                    .tooltip(Tooltip::text("Choose the model new threads start on"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_model_selector(window, cx)
                    }))
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1p5()
                            .child(
                                Icon::new(IconName::Sparkle)
                                    .size(IconSize::Small)
                                    .color(Color::Accent),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Label::new(label).size(LabelSize::Small).truncate_middle())
                                    .child(
                                        Label::new(detail)
                                            .size(LabelSize::XSmall)
                                            .color(Color::Muted)
                                            .truncate_middle(),
                                    ),
                            ),
                    )
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(IconSize::XSmall)
                            .color(Color::Muted),
                    ),
            )
    }

    fn render_empty_state(&self, cx: &Context<Self>) -> impl IntoElement {
        let has_query = !self.query.trim().is_empty();
        let catalog_state = self.store.read(cx).catalog_state().clone();

        v_flex()
            .flex_1()
            .items_center()
            .justify_center()
            .p_4()
            .gap_1()
            .child(
                Label::new(if has_query {
                    "No matching threads"
                } else {
                    "No threads yet"
                })
                .color(Color::Muted),
            )
            .when(!has_query, |this| {
                this.child(
                    Label::new(match catalog_state {
                        CatalogState::Loading => "Loading the models.dev catalog…",
                        CatalogState::Failed(_) => "The models.dev catalog could not be loaded.",
                        _ => "Start a thread to begin.",
                    })
                    .size(LabelSize::Small)
                    .color(Color::Muted),
                )
                // For a window that already has files open, where the home does not open on its
                // own: without this the only way in is the small "+" in the header.
                .child(
                    ui::Button::new("cowork-empty-new-thread", "New thread")
                        .start_icon(Icon::new(IconName::Plus).size(IconSize::Small))
                        .style(ui::ButtonStyle::Tinted(ui::TintColor::Accent))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.new_thread(&NewThread, window, cx)
                        })),
                )
            })
    }
}

/// What decides whether showing the panel opens the home in the center.
#[derive(Clone, Copy, Debug)]
struct HomeCircumstances {
    active: bool,
    thread_open: bool,
    center_empty: bool,
    model_available: bool,
    catalog_loading: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HomeDecision {
    Open,
    WaitForCatalog,
    Stay,
}

/// Whether showing the panel should open the home, the way Claude Code opens on one.
///
/// Only into an empty center: a window with files open is someone in the middle of editing, and a
/// tab appearing on its own would take their focus, so the panel offers a button instead. Nothing
/// opens beside a thread that is already open. And nothing opens, or complains about a missing
/// model, while the catalog that would supply one is still loading: the home waits for it.
fn home_decision(circumstances: HomeCircumstances) -> HomeDecision {
    if !circumstances.active || circumstances.thread_open || !circumstances.center_empty {
        HomeDecision::Stay
    } else if circumstances.model_available {
        HomeDecision::Open
    } else if circumstances.catalog_loading {
        HomeDecision::WaitForCatalog
    } else {
        HomeDecision::Stay
    }
}

impl Focusable for CoworkPanel {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<PanelEvent> for CoworkPanel {}

impl Panel for CoworkPanel {
    fn activation_focus_handle(&self, cx: &App) -> FocusHandle {
        self.search_editor.focus_handle(cx)
    }

    fn persistent_name() -> &'static str {
        "Cowork Panel"
    }

    fn panel_key() -> &'static str {
        COWORK_PANEL_KEY
    }

    fn position(&self, _window: &Window, cx: &App) -> DockPosition {
        match CoworkSettings::get_global(cx).dock {
            DockSide::Left => DockPosition::Left,
            DockSide::Right => DockPosition::Right,
        }
    }

    fn position_is_valid(&self, position: DockPosition) -> bool {
        matches!(position, DockPosition::Left | DockPosition::Right)
    }

    fn set_position(&mut self, position: DockPosition, _window: &mut Window, cx: &mut Context<Self>) {
        settings::update_settings_file(self.fs.clone(), cx, move |settings, _| {
            let dock = match position {
                DockPosition::Left | DockPosition::Bottom => DockSide::Left,
                DockPosition::Right => DockSide::Right,
            };
            settings.cowork.get_or_insert_default().dock = Some(dock);
        });
    }

    fn default_size(&self, _window: &Window, cx: &App) -> Pixels {
        CoworkSettings::get_global(cx).default_width
    }

    fn icon(&self, _window: &Window, cx: &App) -> Option<IconName> {
        CoworkSettings::get_global(cx)
            .button
            .then_some(IconName::Sparkle)
    }

    fn icon_tooltip(&self, _window: &Window, _cx: &App) -> Option<&'static str> {
        Some("Cowork")
    }

    fn toggle_action(&self) -> Box<dyn Action> {
        Box::new(ToggleFocus)
    }

    fn activation_priority(&self) -> u32 {
        4
    }

    fn set_active(&mut self, active: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.active = active;
        if active {
            self.show_home_later(window, cx);
        } else {
            self.home_pending = false;
        }
    }

    fn hide_button_setting(&self, _cx: &App) -> Option<workspace::HideStatusItem> {
        Some(workspace::HideStatusItem::new(|settings| {
            settings.cowork.get_or_insert_default().button = Some(false);
        }))
    }
}

impl Render for CoworkPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let has_threads = !self.visible_threads.is_empty();

        v_flex()
            .key_context("CoworkPanel")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(Self::new_thread))
            .on_action(cx.listener(Self::open_settings))
            .on_action(cx.listener(Self::select_model))
            .on_action(cx.listener(Self::delete_selected_thread))
            .on_action(cx.listener(Self::select_next_thread))
            .on_action(cx.listener(Self::select_previous_thread))
            .on_action(cx.listener(Self::open_selected_thread))
            .on_action(cx.listener(Self::export_all_session_logs))
            .size_full()
            .bg(cx.theme().colors().panel_background)
            .child(self.render_header(cx))
            .child(self.render_search())
            .map(|this| {
                if has_threads {
                    this.child(self.render_thread_list(cx))
                } else {
                    this.child(self.render_empty_state(cx))
                }
            })
            .child(self.render_model_footer(cx))
    }
}

#[cfg(test)]
mod home_tests {
    use super::*;

    fn empty_window() -> HomeCircumstances {
        HomeCircumstances {
            active: true,
            thread_open: false,
            center_empty: true,
            model_available: true,
            catalog_loading: false,
        }
    }

    #[test]
    fn an_empty_window_opens_on_the_home_when_the_panel_shows() {
        assert_eq!(home_decision(empty_window()), HomeDecision::Open);
    }

    #[test]
    fn nothing_opens_over_work_already_on_screen() {
        let files_open = HomeCircumstances {
            center_empty: false,
            ..empty_window()
        };
        let thread_open = HomeCircumstances {
            thread_open: true,
            ..empty_window()
        };
        let panel_hidden = HomeCircumstances {
            active: false,
            ..empty_window()
        };
        assert_eq!(home_decision(files_open), HomeDecision::Stay);
        assert_eq!(home_decision(thread_open), HomeDecision::Stay);
        assert_eq!(home_decision(panel_hidden), HomeDecision::Stay);
    }

    #[test]
    fn the_home_waits_for_the_catalog_rather_than_reporting_no_model() {
        let loading = HomeCircumstances {
            model_available: false,
            catalog_loading: true,
            ..empty_window()
        };
        let loaded_without_a_model = HomeCircumstances {
            model_available: false,
            catalog_loading: false,
            ..empty_window()
        };
        assert_eq!(home_decision(loading), HomeDecision::WaitForCatalog);
        assert_eq!(home_decision(loaded_without_a_model), HomeDecision::Stay);
    }
}
