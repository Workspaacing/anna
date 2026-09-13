//! A picture shown large, over everything else.
//!
//! Opened by clicking a picture's tile in the composer, to check it before it is sent, or a picture
//! in a message that was sent. It shows the picture whole — never cropped, never enlarged past its
//! own size — and steps through the other pictures that came with it.

use crate::thread_view::AttachmentPreview;
use gpui::{DismissEvent, EventEmitter, FocusHandle, Focusable, KeyDownEvent};
use ui::{Tooltip, prelude::*};
use workspace::ModalView;

/// The modal layer draws its content this far below the top of the window.
const MODAL_TOP: f32 = 80.;

/// Space kept clear around the picture, so there is always some of the window left to click to
/// close it.
const MARGIN: f32 = 32.;

/// The row under the picture with its name and the controls.
const CAPTION_HEIGHT: f32 = 40.;

pub(crate) struct ImagePreview {
    pictures: Vec<(SharedString, AttachmentPreview)>,
    index: usize,
    focus_handle: FocusHandle,
}

impl ImagePreview {
    pub(crate) fn new(
        pictures: Vec<(SharedString, AttachmentPreview)>,
        index: usize,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            index: index.min(pictures.len().saturating_sub(1)),
            pictures,
            focus_handle: cx.focus_handle(),
        }
    }

    /// Escape closes, the arrow keys step. Read from the key itself rather than from bound
    /// actions, so the preview behaves the same whatever the keymap binds in this context.
    fn handle_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => cx.emit(DismissEvent),
            "left" => self.step(false, cx),
            "right" => self.step(true, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        self.index = stepped(self.index, self.pictures.len(), forward);
        cx.notify();
    }
}

/// The picture after or before `index`, wrapping around at either end.
fn stepped(index: usize, count: usize, forward: bool) -> usize {
    if count == 0 {
        0
    } else if forward {
        (index + 1) % count
    } else {
        (index + count - 1) % count
    }
}

/// The size a picture is drawn at: its own, shrunk to fit the space when it is larger, and never
/// enlarged, because a small picture blown up shows its pixels rather than more of itself.
fn fitted_size(
    natural_width: f32,
    aspect_ratio: f32,
    available_width: f32,
    available_height: f32,
) -> (f32, f32) {
    let natural_height = natural_width / aspect_ratio;
    let scale = (available_width / natural_width)
        .min(available_height / natural_height)
        .min(1.);
    (natural_width * scale, natural_height * scale)
}

impl EventEmitter<DismissEvent> for ImagePreview {}

impl Focusable for ImagePreview {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ModalView for ImagePreview {
    /// A picture is looked at rather than worked beside, so the window behind it steps back.
    fn fade_out_background(&self) -> bool {
        true
    }
}

impl Render for ImagePreview {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((name, preview)) = self.pictures.get(self.index) else {
            return div().into_any_element();
        };

        let colors = cx.theme().colors();
        let viewport = window.viewport_size();
        let available_width = (f32::from(viewport.width) - MARGIN * 2.).max(1.);
        let available_height =
            (f32::from(viewport.height) - MODAL_TOP - MARGIN - CAPTION_HEIGHT).max(1.);
        let (width, height) = fitted_size(
            preview.natural_width,
            preview.aspect_ratio,
            available_width,
            available_height,
        );
        let count = self.pictures.len();

        v_flex()
            .key_context("ImagePreview")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::handle_key))
            .p_2()
            .gap_2()
            .rounded_lg()
            .border_1()
            .border_color(colors.border)
            .bg(colors.elevated_surface_background)
            .child(
                div()
                    .w(px(width))
                    .h(px(height))
                    .rounded_md()
                    .overflow_hidden()
                    .bg(colors.editor_background)
                    .child(
                        gpui::img(preview.image.clone())
                            .size_full()
                            .object_fit(gpui::ObjectFit::Contain),
                    ),
            )
            .child(
                h_flex()
                    // Never narrower than the controls, however small the picture.
                    .w(px(width.max(240.)))
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_ui_sm(cx)
                            .child(name.clone()),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .when(count > 1, |this| {
                                this.child(
                                    IconButton::new("image-preview-previous", IconName::ChevronLeft)
                                        .icon_size(IconSize::Small)
                                        .tooltip(Tooltip::text("Previous picture"))
                                        .on_click(cx.listener(|this, _, _, cx| this.step(false, cx))),
                                )
                                .child(
                                    Label::new(format!("{} / {count}", self.index + 1))
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                                .child(
                                    IconButton::new("image-preview-next", IconName::ChevronRight)
                                        .icon_size(IconSize::Small)
                                        .tooltip(Tooltip::text("Next picture"))
                                        .on_click(cx.listener(|this, _, _, cx| this.step(true, cx))),
                                )
                            })
                            .child(
                                IconButton::new("image-preview-close", IconName::Close)
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Close"))
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                            ),
                    ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_wraps_around_at_either_end() {
        assert_eq!(stepped(2, 3, true), 0);
        assert_eq!(stepped(0, 3, false), 2);
        assert_eq!(stepped(1, 3, true), 2);
        assert_eq!(stepped(0, 0, true), 0);
    }

    #[test]
    fn a_large_picture_shrinks_to_fit_and_keeps_its_shape() {
        // Width is the tighter limit for a wide picture, height for a tall one.
        assert_eq!(fitted_size(4000., 2., 1000., 800.), (1000., 500.));
        assert_eq!(fitted_size(1000., 0.5, 1600., 1000.), (500., 1000.));
    }

    #[test]
    fn a_small_picture_is_never_enlarged() {
        assert_eq!(fitted_size(200., 2., 1600., 1000.), (200., 100.));
    }
}
