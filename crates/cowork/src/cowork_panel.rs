use crate::{
    NewThread, OpenSettings, SelectModel, ToggleFocus,
    catalog::ModelRef,
    cowork_settings::CoworkSettings,
    model_selector::ModelSelector,
    thread::{CatalogState, CoworkStore, CoworkStoreEvent, ThreadId, ThreadMetadata, format_age},
    thread_view::CoworkThreadView,
};
use editor::{Editor, EditorEvent};
use fs::Fs;
use gpui::{
    Action, AsyncWindowContext, Entity, EventEmitter, FocusHandle, Focusable, Pixels, Subscription,
    WeakEntity, actions, uniform_list,
};
use settings::{DockSide, Settings as _};
use std::sync::Arc;
use ui::{ListItem, ListItemSpacing, Tooltip, prelude::*};
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

pub struct CoworkPanel {
    store: Entity<CoworkStore>,
    workspace: WeakEntity<Workspace>,
    fs: Arc<dyn Fs>,
    focus_handle: FocusHandle,
    search_editor: Entity<Editor>,
    query: String,
    visible_threads: Vec<ThreadMetadata>,
    selected_index: usize,
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
            subscriptions.push(cx.subscribe(&store, |this: &mut Self, _, event, cx| match event {
                CoworkStoreEvent::ThreadsChanged => {
                    this.refresh_visible_threads(cx);
                    cx.notify();
                }
                CoworkStoreEvent::CatalogChanged => cx.notify(),
            }));
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
                fs,
                focus_handle: cx.focus_handle(),
                search_editor,
                query: String::new(),
                visible_threads: Vec::new(),
                selected_index: 0,
                _subscriptions: subscriptions,
            };
            this.refresh_visible_threads(cx);
            this
        })
    }

    /// Threads are matched on their title and their preview so a search finds a conversation by
    /// what was said in it, not only by the prompt that named it.
    fn refresh_visible_threads(&mut self, cx: &mut Context<Self>) {
        let query = self.query.trim().to_lowercase();
        let threads = self.store.read(cx).threads();

        self.visible_threads = if query.is_empty() {
            threads.to_vec()
        } else {
            threads
                .iter()
                .filter(|thread| {
                    thread.title.to_lowercase().contains(&query)
                        || thread.preview.to_lowercase().contains(&query)
                        || thread.model.qualified().to_lowercase().contains(&query)
                })
                .cloned()
                .collect()
        };

        self.selected_index = self
            .selected_index
            .min(self.visible_threads.len().saturating_sub(1));
    }

    fn new_thread(&mut self, _: &NewThread, window: &mut Window, cx: &mut Context<Self>) {
        self.start_new_thread(window, cx);
    }

    pub fn start_new_thread(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(model) = self.store.read(cx).default_model(cx) else {
            self.report_no_model(cx);
            return;
        };
        self.open_new_thread(model, window, cx);
    }

    fn open_new_thread(&mut self, model: ModelRef, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        let thread = self
            .store
            .update(cx, |store, cx| store.create_thread(model, cx));
        let store = self.store.clone();
        let workspace_handle = self.workspace.clone();

        workspace.update(cx, |workspace, cx| {
            let project = workspace.project().clone();
            let view = cx.new(|cx| {
                CoworkThreadView::new(thread, store, workspace_handle, project, window, cx)
            });
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
                    let view = cx.new(|cx| {
                        CoworkThreadView::new(thread, store, workspace_handle, project, window, cx)
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
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.visible_threads.get(self.selected_index) else {
            return;
        };
        let id = thread.id.clone();
        self.store.update(cx, |store, cx| store.delete_thread(id, cx));
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

    /// Choosing here writes `cowork.default_model`, which is what new threads start on. An existing
    /// thread keeps the model it was created with; that one is changed from the thread's own header.
    fn open_model_selector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        // The environment may have gained a key since the catalog was last read, and a model whose
        // provider is not connected is never offered.
        self.store
            .update(cx, |store, cx| store.refresh_connections(cx));

        let selected = self.store.read(cx).default_model(cx);
        let fs = self.fs.clone();
        let on_confirm: Arc<dyn Fn(ModelRef, &mut Window, &mut App) + Send + Sync> =
            Arc::new(move |model, _window, cx| {
                let qualified = model.qualified();
                settings::update_settings_file(fs.clone(), cx, move |settings, _| {
                    settings.cowork.get_or_insert_default().default_model = Some(qualified);
                });
            });

        workspace.update(cx, |workspace, cx| {
            workspace.toggle_modal(window, cx, move |window, cx| {
                ModelSelector::new(selected, on_confirm, window, cx)
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
            "No model is available yet. Refresh the models.dev catalog and set a provider key in \
             your environment."
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
        let delete_id = thread.id.clone();

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
                    .on_click(cx.listener(move |this, _, _window, cx| {
                        let id = delete_id.clone();
                        this.store.update(cx, |store, cx| store.delete_thread(id, cx));
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
        let configured = CoworkSettings::get_global(cx).default_model.clone();
        let connected = store.connected_count();

        let (label, detail) = match store.default_model(cx) {
            Some(model) => (model.model_id, model.provider_id),
            None if connected == 0 => (
                "No provider connected".to_owned(),
                "Set an API key environment variable".to_owned(),
            ),
            None => (configured, "Not available from a connected provider".to_owned()),
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
            })
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
