//! GitHub, as things the agent can read for itself.
//!
//! The point of these is that the agent should not need anything pasted into the chat. Asked to
//! look at an issue, it fetches the issue; asked why a pull request is failing, it fetches the
//! pull request and its diff. The repository is not a parameter it has to guess either — it
//! defaults to the one this project was cloned from, because that is almost always the answer and
//! a model that has to ask is a model that gets it wrong sometimes.
//!
//! Everything here reads. Nothing posts a comment, opens a pull request, or closes anything: a
//! tool that writes to a repository other people can see is a different kind of decision, and it
//! belongs behind the permission prompt rather than in the same commit as the reading.

use anyhow::{Context as _, Result, bail};
use gpui::{App, AppContext as _, Task};
use serde_json::{Value, json};

use crate::tool::{Tool, ToolContext, ToolKind, ToolOutput};

/// How much of a diff is worth sending to a model.
///
/// A pull request that touches a lockfile is tens of thousands of lines, nearly all of it noise,
/// and sending it would cost more than the rest of the conversation put together. The head of a
/// diff is the part that carries the change; what is cut is said plainly so the model knows there
/// was more rather than believing it saw everything.
const MAX_DIFF_LINES: usize = 1_200;

/// The repository the tools act on when the model does not name one.
///
/// Read from the project's own remote, which is what makes these tools feel like part of the
/// window rather than a generic API: the repository on screen is the repository they answer about.
fn project_repository(context: &ToolContext, cx: &App) -> Option<String> {
    let repository = context
        .project
        .read(cx)
        .git_store()
        .read(cx)
        .active_repository()?;

    let remote = {
        let repository = repository.read(cx);
        repository
            .remote_origin_url
            .clone()
            .or_else(|| repository.remote_upstream_url.clone())?
    };

    let registry = git::GitHostingProviderRegistry::global(cx);
    let (_, parsed) = git::parse_git_remote_url(registry, &remote)?;
    Some(format!("{}/{}", parsed.owner, parsed.repo))
}

/// Splits `owner/name`, which is how both the model and GitHub itself spell a repository.
fn split_repository(full_name: &str) -> Result<(String, String)> {
    let (owner, name) = full_name
        .trim()
        .trim_end_matches(".git")
        .split_once('/')
        .with_context(|| format!("`{full_name}` is not a repository — write it as owner/name"))?;

    if owner.is_empty() || name.is_empty() || name.contains('/') {
        bail!("`{full_name}` is not a repository — write it as owner/name");
    }
    Ok((owner.to_owned(), name.to_owned()))
}

/// The repository named in the arguments, or the project's own.
fn resolve_repository(input: &Value, context: &ToolContext, cx: &App) -> Result<(String, String)> {
    if let Some(named) = input
        .get("repo")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|repo| !repo.is_empty())
    {
        return split_repository(named);
    }

    let found = project_repository(context, cx).context(
        "this project has no GitHub remote, so there is no repository to assume — pass `repo` as \
         owner/name",
    )?;
    split_repository(&found)
}

fn number_argument(input: &Value) -> Result<i64> {
    input
        .get("number")
        .and_then(Value::as_i64)
        .filter(|number| *number > 0)
        .context("`number` is required and must be the issue or pull request number")
}

/// The shared preamble: a connected client, or a sentence saying how to get one.
async fn connect(
    token: gpui::Task<Result<Option<String>>>,
    http: std::sync::Arc<dyn http_client::HttpClient>,
) -> Result<github::Client> {
    let Some(token) = token.await? else {
        bail!(
            "Wu is not connected to GitHub yet. Open GitHub from the side of the window and \
             connect, then try again."
        );
    };
    Ok(github::Client::new(http, token))
}

const ISSUE_QUERY: &str = r#"
query($owner:String!,$name:String!,$number:Int!){
  repository(owner:$owner,name:$name){
    issue(number:$number){
      number title state url createdAt
      author{login}
      body
      labels(first:20){nodes{name}}
      assignees(first:10){nodes{login}}
      comments(first:50){nodes{author{login} createdAt body}}
    }
  }
}"#;

const PULL_REQUEST_QUERY: &str = r#"
query($owner:String!,$name:String!,$number:Int!){
  repository(owner:$owner,name:$name){
    pullRequest(number:$number){
      number title state isDraft merged url createdAt
      headRefName baseRefName additions deletions changedFiles reviewDecision
      author{login}
      body
      labels(first:20){nodes{name}}
      files(first:100){nodes{path additions deletions changeType}}
      comments(first:30){nodes{author{login} body}}
      reviews(first:20){nodes{author{login} state body}}
      commits(last:1){nodes{commit{oid statusCheckRollup{state
        contexts(first:50){nodes{
          __typename
          ... on CheckRun{name conclusion detailsUrl}
          ... on StatusContext{context state targetUrl}
        }}}}}}
    }
  }
}"#;

// =================================================================================================
// Rendering. These are what the model reads, so they are prose with structure rather than JSON:
// a model given raw API output spends its attention parsing shapes instead of reading the problem.
// =================================================================================================

fn names(node: &Value, field: &str, key: &str) -> Vec<String> {
    node.get(field)
        .and_then(|value| value.get("nodes"))
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|node| node.get(key).and_then(Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn text(node: &Value, field: &str) -> String {
    node.get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn author(node: &Value) -> String {
    node.get("author")
        .and_then(|author| author.get("login"))
        .and_then(Value::as_str)
        .unwrap_or("someone since deleted")
        .to_owned()
}

fn render_issue(repo: &str, issue: &Value) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{repo}#{} — {}\n{} · opened by @{}\n",
        issue.get("number").and_then(Value::as_i64).unwrap_or(0),
        text(issue, "title"),
        text(issue, "state"),
        author(issue),
    ));

    let labels = names(issue, "labels", "name");
    if !labels.is_empty() {
        out.push_str(&format!("Labels: {}\n", labels.join(", ")));
    }
    let assignees = names(issue, "assignees", "login");
    if !assignees.is_empty() {
        out.push_str(&format!("Assigned to: @{}\n", assignees.join(", @")));
    }
    out.push_str(&format!("{}\n\n", text(issue, "url")));

    let body = text(issue, "body");
    out.push_str(if body.trim().is_empty() {
        "(no description)\n"
    } else {
        &body
    });

    let comments = issue
        .get("comments")
        .and_then(|comments| comments.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !comments.is_empty() {
        out.push_str(&format!("\n\n--- {} comments ---\n", comments.len()));
        for comment in &comments {
            out.push_str(&format!(
                "\n@{}:\n{}\n",
                author(comment),
                text(comment, "body").trim()
            ));
        }
    }
    out
}

fn render_pull_request(repo: &str, pr: &Value, diff: Option<&str>) -> String {
    let number = pr.get("number").and_then(Value::as_i64).unwrap_or(0);
    let state = if pr.get("merged").and_then(Value::as_bool) == Some(true) {
        "MERGED".to_owned()
    } else if pr.get("isDraft").and_then(Value::as_bool) == Some(true) {
        format!("{} (draft)", text(pr, "state"))
    } else {
        text(pr, "state")
    };

    let mut out = format!(
        "{repo}#{number} — {}\n{state} · @{} · {} → {}\n+{} −{} across {} files\n",
        text(pr, "title"),
        author(pr),
        text(pr, "headRefName"),
        text(pr, "baseRefName"),
        pr.get("additions").and_then(Value::as_i64).unwrap_or(0),
        pr.get("deletions").and_then(Value::as_i64).unwrap_or(0),
        pr.get("changedFiles").and_then(Value::as_i64).unwrap_or(0),
    );

    let review = text(pr, "reviewDecision");
    if !review.is_empty() {
        out.push_str(&format!("Review: {review}\n"));
    }
    out.push_str(&format!("{}\n", text(pr, "url")));

    if let Some(checks) = render_checks(pr) {
        out.push_str(&checks);
    }

    let body = text(pr, "body");
    out.push_str(&format!(
        "\n{}\n",
        if body.trim().is_empty() {
            "(no description)"
        } else {
            body.trim()
        }
    ));

    let files = pr
        .get("files")
        .and_then(|files| files.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !files.is_empty() {
        out.push_str("\nFiles:\n");
        for file in &files {
            out.push_str(&format!(
                "  {} (+{} −{}) {}\n",
                text(file, "path"),
                file.get("additions").and_then(Value::as_i64).unwrap_or(0),
                file.get("deletions").and_then(Value::as_i64).unwrap_or(0),
                text(file, "changeType").to_lowercase(),
            ));
        }
    }

    if let Some(diff) = diff {
        out.push_str("\n--- diff ---\n");
        out.push_str(&truncate_diff(diff));
    }
    out
}

/// The checks on the head commit, which is the part that says whether this can merge.
fn render_checks(pr: &Value) -> Option<String> {
    let rollup = pr
        .get("commits")?
        .get("nodes")?
        .as_array()?
        .first()?
        .get("commit")?
        .get("statusCheckRollup")?;
    if rollup.is_null() {
        return None;
    }

    let mut out = format!("Checks: {}\n", text(rollup, "state"));
    let contexts = rollup
        .get("contexts")
        .and_then(|contexts| contexts.get("nodes"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Only the failures are listed. A green run has nothing to say, and listing forty passing
    // checks buries the one that did not.
    for context in &contexts {
        let (name, result) = match text(context, "__typename").as_str() {
            "CheckRun" => (text(context, "name"), text(context, "conclusion")),
            _ => (text(context, "context"), text(context, "state")),
        };
        if matches!(
            result.as_str(),
            "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "STARTUP_FAILURE"
        ) {
            out.push_str(&format!("  failing: {name} ({})\n", result.to_lowercase()));
        }
    }
    Some(out)
}

fn truncate_diff(diff: &str) -> String {
    let mut lines = diff.lines();
    let head = lines
        .by_ref()
        .take(MAX_DIFF_LINES)
        .collect::<Vec<_>>()
        .join("\n");
    let remaining = lines.count();

    if remaining == 0 {
        head
    } else {
        format!("{head}\n… {remaining} more lines of diff not shown\n")
    }
}

// =================================================================================================
// The tools themselves.
// =================================================================================================

fn repository_parameter() -> Value {
    json!({
        "type": "string",
        "description": "The repository as owner/name. Leave this out to use the one this project \
                        came from, which is usually what is wanted.",
    })
}

pub struct IssueTool;

impl Tool for IssueTool {
    fn name(&self) -> &'static str {
        "github_issue"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn description(&self) -> &'static str {
        "Read a GitHub issue: its description, labels, who it is assigned to, and the discussion \
         on it. Use this before starting work described by an issue number, rather than asking the \
         user to paste it."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "number": { "type": "integer", "description": "The issue number." },
                "repo": repository_parameter(),
            },
            "required": ["number"],
        })
    }

    fn run(&self, input: Value, context: ToolContext, cx: &mut App) -> Task<Result<ToolOutput>> {
        let resolved = resolve_repository(&input, &context, cx);
        let token = github::stored_token(cx);
        let http = cx.http_client();

        cx.background_spawn(async move {
            let (owner, name) = resolved?;
            let number = number_argument(&input)?;
            let client = connect(token, http).await?;

            let data = client
                .graphql(
                    ISSUE_QUERY,
                    json!({ "owner": owner, "name": name, "number": number }),
                )
                .await?;

            let issue = data
                .get("repository")
                .and_then(|repository| repository.get("issue"))
                .filter(|issue| !issue.is_null())
                .with_context(|| format!("{owner}/{name} has no issue #{number}"))?;

            let full_name = format!("{owner}/{name}");
            Ok(ToolOutput::new(
                render_issue(&full_name, issue),
                format!("Read {full_name}#{number}"),
            ))
        })
    }
}

pub struct PullRequestTool;

impl Tool for PullRequestTool {
    fn name(&self) -> &'static str {
        "github_pull_request"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn description(&self) -> &'static str {
        "Read a GitHub pull request: what it changes, which checks are failing, the review state, \
         and optionally the whole diff. Use this to review a pull request or to find out why one \
         is red."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "number": { "type": "integer", "description": "The pull request number." },
                "repo": repository_parameter(),
                "include_diff": {
                    "type": "boolean",
                    "description": "Fetch the unified diff as well. Leave this out unless the \
                                    actual changes are needed — a diff can be very large.",
                },
            },
            "required": ["number"],
        })
    }

    fn run(&self, input: Value, context: ToolContext, cx: &mut App) -> Task<Result<ToolOutput>> {
        let resolved = resolve_repository(&input, &context, cx);
        let token = github::stored_token(cx);
        let http = cx.http_client();

        cx.background_spawn(async move {
            let (owner, name) = resolved?;
            let number = number_argument(&input)?;
            let want_diff = input
                .get("include_diff")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let client = connect(token, http).await?;

            let data = client
                .graphql(
                    PULL_REQUEST_QUERY,
                    json!({ "owner": owner, "name": name, "number": number }),
                )
                .await?;

            let pull_request = data
                .get("repository")
                .and_then(|repository| repository.get("pullRequest"))
                .filter(|pull_request| !pull_request.is_null())
                .with_context(|| format!("{owner}/{name} has no pull request #{number}"))?;

            // The diff is REST, not GraphQL: GitHub will hand over the real unified diff for a
            // media type, and reconstructing one from the file list would be a worse copy of it.
            let diff = if want_diff {
                Some(client.pull_request_diff(&owner, &name, number).await?)
            } else {
                None
            };

            let full_name = format!("{owner}/{name}");
            Ok(ToolOutput::new(
                render_pull_request(&full_name, pull_request, diff.as_deref()),
                format!("Read {full_name}#{number}"),
            ))
        })
    }
}

/// How many annotations to show per failing check.
///
/// A failing lint run can produce hundreds of identical complaints. The first few name the problem;
/// the rest are the same sentence with different line numbers.
const MAX_ANNOTATIONS: usize = 12;

fn render_check_runs(
    reference: &str,
    runs: &[Value],
    annotations: &[(String, Vec<Value>)],
) -> String {
    let failing = runs
        .iter()
        .filter(|run| is_failure(&text(run, "conclusion")))
        .count();
    let pending = runs
        .iter()
        .filter(|run| text(run, "status") != "completed")
        .count();

    let mut out = format!(
        "{} checks on {reference}: {failing} failing, {pending} still running\n",
        runs.len()
    );

    if runs.is_empty() {
        out.push_str("\nThis ref has no checks — nothing is configured, or none have started.\n");
        return out;
    }

    for run in runs {
        let conclusion = text(run, "conclusion");
        if !is_failure(&conclusion) {
            continue;
        }
        out.push_str(&format!(
            "\n--- {} ({}) ---\n",
            text(run, "name"),
            conclusion.to_lowercase()
        ));

        let output = run.get("output").cloned().unwrap_or(Value::Null);
        let title = text(&output, "title");
        if !title.is_empty() {
            out.push_str(&format!("{title}\n"));
        }
        let summary = text(&output, "summary");
        if !summary.trim().is_empty() {
            out.push_str(&format!("{}\n", summary.trim()));
        }

        if let Some((_, found)) = annotations
            .iter()
            .find(|(name, _)| name == &text(run, "name"))
        {
            for annotation in found.iter().take(MAX_ANNOTATIONS) {
                let line = annotation
                    .get("start_line")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                out.push_str(&format!(
                    "  {}:{} {} — {}\n",
                    text(annotation, "path"),
                    line,
                    text(annotation, "annotation_level"),
                    text(annotation, "message").replace('\n', " "),
                ));
            }
            if found.len() > MAX_ANNOTATIONS {
                out.push_str(&format!(
                    "  … and {} more of the same\n",
                    found.len() - MAX_ANNOTATIONS
                ));
            }
        }
    }

    if failing == 0 {
        out.push_str("\nNothing is failing.\n");
    }
    out
}

fn is_failure(conclusion: &str) -> bool {
    matches!(
        conclusion,
        "failure" | "timed_out" | "action_required" | "startup_failure" | "stale"
    )
}

pub struct ChecksTool;

impl Tool for ChecksTool {
    fn name(&self) -> &'static str {
        "github_checks"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn description(&self) -> &'static str {
        "Find out why a build is red. Reports the checks on a branch, commit or pull request, and \
         for each failing one the file and line its annotations point at. Use this when asked why \
         CI is failing, before guessing from the code."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "ref": {
                    "type": "string",
                    "description": "A branch name, a commit SHA, or `pull/123/head`. Leave this \
                                    out for the repository's default branch.",
                },
                "repo": repository_parameter(),
            },
        })
    }

    fn run(&self, input: Value, context: ToolContext, cx: &mut App) -> Task<Result<ToolOutput>> {
        let resolved = resolve_repository(&input, &context, cx);
        let token = github::stored_token(cx);
        let http = cx.http_client();

        cx.background_spawn(async move {
            let (owner, name) = resolved?;
            let client = connect(token, http).await?;

            // No ref given means the branch the repository itself considers current, which is
            // almost always what "is the build red" is asking about.
            let reference = match input.get("ref").and_then(Value::as_str) {
                Some(reference) if !reference.trim().is_empty() => reference.trim().to_owned(),
                _ => {
                    let repository = client.rest(&format!("repos/{owner}/{name}")).await?;
                    text(&repository, "default_branch")
                }
            };

            let answer = client
                .rest(&format!(
                    "repos/{owner}/{name}/commits/{reference}/check-runs?per_page=100"
                ))
                .await?;
            let runs = answer
                .get("check_runs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();

            // Annotations are a request each, so they are fetched only for what actually failed.
            let mut annotations = Vec::new();
            for run in &runs {
                if !is_failure(&text(run, "conclusion")) {
                    continue;
                }
                let Some(id) = run.get("id").and_then(Value::as_i64) else {
                    continue;
                };
                if let Ok(found) = client
                    .rest(&format!("repos/{owner}/{name}/check-runs/{id}/annotations"))
                    .await
                    && let Some(found) = found.as_array()
                {
                    annotations.push((text(run, "name"), found.clone()));
                }
            }

            let failing = runs
                .iter()
                .filter(|run| is_failure(&text(run, "conclusion")))
                .count();
            Ok(ToolOutput::new(
                render_check_runs(&reference, &runs, &annotations),
                format!("Checked {owner}/{name} at {reference}: {failing} failing"),
            ))
        })
    }
}


// =================================================================================================
// Security alerts. The deep analysis happens on GitHub; this reads the answer.
// =================================================================================================

/// How much of a rule's remediation text is worth carrying.
///
/// CodeQL ships a full page of markdown per rule — overview, two code examples, a reference list.
/// The recommendation is the part that says what to do; the rest is a tutorial the model does not
/// need and the user is not reading in a chat transcript.
const MAX_REMEDIATION: usize = 600;

/// The "## Recommendation" section of a CodeQL help page, or its first paragraph.
fn remediation(help: &str) -> String {
    let body = help
        .split("## Recommendation")
        .nth(1)
        .unwrap_or(help)
        .split("\n## ")
        .next()
        .unwrap_or_default()
        .trim();

    if body.chars().count() <= MAX_REMEDIATION {
        return body.to_owned();
    }
    let cut: String = body.chars().take(MAX_REMEDIATION).collect();
    // Cut at a sentence rather than mid-word, when there is one to cut at.
    match cut.rfind(". ") {
        Some(end) => format!("{}.", &cut[..end]),
        None => format!("{cut}…"),
    }
}

fn render_code_scanning(alerts: &[Value]) -> String {
    if alerts.is_empty() {
        return String::new();
    }
    let mut out = format!("\n--- code scanning ({}) ---\n", alerts.len());

    for alert in alerts {
        let rule = alert.get("rule").cloned().unwrap_or(Value::Null);
        let instance = alert
            .get("most_recent_instance")
            .cloned()
            .unwrap_or(Value::Null);
        let location = instance.get("location").cloned().unwrap_or(Value::Null);

        out.push_str(&format!(
            "\n{} {} ({})\n",
            // `security_severity_level` is the one worth sorting by; `severity` is the rule's own
            // warning/error and says nothing about how much it matters.
            text(&rule, "security_severity_level")
                .is_empty()
                .then(|| text(&rule, "severity"))
                .unwrap_or_else(|| text(&rule, "security_severity_level")),
            text(&rule, "id"),
            text(&alert.get("tool").cloned().unwrap_or(Value::Null), "name"),
        ));

        let path = text(&location, "path");
        if !path.is_empty() {
            out.push_str(&format!(
                "  {path}:{}\n",
                location.get("start_line").and_then(Value::as_i64).unwrap_or(0)
            ));
        }
        let message = text(&instance.get("message").cloned().unwrap_or(Value::Null), "text");
        if !message.is_empty() {
            out.push_str(&format!("  {message}\n"));
        }
        let fix = remediation(&text(&rule, "help"));
        if !fix.is_empty() {
            out.push_str(&format!("  Fix: {}\n", fix.replace('\n', "\n       ")));
        }
    }
    out
}

fn render_dependabot(alerts: &[Value]) -> String {
    if alerts.is_empty() {
        return String::new();
    }
    let mut out = format!("\n--- vulnerable dependencies ({}) ---\n", alerts.len());

    for alert in alerts {
        let advisory = alert
            .get("security_advisory")
            .cloned()
            .unwrap_or(Value::Null);
        let dependency = alert.get("dependency").cloned().unwrap_or(Value::Null);
        let package = dependency.get("package").cloned().unwrap_or(Value::Null);
        let vulnerability = alert
            .get("security_vulnerability")
            .cloned()
            .unwrap_or(Value::Null);

        let scope = text(&dependency, "scope");
        out.push_str(&format!(
            "\n{} {} ({}{})\n",
            text(&advisory, "severity"),
            text(&package, "name"),
            text(&package, "ecosystem"),
            if scope.is_empty() {
                String::new()
            } else {
                format!(", {scope}")
            },
        ));
        out.push_str(&format!("  {}\n", text(&advisory, "summary")));

        let manifest = text(&dependency, "manifest_path");
        if !manifest.is_empty() {
            out.push_str(&format!("  declared in {manifest}\n"));
        }

        // The one line that says how to fix it.
        let patched = text(
            &vulnerability
                .get("first_patched_version")
                .cloned()
                .unwrap_or(Value::Null),
            "identifier",
        );
        let range = text(&vulnerability, "vulnerable_version_range");
        if !patched.is_empty() {
            out.push_str(&format!("  affected {range} — fixed in {patched}\n"));
        } else if !range.is_empty() {
            out.push_str(&format!("  affected {range} — no fixed version published\n"));
        }

        let identifier = [text(&advisory, "ghsa_id"), text(&advisory, "cve_id")]
            .into_iter()
            .filter(|id| !id.is_empty())
            .collect::<Vec<_>>()
            .join(" / ");
        if !identifier.is_empty() {
            out.push_str(&format!("  {identifier}\n"));
        }
    }
    out
}

pub struct AlertsTool;

impl Tool for AlertsTool {
    fn name(&self) -> &'static str {
        "github_alerts"
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Read
    }

    fn description(&self) -> &'static str {
        "Read the security alerts GitHub already found for this repository: CodeQL code scanning \
         results with the file and line they point at, and Dependabot's vulnerable dependencies \
         with the version that fixes each one. Use this when asked to fix security problems, \
         rather than searching the code for them — this analysis has already run."
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "repo": repository_parameter(),
                "kind": {
                    "type": "string",
                    "enum": ["all", "code", "dependencies"],
                    "description": "Which alerts to read. Leave this out for both.",
                },
                "state": {
                    "type": "string",
                    "enum": ["open", "all"],
                    "description": "Leave this out for open alerts only, which is nearly always \
                                    what is wanted.",
                },
            },
        })
    }

    fn run(&self, input: Value, context: ToolContext, cx: &mut App) -> Task<Result<ToolOutput>> {
        let resolved = resolve_repository(&input, &context, cx);
        let token = github::stored_token(cx);
        let http = cx.http_client();

        cx.background_spawn(async move {
            let (owner, name) = resolved?;
            let client = connect(token, http).await?;

            let kind = input.get("kind").and_then(Value::as_str).unwrap_or("all");
            let state = input.get("state").and_then(Value::as_str).unwrap_or("open");
            let want_code = matches!(kind, "all" | "code");
            let want_dependencies = matches!(kind, "all" | "dependencies");

            // Either endpoint 404s when the feature is off for the repository, which is an answer
            // rather than a failure: "nothing is scanning this" is what the user needs to hear.
            let mut disabled = Vec::new();

            let code = if want_code {
                match client
                    .rest(&format!(
                        "repos/{owner}/{name}/code-scanning/alerts?state={state}&per_page=50"
                    ))
                    .await
                {
                    Ok(value) => value.as_array().cloned().unwrap_or_default(),
                    Err(_) => {
                        disabled.push("code scanning");
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };

            let dependencies = if want_dependencies {
                match client
                    .rest(&format!(
                        "repos/{owner}/{name}/dependabot/alerts?state={state}&per_page=50"
                    ))
                    .await
                {
                    Ok(value) => value.as_array().cloned().unwrap_or_default(),
                    Err(_) => {
                        disabled.push("Dependabot");
                        Vec::new()
                    }
                }
            } else {
                Vec::new()
            };

            let total = code.len() + dependencies.len();
            let mut content = format!(
                "{owner}/{name}: {total} {state} alert{}\n",
                if total == 1 { "" } else { "s" }
            );
            content.push_str(&render_code_scanning(&code));
            content.push_str(&render_dependabot(&dependencies));

            if !disabled.is_empty() {
                content.push_str(&format!(
                    "\n{} is not enabled for this repository, or this token may not read it.\n",
                    disabled.join(" and ")
                ));
            }
            if total == 0 && disabled.is_empty() {
                content.push_str("\nNothing is open.\n");
            }

            Ok(ToolOutput::new(
                content,
                format!("{owner}/{name}: {total} alerts"),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_code_scanning_alert_reads_as_a_place_and_a_fix() {
        // The shape GitHub actually returned, trimmed.
        let alert = json!({
            "rule": {
                "id": "rust/access-invalid-pointer",
                "severity": "error",
                "security_severity_level": "high",
                "help": "# Access of invalid pointer\nDereferencing an invalid pointer...\n\n## Recommendation\nWhen dereferencing a pointer in `unsafe` code, take care that the pointer is valid.\n\n## Example\nlots of markdown"
            },
            "tool": { "name": "CodeQL", "version": "2.26.4" },
            "most_recent_instance": {
                "message": { "text": "This operation dereferences a pointer that may be invalid." },
                "location": { "path": "cli/src/tunnels/acl_windows.rs", "start_line": 324 }
            }
        });

        let rendered = render_code_scanning(&[alert]);
        assert!(rendered.contains("high rust/access-invalid-pointer (CodeQL)"), "{rendered}");
        assert!(rendered.contains("cli/src/tunnels/acl_windows.rs:324"), "{rendered}");
        assert!(rendered.contains("dereferences a pointer"), "{rendered}");
        assert!(rendered.contains("Fix: When dereferencing"), "{rendered}");
        assert!(!rendered.contains("lots of markdown"), "the tutorial is not the fix");
    }

    #[test]
    fn the_recommendation_is_taken_rather_than_the_whole_help_page() {
        let help = "# Title\nOverview prose.\n\n## Recommendation\nDo the thing.\n\n## Example\nCode.\n\n## References\nLinks.";
        assert_eq!(remediation(help), "Do the thing.");
    }

    #[test]
    fn a_help_page_with_no_recommendation_still_yields_something() {
        assert_eq!(remediation("Just one paragraph."), "Just one paragraph.");
        assert_eq!(remediation(""), "");
    }

    #[test]
    fn a_long_recommendation_is_cut_at_a_sentence() {
        let long = format!("## Recommendation\n{} End of it.", "Sentence here. ".repeat(80));
        let cut = remediation(&long);

        assert!(cut.chars().count() <= MAX_REMEDIATION + 1, "{}", cut.len());
        assert!(cut.ends_with('.'), "{cut}");
    }

    #[test]
    fn a_dependency_alert_names_the_version_that_fixes_it() {
        // Without the patched version the model has to go and look it up, and usually guesses.
        let alert = json!({
            "security_advisory": {
                "severity": "medium",
                "summary": "SVGO: removeScripts incompletely sanitizes executable HTML",
                "ghsa_id": "GHSA-4vpr-x523-8j87",
                "cve_id": "CVE-2026-84369"
            },
            "dependency": {
                "package": { "ecosystem": "npm", "name": "svgo" },
                "manifest_path": "package-lock.json",
                "scope": "development"
            },
            "security_vulnerability": {
                "vulnerable_version_range": ">= 1.0.0, < 2.8.4",
                "first_patched_version": { "identifier": "2.8.4" }
            }
        });

        let rendered = render_dependabot(&[alert]);
        assert!(rendered.contains("medium svgo (npm, development)"), "{rendered}");
        assert!(rendered.contains("declared in package-lock.json"), "{rendered}");
        assert!(rendered.contains("affected >= 1.0.0, < 2.8.4 — fixed in 2.8.4"), "{rendered}");
        assert!(rendered.contains("GHSA-4vpr-x523-8j87 / CVE-2026-84369"), "{rendered}");
    }

    #[test]
    fn a_vulnerability_with_no_fix_yet_says_so_rather_than_implying_one() {
        let alert = json!({
            "security_advisory": { "severity": "high", "summary": "x" },
            "dependency": { "package": { "ecosystem": "npm", "name": "y" } },
            "security_vulnerability": { "vulnerable_version_range": "< 9.9.9" }
        });

        let rendered = render_dependabot(&[alert]);
        assert!(rendered.contains("no fixed version published"), "{rendered}");
    }

    #[test]
    fn nothing_to_report_renders_as_nothing_rather_than_an_empty_heading() {
        assert!(render_code_scanning(&[]).is_empty());
        assert!(render_dependabot(&[]).is_empty());
    }

    #[test]
    fn a_green_run_is_reported_as_green_without_listing_everything() {
        // 60 passing checks is the real shape on a busy repository; naming them all is noise.
        let runs = (0..60)
            .map(|index| {
                json!({ "name": format!("check-{index}"), "status": "completed",
                        "conclusion": "success" })
            })
            .collect::<Vec<_>>();

        let rendered = render_check_runs("master", &runs, &[]);
        assert!(rendered.contains("60 checks on master: 0 failing"), "{rendered}");
        assert!(rendered.contains("Nothing is failing."), "{rendered}");
        assert!(!rendered.contains("check-7"), "passing checks are not listed");
    }

    #[test]
    fn a_failure_is_reported_with_the_file_and_line_to_fix() {
        // The whole point: an agent should come away knowing where to look.
        let runs = vec![json!({
            "id": 1, "name": "clippy", "status": "completed", "conclusion": "failure",
            "output": { "title": "clippy found 1 error", "summary": "" }
        })];
        let annotations = vec![(
            "clippy".to_owned(),
            vec![json!({
                "path": "crates/cowork/src/tool.rs",
                "start_line": 412,
                "annotation_level": "failure",
                "message": "unused variable: `folders`"
            })],
        )];

        let rendered = render_check_runs("main", &runs, &annotations);
        assert!(rendered.contains("--- clippy (failure) ---"), "{rendered}");
        assert!(
            rendered.contains("crates/cowork/src/tool.rs:412 failure — unused variable"),
            "{rendered}"
        );
    }

    #[test]
    fn hundreds_of_identical_annotations_are_cut_with_the_count_kept() {
        let runs = vec![json!({
            "id": 1, "name": "eslint", "status": "completed", "conclusion": "failure"
        })];
        let many = (0..200)
            .map(|line| {
                json!({ "path": "src/a.ts", "start_line": line,
                        "annotation_level": "failure", "message": "missing semicolon" })
            })
            .collect::<Vec<_>>();

        let rendered = render_check_runs("main", &runs, &[("eslint".to_owned(), many)]);
        assert_eq!(rendered.matches("missing semicolon").count(), MAX_ANNOTATIONS);
        assert!(rendered.contains("188 more of the same"), "{rendered}");
    }

    #[test]
    fn a_check_still_running_is_counted_rather_than_called_a_failure() {
        // `conclusion` is null while a check is in progress; reading that as failure would report
        // a red build every time someone looks during a run.
        let runs = vec![
            json!({ "name": "build", "status": "in_progress", "conclusion": null }),
            json!({ "name": "test", "status": "completed", "conclusion": "success" }),
        ];

        let rendered = render_check_runs("main", &runs, &[]);
        assert!(rendered.contains("0 failing, 1 still running"), "{rendered}");
    }

    #[test]
    fn a_ref_with_no_checks_says_so_rather_than_implying_success() {
        let rendered = render_check_runs("main", &[], &[]);
        assert!(rendered.contains("no checks"), "{rendered}");
    }

    #[test]
    fn every_way_github_spells_a_failure_is_treated_as_one() {
        for conclusion in ["failure", "timed_out", "action_required", "startup_failure", "stale"] {
            assert!(is_failure(conclusion), "{conclusion}");
        }
        for fine in ["success", "neutral", "skipped", "cancelled", ""] {
            assert!(!is_failure(fine), "{fine}");
        }
    }

    #[test]
    fn a_repository_is_owner_then_name() {
        assert_eq!(
            split_repository("Workspaacing/wu").unwrap(),
            ("Workspaacing".to_owned(), "wu".to_owned())
        );
        // How it arrives from a remote URL.
        assert_eq!(
            split_repository("Workspaacing/wu.git").unwrap(),
            ("Workspaacing".to_owned(), "wu".to_owned())
        );
        assert_eq!(
            split_repository("  Workspaacing/wu  ").unwrap(),
            ("Workspaacing".to_owned(), "wu".to_owned())
        );
    }

    #[test]
    fn anything_that_is_not_owner_slash_name_is_refused_with_the_shape_wanted() {
        for wrong in [
            "wu",
            "",
            "/wu",
            "Workspaacing/",
            "https://github.com/Workspaacing/wu",
        ] {
            let error = split_repository(wrong)
                .unwrap_err()
                .to_string();
            assert!(error.contains("owner/name"), "{wrong}: {error}");
        }
    }

    #[test]
    fn a_diff_is_cut_at_a_length_a_model_can_afford() {
        let long = (0..2_000)
            .map(|line| format!("+line {line}"))
            .collect::<Vec<_>>()
            .join("\n");

        let shown = truncate_diff(&long);
        assert_eq!(shown.lines().filter(|l| l.starts_with('+')).count(), 1_200);
        assert!(shown.contains("800 more lines"), "the cut has to be stated");
    }

    #[test]
    fn a_diff_that_fits_is_left_exactly_as_it_was() {
        let diff = "--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b";
        assert_eq!(truncate_diff(diff), diff);
    }

    #[test]
    fn an_issue_reads_as_prose_with_its_discussion() {
        let issue = json!({
            "number": 42,
            "title": "Sessions vanish after a restart",
            "state": "OPEN",
            "url": "https://github.com/Workspaacing/wu/issues/42",
            "author": { "login": "devconnecting1" },
            "body": "Created a session, closed the app, it was gone.",
            "labels": { "nodes": [{ "name": "bug" }] },
            "assignees": { "nodes": [{ "login": "someone" }] },
            "comments": { "nodes": [
                { "author": { "login": "other" }, "body": "Reproduced." }
            ]},
        });

        let rendered = render_issue("Workspaacing/wu", &issue);

        assert!(rendered.contains("Workspaacing/wu#42 — Sessions vanish after a restart"));
        assert!(rendered.contains("OPEN · opened by @devconnecting1"));
        assert!(rendered.contains("Labels: bug"));
        assert!(rendered.contains("Assigned to: @someone"));
        assert!(rendered.contains("1 comments"));
        assert!(rendered.contains("@other:\nReproduced."));
    }

    #[test]
    fn a_deleted_account_does_not_render_as_null() {
        // GitHub returns `author: null` for an account that no longer exists, which is common on
        // old issues. `@null` in the transcript would be a bug the model then reasons about.
        let issue = json!({ "number": 1, "title": "x", "state": "CLOSED", "author": null });
        assert!(render_issue("a/b", &issue).contains("@someone since deleted"));
    }

    #[test]
    fn an_issue_with_no_body_says_so_rather_than_showing_a_gap() {
        let issue = json!({ "number": 1, "title": "x", "state": "OPEN", "body": "   " });
        assert!(render_issue("a/b", &issue).contains("(no description)"));
    }

    #[test]
    fn a_merged_pull_request_is_not_reported_as_closed() {
        // GitHub's `state` for a merged pull request is `MERGED`, but a closed-without-merge one is
        // also not `OPEN`; saying "merged" only when it is merged is the distinction that matters.
        let merged = json!({ "number": 1, "title": "x", "state": "MERGED", "merged": true });
        let closed = json!({ "number": 2, "title": "y", "state": "CLOSED", "merged": false });

        assert!(render_pull_request("a/b", &merged, None).contains("MERGED"));
        let closed = render_pull_request("a/b", &closed, None);
        assert!(closed.contains("CLOSED"), "{closed}");
        assert!(!closed.contains("MERGED"), "{closed}");
    }

    #[test]
    fn a_draft_says_it_is_a_draft() {
        let draft = json!({ "number": 1, "title": "x", "state": "OPEN", "isDraft": true });
        assert!(render_pull_request("a/b", &draft, None).contains("(draft)"));
    }

    #[test]
    fn only_the_failing_checks_are_listed() {
        // A run with forty green checks and one red one should read as one line about the red one.
        let mut contexts = (0..40)
            .map(|index| {
                json!({ "__typename": "CheckRun", "name": format!("ok-{index}"), "conclusion": "SUCCESS" })
            })
            .collect::<Vec<_>>();
        contexts.push(json!({
            "__typename": "CheckRun", "name": "clippy", "conclusion": "FAILURE"
        }));

        let pr = json!({
            "number": 1, "title": "x", "state": "OPEN",
            "commits": { "nodes": [{ "commit": {
                "oid": "abc", "statusCheckRollup": { "state": "FAILURE",
                    "contexts": { "nodes": contexts } }
            }}]},
        });

        let rendered = render_pull_request("a/b", &pr, None);
        assert!(rendered.contains("failing: clippy (failure)"), "{rendered}");
        assert!(!rendered.contains("ok-7"), "passing checks are noise");
    }

    #[test]
    fn a_pull_request_with_no_checks_configured_does_not_claim_anything_about_them() {
        // `statusCheckRollup` is null on a repository without CI — this is the shape the real
        // query returned for Workspaacing/wu#1.
        let pr = json!({
            "number": 1, "title": "x", "state": "OPEN",
            "commits": { "nodes": [{ "commit": { "oid": "abc", "statusCheckRollup": null }}]},
        });

        assert!(!render_pull_request("a/b", &pr, None).contains("Checks:"));
    }
}
