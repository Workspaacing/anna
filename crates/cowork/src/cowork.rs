//! Cowork is Wu's native AI pairing surface.
//!
//! It is split in two, matching how the rest of the workspace is organized: a dock panel
//! ([`CoworkPanel`]) owns the session history, search and provider status, while each conversation
//! opens as a regular workspace item ([`CoworkThreadView`]) in the center, next to the editors it is
//! about.
//!
//! Models come exclusively from the [models.dev](https://models.dev) catalog, the same registry the
//! AI SDK publishes, and provider credentials are read from the environment variables that catalog
//! declares. Cowork never stores a key.

mod catalog;
mod cowork_panel;
mod cowork_settings;
mod model_selector;
mod provider;
mod thread;
mod thread_view;

pub use catalog::{Catalog, CatalogEntry, ModelRef};
pub use cowork_panel::CoworkPanel;
pub use cowork_settings::CoworkSettings;
pub use thread_view::CoworkThreadView;

use gpui::{App, actions};
use workspace::Workspace;

actions!(
    cowork,
    [
        /// Toggles focus on the Cowork panel.
        ToggleFocus,
        /// Starts a new Cowork thread in the center pane.
        NewThread,
        /// Sends the composed message to the model.
        Submit,
        /// Stops the response that is currently streaming.
        Cancel,
        /// Chooses the model for the active thread.
        SelectModel,
        /// Shows or hides the provider status section of the Cowork panel.
        ToggleSettings,
        /// Refetches the models.dev catalog.
        RefreshCatalog,
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            workspace.toggle_panel_focus::<CoworkPanel>(window, cx);
        });
        workspace.register_action(|workspace, _: &NewThread, window, cx| {
            let Some(panel) = workspace.panel::<CoworkPanel>(cx) else {
                return;
            };
            panel.update(cx, |panel, cx| panel.start_new_thread(window, cx));
        });
    })
    .detach();
}
