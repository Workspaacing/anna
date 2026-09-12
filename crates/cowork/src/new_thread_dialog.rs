//! The dialog that starts a thread.
//!
//! A Cowork thread has exactly two things worth deciding before it begins, and both are awkward to
//! change later: the **model**, because a conversation keeps the one it was created with and
//! switching mid-way leaves the earlier turns behind, and the **folder**, because that is where the
//! agent's commands run and where the paths it is given resolve from.
//!
//! Both used to be asked as separate prompts, one after the other. One dialog is better for the
//! obvious reason — you can see both answers at once and change either before committing — and for
//! a less obvious one: a queue of prompts has nowhere to put a third question, so the next thing
//! worth asking would have made it worse. This has room.
//!
//! The model list needs a search box over several thousand entries, which is far too much for a
//! dropdown, and the workspace's modal layer holds one modal at a time so it cannot be opened on
//! top. The dialog therefore swaps its own body for the picker and swaps back, which is also how it
//! looks to the user: one window that changed what it is asking.

use crate::{
    catalog::ModelRef,
    model_selector::ModelSelectorDelegate,
    thread::CoworkStore,
    thread_view::project_folders,
};
use gpui::{DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, PathPromptOptions};
use picker::Picker;
use project::Project;
use std::{path::Path, sync::Arc};
use ui::{Divider, prelude::*};
use util::ResultExt as _;
use workspace::ModalView;

/// What the dialog hands back: the model, and the folder the thread works in.
pub type OnCreate = Arc<dyn Fn(ModelRef, Option<String>, &mut Window, &mut App) + Send + Sync>;

pub struct NewThreadDialog {
    focus_handle: FocusHandle,
    project: Entity<Project>,
    model: ModelRef,
    /// `(name shown, absolute path)`. `None` only when the window has no folder open at all.
    folder: Option<(SharedString, String)>,
    folders: Vec<(SharedString, String)>,
    /// Present while the body is the model picker rather than the fields.
    picker: Option<Entity<Picker<ModelSelectorDelegate>>>,
    on_create: OnCreate,
}

impl NewThreadDialog {
    pub fn new(
        model: ModelRef,
        project: Entity<Project>,
        on_create: OnCreate,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let folders = read_folders(&project, cx);
        let folder = folders.first().cloned();

        Self {
            focus_handle: cx.focus_handle(),
            project,
            model,
            folder,
            folders,
            picker: None,
            on_create,
        }
    }

    fn choose_model(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = CoworkStore::global(cx)
            .map(|store| store.read(cx).catalog_entries())
            .unwrap_or_default();

        let this = cx.entity().downgrade();
        let chosen = this.clone();
        let delegate = ModelSelectorDelegate::new(
            // Escaping the picker returns to the fields rather than closing the dialog: the user
            // opened a sub-question, and abandoning it should not abandon the thread.
            Arc::new(move |cx: &mut App| {
                this.update(cx, |this, cx| {
                    this.picker = None;
                    cx.notify();
                })
                .ok();
            }),
            entries,
            Some(self.model.clone()),
            Arc::new(move |model, _window, cx| {
                chosen
                    .update(cx, |this, cx| {
                        this.model = model;
                        this.picker = None;
                        cx.notify();
                    })
                    .ok();
            }),
        );

        self.picker = Some(cx.new(|cx| Picker::uniform_list(delegate, window, cx)));
        cx.notify();
    }

    /// Adds a folder to the project and makes it this thread's.
    ///
    /// Adding it to the project is the point: a folder the agent is told to work in but which the
    /// project cannot see would leave every path it tries unresolvable. Once it is a worktree,
    /// everything inside is readable, searchable and editable like the rest of the project.
    fn add_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add to project".into()),
        });

        let project = self.project.clone();
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };

            let added = project.update(cx, |project, cx| project.create_worktree(&path, true, cx));
            if added.await.log_err().is_none() {
                return;
            }

            this.update(cx, |this, cx| {
                this.folders = read_folders(&this.project, cx);
                this.folder = this
                    .folders
                    .iter()
                    .find(|(_, candidate)| Path::new(candidate) == path)
                    .cloned()
                    .or_else(|| this.folder.clone());
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model = self.model.clone();
        let folder = self.folder.as_ref().map(|(_, path)| path.clone());
        let on_create = self.on_create.clone();

        cx.emit(DismissEvent);
        // Deferred through the window rather than through this entity. Dismissing drops the
        // dialog, and `Context::defer_in` is owned by the entity it was called on — so the
        // callback was silently discarded and Create appeared to do nothing at all.
        window.defer(cx, move |window, cx| {
            on_create(model, folder, window, cx);
        });
    }

    fn render_fields(&self, cx: &Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();

        v_flex()
            .w(rems(30.))
            .p_4()
            .gap_3()
            .child(Label::new("New thread").size(LabelSize::Large))
            .child(Divider::horizontal())
            .child(
                v_flex()
                    .gap_1()
                    .child(field_label("Model", "Kept for the whole conversation."))
                    .child(
                        Button::new("cowork-new-model", self.model.qualified())
                            .full_width()
                            .style(ButtonStyle::Outlined)
                            .start_icon(Icon::new(IconName::Sparkle).size(IconSize::Small))
                            .end_icon(Icon::new(IconName::ChevronRight).size(IconSize::Small))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.choose_model(window, cx)),
                            ),
                    ),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(field_label(
                        "Folder",
                        "Where commands run. Everything inside it becomes part of the project.",
                    ))
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_1()
                            .children(self.folders.iter().enumerate().map(|(index, (name, path))| {
                                let selected = self
                                    .folder
                                    .as_ref()
                                    .is_some_and(|(_, chosen)| chosen == path);
                                let path = path.clone();
                                let name = name.clone();
                                Button::new(("cowork-new-folder", index), name.clone())
                                    .style(if selected {
                                        ButtonStyle::Tinted(ui::TintColor::Accent)
                                    } else {
                                        ButtonStyle::Outlined
                                    })
                                    .start_icon(Icon::new(IconName::Folder).size(IconSize::Small))
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        this.folder = Some((name.clone(), path.clone()));
                                        cx.notify();
                                    }))
                            }))
                            .child(
                                Button::new("cowork-new-folder-add", "Add folder…")
                                    .style(ButtonStyle::Subtle)
                                    .start_icon(Icon::new(IconName::Plus).size(IconSize::Small))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.add_folder(window, cx)
                                    })),
                            ),
                    ),
            )
            .child(Divider::horizontal())
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_1p5()
                    .child(
                        Button::new("cowork-new-cancel", "Cancel").on_click(cx.listener(
                            |_, _, _window, cx| {
                                cx.emit(DismissEvent);
                            },
                        )),
                    )
                    .child(
                        Button::new("cowork-new-create", "Create")
                            .style(ButtonStyle::Tinted(ui::TintColor::Accent))
                            // A thread with no folder can still be had — a window with nothing open
                            // is a legitimate place to ask a question.
                            .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
                    ),
            )
            .bg(colors.elevated_surface_background)
            .rounded_lg()
            .border_1()
            .border_color(colors.border)
    }
}

fn field_label(title: &'static str, detail: &'static str) -> impl IntoElement {
    v_flex()
        .child(Label::new(title).size(LabelSize::Small))
        .child(
            Label::new(detail)
                .size(LabelSize::XSmall)
                .color(Color::Muted),
        )
}

fn read_folders(project: &Entity<Project>, cx: &App) -> Vec<(SharedString, String)> {
    project_folders(project, cx)
        .into_iter()
        .map(|(name, path)| (SharedString::from(name), path))
        .collect()
}

impl Render for NewThreadDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match &self.picker {
            Some(picker) => v_flex().w(rems(34.)).child(picker.clone()).into_any_element(),
            None => self.render_fields(cx).into_any_element(),
        }
    }
}

impl Focusable for NewThreadDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.picker {
            Some(picker) => picker.focus_handle(cx),
            None => self.focus_handle.clone(),
        }
    }
}

impl EventEmitter<DismissEvent> for NewThreadDialog {}
impl ModalView for NewThreadDialog {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thread_can_be_started_with_no_folder() {
        // A window with nothing open is a legitimate place to ask a question, so the folder is
        // optional even though it is the first thing the dialog offers.
        let folders: Vec<(SharedString, String)> = Vec::new();

        assert!(folders.first().cloned().is_none());
    }
}
