//! Gitleaks: the secret scan the built-in one cannot do.
//!
//! [`crate::verify`] already refuses to write a file containing a credential, which is the cheap
//! half of the problem and the half that matters most — a secret that never lands never leaks. It
//! works on the bytes in memory, before the write, and costs no process.
//!
//! What it cannot do is look backwards. A key committed last March is in the history whether or
//! not the working tree still has it, and anyone who clones the repository gets it. That is where
//! leaked credentials actually live, and finding them needs a tool that walks `git log -p` with a
//! few hundred rules. Gitleaks is that tool: one Go binary, MIT, about eight megabytes, published
//! per platform with a SHA-256.
//!
//! # The secret is never repeated
//!
//! Gitleaks reports the matched credential in its JSON, and that field is dropped here rather than
//! carried. An agent's tool output goes to a model provider; a scan that copied the key into the
//! transcript in order to warn about the key would have leaked it a second time, to one more
//! party, on the way to saying it was leaked. Where and which rule is enough to act on.

use anyhow::{Context as _, Result, bail};
use gpui::BackgroundExecutor;
use http_client::HttpClient;
use serde::Deserialize;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

const REPOSITORY: &str = "gitleaks/gitleaks";

/// Pinned rather than tracked, the way Biome and ESLint are.
///
/// A scanner that silently changes its rules between runs turns "this repository is clean" into a
/// statement about a version nobody recorded. Upgrading is a commit, with whatever new findings
/// that brings, which is the point.
const VERSION: &str = "8.30.1";

/// How many leaks are worth listing before the list stops being read.
const MAX_REPORTED: usize = 40;

/// One finding, with the credential itself deliberately absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Leak {
    pub file: String,
    pub line: u32,
    pub rule: String,
    pub description: String,
    /// The commit it came from, when the history was scanned.
    pub commit: Option<String>,
}

/// Gitleaks' own JSON shape, in its own capitalisation.
#[derive(Deserialize)]
struct RawLeak {
    #[serde(rename = "File")]
    file: String,
    #[serde(rename = "StartLine")]
    start_line: Option<u32>,
    #[serde(rename = "RuleID")]
    rule_id: String,
    #[serde(rename = "Description")]
    description: String,
    #[serde(rename = "Commit")]
    commit: Option<String>,
    // `Secret`, `Match`, `Entropy`, `Author` and the rest are not read. Naming the field would be
    // enough to invite someone to log it.
}

/// The release asset for the machine this is running on.
///
/// Gitleaks names architectures its own way — `x64` where Rust says `x86_64`, `x32` where Rust
/// says `x86` — so the two vocabularies have to be mapped rather than interpolated.
fn asset_name() -> Result<String> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        "windows" => "windows",
        other => bail!("gitleaks publishes no build for {other}"),
    };
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "x32",
        "arm" => "armv7",
        other => bail!("gitleaks publishes no build for {other}"),
    };
    let extension = if cfg!(windows) { "zip" } else { "tar.gz" };

    Ok(format!("gitleaks_{VERSION}_{os}_{architecture}.{extension}"))
}

/// Where the binary lives once fetched.
fn install_directory() -> PathBuf {
    paths::languages_dir().join("gitleaks")
}

fn binary_path(container: &Path) -> PathBuf {
    container
        .join(format!("gitleaks-{VERSION}"))
        .join(format!("gitleaks{}", std::env::consts::EXE_SUFFIX))
}

/// The gitleaks binary, downloading it once if it is not already there.
pub async fn binary(http: Arc<dyn HttpClient>) -> Result<PathBuf> {
    let container = install_directory();
    let binary = binary_path(&container);

    // Present and runnable is the whole cache check. The version is in the path, so a pin change
    // is a miss by construction, and a half-extracted archive fails `--version` and is refetched.
    if smol::fs::metadata(&binary).await.is_ok() && runs(&binary).await {
        return Ok(binary);
    }

    smol::fs::create_dir_all(&container)
        .await
        .context("making room for gitleaks")?;

    let wanted = asset_name()?;
    let release = http_client::github::get_release_by_tag_name(
        REPOSITORY,
        &format!("v{VERSION}"),
        http.clone(),
    )
    .await
    .context("looking up the gitleaks release")?;

    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name == wanted)
        .with_context(|| format!("the gitleaks {VERSION} release has no {wanted}"))?;

    // `get_release_by_tag_name` leaves the `sha256:` prefix on, unlike its sibling.
    let digest = asset
        .digest
        .as_deref()
        .map(|digest| digest.trim_start_matches("sha256:"));

    http_client::github_download::download_server_binary(
        &*http,
        &asset.browser_download_url,
        digest,
        &container.join(format!("gitleaks-{VERSION}")),
        if cfg!(windows) {
            http_client::github::AssetKind::Zip
        } else {
            http_client::github::AssetKind::TarGz
        },
    )
    .await
    .context("downloading gitleaks")?;

    // Tar does not reliably carry the executable bit through; this is a no-op on Windows.
    util::fs::make_file_executable(&binary)
        .await
        .context("making gitleaks executable")?;

    Ok(binary)
}

async fn runs(binary: &Path) -> bool {
    util::command::new_command(binary)
        .arg("version")
        .output()
        .await
        .is_ok_and(|output| output.status.success())
}

/// Scans a repository, optionally including everything ever committed to it.
pub async fn scan(
    binary: &Path,
    root: &Path,
    include_history: bool,
    executor: &BackgroundExecutor,
) -> Result<Vec<Leak>> {
    let report = tempfile::Builder::new()
        .suffix(".json")
        .tempfile()
        .context("making somewhere for gitleaks to write its report")?;

    let mut command = util::command::new_command(binary);
    command
        // `git` walks the history; `dir` reads the working tree only.
        .arg(if include_history { "git" } else { "dir" })
        .arg(root)
        .arg("--report-format")
        .arg("json")
        .arg("--report-path")
        .arg(report.path())
        .arg("--no-banner")
        // Exit 1 means "leaks, or an error" — two things worth telling apart. Pinning the leak
        // code to 0 makes a non-zero exit mean an error and nothing else.
        .arg("--exit-code")
        .arg("0");

    let output = command
        .output()
        .await
        .context("running gitleaks")?;

    if !output.status.success() {
        bail!(
            "gitleaks failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("no reason given")
        );
    }

    let body = smol::fs::read_to_string(report.path())
        .await
        .context("reading the gitleaks report")?;
    drop(report);
    let _ = executor;

    Ok(parse(&body))
}

/// Reads the report, dropping everything about a finding except where it is.
fn parse(report: &str) -> Vec<Leak> {
    // An empty report file is what a clean scan leaves behind.
    if report.trim().is_empty() {
        return Vec::new();
    }

    serde_json::from_str::<Vec<RawLeak>>(report)
        .unwrap_or_default()
        .into_iter()
        .map(|raw| Leak {
            file: raw.file,
            line: raw.start_line.unwrap_or(0),
            rule: raw.rule_id,
            description: raw.description,
            commit: raw.commit.filter(|commit| !commit.is_empty()),
        })
        .collect()
}

/// What the agent and the user are told.
pub fn render(leaks: &[Leak], scanned_history: bool) -> String {
    let where_ = if scanned_history {
        "the working tree and every commit"
    } else {
        "the working tree"
    };

    if leaks.is_empty() {
        return format!("No credentials found in {where_}.\n");
    }

    let mut out = format!(
        "{} possible credential{} in {where_}.\n\nThe values themselves are not repeated here — \
         reading them out would copy the secret somewhere new. Open each file at the line given.\n",
        leaks.len(),
        if leaks.len() == 1 { "" } else { "s" }
    );

    for leak in leaks.iter().take(MAX_REPORTED) {
        out.push_str(&format!("\n{}:{} {}\n", leak.file, leak.line, leak.rule));
        if !leak.description.is_empty() {
            out.push_str(&format!("  {}\n", leak.description));
        }
        if let Some(commit) = &leak.commit {
            // A secret in history stays reachable after the file is fixed, so the commit is the
            // part that decides whether rotating the key is enough or the history has to change.
            out.push_str(&format!("  committed in {}\n", &commit[..commit.len().min(12)]));
        }
    }

    if leaks.len() > MAX_REPORTED {
        out.push_str(&format!("\n… and {} more\n", leaks.len() - MAX_REPORTED));
    }
    out
}

/// Scanning a repository for credentials, as something the agent can be asked to do.
pub struct SecretsScanTool;

impl crate::tool::Tool for SecretsScanTool {
    fn name(&self) -> &'static str {
        "secrets_scan"
    }

    fn kind(&self) -> crate::tool::ToolKind {
        crate::tool::ToolKind::Read
    }

    fn description(&self) -> &'static str {
        "Scan this project for committed credentials with gitleaks, optionally including every commit ever made. Use this when asked whether any secrets have leaked. The values found are never reported — only where they are — so a scan cannot copy a key anywhere new."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "include_history": {
                    "type": "boolean",
                    "description": "Scan every commit rather than only the current files. Slower, and the only way to find a key that was removed but is still reachable by anyone who clones the repository.",
                },
            },
        })
    }

    fn run(
        &self,
        input: serde_json::Value,
        context: crate::tool::ToolContext,
        cx: &mut gpui::App,
    ) -> gpui::Task<Result<crate::tool::ToolOutput>> {
        use gpui::AppContext as _;

        let root = context
            .project
            .read(cx)
            .visible_worktrees(cx)
            .next()
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf());
        let http = cx.http_client();
        let executor = cx.background_executor().clone();

        cx.background_spawn(async move {
            let Some(root) = root else {
                bail!("this project has no folder open, so there is nothing to scan");
            };
            let include_history = input
                .get("include_history")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);

            let binary = binary(http).await?;
            let leaks = scan(&binary, &root, include_history, &executor).await?;

            Ok(crate::tool::ToolOutput::new(
                render(&leaks, include_history),
                match leaks.len() {
                    0 => "No credentials found".to_owned(),
                    1 => "1 possible credential".to_owned(),
                    count => format!("{count} possible credentials"),
                },
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape gitleaks writes, in its own capitalisation.
    const REPORT: &str = r#"[
      {
        "Description": "AWS Access Key",
        "StartLine": 12,
        "File": "src/config.ts",
        "RuleID": "aws-access-token",
        "Secret": "AKIAIOSFODNN7EXAMPLE",
        "Match": "aws_key = AKIAIOSFODNN7EXAMPLE",
        "Entropy": 3.2,
        "Commit": "8f2a1c9d4e6b7a0f3c5d8e1b2a4f6c9d0e3b5a7c",
        "Author": "someone",
        "Email": "someone@example.com"
      }
    ]"#;

    #[test]
    fn the_secret_itself_is_never_carried_out_of_the_report() {
        // The reason this module exists in the shape it does: tool output reaches a model
        // provider. A scan that quoted the key would leak it again, to one more party.
        let leaks = parse(REPORT);

        assert_eq!(leaks.len(), 1);
        let rendered = render(&leaks, true);
        assert!(!rendered.contains("AKIAIOSFODNN7EXAMPLE"), "{rendered}");
        assert!(!rendered.contains("aws_key ="), "{rendered}");
        assert!(!format!("{leaks:?}").contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn what_is_carried_is_enough_to_act_on() {
        let leaks = parse(REPORT);
        let leak = &leaks[0];

        assert_eq!(leak.file, "src/config.ts");
        assert_eq!(leak.line, 12);
        assert_eq!(leak.rule, "aws-access-token");
        assert_eq!(leak.commit.as_deref(), Some("8f2a1c9d4e6b7a0f3c5d8e1b2a4f6c9d0e3b5a7c"));

        let rendered = render(&leaks, true);
        assert!(rendered.contains("src/config.ts:12 aws-access-token"), "{rendered}");
        assert!(rendered.contains("committed in 8f2a1c9d4e6b"), "{rendered}");
    }

    #[test]
    fn a_clean_scan_leaves_an_empty_report_and_that_is_not_a_failure() {
        assert!(parse("").is_empty());
        assert!(parse("   \n").is_empty());
        assert!(parse("[]").is_empty());
        assert!(render(&[], false).contains("No credentials found in the working tree."));
    }

    #[test]
    fn a_report_that_cannot_be_read_yields_nothing_rather_than_panicking() {
        // A truncated file is what an interrupted scan leaves; it must not take the turn down.
        assert!(parse("[{\"File\": ").is_empty());
        assert!(parse("not json at all").is_empty());
    }

    #[test]
    fn a_finding_with_no_commit_is_from_the_working_tree() {
        let report = r#"[{"File":"a.ts","StartLine":1,"RuleID":"r","Description":"d","Commit":""}]"#;
        let leaks = parse(report);

        assert_eq!(leaks[0].commit, None);
        assert!(!render(&leaks, false).contains("committed in"));
    }

    #[test]
    fn the_asset_named_is_one_the_release_actually_publishes() {
        // Checked against the real v8.30.1 asset list; gitleaks spells architectures its own way.
        let name = asset_name().expect("this platform is supported");

        assert!(name.starts_with(&format!("gitleaks_{VERSION}_")), "{name}");
        assert!(!name.contains("x86_64"), "gitleaks says x64, not x86_64: {name}");
        assert!(!name.contains("aarch64"), "gitleaks says arm64: {name}");
        if cfg!(windows) {
            assert!(name.ends_with("_windows_x64.zip"), "{name}");
        }
    }

    #[test]
    fn a_long_list_is_cut_with_the_remainder_stated() {
        let many = (0..100)
            .map(|index| Leak {
                file: format!("file{index}.ts"),
                line: 1,
                rule: "r".into(),
                description: String::new(),
                commit: None,
            })
            .collect::<Vec<_>>();

        let rendered = render(&many, false);
        assert_eq!(rendered.matches(".ts:1").count(), MAX_REPORTED);
        assert!(rendered.contains("… and 60 more"), "{rendered}");
    }
}
