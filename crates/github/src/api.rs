//! Talking to GitHub.
//!
//! Two APIs, because GitHub splits them: REST answers "who am I and what may I do", and Projects
//! exists only in GraphQL. Both go through here so there is one place that knows how a GitHub
//! failure is shaped.
//!
//! The shape is the reason this module is not three lines. GraphQL answers `200 OK` and puts the
//! failure in the body, so the usual `status.is_success()` gate passes over every error there is —
//! a missing scope, a repository that does not exist, a token that was revoked. Worse, it reports
//! one error *per field*: asking for a project the token may not read comes back as thirty-seven
//! separate messages that all say the same thing. Handing those to a user unedited would be
//! useless, so they are read, classified and reduced to the one fact that matters.

use anyhow::{Context as _, Result, anyhow, bail};
use futures::AsyncReadExt as _;
use http_client::{AsyncBody, HttpClient, HttpRequestExt as _, Request};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};

const API_BASE: &str = "https://api.github.com";
const GRAPHQL_URL: &str = "https://api.github.com/graphql";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// The scope a token needs before any of the Projects queries will answer.
pub const PROJECT_READ_SCOPE: &str = "read:project";

/// What went wrong, at a granularity the user interface can act on.
///
/// The distinction that earns this enum is `MissingScopes`: it is the one failure with a specific
/// remedy the user can carry out, and telling them the remedy is the entire difference between a
/// dead end and a button.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The token is valid but was not granted something the query needed.
    MissingScopes { needed: Vec<String> },
    /// The token is not valid, or no longer is.
    Unauthorized,
    /// Anything else GitHub said, already reduced to one line.
    Message(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::MissingScopes { needed } => write!(
                formatter,
                "this token is missing the {} scope",
                needed.join(" and ")
            ),
            Failure::Unauthorized => write!(formatter, "this token was rejected by GitHub"),
            Failure::Message(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for Failure {}

/// An authenticated connection to GitHub.
#[derive(Clone)]
pub struct Client {
    http: Arc<dyn HttpClient>,
    token: Arc<str>,
}

/// Who the token belongs to, and what it is allowed to do.
///
/// Both come from one request, because `GET /user` reports the granted scopes in a response header.
/// Asking first is worth a round trip: the alternative is discovering a missing scope as a wall of
/// GraphQL errors after the user has already picked a repository.
#[derive(Debug, Clone)]
pub struct Identity {
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
    pub scopes: Vec<String>,
}

impl Identity {
    pub fn can_read_projects(&self) -> bool {
        self.scopes
            .iter()
            .any(|scope| scope == PROJECT_READ_SCOPE || scope == "project")
    }

    /// The name to show, which is the display name when there is one.
    pub fn label(&self) -> &str {
        self.name
            .as_deref()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.login)
    }
}

impl Client {
    pub fn new(http: Arc<dyn HttpClient>, token: impl Into<Arc<str>>) -> Self {
        Self {
            http,
            token: token.into(),
        }
    }

    /// Confirms the token works and reports what it may do.
    pub async fn identity(&self) -> Result<Identity> {
        let url = format!("{API_BASE}/user");
        let request = Request::get(&url)
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .header("user-agent", "Anna")
            .header("authorization", format!("Bearer {}", self.token))
            .timeout(REQUEST_TIMEOUT)
            .body(AsyncBody::empty())
            .context("building the identity request")?;

        let mut response = self
            .http
            .send(request)
            .await
            .context("asking GitHub who this token belongs to")?;

        // Read before branching on the status: the failure body is where GitHub explains itself.
        let status = response.status();
        let scopes = response
            .headers()
            .get("x-oauth-scopes")
            .and_then(|value| value.to_str().ok())
            .map(parse_scopes)
            .unwrap_or_default();

        let mut body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut body)
            .await
            .context("reading GitHub's answer")?;

        if status == http_client::StatusCode::UNAUTHORIZED {
            return Err(Failure::Unauthorized.into());
        }
        if !status.is_success() {
            return Err(Failure::Message(format!("GitHub returned {status}")).into());
        }

        #[derive(Deserialize)]
        struct User {
            login: String,
            name: Option<String>,
            avatar_url: Option<String>,
        }

        let user: User = serde_json::from_slice(&body).context("parsing the GitHub user")?;
        Ok(Identity {
            login: user.login,
            name: user.name,
            avatar_url: user.avatar_url,
            scopes,
        })
    }

    /// The unified diff of a pull request, exactly as GitHub renders it.
    ///
    /// This is the one thing here that has to be REST. GraphQL will describe which files changed
    /// and by how many lines, but it will not hand over a patch, and assembling one from the file
    /// list would produce a worse copy of something GitHub already generates correctly — including
    /// renames, mode changes and binary files. Asking for the `diff` media type on the ordinary
    /// pull request endpoint returns the real thing.
    pub async fn pull_request_diff(&self, owner: &str, name: &str, number: i64) -> Result<String> {
        let url = format!("{API_BASE}/repos/{owner}/{name}/pulls/{number}");
        let request = Request::get(&url)
            .header("accept", "application/vnd.github.diff")
            .header("x-github-api-version", "2022-11-28")
            .header("user-agent", "Anna")
            .header("authorization", format!("Bearer {}", self.token))
            .follow_redirects(http_client::RedirectPolicy::FollowAll)
            .timeout(REQUEST_TIMEOUT)
            .body(AsyncBody::empty())
            .context("building the diff request")?;

        let mut response = self
            .http
            .send(request)
            .await
            .context("asking GitHub for the diff")?;

        let status = response.status();
        let mut body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut body)
            .await
            .context("reading the diff")?;

        if status == http_client::StatusCode::UNAUTHORIZED {
            return Err(Failure::Unauthorized.into());
        }
        if !status.is_success() {
            return Err(Failure::Message(format!(
                "GitHub returned {status} for the diff of {owner}/{name}#{number}"
            ))
            .into());
        }

        // A diff is bytes, not necessarily UTF-8 — a patch touching a latin-1 file is ordinary.
        // Losing a character is better than refusing to show the change.
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    /// A plain REST GET, for the corners of GitHub that GraphQL does not reach.
    ///
    /// Checks are one of them: the Actions API is REST-only, and it is where the reason a build is
    /// red actually lives.
    pub async fn rest(&self, path: &str) -> Result<Value> {
        let url = format!("{API_BASE}/{}", path.trim_start_matches('/'));
        let request = Request::get(&url)
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .header("user-agent", "Anna")
            .header("authorization", format!("Bearer {}", self.token))
            .follow_redirects(http_client::RedirectPolicy::FollowAll)
            .timeout(REQUEST_TIMEOUT)
            .body(AsyncBody::empty())
            .with_context(|| format!("building a request for {url}"))?;

        let mut response = self
            .http
            .send(request)
            .await
            .with_context(|| format!("asking GitHub for {path}"))?;

        let status = response.status();
        let mut body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut body)
            .await
            .context("reading GitHub's answer")?;

        if status == http_client::StatusCode::UNAUTHORIZED {
            return Err(Failure::Unauthorized.into());
        }
        if !status.is_success() {
            let detail = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| format!("GitHub returned {status}"));
            return Err(Failure::Message(detail).into());
        }

        serde_json::from_slice(&body).with_context(|| format!("parsing GitHub's answer for {path}"))
    }

    /// A REST GET whose answer is text rather than JSON.
    ///
    /// A job's log is the case this exists for: GitHub answers with a redirect to a plain-text
    /// file, and that file is the only place the reason a step failed is written down.
    pub async fn rest_text(&self, path: &str) -> Result<String> {
        let url = format!("{API_BASE}/{}", path.trim_start_matches('/'));
        let request = Request::get(&url)
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .header("user-agent", "Anna")
            .header("authorization", format!("Bearer {}", self.token))
            .follow_redirects(http_client::RedirectPolicy::FollowAll)
            .timeout(REQUEST_TIMEOUT)
            .body(AsyncBody::empty())
            .with_context(|| format!("building a request for {url}"))?;

        let mut response = self
            .http
            .send(request)
            .await
            .with_context(|| format!("asking GitHub for {path}"))?;

        let status = response.status();
        let mut body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut body)
            .await
            .context("reading GitHub's answer")?;

        if status == http_client::StatusCode::UNAUTHORIZED {
            return Err(Failure::Unauthorized.into());
        }
        if !status.is_success() {
            let detail = serde_json::from_slice::<Value>(&body)
                .ok()
                .and_then(|value| {
                    value
                        .get("message")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| format!("GitHub returned {status}"));
            return Err(Failure::Message(detail).into());
        }

        // A log is whatever the job printed, which is not always UTF-8.
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    /// Runs one GraphQL query and returns its `data`, or the reduced failure.
    pub async fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let body = serde_json::to_vec(&json!({ "query": query, "variables": variables }))
            .context("serializing the GraphQL query")?;

        let request = Request::post(GRAPHQL_URL)
            .header("content-type", "application/json")
            .header("user-agent", "Anna")
            .header("authorization", format!("Bearer {}", self.token))
            .timeout(REQUEST_TIMEOUT)
            .body(AsyncBody::from(body))
            .context("building the GraphQL request")?;

        let mut response = self
            .http
            .send(request)
            .await
            .context("sending a query to GitHub")?;

        let status = response.status();
        let mut body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut body)
            .await
            .context("reading GitHub's answer")?;

        if status == http_client::StatusCode::UNAUTHORIZED {
            return Err(Failure::Unauthorized.into());
        }

        let payload: Value = serde_json::from_slice(&body).with_context(|| {
            format!("parsing GitHub's answer, which came back as {status} and was not JSON")
        })?;

        if let Some(failure) = classify(&payload) {
            return Err(failure.into());
        }

        // A transport failure with no `errors` array is still a failure, just an unhelpful one.
        if !status.is_success() {
            bail!("GitHub returned {status}");
        }

        payload
            .get("data")
            .cloned()
            .ok_or_else(|| anyhow!("GitHub answered without any data"))
    }
}

/// Reduces GraphQL's `errors` array to the single fact worth reporting.
///
/// Returns `None` when there is nothing wrong. Every error in the array is examined rather than
/// only the first, because GitHub orders them by field position, not by importance: an expired
/// token buried behind thirty scope complaints is still the thing to say.
fn classify(payload: &Value) -> Option<Failure> {
    let errors = payload.get("errors")?.as_array()?;
    if errors.is_empty() {
        return None;
    }

    let mut needed: Vec<String> = Vec::new();
    let mut first_message: Option<String> = None;

    for error in errors {
        let kind = error.get("type").and_then(Value::as_str).unwrap_or_default();
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();

        if kind == "INSUFFICIENT_SCOPES" {
            for scope in required_scopes(message) {
                if !needed.contains(&scope) {
                    needed.push(scope);
                }
            }
        } else if first_message.is_none() && !message.is_empty() {
            first_message = Some(message.to_owned());
        }
    }

    if !needed.is_empty() {
        return Some(Failure::MissingScopes { needed });
    }
    Some(Failure::Message(first_message.unwrap_or_else(|| {
        "GitHub refused the query without saying why".to_owned()
    })))
}

/// The scopes named in an `INSUFFICIENT_SCOPES` message.
///
/// GitHub states them twice — what is required, then what was granted — as two bracketed lists in
/// a sentence. Only the first is wanted, and reading it out of prose is unpleasant but beats
/// telling the user "insufficient scopes" and leaving them to guess which one.
fn required_scopes(message: &str) -> Vec<String> {
    let Some(open) = message.find('[') else {
        return Vec::new();
    };
    let Some(close) = message[open..].find(']').map(|offset| open + offset) else {
        return Vec::new();
    };

    message[open + 1..close]
        .split(',')
        .map(|scope| scope.trim().trim_matches('\'').trim_matches('"').to_owned())
        .filter(|scope| !scope.is_empty())
        .collect()
}

/// GitHub reports granted scopes as one comma-separated header value.
fn parse_scopes(header: &str) -> Vec<String> {
    header
        .split(',')
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact text GitHub returned when asked for a project without `read:project`.
    const REAL_SCOPE_ERROR: &str = "Your token has not been granted the required scopes to execute \
        this query. The 'title' field requires one of the following scopes: ['read:project'], but \
        your token has only been granted the: ['gist', 'read:org', 'repo'] scopes. Please modify \
        your token's scopes at: https://github.com/settings/tokens.";

    #[test]
    fn the_scope_asked_for_is_read_out_of_the_sentence() {
        assert_eq!(required_scopes(REAL_SCOPE_ERROR), vec!["read:project"]);
    }

    #[test]
    fn the_granted_list_is_not_mistaken_for_the_required_one() {
        // Both lists are in the same sentence; taking the wrong one would tell the user to grant
        // the scopes they already have.
        let scopes = required_scopes(REAL_SCOPE_ERROR);
        assert!(!scopes.contains(&"repo".to_owned()), "{scopes:?}");
    }

    #[test]
    fn a_message_without_a_list_yields_nothing_rather_than_nonsense() {
        assert!(required_scopes("something went wrong").is_empty());
        assert!(required_scopes("an unclosed [ bracket").is_empty());
    }

    #[test]
    fn thirty_seven_complaints_about_one_missing_scope_become_one_sentence() {
        // What GitHub actually sent: one error per field of the query, every one of them saying
        // the same thing. Reporting them as they arrive would fill the screen.
        let errors = (0..37)
            .map(|_| json!({ "type": "INSUFFICIENT_SCOPES", "message": REAL_SCOPE_ERROR }))
            .collect::<Vec<_>>();

        let failure = classify(&json!({ "errors": errors })).expect("this is a failure");

        assert_eq!(
            failure,
            Failure::MissingScopes {
                needed: vec!["read:project".to_owned()]
            }
        );
        assert_eq!(
            failure.to_string(),
            "this token is missing the read:project scope"
        );
    }

    #[test]
    fn a_real_problem_is_not_buried_under_the_scope_complaints() {
        // GitHub orders errors by where the field sits in the query, so the important one can
        // arrive last. Taking `errors[0]` would report the wrong thing.
        let failure = classify(&json!({
            "errors": [
                { "type": "INSUFFICIENT_SCOPES", "message": REAL_SCOPE_ERROR },
                { "type": "NOT_FOUND", "message": "Could not resolve to an Organization." },
            ]
        }));

        // A missing scope is still the actionable one, and it is what gets reported — but the
        // other message must not be what decides the classification either way.
        assert_eq!(
            failure,
            Some(Failure::MissingScopes {
                needed: vec!["read:project".to_owned()]
            })
        );
    }

    #[test]
    fn a_query_that_worked_is_not_a_failure() {
        assert_eq!(classify(&json!({ "data": { "viewer": {} } })), None);
        assert_eq!(classify(&json!({ "data": {}, "errors": [] })), None);
    }

    #[test]
    fn a_failure_with_no_recognised_type_still_says_something() {
        let failure = classify(&json!({
            "errors": [{ "message": "Could not resolve to a Repository with the name 'x/y'." }]
        }));

        assert_eq!(
            failure,
            Some(Failure::Message(
                "Could not resolve to a Repository with the name 'x/y'.".to_owned()
            ))
        );
    }

    #[test]
    fn granted_scopes_are_read_from_the_header_github_actually_sends() {
        // Verbatim from `GET /user`.
        assert_eq!(
            parse_scopes("gist, read:org, repo"),
            vec!["gist", "read:org", "repo"]
        );
        assert!(parse_scopes("").is_empty());
    }

    #[test]
    fn a_token_that_may_read_projects_is_recognised_either_way() {
        // `project` is the write scope and implies the read one; a token with it must not be told
        // to go and grant `read:project`.
        let with_read = Identity {
            login: "a".into(),
            name: None,
            avatar_url: None,
            scopes: vec!["repo".into(), "read:project".into()],
        };
        let with_write = Identity {
            scopes: vec!["project".into()],
            ..with_read.clone()
        };
        let without = Identity {
            scopes: vec!["repo".into()],
            ..with_read.clone()
        };

        assert!(with_read.can_read_projects());
        assert!(with_write.can_read_projects());
        assert!(!without.can_read_projects());
    }

    #[test]
    fn the_name_shown_falls_back_to_the_login() {
        let named = Identity {
            login: "devconnecting1".into(),
            name: Some("Fabrício".into()),
            avatar_url: None,
            scopes: Vec::new(),
        };
        let unnamed = Identity {
            name: None,
            ..named.clone()
        };
        let blank = Identity {
            name: Some(String::new()),
            ..named.clone()
        };

        assert_eq!(named.label(), "Fabrício");
        assert_eq!(unnamed.label(), "devconnecting1");
        assert_eq!(blank.label(), "devconnecting1", "an empty name is no name");
    }
}
