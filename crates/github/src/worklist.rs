//! What is waiting for you, across every repository.
//!
//! Three questions, asked at once because they are one screen: what is assigned to me, what is
//! waiting on my review, and what have I opened. That is the whole of "what should I be doing",
//! and it is deliberately not a list of a repository's issues — the repository view is the one
//! thing github.com does better than any editor could, and it is one keystroke away.
//!
//! The point of having it here instead is what sits next to each row: the code, and an agent that
//! can be handed the item.

use anyhow::{Context as _, Result};
use serde_json::{Value, json};

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
}
