//! The GitHub window: one screen, opened from the side of the app.
//!
//! It is a window of its own rather than a tab, for the same reason settings is one. What is on it
//! is not a document, nothing about it belongs in a split, and it is worth the whole screen while
//! it is open — a list of work with the code behind it.

use editor::Editor;
use gpui::{
    App, Context, DEFAULT_ADDITIONAL_WINDOW_SIZE, Entity, FocusHandle, Focusable, Task,
    TitlebarOptions, Window, WindowBounds, WindowOptions, point, px,
};
use settings::Settings as _;
use ui::{Divider, Tooltip, prelude::*};
use util::ResultExt as _;
use workspace::{WorkspaceSettings, client_side_decorations};

use crate::{
    api::{Client, Failure, Identity},
    auth,
    repositories::{self, Owner, OwnerKind, Repository},
    worklist::{self, Item, Kind, Worklist},
};
use gpui::WindowHandle;
use workspace::MultiWorkspace;

/// How wide the content is allowed to get before it stops following the window.
///
/// A list of issues set in a line that runs the width of a 27-inch monitor is unreadable; the eye
/// loses the row on the way back. The window still fills the screen — the text inside does not.
const CONTENT_MAX_WIDTH: Pixels = px(1100.);

/// Opens the GitHub window, or brings the open one forward.
pub fn open(origin: Option<WindowHandle<MultiWorkspace>>, cx: &mut App) {
    if let Some(existing) = cx
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<GitHubWindow>())
    {
        existing
            .update(cx, |this, window, cx| {
                // The window it should hand work back to is whichever one just asked for it.
                if origin.is_some() {
                    this.origin = origin;
                }
                window.activate_window();
                cx.notify();
            })
            .log_err();
        return;
    }

    // Deferred to get the workspace off the stack: the action is handled inside an update of it,
    // and opening a window re-enters. Settings hit the same thing and solved it the same way.
    cx.defer(move |cx| {
        let window_decorations = match WorkspaceSettings::get_global(cx).window_decorations {
            settings::WindowDecorations::Server => gpui::WindowDecorations::Server,
            settings::WindowDecorations::Client => gpui::WindowDecorations::Client,
        };

        cx.open_window(
            WindowOptions {
                titlebar: Some(TitlebarOptions {
                    title: Some("Anna — GitHub".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(12.0), px(12.0))),
                }),
                focus: true,
                show: true,
                is_movable: true,
                kind: gpui::WindowKind::Normal,
                window_background: cx.theme().window_background_appearance(),
                window_decorations: Some(window_decorations),
                window_min_size: Some(gpui::Size {
                    width: px(520.),
                    height: px(360.),
                }),
                window_bounds: Some(WindowBounds::Maximized(
                    WindowBounds::centered(DEFAULT_ADDITIONAL_WINDOW_SIZE, cx).get_bounds(),
                )),
                ..Default::default()
            },
            |window, cx| cx.new(|cx| GitHubWindow::new(origin, window, cx)),
        )
        .log_err();
    });
}

/// Where the window is in the one flow it has: find a token, prove it works, show the work.
enum State {
    /// Looking for a token saved on a previous run. The first frame is always this.
    Looking,
    /// There is no working token yet.
    Disconnected {
        /// What went wrong last time, if anything did. Shown above the buttons.
        error: Option<SharedString>,
        /// True while a connection attempt is in flight, so the buttons can say so.
        busy: bool,
        /// Whether the paste-a-token field is showing.
        pasting: bool,
    },
    /// A token that GitHub accepted, and who it belongs to.
    Connected { identity: Identity, work: Work },
}

/// The worklist's own state, which moves independently of the connection's.
enum Work {
    Loading,
    Ready(Worklist),
    Failed(SharedString),
}

/// The repository list's state, fetched the first time it is looked at.
enum Repositories {
    Untouched,
    Loading,
    Ready(Vec<Owner>),
    Failed(SharedString),
}

/// The two things this window is for.
///
/// Work is what is waiting on you; repositories is everything you could work on. They are separate
/// because they answer different questions and a screen that tried to answer both at once would
/// answer neither.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Work,
    Repositories,
}

pub struct GitHubWindow {
    state: State,
    /// Set once a token is accepted; every query goes through it.
    client: Option<Client>,
    /// The paste-a-token field, masked like the one Cowork uses for provider keys.
    token_entry: Entity<Editor>,
    focus_handle: FocusHandle,
    /// The workspace window work is handed back to.
    origin: Option<WindowHandle<MultiWorkspace>>,
    /// Said under the header after an action, so a click is never silent.
    note: Option<SharedString>,
    tab: Tab,
    repositories: Repositories,
    /// Which owner's repositories are shown, as an index into the fetched list.
    ///
    /// An index rather than a login because the list is short and ordered, and an index cannot
    /// name an owner that is no longer there.
    owner: usize,
    _repositories: Task<()>,
    _connect: Task<()>,
    _work: Task<()>,
}

impl GitHubWindow {
    fn new(
        origin: Option<WindowHandle<MultiWorkspace>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let token_entry = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_masked(true, cx);
            editor.set_placeholder_text("Paste a personal access token", window, cx);
            editor
        });

        let mut this = Self {
            state: State::Looking,
            client: None,
            token_entry,
            focus_handle: cx.focus_handle(),
            origin,
            note: None,
            tab: Tab::Work,
            repositories: Repositories::Untouched,
            owner: 0,
            _connect: Task::ready(()),
            _work: Task::ready(()),
            _repositories: Task::ready(()),
        };
        this.resume(cx);
        this
    }

    /// Tries the token saved last time, so a returning user never sees the connect screen.
    ///
    /// A saved token that GitHub now rejects — revoked, expired, scopes changed — lands on the
    /// connect screen with the reason, rather than silently pretending nobody ever connected.
    fn resume(&mut self, cx: &mut Context<Self>) {
        let stored = auth::stored(cx);
        self._connect = cx.spawn(async move |this, cx| {
            let token = match stored.await {
                Ok(Some(token)) => token,
                Ok(None) => {
                    this.update(cx, |this, cx| this.disconnect(None, cx)).log_err();
                    return;
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    this.update(cx, |this, cx| this.disconnect(Some(message.into()), cx))
                        .log_err();
                    return;
                }
            };

            Self::finish_connecting(token, this, cx).await;
        });
    }

    /// Asks the GitHub CLI for the token it is already holding.
    fn connect_with_cli(&mut self, cx: &mut Context<Self>) {
        self.set_busy(cx);
        self._connect = cx.spawn(async move |this, cx| {
            let token = match auth::from_cli().await {
                Ok(token) => token,
                Err(error) => {
                    let message = format!("{error:#}");
                    this.update(cx, |this, cx| this.disconnect(Some(message.into()), cx))
                        .log_err();
                    return;
                }
            };
            Self::finish_connecting(token, this, cx).await;
        });
    }

    /// Uses whatever was pasted into the field.
    fn connect_with_pasted_token(&mut self, cx: &mut Context<Self>) {
        let token = self.token_entry.read(cx).text(cx);
        if let Err(error) = auth::looks_like_a_token(&token) {
            self.disconnect(Some(format!("{error}").into()), cx);
            if let State::Disconnected { pasting, .. } = &mut self.state {
                // Keep the field open; the user is mid-correction.
                *pasting = true;
            }
            cx.notify();
            return;
        }

        self.set_busy(cx);
        let token = token.trim().to_owned();
        self._connect =
            cx.spawn(async move |this, cx| Self::finish_connecting(token, this, cx).await);
    }

    /// Proves the token works, then keeps it.
    ///
    /// The token is only written to the credential store *after* GitHub accepts it, so a typo is
    /// never saved and never has to be cleaned up.
    async fn finish_connecting(
        token: String,
        this: gpui::WeakEntity<Self>,
        cx: &mut gpui::AsyncApp,
    ) {
        let http = cx.update(|cx| cx.http_client());
        let client = Client::new(http, token.clone());

        match client.identity().await {
            Ok(identity) => {
                // A keychain that refuses to store is worth a log, not a failed connection:
                // the session in front of the user still works.
                cx.update(|cx| auth::store(&token, &identity.login, cx))
                    .await
                    .log_err();
                this.update(cx, |this, cx| this.connected(client, identity, cx))
                    .log_err();
            }
            Err(error) => {
                let message = describe(&error);
                this.update(cx, |this, cx| this.disconnect(Some(message), cx))
                    .log_err();
            }
        }
    }

    fn connected(&mut self, client: Client, identity: Identity, cx: &mut Context<Self>) {
        self.client = Some(client);
        self.state = State::Connected {
            identity,
            work: Work::Loading,
        };
        self.refresh_work(cx);
        cx.notify();
    }

    /// Asks GitHub what is waiting.
    fn refresh_work(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        if let State::Connected { work, .. } = &mut self.state {
            *work = Work::Loading;
        }
        self.note = None;
        cx.notify();

        self._work = cx.spawn(async move |this, cx| {
            let outcome = worklist::fetch(&client).await;
            this.update(cx, |this, cx| {
                if let State::Connected { work, .. } = &mut this.state {
                    *work = match outcome {
                        Ok(list) => Work::Ready(list),
                        Err(error) => Work::Failed(describe(&error)),
                    };
                }
                cx.notify();
            })
            .log_err();
        });
    }

    /// Hands one item to Cowork, in the window this one was opened from.
    fn send_to_cowork(&mut self, item: &Item, cx: &mut Context<Self>) {
        let Some(origin) = self.origin else {
            self.note = Some(
                "Open this from a project window to send work to Anna — it needs a project to \
                 work in."
                    .into(),
            );
            cx.notify();
            return;
        };

        let action = wu_actions::StartThreadWith {
            prompt: item.handoff_prompt(),
        };
        let slug = item.slug();

        let sent = origin
            .update(cx, |_, window, cx| {
                window.activate_window();
                window.dispatch_action(Box::new(action), cx);
            })
            .log_err()
            .is_some();

        self.note = Some(if sent {
            format!("Sent {slug} to Anna.").into()
        } else {
            "That project window is gone. Open GitHub again from the one you want to work in."
                .into()
        });
        cx.notify();
    }

    fn disconnect(&mut self, error: Option<SharedString>, cx: &mut Context<Self>) {
        self.client = None;
        self.state = State::Disconnected {
            error,
            busy: false,
            pasting: false,
        };
        cx.notify();
    }

    fn set_busy(&mut self, cx: &mut Context<Self>) {
        if let State::Disconnected { busy, error, .. } = &mut self.state {
            *busy = true;
            *error = None;
        }
        cx.notify();
    }

    /// Forgets the token, after asking.
    ///
    /// Worth a question because of what it costs to undo rather than what it breaks: nothing is
    /// lost on GitHub's side, but getting back in means finding the CLI again or minting another
    /// token, and the button sits next to one that only refreshes a list.
    fn sign_out(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(login) = self.connected_login() else {
            return;
        };

        let answer = window.prompt(
            gpui::PromptLevel::Warning,
            &format!("Disconnect from GitHub as @{login}?"),
            Some(
                "Anna will forget this token. Nothing changes on GitHub, and no repository is \
                 touched — but the agent loses access to issues, pull requests, checks and \
                 security alerts until you connect again.",
            ),
            &["Disconnect", "Cancel"],
            cx,
        );

        cx.spawn_in(window, async move |this, cx| {
            if answer.await.ok() != Some(0) {
                return;
            }
            this.update_in(cx, |this, window, cx| {
                auth::forget(cx).detach_and_log_err(cx);
                this.token_entry
                    .update(cx, |editor, cx| editor.set_text("", window, cx));
                this.repositories = Repositories::Untouched;
                this.tab = Tab::Work;
                this.disconnect(None, cx);
            })
            .log_err();
        })
        .detach();
    }

    fn connected_login(&self) -> Option<String> {
        match &self.state {
            State::Connected { identity, .. } => Some(identity.login.clone()),
            _ => None,
        }
    }

    /// Moves between the two lists, fetching the repositories the first time they are asked for.
    fn show(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.note = None;
        if tab == Tab::Repositories && matches!(self.repositories, Repositories::Untouched) {
            self.refresh_repositories(cx);
        }
        cx.notify();
    }

    fn refresh_repositories(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.repositories = Repositories::Loading;
        cx.notify();

        self._repositories = cx.spawn(async move |this, cx| {
            let outcome = repositories::fetch(&client).await;
            this.update(cx, |this, cx| {
                this.repositories = match outcome {
                    Ok(owners) => {
                        this.owner = 0;
                        Repositories::Ready(owners)
                    }
                    Err(error) => Repositories::Failed(describe(&error)),
                };
                cx.notify();
            })
            .log_err();
        });
    }
}

/// Turns a failure into the one sentence worth putting on the screen.
pub fn describe(error: &anyhow::Error) -> SharedString {
    match error.downcast_ref::<Failure>() {
        Some(Failure::Unauthorized) => {
            "GitHub rejected this token. It may have been revoked or have expired.".into()
        }
        Some(failure) => format!("{failure}").into(),
        None => format!("{error:#}").into(),
    }
}

impl Focusable for GitHubWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for GitHubWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.state {
            State::Looking => self.render_looking(),
            State::Disconnected {
                error,
                busy,
                pasting,
            } => self
                .render_connect(error.clone(), *busy, *pasting, cx)
                .into_any_element(),
            State::Connected { identity, work } => self
                .render_connected(identity, work, cx)
                .into_any_element(),
        };

        // Read after the body: `render_*` need `&mut cx`, and a live `&Theme` borrowed from it
        // would make that impossible.
        let colors = cx.theme().colors();

        client_side_decorations(
            v_flex()
                .size_full()
                .bg(colors.background)
                .text_color(colors.text)
                // The window has no title bar of its own, so this is the only way to close it with
                // the mouse. `escape` is bound to the same thing.
                .child(
                    h_flex().w_full().justify_end().px_2().pt_2().child(
                        IconButton::new("github-close", IconName::Close)
                            .icon_size(IconSize::Small)
                            .tooltip(Tooltip::text("Close"))
                            .on_click(|_, window, _| window.remove_window()),
                    ),
                )
                .child(
                    v_flex()
                        .id("github-window")
                        .key_context("GitHubWindow")
                        .track_focus(&self.focus_handle)
                        .flex_1()
                        .min_h_0()
                        .items_center()
                        .child(
                            v_flex()
                                .w_full()
                                .max_w(CONTENT_MAX_WIDTH)
                                .flex_1()
                                .min_h_0()
                                .px_8()
                                .child(body),
                        ),
                ),
            window,
            cx,
        )
    }
}

impl GitHubWindow {
    fn render_looking(&self) -> gpui::AnyElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .child(Label::new("Checking for a saved connection…").color(Color::Muted))
            .into_any_element()
    }

    fn render_connect(
        &self,
        error: Option<SharedString>,
        busy: bool,
        pasting: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_1()
            .child(Icon::new(IconName::Github).size(IconSize::XLarge))
            .child(Label::new("Connect to GitHub").size(LabelSize::Large))
            .child(
                div().max_w(px(460.)).text_center().child(
                    Label::new(
                        "See the issues and pull requests waiting on you, and hand any of them to \
                         an agent with this project already open.",
                    )
                    .color(Color::Muted),
                ),
            )
            .when_some(error, |this, error| {
                this.child(
                    div()
                        .mt_3()
                        .max_w(px(460.))
                        .text_center()
                        .child(Label::new(error).color(Color::Error)),
                )
            })
            .child(
                v_flex()
                    .mt_4()
                    .w(px(320.))
                    .gap_2()
                    .child(
                        Button::new("github-connect-cli", "Use the GitHub CLI")
                            .full_width()
                            .style(ButtonStyle::Filled)
                            .disabled(busy)
                            .tooltip(Tooltip::text(
                                "Uses the token `gh` is already signed in with — nothing to paste",
                            ))
                            .on_click(cx.listener(|this, _, _, cx| this.connect_with_cli(cx))),
                    )
                    .when(!pasting, |this| {
                        this.child(
                            Button::new("github-connect-token", "Paste a token instead")
                                .full_width()
                                .style(ButtonStyle::Subtle)
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let State::Disconnected { pasting, .. } = &mut this.state {
                                        *pasting = true;
                                    }
                                    cx.notify();
                                })),
                        )
                    })
                    .when(pasting, |this| {
                        this.child(
                            h_flex()
                                .w_full()
                                .px_2()
                                .py_1()
                                .gap_2()
                                .rounded_sm()
                                .border_1()
                                .border_color(cx.theme().colors().border)
                                .bg(cx.theme().colors().editor_background)
                                .child(div().flex_1().child(self.token_entry.clone())),
                        )
                        .child(
                            Button::new("github-connect-submit", "Connect")
                                .full_width()
                                .style(ButtonStyle::Filled)
                                .disabled(busy)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.connect_with_pasted_token(cx)
                                })),
                        )
                    })
                    .when(busy, |this| {
                        this.child(
                            div()
                                .text_center()
                                .child(Label::new("Connecting…").color(Color::Muted)),
                        )
                    }),
            )
    }

    fn render_connected(
        &self,
        identity: &Identity,
        work: &Work,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        v_flex()
            .size_full()
            .pt_6()
            .gap_3()
            .child(self.render_header(identity, work, cx))
            .child(self.render_tabs(cx))
            .children(self.note.clone().map(|note| {
                div()
                    .child(Label::new(note).size(LabelSize::Small).color(Color::Muted))
            }))
            .child(Divider::horizontal())
            .child(
                v_flex()
                    .id("github-worklist")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(match (self.tab, work) {
                        (Tab::Repositories, _) => self.render_repositories(cx),
                        (Tab::Work, Work::Loading) => {
                            centred("Looking for what is waiting…").into_any_element()
                        }
                        (Tab::Work, Work::Failed(error)) => {
                            self.render_failure(error.clone(), cx)
                        }
                        (Tab::Work, Work::Ready(list)) if list.is_empty() => {
                            centred("Nothing is waiting on you.").into_any_element()
                        }
                        (Tab::Work, Work::Ready(list)) => v_flex()
                            .w_full()
                            .pb_6()
                            .children(self.render_section(
                                "Awaiting your review",
                                &list.reviews,
                                cx,
                            ))
                            .children(self.render_section("Assigned to you", &list.assigned, cx))
                            .children(self.render_section("Opened by you", &list.authored, cx))
                            .into_any_element(),
                    }),
            )
    }

    fn render_header(
        &self,
        identity: &Identity,
        work: &Work,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let waiting = match work {
            Work::Ready(list) => Some(list.total()),
            _ => None,
        };

        h_flex()
            .w_full()
            .justify_between()
            .items_center()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(Icon::new(IconName::Github))
                    .child(Label::new(identity.label().to_owned()).size(LabelSize::Large))
                    .child(Label::new(format!("@{}", identity.login)).color(Color::Muted))
                    .children(waiting.map(|count| {
                        Label::new(match count {
                            1 => "1 thing waiting".to_owned(),
                            count => format!("{count} things waiting"),
                        })
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                    })),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(
                        Button::new("github-refresh", "Refresh")
                            .style(ButtonStyle::Subtle)
                            .label_size(LabelSize::Small)
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_work(cx))),
                    )
                    .child(
                        Button::new("github-sign-out", "Disconnect")
                            .style(ButtonStyle::Subtle)
                            .label_size(LabelSize::Small)
                            .tooltip(Tooltip::text("Forget this token"))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.sign_out(window, cx)),
                            ),
                    ),
            )
    }

    /// A failure with the one remedy it has, when it has one.
    ///
    /// The missing-scope case is the only failure here a user can fix themselves, and telling them
    /// the command is the difference between a dead end and thirty seconds of work.
    fn render_failure(&self, error: SharedString, cx: &mut Context<Self>) -> gpui::AnyElement {
        let scope_problem = error.contains("scope");

        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .child(Label::new(error).color(Color::Error))
            .when(scope_problem, |this| {
                this.child(
                    Label::new("Run `gh auth refresh -s read:project`, then reconnect.")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .child(
                Button::new("github-retry", "Try again")
                    .style(ButtonStyle::Filled)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh_work(cx))),
            )
            .into_any_element()
    }

    /// One bucket, skipped entirely when it is empty.
    ///
    /// An empty heading is a row of noise on a screen whose whole job is to be scannable.
    fn render_section(
        &self,
        title: &'static str,
        items: &[Item],
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        if items.is_empty() {
            return None;
        }
        let colors = cx.theme().colors();

        Some(
            v_flex()
                .w_full()
                .mt_4()
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .items_center()
                        .pb_1()
                        .child(Label::new(title).size(LabelSize::Small).color(Color::Muted))
                        .child(
                            Label::new(items.len().to_string())
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                        .child(div().flex_1().h(px(1.)).bg(colors.border_variant)),
                )
                .children(
                    items
                        .iter()
                        .enumerate()
                        .map(|(index, item)| self.render_item(title, index, item, cx)),
                ),
        )
    }

    fn render_item(
        &self,
        section: &'static str,
        index: usize,
        item: &Item,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let colors = cx.theme().colors();
        let url = item.url.clone();
        let for_handoff = item.clone();

        h_flex()
            .id(SharedString::from(format!("{section}-{index}")))
            .w_full()
            .py_1p5()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(colors.border_variant)
            .hover(|style| style.bg(colors.element_hover))
            .child(
                div()
                    .id(SharedString::from(format!("{section}-{index}-kind")))
                    .tooltip(Tooltip::text(item.kind.label()))
                    .child(
                        Icon::new(match item.kind {
                            Kind::Issue => IconName::Info,
                            Kind::PullRequest => IconName::GitBranch,
                        })
                        .size(IconSize::Small)
                        .color(if item.is_draft {
                            Color::Muted
                        } else {
                            Color::Default
                        }),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(Label::new(item.title.clone()).truncate())
                    .child(
                        h_flex()
                            .gap_1p5()
                            .child(
                                Label::new(item.slug())
                                    .size(LabelSize::Small)
                                    .color(Color::Muted),
                            )
                            .when(item.is_draft, |this| {
                                this.child(
                                    Label::new("draft")
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                            })
                            .children(item.labels.iter().take(3).map(|label| {
                                Label::new(label.clone())
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                            })),
                    ),
            )
            .child(
                Button::new(("github-send", index), "Send to Anna")
                    .style(ButtonStyle::Subtle)
                    .label_size(LabelSize::Small)
                    .tooltip(Tooltip::text(
                        "Open a thread with this, in the project you came from",
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.send_to_cowork(&for_handoff, cx)
                    })),
            )
            .child(
                IconButton::new(("github-open", index), IconName::ArrowUpRight)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Open on GitHub"))
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            )
    }
}

impl GitHubWindow {
    /// Work or repositories: two questions, two lists.
    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = cx.theme().colors();
        let tab = |label: &'static str, which: Tab| {
            let selected = self.tab == which;
            Button::new(label, label)
                .label_size(LabelSize::Small)
                .style(if selected {
                    ButtonStyle::Filled
                } else {
                    ButtonStyle::Subtle
                })
                .on_click(cx.listener(move |this, _, _, cx| this.show(which, cx)))
        };

        h_flex()
            .w_full()
            .gap_1()
            .pb_1()
            .border_b_1()
            .border_color(colors.border_variant)
            .child(tab("Work", Tab::Work))
            .child(tab("Repositories", Tab::Repositories))
    }

    fn render_repositories(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        match &self.repositories {
            Repositories::Untouched | Repositories::Loading => {
                centred("Looking at what you can reach…").into_any_element()
            }
            Repositories::Failed(error) => self.render_failure(error.clone(), cx),
            Repositories::Ready(owners) if owners.is_empty() => {
                centred("This token can see no repositories.").into_any_element()
            }
            Repositories::Ready(owners) => {
                // An index that outran the list means the list was refetched under it.
                let Some(owner) = owners.get(self.owner).or_else(|| owners.first()) else {
                    return centred("This token can see no repositories.").into_any_element();
                };

                v_flex()
                    .w_full()
                    .pb_6()
                    .child(self.render_owner_filter(owners, cx))
                    .children(
                        owner
                            .repositories
                            .iter()
                            .enumerate()
                            .map(|(index, repository)| {
                                self.render_repository(index, repository, cx)
                            }),
                    )
                    .when(owner.truncated(), |this| {
                        this.child(
                            div().pt_2().child(
                                Label::new(format!(
                                    "Showing {} of {}. The rest are older.",
                                    owner.repositories.len(),
                                    owner.total
                                ))
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                            ),
                        )
                    })
                    .into_any_element()
            }
        }
    }

    /// One chip per namespace: personal first, then each organisation.
    ///
    /// Shown even when there is only one, because "Personal" with nothing beside it is itself the
    /// answer to "which organisations am I in".
    fn render_owner_filter(&self, owners: &[Owner], cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .w_full()
            .gap_1()
            .py_2()
            .flex_wrap()
            .children(owners.iter().enumerate().map(|(index, owner)| {
                let selected = index == self.owner;
                Button::new(("github-owner", index), owner.label())
                    .label_size(LabelSize::Small)
                    .style(if selected {
                        ButtonStyle::Filled
                    } else {
                        ButtonStyle::Subtle
                    })
                    .start_icon(
                        Icon::new(match owner.kind {
                            OwnerKind::Personal => IconName::Person,
                            OwnerKind::Organisation => IconName::Github,
                        })
                        .size(IconSize::XSmall),
                    )
                    .tooltip(Tooltip::text(match owner.kind {
                        OwnerKind::Personal => "Repositories you own".to_owned(),
                        OwnerKind::Organisation => {
                            format!("Repositories of @{}", owner.login)
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.owner = index;
                        cx.notify();
                    }))
            }))
    }

    fn render_repository(
        &self,
        index: usize,
        repository: &Repository,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let colors = cx.theme().colors();
        let url = repository.url.clone();

        h_flex()
            .id(SharedString::from(format!("github-repo-{index}")))
            .w_full()
            .py_1p5()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(colors.border_variant)
            .hover(|style| style.bg(colors.element_hover))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(Label::new(repository.name.clone()).truncate())
                            .when(repository.private, |this| {
                                this.child(
                                    Icon::new(IconName::Lock)
                                        .size(IconSize::XSmall)
                                        .color(Color::Muted),
                                )
                            })
                            .when(repository.archived, |this| {
                                this.child(
                                    Label::new("archived")
                                        .size(LabelSize::Small)
                                        .color(Color::Muted),
                                )
                            }),
                    )
                    .children(repository.description.clone().map(|description| {
                        Label::new(description)
                            .size(LabelSize::Small)
                            .color(Color::Muted)
                            .truncate()
                    })),
            )
            .children(repository.language.clone().map(|language| {
                Label::new(language)
                    .size(LabelSize::Small)
                    .color(Color::Muted)
            }))
            // Counted rather than listed: the number says whether it is worth opening, and the
            // Work tab is where the individual items already are.
            .when(repository.open_issues > 0, |this| {
                this.child(
                    Label::new(format!("{} issues", repository.open_issues))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .when(repository.open_pull_requests > 0, |this| {
                this.child(
                    Label::new(format!("{} PRs", repository.open_pull_requests))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                )
            })
            .child(
                IconButton::new(("github-repo-open", index), IconName::ArrowUpRight)
                    .icon_size(IconSize::Small)
                    .tooltip(Tooltip::text("Open on GitHub"))
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            )
    }
}

/// One line of muted text, centred — the shape every empty and waiting state takes here.
fn centred(message: &'static str) -> impl IntoElement {
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .child(Label::new(message).color(Color::Muted))
}
