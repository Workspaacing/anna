//! The repositories reachable from this account, grouped by who owns them.
//!
//! Grouping is the whole point. An account with a personal namespace and three organisations has
//! four different sets of repositories with four different sets of collaborators, and a flat list
//! of two hundred names sorted by date tells you nothing about which is which. `acme/api` and
//! `myname/api` are different projects and the only thing separating them in a flat list is a
//! prefix the eye skips.

use anyhow::{Context as _, Result};
use serde_json::{Value, json};

use crate::api::Client;

/// How many repositories to ask for per owner.
///
/// Past this the list stops being browsable and wants a search instead, which is a different
/// feature. GitHub caps a page at a hundred in any case.
const PER_OWNER: usize = 100;

/// How many organisations to look at.
const MAX_ORGANISATIONS: usize = 25;

const QUERY: &str = r#"
query($perOwner:Int!,$organisations:Int!){
  viewer{
    login avatarUrl
    repositories(first:$perOwner, ownerAffiliations:[OWNER],
                 orderBy:{field:UPDATED_AT,direction:DESC}){
      totalCount nodes{ ...Repo }
    }
    organizations(first:$organisations){
      nodes{
        login name avatarUrl
        repositories(first:$perOwner, orderBy:{field:UPDATED_AT,direction:DESC}){
          totalCount nodes{ ...Repo }
        }
      }
    }
  }
}
fragment Repo on Repository {
  name nameWithOwner description isPrivate isArchived url
  primaryLanguage{name}
  issues(states:OPEN){totalCount}
  pullRequests(states:OPEN){totalCount}
}"#;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OwnerKind {
    Personal,
    Organisation,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Owner {
    pub login: String,
    /// An organisation's display name, when it has one different from its login.
    pub name: Option<String>,
    pub kind: OwnerKind,
    pub repositories: Vec<Repository>,
    /// What GitHub says the total is, which may exceed what was fetched.
    pub total: usize,
}

impl Owner {
    /// What the filter chip reads.
    pub fn label(&self) -> String {
        match self.kind {
            OwnerKind::Personal => "Personal".to_owned(),
            OwnerKind::Organisation => self
                .name
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| self.login.clone()),
        }
    }

    /// Whether more exist than were fetched, which the list has to admit rather than imply.
    pub fn truncated(&self) -> bool {
        self.total > self.repositories.len()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Repository {
    pub name: String,
    /// `owner/name`, which is what every tool here takes.
    pub full_name: String,
    pub description: Option<String>,
    pub private: bool,
    pub archived: bool,
    pub language: Option<String>,
    pub url: String,
    pub open_issues: usize,
    pub open_pull_requests: usize,
}

/// Every owner whose repositories this token can see, personal first.
pub async fn fetch(client: &Client) -> Result<Vec<Owner>> {
    let data = client
        .graphql(
            QUERY,
            json!({ "perOwner": PER_OWNER, "organisations": MAX_ORGANISATIONS }),
        )
        .await
        .context("asking GitHub which repositories you can see")?;

    Ok(parse(&data))
}

fn parse(data: &Value) -> Vec<Owner> {
    let Some(viewer) = data.get("viewer") else {
        return Vec::new();
    };

    // Personal first: it is the one namespace nobody has to be told the name of.
    let mut owners = vec![Owner {
        login: text(viewer, "login"),
        name: None,
        kind: OwnerKind::Personal,
        repositories: repositories(viewer.get("repositories")),
        total: total(viewer.get("repositories")),
    }];

    if let Some(nodes) = viewer
        .get("organizations")
        .and_then(|organisations| organisations.get("nodes"))
        .and_then(Value::as_array)
    {
        for organisation in nodes {
            owners.push(Owner {
                login: text(organisation, "login"),
                name: Some(text(organisation, "name")).filter(|name| !name.is_empty()),
                kind: OwnerKind::Organisation,
                repositories: repositories(organisation.get("repositories")),
                total: total(organisation.get("repositories")),
            });
        }
    }

    // An owner with nothing to show is a chip that leads to an empty screen.
    owners.retain(|owner| !owner.repositories.is_empty());
    owners
}

fn total(node: Option<&Value>) -> usize {
    node.and_then(|node| node.get("totalCount"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize
}

fn repositories(node: Option<&Value>) -> Vec<Repository> {
    node.and_then(|node| node.get("nodes"))
        .and_then(Value::as_array)
        .map(|nodes| nodes.iter().filter_map(repository).collect())
        .unwrap_or_default()
}

fn repository(node: &Value) -> Option<Repository> {
    let full_name = text(node, "nameWithOwner");
    if full_name.is_empty() {
        return None;
    }

    Some(Repository {
        name: text(node, "name"),
        full_name,
        description: Some(text(node, "description")).filter(|body| !body.is_empty()),
        private: node.get("isPrivate").and_then(Value::as_bool).unwrap_or(false),
        archived: node
            .get("isArchived")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        language: node
            .get("primaryLanguage")
            .and_then(|language| language.get("name"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        url: text(node, "url"),
        open_issues: count(node, "issues"),
        open_pull_requests: count(node, "pullRequests"),
    })
}

fn count(node: &Value, field: &str) -> usize {
    node.get(field)
        .and_then(|value| value.get("totalCount"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize
}

fn text(node: &Value, field: &str) -> String {
    node.get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape GitHub returned for this account, trimmed to two repositories per owner.
    fn answer() -> Value {
        json!({
            "viewer": {
                "login": "devconnecting1",
                "repositories": {
                    "totalCount": 22,
                    "nodes": [
                        {
                            "name": "coolify", "nameWithOwner": "devconnecting1/coolify",
                            "description": "", "isPrivate": false, "isArchived": false,
                            "url": "https://github.com/devconnecting1/coolify",
                            "primaryLanguage": { "name": "PHP" },
                            "issues": { "totalCount": 0 },
                            "pullRequests": { "totalCount": 0 }
                        },
                        {
                            "name": "rei-do-ape-vercel", "nameWithOwner": "devconnecting1/rei-do-ape-vercel",
                            "description": "A shop", "isPrivate": true, "isArchived": false,
                            "url": "https://github.com/devconnecting1/rei-do-ape-vercel",
                            "primaryLanguage": null,
                            "issues": { "totalCount": 1 },
                            "pullRequests": { "totalCount": 0 }
                        }
                    ]
                },
                "organizations": { "nodes": [
                    {
                        "login": "Workspaacing", "name": "Workspaacing",
                        "repositories": {
                            "totalCount": 22,
                            "nodes": [{
                                "name": "wu", "nameWithOwner": "Workspaacing/wu",
                                "description": "An editor", "isPrivate": false, "isArchived": false,
                                "url": "https://github.com/Workspaacing/wu",
                                "primaryLanguage": { "name": "Rust" },
                                "issues": { "totalCount": 0 },
                                "pullRequests": { "totalCount": 1 }
                            }]
                        }
                    }
                ]}
            }
        })
    }

    #[test]
    fn personal_comes_first_and_organisations_follow() {
        // Personal is the one namespace nobody needs told the name of, so it leads.
        let owners = parse(&answer());

        assert_eq!(owners.len(), 2);
        assert_eq!(owners[0].kind, OwnerKind::Personal);
        assert_eq!(owners[0].label(), "Personal");
        assert_eq!(owners[1].kind, OwnerKind::Organisation);
        assert_eq!(owners[1].label(), "Workspaacing");
    }

    #[test]
    fn a_repository_carries_what_a_row_needs_to_be_worth_reading() {
        let owners = parse(&answer());
        let shop = &owners[0].repositories[1];

        assert_eq!(shop.full_name, "devconnecting1/rei-do-ape-vercel");
        assert_eq!(shop.description.as_deref(), Some("A shop"));
        assert!(shop.private);
        assert_eq!(shop.open_issues, 1);
        assert_eq!(shop.language, None, "a repo with no language says none");
    }

    #[test]
    fn an_empty_description_is_no_description_rather_than_an_empty_line() {
        let owners = parse(&answer());
        assert_eq!(owners[0].repositories[0].description, None);
    }

    #[test]
    fn the_two_namespaces_that_share_a_name_stay_apart() {
        // `acme/api` and `myname/api` are different projects, and a flat list separates them only
        // by a prefix the eye skips. This is the reason the view groups at all.
        let owners = parse(&json!({
            "viewer": {
                "login": "me",
                "repositories": { "totalCount": 1, "nodes": [
                    { "name": "api", "nameWithOwner": "me/api" }
                ]},
                "organizations": { "nodes": [{
                    "login": "acme", "name": "Acme",
                    "repositories": { "totalCount": 1, "nodes": [
                        { "name": "api", "nameWithOwner": "acme/api" }
                    ]}
                }]}
            }
        }));

        assert_eq!(owners[0].repositories[0].full_name, "me/api");
        assert_eq!(owners[1].repositories[0].full_name, "acme/api");
        assert_eq!(owners[1].label(), "Acme");
    }

    #[test]
    fn an_owner_with_nothing_to_show_is_not_offered_as_a_filter() {
        // A chip that leads to an empty screen is worse than no chip.
        let owners = parse(&json!({
            "viewer": {
                "login": "me",
                "repositories": { "totalCount": 0, "nodes": [] },
                "organizations": { "nodes": [{
                    "login": "empty", "name": "Empty",
                    "repositories": { "totalCount": 0, "nodes": [] }
                }]}
            }
        }));

        assert!(owners.is_empty());
    }

    #[test]
    fn more_repositories_than_were_fetched_is_admitted_rather_than_implied() {
        let owners = parse(&answer());

        // 22 exist, two came back.
        assert!(owners[0].truncated());
        assert_eq!(owners[0].total, 22);
        assert_eq!(owners[0].repositories.len(), 2);
    }

    #[test]
    fn an_organisation_with_no_display_name_falls_back_to_its_login() {
        let owners = parse(&json!({
            "viewer": {
                "login": "me",
                "repositories": { "totalCount": 0, "nodes": [] },
                "organizations": { "nodes": [{
                    "login": "bare", "name": "",
                    "repositories": { "totalCount": 1, "nodes": [
                        { "name": "x", "nameWithOwner": "bare/x" }
                    ]}
                }]}
            }
        }));

        assert_eq!(owners[0].label(), "bare");
    }

    #[test]
    fn a_node_with_no_full_name_is_dropped_rather_than_shown_blank() {
        let owners = parse(&json!({
            "viewer": {
                "login": "me",
                "repositories": { "totalCount": 2, "nodes": [
                    { "name": "good", "nameWithOwner": "me/good" },
                    { "name": "broken" }
                ]},
                "organizations": { "nodes": [] }
            }
        }));

        assert_eq!(owners[0].repositories.len(), 1);
        assert_eq!(owners[0].repositories[0].full_name, "me/good");
    }
}
