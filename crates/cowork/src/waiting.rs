//! What is waiting on you in this project's repository, for the top of the home screen.
//!
//! The GitHub window already answers this across every repository. The home screen asks a smaller
//! question — is anyone here blocked on me — so this looks only at the repository the project was
//! cloned from, and shows a handful of rows rather than a list to work through.

use std::rc::Rc;

use gpui::{AnyElement, App, Context, Entity, SharedString, Subscription, Task, Window};
use project::{Project, git_store::GitStoreEvent};
use time::{OffsetDateTime, UtcOffset};
use ui::{ButtonLike, Tooltip, prelude::*};
use util::ResultExt as _;

use crate::github_tools::project_repository;

/// How many rows the section shows.
///
/// Past a handful this stops being a glance above the composer and becomes the GitHub window, which
/// already exists and is one click away.
const MAX_ROWS: usize = 5;

/// What the host does with a clicked row: start a thread from the prompt it is given, which is the
/// same prompt the GitHub window hands to Cowork for that item.
pub type OpenPrompt = Rc<dyn Fn(String, &mut Window, &mut App)>;

enum State {
    Loading,
    NotConnected,
    /// Also where a project sits until git has read its remotes. Nothing can be said about either,
    /// and both render nothing.
    NoRemote,
    Ready(Vec<github::Waiting>),
    Failed(SharedString),
}

pub struct WaitingOnYou {
    project: Entity<Project>,
    /// The `owner/name` the current state is about.
    repository: Option<String>,
    state: State,
    _load: Task<()>,
    _git_store: Subscription,
}

impl WaitingOnYou {
    pub fn new(project: Entity<Project>, cx: &mut Context<Self>) -> Self {
        let git_store = project.read(cx).git_store().clone();
        // The remote is read by a git scan that usually finishes after the project opens, so the
        // first look tends to find nothing. Any of these can be the moment it arrives; a load only
        // follows when the repository they resolve to is actually a different one.
        let git_store_subscription = cx.subscribe(&git_store, |this, _, event, cx| match event {
            GitStoreEvent::ActiveRepositoryChanged(_)
            | GitStoreEvent::RepositoryAdded
            | GitStoreEvent::RepositoryRemoved(_)
            | GitStoreEvent::RepositoryUpdated(_, _, true) => this.follow_repository(cx),
            _ => {}
        });

        let mut this = Self {
            project,
            repository: None,
            state: State::NoRemote,
            _load: Task::ready(()),
            _git_store: git_store_subscription,
        };
        this.refresh(cx);
        this
    }

    /// Asks GitHub again.
    ///
    /// Nothing reloads on a timer or per frame. Besides the project's repository changing, this is
    /// the only way the rows are refetched — for instance after the user connects GitHub.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.repository = project_repository(&self.project, cx);
        self.load(cx);
    }

    fn follow_repository(&mut self, cx: &mut Context<Self>) {
        let repository = project_repository(&self.project, cx);
        if repository != self.repository {
            self.repository = repository;
            self.load(cx);
        }
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(repository) = self.repository.clone() else {
            self.state = State::NoRemote;
            self._load = Task::ready(());
            cx.notify();
            return;
        };

        self.state = State::Loading;
        cx.notify();

        let token = github::stored_token(cx);
        let http = cx.http_client();
        // Replacing the task drops the previous one, so an answer about a repository this project
        // has since moved away from never lands.
        self._load = cx.spawn(async move |this, cx| {
            let state = match token.await {
                Ok(None) => State::NotConnected,
                Ok(Some(token)) => {
                    let client = github::Client::new(http, token);
                    match github::fetch_waiting_in_repository(&client, &repository, MAX_ROWS).await
                    {
                        Ok(waiting) => State::Ready(waiting),
                        Err(error) => State::Failed(github::describe_failure(&error)),
                    }
                }
                Err(error) => State::Failed(github::describe_failure(&error)),
            };

            this.update(cx, |this, cx| {
                this.state = state;
                cx.notify();
            })
            .log_err();
        });
    }

    /// The section, or `None` when there is nothing worth a line above the composer.
    pub fn render(&self, on_open: OpenPrompt) -> Option<AnyElement> {
        match &self.state {
            // Most projects turn out to have nothing waiting, so a placeholder shown while loading
            // would usually appear and then vanish, moving the composer twice for no news.
            State::Loading | State::NoRemote => None,
            // Nothing waiting is the good outcome. A line saying so would be read on every visit
            // and carry no information, which is the one thing a home screen cannot afford.
            State::Ready(waiting) if waiting.is_empty() => None,
            State::Ready(waiting) => Some(render_rows(waiting, on_open)),
            State::NotConnected => Some(render_connect()),
            State::Failed(reason) => Some(render_failure(reason.clone())),
        }
    }
}

fn render_rows(waiting: &[github::Waiting], on_open: OpenPrompt) -> AnyElement {
    let now = OffsetDateTime::now_utc();
    let offset = UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC);

    v_flex()
        .w_full()
        .gap_0p5()
        .child(
            div().px_2().pb_0p5().child(
                Label::new("Waiting on you")
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            ),
        )
        .children(
            waiting.iter().enumerate().map(|(index, entry)| {
                render_row(index, entry, now, offset, on_open.clone())
            }),
        )
        .into_any_element()
}

fn render_row(
    index: usize,
    waiting: &github::Waiting,
    now: OffsetDateTime,
    offset: UtcOffset,
    on_open: OpenPrompt,
) -> impl IntoElement {
    let item = &waiting.item;
    let prompt = item.handoff_prompt();
    // The title is what truncates, so the tooltip carries it whole, with where it lives.
    let tooltip = format!("{} — {}", item.title, item.slug());

    ButtonLike::new(("waiting-on-you", index))
        .tab_index(0isize)
        .full_width()
        .size(ButtonSize::Medium)
        .tooltip(Tooltip::text(tooltip))
        .child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                // `full_width` centres a button's text, which would float a short title mid-row.
                .text_left()
                .child(
                    div().flex_none().child(
                        Label::new(waiting.reason.label())
                            .size(LabelSize::Small)
                            .color(reason_color(waiting.reason)),
                    ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(Label::new(item.title.clone()).truncate()),
                )
                .child(
                    h_flex()
                        .flex_none()
                        .gap_2()
                        .child(
                            Label::new(format!("#{}", item.number))
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                        .child(
                            Label::new(item.repository.clone())
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        )
                        .children(waiting.updated_at.map(|updated_at| {
                            Label::new(relative_time(updated_at, now, offset))
                                .size(LabelSize::Small)
                                .color(Color::Muted)
                        }))
                        .child(
                            Icon::new(IconName::ChevronRight)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        ),
                ),
        )
        .on_click(move |_, window, cx| on_open(prompt.clone(), window, cx))
}

fn render_connect() -> AnyElement {
    ButtonLike::new("waiting-on-you-connect")
        .tab_index(0isize)
        .full_width()
        .size(ButtonSize::Medium)
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .text_left()
                .child(
                    Icon::new(IconName::Github)
                        .size(IconSize::Small)
                        .color(Color::Muted),
                )
                .child(
                    Label::new("Connect GitHub to see pull requests waiting on you")
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                ),
        )
        .on_click(|_, window, cx| window.dispatch_action(Box::new(wu_actions::OpenGitHub), cx))
        .into_any_element()
}

fn render_failure(reason: SharedString) -> AnyElement {
    div()
        .id("waiting-on-you-failure")
        .w_full()
        .px_2()
        // Transport failures arrive as a chain of causes, far longer than one line.
        .tooltip(Tooltip::text(reason.clone()))
        .child(
            Label::new(format!("Could not check GitHub: {reason}"))
                .size(LabelSize::Small)
                .color(Color::Muted)
                .truncate(),
        )
        .into_any_element()
}

fn reason_color(reason: github::WaitingReason) -> Color {
    match reason {
        github::WaitingReason::ReviewRequested => Color::Accent,
        github::WaitingReason::ChangesRequested => Color::Warning,
        github::WaitingReason::ChecksFailing => Color::Error,
        github::WaitingReason::Assigned => Color::Muted,
    }
}

/// "2 minutes ago", measured from `now` and shown in the local time zone.
///
/// GitHub's clock can be a little ahead of this machine's. The shared formatter assumes the past:
/// a moment ahead of `now` across midnight reads as "1 month ago", and across the end of a month it
/// underflows. Anything ahead is read as now.
fn relative_time(updated_at: OffsetDateTime, now: OffsetDateTime, offset: UtcOffset) -> String {
    time_format::format_localized_timestamp(
        updated_at.min(now),
        now,
        offset,
        time_format::TimestampFormat::Relative,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn activity_a_few_minutes_old_reads_in_minutes() {
        assert_eq!(
            relative_time(
                datetime!(2026-09-12 10:00 UTC),
                datetime!(2026-09-12 10:02 UTC),
                UtcOffset::UTC
            ),
            "2 minutes ago"
        );
    }

    #[test]
    fn older_activity_reads_in_days() {
        assert_eq!(
            relative_time(
                datetime!(2026-09-09 10:00 UTC),
                datetime!(2026-09-12 10:00 UTC),
                UtcOffset::UTC
            ),
            "3 days ago"
        );
    }

    #[test]
    fn a_timestamp_ahead_of_this_clock_reads_as_just_now() {
        // Five minutes ahead, across the end of a month: the formatter alone underflows here.
        assert_eq!(
            relative_time(
                datetime!(2026-10-01 00:03 UTC),
                datetime!(2026-09-30 23:58 UTC),
                UtcOffset::UTC
            ),
            "Just now"
        );
    }
}
