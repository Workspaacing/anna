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

#[cfg(test)]
mod tests {
    use super::*;

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
