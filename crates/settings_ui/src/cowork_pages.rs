//! The Providers and Models sub-pages of the Cowork settings page.
//!
//! These are `SubPageLink`s rather than ordinary setting items because neither list comes from
//! settings JSON: providers are the models.dev catalog crossed with the process environment, and
//! models are whatever the connected providers offer. The declarative `SettingField` machinery has
//! nowhere to put either.
//!
//! Rows are resolved once by [`cowork::CoworkStore`], when the catalog loads or the settings window
//! opens, and both tables are virtualized. Nothing here probes the environment or sorts per frame:
//! the catalog carries 213 providers, and doing that work per frame is what made an earlier version
//! of this list stutter.

use crate::SettingsWindow;
use cowork::{ApiKeyMode, CoworkStore, ModelRow, ProviderRow};
use fs::Fs;
use std::sync::Arc;
use gpui::Entity as GpuiEntity;
use editor::{Editor, EditorEvent};
use gpui::{
    AnyElement, App, Context, DefiniteLength, Entity, ScrollHandle, Subscription, WeakEntity,
    Window, px,
};
use ui::{
    ColumnWidthConfig, Switch, Table, TableInteractionState, ToggleState, Tooltip, prelude::*,
};

/// A search box that survives across frames.
///
/// Sub-pages are rendered from a plain `fn`, so they have nowhere of their own to keep an editor.
/// `Window::use_state` keys the state to the call site instead. The subscription pokes the settings
/// window rather than this state entity, because the filtered table is drawn by the settings window
/// and would otherwise not repaint while typing.
struct PageState {
    editor: Entity<Editor>,
    table: Entity<TableInteractionState>,
    _subscription: Subscription,
}

impl PageState {
    fn new(
        placeholder: &'static str,
        settings_window: WeakEntity<SettingsWindow>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text(placeholder, window, cx);
            editor
        });
        let subscription = cx.subscribe(&editor, move |_, _, event: &EditorEvent, cx| {
            if matches!(event, EditorEvent::BufferEdited) {
                settings_window.update(cx, |_, cx| cx.notify()).ok();
            }
        });

        Self {
            editor,
            table: cx.new(|cx| TableInteractionState::new(cx)),
            _subscription: subscription,
        }
    }

    fn query(&self, cx: &App) -> String {
        self.editor.read(cx).text(cx).trim().to_lowercase()
    }
}

fn render_intro(
    heading: String,
    detail: &'static str,
    state: &PageState,
) -> impl IntoElement {
    v_flex()
        .w_full()
        .gap_2()
        .child(
            v_flex()
                .gap_0p5()
                .child(Label::new(heading))
                .child(
                    Label::new(detail)
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
        )
        .child(
            h_flex()
                .w_full()
                .gap_1p5()
                .child(
                    Icon::new(IconName::MagnifyingGlass)
                        .size(IconSize::Small)
                        .color(Color::Muted),
                )
                .child(div().flex_1().child(state.editor.clone())),
        )
}

pub(crate) fn render_providers(
    _settings_window: &SettingsWindow,
    _scroll_handle: &ScrollHandle,
    window: &mut Window,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let Some(store) = CoworkStore::global(cx) else {
        return unavailable("Cowork is still starting up.").into_any_element();
    };

    let settings_window = cx.entity().downgrade();
    let state = window.use_state(cx, move |window, cx| {
        PageState::new("Search providers…", settings_window, window, cx)
    });
    let state = state.read(cx);
    let query = state.query(cx);

    let store_handle = store.clone();
    let store = store.read(cx);
    let rows = store.provider_list();
    let connected = store.connected_count();

    let visible: Vec<usize> = if query.is_empty() {
        (0..rows.len()).collect()
    } else {
        rows.iter()
            .enumerate()
            .filter(|(_, row)| {
                row.name.to_lowercase().contains(&query)
                    || row.id.to_lowercase().contains(&query)
                    || row.env_label.to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    };

    v_flex()
        .size_full()
        .px_8()
        .gap_2()
        .child(render_intro(
            format!("{connected} of {} providers connected", rows.len()),
            "Cowork never stores an API key. A provider is connected when one of its environment \
             variables is set in the environment Wu was started from. Set it, then restart Wu.",
            state,
        ))
        .when(visible.is_empty(), |this| {
            this.child(unavailable_owned(format!("No provider matches “{query}”.")))
        })
        .when(!visible.is_empty(), |this| {
            this.child(
                Table::new(4)
                    .interactable(&state.table)
                    .striped()
                    .width_config(ColumnWidthConfig::explicit::<DefiniteLength>(vec![
                        px(28.).into(),
                        relative(0.34),
                        relative(0.5),
                        px(72.).into(),
                    ]))
                    .header(vec!["", "Provider", "Environment variable", "Models"])
                    .uniform_list("cowork-provider-table", visible.len(), {
                        let rows = rows.clone();
                        let store = store_handle.clone();
                        move |range, _window, _cx| {
                            range
                                .filter_map(|position| {
                                    let row = rows.get(*visible.get(position)?)?;
                                    Some(provider_row(row, &store))
                                })
                                .collect()
                        }
                    })
                    .into_any_element(),
            )
        })
        .children(render_api_key_dialog(&store_handle, cx))
        .into_any_element()
}

pub(crate) fn render_models(
    _settings_window: &SettingsWindow,
    _scroll_handle: &ScrollHandle,
    window: &mut Window,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let Some(store) = CoworkStore::global(cx) else {
        return unavailable("Cowork is still starting up.").into_any_element();
    };

    let settings_window = cx.entity().downgrade();
    let state = window.use_state(cx, move |window, cx| {
        PageState::new("Search models…", settings_window, window, cx)
    });
    let state = state.read(cx);
    let query = state.query(cx);

    let store_handle = store.clone();
    let fs = <dyn Fs>::global(cx);
    let rows = store.read(cx).model_list();
    if rows.is_empty() {
        return unavailable(
            "No models yet. Models appear here once a provider is connected, grouped by provider.",
        )
        .into_any_element();
    }

    let visible: Vec<usize> = if query.is_empty() {
        (0..rows.len()).collect()
    } else {
        rows.iter()
            .enumerate()
            .filter(|(_, row)| {
                row.name.to_lowercase().contains(&query)
                    || row.provider_name.to_lowercase().contains(&query)
                    || row.model.qualified().to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    };

    v_flex()
        .size_full()
        .px_8()
        .gap_2()
        .child(render_intro(
            format!("{} models available", rows.len()),
            "Only connected providers are listed. Pick a thread's model from the Cowork panel or \
             the thread header.",
            state,
        ))
        .when(visible.is_empty(), |this| {
            this.child(unavailable_owned(format!("No model matches “{query}”.")))
        })
        .when(!visible.is_empty(), |this| {
            this.child(
                Table::new(4)
                    .interactable(&state.table)
                    .striped()
                    .width_config(ColumnWidthConfig::explicit::<DefiniteLength>(vec![
                        px(56.).into(),
                        relative(0.3),
                        relative(0.2),
                        relative(0.5),
                    ]))
                    .header(vec!["Show", "Model", "Provider", "Capabilities"])
                    .uniform_list("cowork-model-table", visible.len(), {
                        let rows = rows.clone();
                        let store = store_handle.clone();
                        let fs = fs.clone();
                        move |range, _window, _cx| {
                            range
                                .filter_map(|position| {
                                    let row = rows.get(*visible.get(position)?)?;
                                    Some(model_row(row, &store, &fs))
                                })
                                .collect()
                        }
                    })
                    .into_any_element(),
            )
        })
        .into_any_element()
}

fn provider_row(row: &ProviderRow, store: &GpuiEntity<CoworkStore>) -> Vec<AnyElement> {
    let (icon, color, tooltip) = if !row.supported {
        (
            IconName::Warning,
            Color::Warning,
            "Cowork cannot talk to this provider yet: it needs a request format that is not \
             implemented.",
        )
    } else if row.stored {
        (
            IconName::Check,
            Color::Success,
            "Connected with a key saved in your OS credential store. Click to change or remove it.",
        )
    } else if row.connected {
        (
            IconName::Check,
            Color::Success,
            "Connected through the environment. Click to save a key in Cowork instead.",
        )
    } else {
        (
            IconName::Lock,
            Color::Muted,
            "Not connected. Click to add an API key.",
        )
    };

    let clickable = |element: Div, suffix: &'static str| {
        let store = store.clone();
        let row = row.clone();
        element
            .id(SharedString::from(format!("cowork-provider-{suffix}-{}", row.id)))
            .w_full()
            .h_full()
            .items_center()
            .cursor_pointer()
            .on_click(move |_, window, cx| {
                let row = row.clone();
                store.update(cx, |store, cx| store.begin_api_key(&row, window, cx));
            })
    };

    vec![
        clickable(h_flex(), "status")
            .justify_center()
            .child(Icon::new(icon).size(IconSize::Small).color(color))
            .tooltip(Tooltip::text(tooltip))
            .into_any_element(),
        clickable(h_flex(), "name")
            .child(
                Label::new(row.name.clone())
                    .size(LabelSize::Small)
                    .truncate_middle(),
            )
            .into_any_element(),
        clickable(h_flex(), "env")
            .child(
                Label::new(if row.stored {
                    SharedString::new_static("Saved in Cowork")
                } else {
                    row.env_label.clone()
                })
                .size(LabelSize::Small)
                .color(if row.connected {
                    Color::Success
                } else {
                    Color::Muted
                })
                .truncate_middle(),
            )
            .into_any_element(),
        clickable(h_flex(), "models")
            .child(
                Label::new(row.model_count.to_string())
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .into_any_element(),
    ]
}

/// Drawn inline rather than pushed onto a modal layer: the settings window is not a `Workspace`,
/// so it has none.
fn render_api_key_dialog(
    store: &GpuiEntity<CoworkStore>,
    cx: &mut Context<SettingsWindow>,
) -> Option<AnyElement> {
    let pending = store.read(cx).pending_api_key()?;
    let provider_name = pending.provider_name.clone();
    let env_label = pending.env_label.clone();
    let editor = pending.editor.clone();
    let mode = pending.mode;
    let error = pending.error.clone();
    let colors = cx.theme().colors();

    let confirm = {
        let store = store.clone();
        move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
            store.update(cx, |store, cx| match mode {
                ApiKeyMode::Connect => store.submit_api_key(cx),
                ApiKeyMode::Disconnect => store.remove_api_key(cx),
            });
        }
    };
    let cancel = {
        let store = store.clone();
        move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
            store.update(cx, |store, cx| store.cancel_api_key(cx));
        }
    };

    let (title, body) = match mode {
        ApiKeyMode::Connect => (
            format!("Connect {provider_name}"),
            format!(
                "The key is saved in your operating system's credential store, not in \
                 settings.json. Leaving this empty removes it and falls back to {env_label}."
            ),
        ),
        ApiKeyMode::Disconnect => (
            format!("Disconnect {provider_name}?"),
            format!(
                "The saved key is removed from your operating system's credential store. \
                 {provider_name} stays connected only if {env_label} is set in the environment."
            ),
        ),
    };

    Some(
        div()
            .occlude()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(colors.elevated_surface_background.opacity(0.8))
            .child(
                v_flex()
                    .id("cowork-api-key-dialog")
                    .w(rems(28.))
                    .p_4()
                    .gap_3()
                    .rounded_lg()
                    .border_1()
                    .border_color(colors.border)
                    .bg(colors.elevated_surface_background)
                    .child(
                        v_flex()
                            .gap_1()
                            .child(Label::new(title))
                            .child(Label::new(body).size(LabelSize::Small).color(Color::Muted)),
                    )
                    .when(mode == ApiKeyMode::Connect, |this| {
                        this.child(
                            div()
                                .w_full()
                                .px_2()
                                .py_1()
                                .rounded_md()
                                .border_1()
                                .border_color(colors.border)
                                .bg(colors.editor_background)
                                .child(editor),
                        )
                    })
                    .when_some(error, |this, error| {
                        this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
                    })
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .justify_end()
                            .child(
                                Button::new("cowork-api-key-cancel", "Cancel")
                                    .label_size(LabelSize::Small)
                                    .on_click(cancel),
                            )
                            .child(
                                Button::new(
                                    "cowork-api-key-confirm",
                                    match mode {
                                        ApiKeyMode::Connect => "Save",
                                        ApiKeyMode::Disconnect => "Remove key",
                                    },
                                )
                                .style(ButtonStyle::Filled)
                                .color(match mode {
                                    ApiKeyMode::Connect => Color::Default,
                                    ApiKeyMode::Disconnect => Color::Error,
                                })
                                .label_size(LabelSize::Small)
                                .on_click(confirm),
                            ),
                    ),
            )
            .into_any_element(),
    )
}

fn model_row(
    row: &ModelRow,
    store: &GpuiEntity<CoworkStore>,
    fs: &Arc<dyn Fs>,
) -> Vec<AnyElement> {
    let toggle = {
        let store = store.clone();
        let fs = fs.clone();
        let model = row.model.clone();
        Switch::new(
            SharedString::from(format!("cowork-model-toggle-{}", row.model.qualified())),
            if row.enabled {
                ToggleState::Selected
            } else {
                ToggleState::Unselected
            },
        )
        .on_click(move |state, _window, cx| {
            let enabled = *state == ToggleState::Selected;
            let model = model.clone();
            let fs = fs.clone();
            store.update(cx, |store, cx| {
                store.set_model_enabled(&model, enabled, fs, cx);
            });
        })
    };

    let dimmed = |label: Label| {
        if row.enabled {
            label
        } else {
            label.color(Color::Disabled)
        }
    };

    vec![
        h_flex()
            .h_full()
            .items_center()
            .child(toggle)
            .into_any_element(),
        h_flex()
            .h_full()
            .items_center()
            .child(dimmed(Label::new(row.name.clone()).size(LabelSize::Small)).truncate_middle())
            .into_any_element(),
        h_flex()
            .h_full()
            .items_center()
            .child(
                Label::new(row.provider_name.clone())
                    .size(LabelSize::Small)
                    .color(Color::Muted)
                    .truncate_middle(),
            )
            .into_any_element(),
        h_flex()
            .h_full()
            .items_center()
            .child(
                Label::new(if row.detail.is_empty() {
                    row.model.qualified()
                } else {
                    format!("{} · {}", row.model.qualified(), row.detail)
                })
                .size(LabelSize::Small)
                .color(Color::Muted)
                .truncate_middle(),
            )
            .into_any_element(),
    ]
}

fn unavailable(message: &'static str) -> impl IntoElement {
    unavailable_owned(message.to_owned())
}

fn unavailable_owned(message: String) -> impl IntoElement {
    v_flex()
        .w_full()
        .items_center()
        .justify_center()
        .p_4()
        .child(
            Label::new(message)
                .size(LabelSize::Small)
                .color(Color::Muted),
        )
}
