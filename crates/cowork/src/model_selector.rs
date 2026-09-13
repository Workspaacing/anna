use crate::{
    catalog::{CatalogEntry, ModelRef},
    thread::{CoworkStore, credential_is_present},
};
use fuzzy::{StringMatch, StringMatchCandidate, match_strings};
use gpui::{DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Task};
use picker::{Picker, PickerDelegate};
use std::sync::Arc;
use ui::{ListItem, ListItemSpacing, prelude::*};
use workspace::ModalView;

pub struct ModelSelector {
    picker: Entity<Picker<ModelSelectorDelegate>>,
}

impl ModelSelector {
    pub fn new(
        selected: Option<ModelRef>,
        on_confirm: Arc<dyn Fn(ModelRef, &mut Window, &mut App) + Send + Sync>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let entries = CoworkStore::global(cx)
            .map(|store| store.read(cx).catalog_entries())
            .unwrap_or_default();

        let selector = cx.entity().downgrade();
        let delegate = ModelSelectorDelegate::new(
            Arc::new(move |cx: &mut App| {
                selector.update(cx, |_, cx| cx.emit(DismissEvent)).ok();
            }),
            entries,
            selected,
            on_confirm,
        );
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        Self { picker }
    }
}

impl Render for ModelSelector {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().w(rems(34.)).child(self.picker.clone())
    }
}

impl Focusable for ModelSelector {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl EventEmitter<DismissEvent> for ModelSelector {}
impl ModalView for ModelSelector {}

pub struct ModelSelectorDelegate {
    /// How to close whatever is hosting this picker. A callback rather than a `ModelSelector`
    /// handle, because the new-thread dialog hosts one too and wants escape to return to its
    /// fields rather than close the dialog.
    on_dismiss: Arc<dyn Fn(&mut App) + Send + Sync>,
    entries: Vec<CatalogEntry>,
    matches: Vec<StringMatch>,
    selected: Option<ModelRef>,
    selected_index: usize,
    on_confirm: Arc<dyn Fn(ModelRef, &mut Window, &mut App) + Send + Sync>,
}

impl ModelSelectorDelegate {
    pub(crate) fn new(
        on_dismiss: Arc<dyn Fn(&mut App) + Send + Sync>,
        entries: Vec<CatalogEntry>,
        selected: Option<ModelRef>,
        on_confirm: Arc<dyn Fn(ModelRef, &mut Window, &mut App) + Send + Sync>,
    ) -> Self {
        let matches = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| StringMatch {
                candidate_id: index,
                string: entry.label(),
                positions: Vec::new(),
                score: 0.0,
            })
            .collect();

        let selected_index = selected
            .as_ref()
            .and_then(|selected| {
                entries
                    .iter()
                    .position(|entry| &entry.model_ref == selected)
            })
            .unwrap_or(0);

        Self {
            on_dismiss,
            entries,
            matches,
            selected,
            selected_index,
            on_confirm,
        }
    }

    fn entry_at(&self, index: usize) -> Option<&CatalogEntry> {
        let candidate_id = self.matches.get(index)?.candidate_id;
        self.entries.get(candidate_id)
    }
}

impl PickerDelegate for ModelSelectorDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "cowork model selector"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Select a model…".into()
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix;
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let background = cx.background_executor().clone();
        let candidates = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| StringMatchCandidate::new(index, &entry.label()))
            .collect::<Vec<_>>();

        cx.spawn_in(window, async move |this, cx| {
            let matches = if query.is_empty() {
                candidates
                    .into_iter()
                    .enumerate()
                    .map(|(index, candidate)| StringMatch {
                        candidate_id: index,
                        string: candidate.string,
                        positions: Vec::new(),
                        score: 0.0,
                    })
                    .collect()
            } else {
                match_strings(
                    &candidates,
                    &query,
                    false,
                    true,
                    200,
                    &Default::default(),
                    background,
                )
                .await
            };

            this.update(cx, |this, cx| {
                this.delegate.matches = matches;
                this.delegate.selected_index = this
                    .delegate
                    .selected_index
                    .min(this.delegate.matches.len().saturating_sub(1));
                cx.notify();
            })
            .ok();
        })
    }

    fn confirm(&mut self, _secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        if let Some(entry) = self.entry_at(self.selected_index) {
            let model_ref = entry.model_ref.clone();
            let on_confirm = self.on_confirm.clone();
            cx.defer_in(window, move |_, window, cx| {
                on_confirm(model_ref, window, cx);
            });
        }
        self.dismissed(window, cx);
    }

    fn dismissed(&mut self, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        (self.on_dismiss)(cx);
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let entry = self.entry_at(ix)?;
        let is_current = self.selected.as_ref() == Some(&entry.model_ref);
        let has_credential = entry
            .env_var
            .as_deref()
            .is_some_and(credential_is_present);

        Some(
            ListItem::new(ix)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .start_slot(
                    Icon::new(if has_credential {
                        IconName::Sparkle
                    } else {
                        IconName::Lock
                    })
                    .size(IconSize::Small)
                    .color(if has_credential {
                        Color::Accent
                    } else {
                        Color::Muted
                    }),
                )
                .child(
                    v_flex()
                        .child(Label::new(entry.model_name.clone()))
                        .child(
                            h_flex()
                                .gap_1p5()
                                .child(
                                    Label::new(entry.provider_name.clone())
                                        .size(LabelSize::Small)
                                        .color(Color::Accent),
                                )
                                .child(
                                    Label::new("·")
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                )
                                .child(
                                    Label::new(entry.model_ref.model_id.clone())
                                        .size(LabelSize::XSmall)
                                        .color(Color::Muted),
                                )
                                .when(entry.reasoning, |this| {
                                    this.child(
                                        Label::new("reasoning")
                                            .size(LabelSize::XSmall)
                                            .color(Color::Muted),
                                    )
                                })
                                .when_some(entry.context_limit, |this, limit| {
                                    this.child(
                                        Label::new(format!("{}k ctx", limit / 1000))
                                            .size(LabelSize::XSmall)
                                            .color(Color::Muted),
                                    )
                                }),
                        ),
                )
                .when(is_current, |this| {
                    this.end_slot(Icon::new(IconName::Check).color(Color::Accent))
                }),
        )
    }
}
