//! Cowork is Wu's native AI pairing surface.
//!
//! It is split in two, matching how the rest of the workspace is organized: a dock panel
//! ([`CoworkPanel`]) owns the session history, search and provider status, while each conversation
//! opens as a regular workspace item ([`CoworkThreadView`]) in the center, next to the editors it is
//! about.
//!
//! Models come exclusively from the [models.dev](https://models.dev) catalog, the same registry the
//! AI SDK publishes. Provider credentials are kept in the operating system's credential store, never
//! in `settings.json`.

mod audit;
mod catalog;
pub mod checkpoint;
mod cowork_panel;
mod consequence;
mod cowork_settings;
mod image;
mod inline_calls;
mod fetch;
mod github_tools;
mod gitleaks;
mod outdated;
mod model_selector;
mod new_thread_dialog;
mod permission;
mod provider;
mod thread;
mod thread_view;
mod tool;
mod verify;

pub use audit::{Advisory, Package};
pub use catalog::{Catalog, CatalogEntry, ModelRef, POPULAR_PROVIDERS, Support};
pub use cowork_panel::CoworkPanel;
pub use cowork_settings::{CoworkSettings, VerificationSettings};
pub use thread::{ApiKeyMode, CatalogState, CoworkStore, ModelRow, ProviderRow};
pub use new_thread_dialog::NewThreadDialog;
pub use permission::{Decision, PermissionBroker, PermissionRequest};
pub use thread_view::CoworkThreadView;
pub use tool::{Tool, ToolKind, ToolOutput, ToolRegistry};
pub use verify::{Finding, Severity, VerificationReport};

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
        /// Opens the Cowork page of the settings window.
        OpenSettings,
        /// Refetches the models.dev catalog.
        RefreshCatalog,
    ]
);

pub fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|workspace, _: &ToggleFocus, window, cx| {
            workspace.toggle_panel_focus::<CoworkPanel>(window, cx);
        });
        workspace.register_action(
            |workspace, action: &wu_actions::StartThreadWith, window, cx| {
                let Some(panel) = workspace.panel::<CoworkPanel>(cx) else {
                    return;
                };
                let prompt = action.prompt.clone();
                panel.update(cx, |panel, cx| panel.start_thread_with(prompt, window, cx));
            },
        );
        workspace.register_action(|workspace, _: &NewThread, window, cx| {
            let Some(panel) = workspace.panel::<CoworkPanel>(cx) else {
                return;
            };
            panel.update(cx, |panel, cx| panel.start_new_thread(window, cx));
        });
    })
    .detach();
}
