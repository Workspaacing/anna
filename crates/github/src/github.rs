//! GitHub, close enough to the code to be useful.
//!
//! The premise is that the valuable half of GitHub, for someone with the repository already open,
//! is not the part that renders boards. It is the part that says what is waiting for you and then
//! lets an agent act on it with the working tree right there — read the issue, read the diff, read
//! why the build failed, change the files, run the checks that already exist in this app, and only
//! then open a pull request. None of that is something a browser tab can do.
//!
//! So this crate deliberately does not reimplement project boards or roadmaps. It connects, it
//! shows the work, and it hands items to Cowork.

mod api;
mod auth;
mod github_window;
mod repositories;
mod worklist;

pub use api::{Client, Failure, Identity};
pub use github_window::{GitHubWindow, describe as describe_failure};
pub use worklist::{Item, Kind, Waiting, WaitingReason, fetch_waiting_in_repository};

use anyhow::Result;
use gpui::{App, Task};
use workspace::Workspace;
use anna_actions::OpenGitHub;

/// The token this app is connected with, for anything outside this crate that needs one.
///
/// `None` means nobody has connected yet, which is an ordinary state and not a failure — callers
/// are expected to say so rather than report an error.
pub fn stored_token(cx: &App) -> Task<Result<Option<String>>> {
    auth::stored(cx)
}

pub fn init(cx: &mut App) {
    // Registered twice, for the same reason settings is: the workspace handler is what a keybinding
    // or the activity bar reaches, and the global one is what is left when no window has focus.
    cx.on_action(|_: &OpenGitHub, cx| github_window::open(None, cx));

    cx.observe_new(|workspace: &mut Workspace, _window, _cx| {
        workspace.register_action(|_, _: &OpenGitHub, window, cx| {
            // Kept so work can be handed back to the window it came from. Without it the GitHub
            // window would have somewhere to send an issue but no idea where.
            let origin = window.window_handle().downcast::<workspace::MultiWorkspace>();
            github_window::open(origin, cx);
        });
    })
    .detach();
}
