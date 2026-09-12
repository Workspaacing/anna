//! What is waiting for you, across every repository.
//!
//! Three questions, asked at once because they are one screen: what is assigned to me, what is
//! waiting on my review, and what have I opened. That is the whole of "what should I be doing",
//! and it is deliberately not a list of a repository's issues — the repository view is the one
//! thing github.com does better than any editor could, and it is one keystroke away.
//!
//! The point of having it here instead is what sits next to each row: the code, and an agent that
//! can be handed the item.

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::api::Client;

/// How many of each kind to ask for.
///
/// A worklist that needs paging is a worklist nobody is going to get to the bottom of. Twenty-five
/// is enough to see everything that matters and short enough to read.
const PER_BUCKET: usize = 25;

/// One search per bucket, in a single round trip.
///
/// `@me` resolves server-side, so this needs no login parameter and cannot drift from whoever the
/// token belongs to.
const QUERY: &str = r#"
query($count:Int!){
  assigned: search(query:"assignee:@me is:open", type:ISSUE, first:$count){
    issueCount nodes{ ...Work }
  }
  reviews: search(query:"review-requested:@me is:open is:pr", type:ISSUE, first:$count){
    issueCount nodes{ ...Work }
  }
  authored: search(query:"author:@me is:open", type:ISSUE, first:$count){
    issueCount nodes{ ...Work }
  }
}
fragment Work on SearchResultItem {
  __typename
  ... on Issue {
    number title url updatedAt
    repository{nameWithOwner}
    labels(first:5){nodes{name}}
    comments{totalCount}
  }
  ... on PullRequest {
    number title url updatedAt isDraft
    repository{nameWithOwner}
    labels(first:5){nodes{name}}
    comments{totalCount}
    reviewDecision
  }
}"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Issue,
    PullRequest,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Issue => "Issue",
            Kind::PullRequest => "Pull request",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub kind: Kind,
    /// `owner/name`, which is what the tools take and what the user reads.
    pub repository: String,
    pub number: i64,
    pub title: String,
    pub url: String,
    pub is_draft: bool,
    pub labels: Vec<String>,
}

impl Item {
    /// How the row is named: `owner/name#123`.
    pub fn slug(&self) -> String {
        format!("{}#{}", self.repository, self.number)
    }

    /// The sentence handed to an agent when this item is sent to Cowork.
    ///
    /// Written as an instruction rather than a dump of the item, because the agent has a tool that
    /// will fetch the item itself — telling it the number is both shorter and fresher than pasting
    /// a copy that was accurate a minute ago.
    pub fn handoff_prompt(&self) -> String {
        match self.kind {
            Kind::Issue => format!(
                "Read issue {} with `github_issue`, then work out what it asks for and do it in \
                 this project. Explain what you are changing before you change it.",
                self.slug()
            ),
            Kind::PullRequest => format!(
                "Read pull request {} with `github_pull_request`, including its diff, and review \
                 it against the code in this project. Say what is wrong and what is fine.",
                self.slug()
            ),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Worklist {
    pub assigned: Vec<Item>,
    pub reviews: Vec<Item>,
    pub authored: Vec<Item>,
}

impl Worklist {
    pub fn is_empty(&self) -> bool {
        self.assigned.is_empty() && self.reviews.is_empty() && self.authored.is_empty()
    }

    pub fn total(&self) -> usize {
        self.assigned.len() + self.reviews.len() + self.authored.len()
    }
}

/// Asks GitHub what is waiting.
pub async fn fetch(client: &Client) -> Result<Worklist> {
    let data = client
        .graphql(QUERY, json!({ "count": PER_BUCKET }))
        .await
        .context("asking GitHub what is waiting for you")?;

    Ok(parse(&data))
}

/// Reads the three buckets out of one answer.
///
/// A bucket that is missing or malformed becomes an empty list rather than a failure: one search
/// failing — a repository that went private, a rate limit on one shard — should not blank a screen
/// that has two other useful lists on it.
fn parse(data: &Value) -> Worklist {
    Worklist {
        assigned: items(data, "assigned"),
        reviews: items(data, "reviews"),
        authored: items(data, "authored"),
    }
}

fn items(data: &Value, bucket: &str) -> Vec<Item> {
    data.get(bucket)
        .and_then(|bucket| bucket.get("nodes"))
        .and_then(Value::as_array)
        .map(|nodes| nodes.iter().filter_map(item).collect())
        .unwrap_or_default()
}

fn item(node: &Value) -> Option<Item> {
    let kind = match node.get("__typename").and_then(Value::as_str)? {
        "Issue" => Kind::Issue,
        "PullRequest" => Kind::PullRequest,
        // A search of type ISSUE can still return other things; they are not work.
        _ => return None,
    };

    Some(Item {
        kind,
        repository: node
            .get("repository")
            .and_then(|repository| repository.get("nameWithOwner"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        number: node.get("number").and_then(Value::as_i64)?,
        title: node
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        url: node
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        is_draft: node.get("isDraft").and_then(Value::as_bool).unwrap_or(false),
        labels: node
            .get("labels")
            .and_then(|labels| labels.get("nodes"))
            .and_then(Value::as_array)
            .map(|nodes| {
                nodes
                    .iter()
                    .filter_map(|label| label.get("name").and_then(Value::as_str))
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
    })
}

/// What is waiting on you in one repository, as three searches scoped to it.
///
/// Not the fragment above: this one needs what a row is labelled by — the review decision and the
/// head commit's checks — and asking for those on every item of the cross-repository list would
/// slow that query down for nothing it shows.
const REPOSITORY_QUERY: &str = r#"
query($reviews:String!,$authored:String!,$assigned:String!,$count:Int!){
  reviews: search(query:$reviews, type:ISSUE, first:$count){ nodes{ ...Waiting } }
  authored: search(query:$authored, type:ISSUE, first:$count){ nodes{ ...Waiting } }
  assigned: search(query:$assigned, type:ISSUE, first:$count){ nodes{ ...Waiting } }
}
fragment Waiting on SearchResultItem {
  __typename
  ... on Issue {
    number title url updatedAt
    repository{nameWithOwner}
    labels(first:5){nodes{name}}
  }
  ... on PullRequest {
    number title url updatedAt isDraft reviewDecision
    repository{nameWithOwner}
    labels(first:5){nodes{name}}
    commits(last:1){nodes{commit{statusCheckRollup{state}}}}
  }
}"#;

/// Why something in a repository is waiting on you.
///
/// Declared most urgent first, and the derived order is what sorts the list. A review request means
/// someone else is blocked on you. Your own pull request with changes requested or failing checks
/// is blocked on you, but nobody else is. An assigned issue is waiting, but nothing is stuck.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum WaitingReason {
    ReviewRequested,
    ChangesRequested,
    ChecksFailing,
    Assigned,
}

impl WaitingReason {
    pub fn label(self) -> &'static str {
        match self {
            WaitingReason::ReviewRequested => "Ready for review",
            WaitingReason::ChangesRequested => "Changes requested",
            WaitingReason::ChecksFailing => "Checks failing",
            WaitingReason::Assigned => "Assigned issue",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Waiting {
    pub reason: WaitingReason,
    pub item: Item,
    /// The last activity GitHub recorded, which is what "2 minutes ago" is measured from.
    pub updated_at: Option<OffsetDateTime>,
}

/// Asks GitHub what is waiting on you in `repository`, most urgent first, keeping `limit` of it.
pub async fn fetch_waiting_in_repository(
    client: &Client,
    repository: &str,
    limit: usize,
) -> Result<Vec<Waiting>> {
    let scope = search_scope(repository)?;

    // Sorted by update so that when a search is cut at `PER_BUCKET`, the stale end is what is lost.
    let data = client
        .graphql(
            REPOSITORY_QUERY,
            json!({
                // A draft is its author saying it is not ready, and the row would say it is.
                "reviews": format!(
                    "{scope} is:pr is:open draft:false review-requested:@me sort:updated-desc"
                ),
                "authored": format!("{scope} is:pr is:open author:@me sort:updated-desc"),
                "assigned": format!("{scope} is:issue is:open assignee:@me sort:updated-desc"),
                "count": PER_BUCKET,
            }),
        )
        .await
        .with_context(|| format!("asking GitHub what is waiting on you in {repository}"))?;

    Ok(most_urgent(parse_waiting(&data), limit))
}

/// The `repo:` qualifier for a search, refusing anything that is not plainly `owner/name`.
///
/// The name comes from a git remote, which is text anyone can write. A space in it would end the
/// qualifier and let the rest of the remote add search terms of its own.
fn search_scope(repository: &str) -> Result<String> {
    let plain = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
    };
    let valid = repository
        .split_once('/')
        .is_some_and(|(owner, name)| plain(owner) && plain(name));

    if !valid {
        bail!("`{repository}` is not a GitHub repository name");
    }
    Ok(format!("repo:{repository}"))
}

/// Sorts by urgency and then by most recent activity, and keeps the first `limit`.
pub fn most_urgent(mut waiting: Vec<Waiting>, limit: usize) -> Vec<Waiting> {
    // `None` orders before any time, so comparing right to left puts an item with no timestamp
    // last: it cannot claim to be recent.
    waiting.sort_by(|left, right| {
        left.reason
            .cmp(&right.reason)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
    });

    // One pull request can answer two searches; it is shown once, under its more urgent reason,
    // which the sort has already put first.
    let mut seen: Vec<(String, i64)> = Vec::new();
    waiting.retain(|entry| {
        let key = (entry.item.repository.clone(), entry.item.number);
        if seen.contains(&key) {
            return false;
        }
        seen.push(key);
        true
    });

    waiting.truncate(limit);
    waiting
}

fn parse_waiting(data: &Value) -> Vec<Waiting> {
    let buckets = [
        ("reviews", Some(WaitingReason::ReviewRequested)),
        // Being the author is not by itself a reason; the pull request has to be blocked on you.
        ("authored", None),
        ("assigned", Some(WaitingReason::Assigned)),
    ];

    let mut waiting = Vec::new();
    for (bucket, reason) in buckets {
        let Some(nodes) = data
            .get(bucket)
            .and_then(|bucket| bucket.get("nodes"))
            .and_then(Value::as_array)
        else {
            continue;
        };

        for node in nodes {
            let Some(item) = item(node) else {
                continue;
            };
            let Some(reason) = reason.or_else(|| authored_reason(node)) else {
                continue;
            };
            waiting.push(Waiting {
                reason,
                item,
                // A timestamp that does not parse costs the row its "2 minutes ago", not its place.
                updated_at: node
                    .get("updatedAt")
                    .and_then(Value::as_str)
                    .and_then(|text| OffsetDateTime::parse(text, &Rfc3339).ok()),
            });
        }
    }
    waiting
}

/// Whether your own pull request is blocked on you, and on what.
///
/// Changes requested wins over failing checks: a reviewer's request is the one a push alone will
/// not answer.
fn authored_reason(node: &Value) -> Option<WaitingReason> {
    if node.get("reviewDecision").and_then(Value::as_str) == Some("CHANGES_REQUESTED") {
        return Some(WaitingReason::ChangesRequested);
    }

    // `statusCheckRollup` is null on a commit nothing has checked, which is not a failure.
    let checks = node
        .get("commits")?
        .get("nodes")?
        .as_array()?
        .first()?
        .get("commit")?
        .get("statusCheckRollup")?
        .get("state")?
        .as_str()?;
    matches!(checks, "FAILURE" | "ERROR").then_some(WaitingReason::ChecksFailing)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape GitHub actually returned for this account.
    fn real_answer() -> Value {
        json!({
            "assigned": { "issueCount": 0, "nodes": [] },
            "reviews": { "issueCount": 9, "nodes": [
                {
                    "__typename": "PullRequest",
                    "number": 11,
                    "title": "Bump @vscode/markdown-editor from 0.0.2-90 to 0.0.2-91",
                    "url": "https://github.com/devconnecting1/vscode/pull/11",
                    "isDraft": false,
                    "repository": { "nameWithOwner": "devconnecting1/vscode" },
                    "labels": { "nodes": [{ "name": "dependencies" }] },
                    "reviewDecision": null
                }
            ]},
            "authored": { "issueCount": 1, "nodes": [
                {
                    "__typename": "PullRequest",
                    "number": 1,
                    "title": "Add Cowork, a native AI panel and thread view",
                    "url": "https://github.com/Workspaacing/wu/pull/1",
                    "isDraft": false,
                    "repository": { "nameWithOwner": "Workspaacing/wu" },
                    "labels": { "nodes": [] }
                }
            ]}
        })
    }

    #[test]
    fn the_three_buckets_are_read_from_one_answer() {
        let worklist = parse(&real_answer());

        assert!(worklist.assigned.is_empty());
        assert_eq!(worklist.reviews.len(), 1);
        assert_eq!(worklist.authored.len(), 1);
        assert_eq!(worklist.total(), 2);
        assert!(!worklist.is_empty());
    }

    #[test]
    fn an_item_carries_what_the_row_and_the_agent_both_need() {
        let worklist = parse(&real_answer());
        let review = &worklist.reviews[0];

        assert_eq!(review.kind, Kind::PullRequest);
        assert_eq!(review.slug(), "devconnecting1/vscode#11");
        assert_eq!(review.labels, vec!["dependencies"]);
        assert!(review.url.ends_with("/pull/11"));
    }

    #[test]
    fn a_bucket_that_failed_does_not_take_the_others_with_it() {
        // One search erroring should leave a screen with two useful lists, not a blank one.
        let partial = json!({
            "assigned": null,
            "reviews": { "nodes": [] },
            "authored": { "issueCount": 1, "nodes": [
                { "__typename": "Issue", "number": 7, "title": "x",
                  "repository": { "nameWithOwner": "a/b" } }
            ]}
        });

        let worklist = parse(&partial);
        assert!(worklist.assigned.is_empty());
        assert_eq!(worklist.authored.len(), 1);
    }

    #[test]
    fn something_that_is_neither_an_issue_nor_a_pull_request_is_not_work() {
        // A search of type ISSUE can return other node types; they have no number to act on.
        let odd = json!({ "authored": { "nodes": [
            { "__typename": "Repository", "name": "wu" },
            { "__typename": "Issue", "number": 1, "title": "real",
              "repository": { "nameWithOwner": "a/b" } }
        ]}});

        let worklist = parse(&odd);
        assert_eq!(worklist.authored.len(), 1);
        assert_eq!(worklist.authored[0].title, "real");
    }

    #[test]
    fn a_node_with_no_number_is_dropped_rather_than_shown_as_zero() {
        // `#0` is not a thing, and a row that cannot be opened is worse than no row.
        let broken = json!({ "authored": { "nodes": [
            { "__typename": "Issue", "title": "no number",
              "repository": { "nameWithOwner": "a/b" } }
        ]}});

        assert!(parse(&broken).authored.is_empty());
    }

    #[test]
    fn the_handoff_tells_the_agent_to_fetch_the_item_itself() {
        // Pasting a copy of the issue would be stale by the time it is read, and the agent has a
        // tool for this. The prompt has to name the tool and the item.
        let worklist = parse(&real_answer());

        let review = worklist.reviews[0].handoff_prompt();
        assert!(review.contains("github_pull_request"), "{review}");
        assert!(review.contains("devconnecting1/vscode#11"), "{review}");

        let issue = Item {
            kind: Kind::Issue,
            repository: "a/b".into(),
            number: 3,
            title: "t".into(),
            url: String::new(),
            is_draft: false,
            labels: Vec::new(),
        };
        let prompt = issue.handoff_prompt();
        assert!(prompt.contains("github_issue"), "{prompt}");
        assert!(prompt.contains("a/b#3"), "{prompt}");
    }

    #[test]
    fn a_draft_is_marked_so_the_row_can_say_so() {
        let drafts = json!({ "reviews": { "nodes": [
            { "__typename": "PullRequest", "number": 2, "title": "wip", "isDraft": true,
              "repository": { "nameWithOwner": "a/b" } }
        ]}});

        assert!(parse(&drafts).reviews[0].is_draft);
    }

    fn waiting(reason: WaitingReason, number: i64, updated_at: Option<&str>) -> Waiting {
        Waiting {
            reason,
            item: Item {
                kind: if reason == WaitingReason::Assigned {
                    Kind::Issue
                } else {
                    Kind::PullRequest
                },
                repository: "Workspaacing/wu".into(),
                number,
                title: format!("item {number}"),
                url: String::new(),
                is_draft: false,
                labels: Vec::new(),
            },
            updated_at: updated_at
                .map(|text| OffsetDateTime::parse(text, &Rfc3339).expect("a valid timestamp")),
        }
    }

    fn numbers(waiting: &[Waiting]) -> Vec<i64> {
        waiting.iter().map(|entry| entry.item.number).collect()
    }

    fn pull_request(number: i64, review_decision: Value, checks: Value) -> Value {
        json!({
            "__typename": "PullRequest",
            "number": number,
            "title": format!("pull request {number}"),
            "url": format!("https://github.com/Workspaacing/wu/pull/{number}"),
            "updatedAt": "2026-09-12T10:00:00Z",
            "repository": { "nameWithOwner": "Workspaacing/wu" },
            "reviewDecision": review_decision,
            "commits": { "nodes": [{ "commit": { "statusCheckRollup": checks } }] },
        })
    }

    #[test]
    fn someone_blocked_on_you_comes_before_anything_more_recent() {
        let sorted = most_urgent(
            vec![
                waiting(WaitingReason::Assigned, 1, Some("2026-09-12T12:00:00Z")),
                waiting(WaitingReason::ChecksFailing, 2, Some("2026-09-12T11:00:00Z")),
                waiting(WaitingReason::ChangesRequested, 3, Some("2026-09-12T10:00:00Z")),
                waiting(WaitingReason::ReviewRequested, 4, Some("2026-09-01T00:00:00Z")),
            ],
            5,
        );

        assert_eq!(numbers(&sorted), vec![4, 3, 2, 1]);
    }

    #[test]
    fn within_one_reason_the_most_recent_activity_leads() {
        let sorted = most_urgent(
            vec![
                waiting(WaitingReason::ReviewRequested, 1, Some("2026-09-10T00:00:00Z")),
                waiting(WaitingReason::ReviewRequested, 2, None),
                waiting(WaitingReason::ReviewRequested, 3, Some("2026-09-12T00:00:00Z")),
            ],
            5,
        );

        // No timestamp cannot claim to be recent.
        assert_eq!(numbers(&sorted), vec![3, 1, 2]);
    }

    #[test]
    fn the_list_is_cut_after_sorting_so_the_urgent_item_survives_the_cut() {
        let mut many = (1..=7)
            .map(|number| waiting(WaitingReason::Assigned, number, Some("2026-09-12T00:00:00Z")))
            .collect::<Vec<_>>();
        many.push(waiting(WaitingReason::ReviewRequested, 99, None));

        let kept = most_urgent(many, 5);
        assert_eq!(kept.len(), 5);
        assert_eq!(kept[0].item.number, 99);
    }

    #[test]
    fn an_item_that_answers_two_searches_is_shown_once_under_the_more_urgent_reason() {
        let kept = most_urgent(
            vec![
                waiting(WaitingReason::ChecksFailing, 7, Some("2026-09-12T00:00:00Z")),
                waiting(WaitingReason::ReviewRequested, 7, Some("2026-09-11T00:00:00Z")),
            ],
            5,
        );

        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].reason, WaitingReason::ReviewRequested);
    }

    #[test]
    fn each_reason_reads_the_way_the_row_says_it() {
        assert_eq!(WaitingReason::ReviewRequested.label(), "Ready for review");
        assert_eq!(WaitingReason::ChangesRequested.label(), "Changes requested");
        assert_eq!(WaitingReason::ChecksFailing.label(), "Checks failing");
        assert_eq!(WaitingReason::Assigned.label(), "Assigned issue");
    }

    #[test]
    fn your_own_pull_request_is_waiting_only_when_it_is_blocked_on_you() {
        let answer = json!({ "authored": { "nodes": [
            pull_request(1, json!("CHANGES_REQUESTED"), json!({ "state": "SUCCESS" })),
            pull_request(2, json!("REVIEW_REQUIRED"), json!({ "state": "FAILURE" })),
            pull_request(3, Value::Null, json!({ "state": "ERROR" })),
            pull_request(4, json!("APPROVED"), json!({ "state": "SUCCESS" })),
            pull_request(5, Value::Null, json!({ "state": "PENDING" })),
            // A repository with no CI; the real shape of Workspaacing/wu#1.
            pull_request(6, Value::Null, Value::Null),
            pull_request(7, json!("CHANGES_REQUESTED"), json!({ "state": "FAILURE" })),
        ]}});

        let reasons = parse_waiting(&answer)
            .into_iter()
            .map(|entry| (entry.item.number, entry.reason))
            .collect::<Vec<_>>();

        assert_eq!(
            reasons,
            vec![
                (1, WaitingReason::ChangesRequested),
                (2, WaitingReason::ChecksFailing),
                (3, WaitingReason::ChecksFailing),
                (7, WaitingReason::ChangesRequested),
            ]
        );
    }

    #[test]
    fn a_review_request_or_an_assignment_is_waiting_by_being_found_at_all() {
        let answer = json!({
            "reviews": { "nodes": [
                pull_request(11, json!("REVIEW_REQUIRED"), json!({ "state": "FAILURE" })),
            ]},
            "assigned": { "nodes": [{
                "__typename": "Issue", "number": 3, "title": "Sessions vanish",
                "updatedAt": "2026-09-12T09:58:00Z",
                "repository": { "nameWithOwner": "Workspaacing/wu" }
            }]},
        });

        let found = parse_waiting(&answer);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].reason, WaitingReason::ReviewRequested);
        assert_eq!(found[1].reason, WaitingReason::Assigned);
        assert_eq!(found[1].item.kind, Kind::Issue);
        assert_eq!(
            found[1].updated_at,
            Some(OffsetDateTime::parse("2026-09-12T09:58:00Z", &Rfc3339).expect("valid"))
        );
    }

    #[test]
    fn a_timestamp_that_does_not_parse_costs_the_time_not_the_row() {
        let answer = json!({ "assigned": { "nodes": [{
            "__typename": "Issue", "number": 5, "title": "x", "updatedAt": "yesterday-ish",
            "repository": { "nameWithOwner": "Workspaacing/wu" }
        }]}});

        let found = parse_waiting(&answer);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].updated_at, None);
    }

    #[test]
    fn the_row_hands_off_the_prompt_the_github_window_sends() {
        let entry = waiting(WaitingReason::ReviewRequested, 11, None);
        assert_eq!(
            entry.item.handoff_prompt(),
            "Read pull request Workspaacing/wu#11 with `github_pull_request`, including its diff, \
             and review it against the code in this project. Say what is wrong and what is fine."
        );
    }

    #[test]
    fn the_search_is_scoped_to_exactly_the_repository_named() {
        assert_eq!(
            search_scope("Workspaacing/wu").expect("plain"),
            "repo:Workspaacing/wu"
        );
        assert_eq!(
            search_scope("my.org/re-po_1").expect("plain"),
            "repo:my.org/re-po_1"
        );
    }

    #[test]
    fn a_name_that_would_add_search_terms_of_its_own_is_refused() {
        for wrong in [
            "a/b author:someone",
            "a/b\nis:closed",
            "a",
            "/b",
            "a/",
            "a/b/c",
            "",
        ] {
            assert!(search_scope(wrong).is_err(), "{wrong:?}");
        }
    }
}
