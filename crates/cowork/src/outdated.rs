//! Which dependencies have moved on, and how far.
//!
//! "Outdated" answers a different question from [`crate::audit`], which asks whether a dependency
//! is *vulnerable*. A package can be four years old and perfectly safe, or current and full of
//! holes, and the two lists rarely overlap.
//!
//! The distinction that makes this worth reporting at all is how far behind each one is. A patch
//! release is something you take without reading; a major is a piece of work with a changelog to
//! read and, usually, code to change. Reporting both as "outdated" flattens a decision into a
//! number, and a list of ninety flattened numbers is a list nobody acts on. They are separated
//! here, and the majors are listed last because they are the ones that need a person.

use anyhow::Result;
use futures::AsyncReadExt as _;
use http_client::{AsyncBody, HttpClient, HttpRequestExt as _, Request};
use serde_json::Value;
use std::{sync::Arc, time::Duration};

use crate::audit::Package;

/// How many packages to ask about. Beyond this the answer stops being a list and starts being a
/// report nobody reads, and it is a request per package.
const MAX_PACKAGES: usize = 150;

/// How many registry requests are in flight at once.
///
/// Registries are shared infrastructure and none of this is urgent. Enough to finish a large
/// manifest in a few seconds, not enough to look like a scrape.
const CONCURRENCY: usize = 12;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// How far behind a dependency is, which is the part that decides what to do about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Distance {
    /// Same version. Not reported.
    Current,
    /// A fix, taken without reading.
    Patch,
    /// New behaviour, compatible.
    Minor,
    /// A breaking change, and a piece of work.
    Major,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outdated {
    pub ecosystem: &'static str,
    pub name: String,
    pub installed: String,
    pub latest: String,
    pub distance: Distance,
}

/// How far `installed` is from `latest`.
///
/// Both are compared as semantic versions where they parse, which is nearly always. When one of
/// them does not — a git revision, a date-stamped build, a Go pseudo-version — the only honest
/// answer is that they differ, and that is reported as a minor rather than guessed either way.
pub fn distance(installed: &str, latest: &str) -> Distance {
    fn strip(version: &str) -> &str {
        version.trim().trim_start_matches(['v', '=', '^', '~', ' '])
    }
    let (installed, latest) = (strip(installed), strip(latest));

    if installed == latest {
        return Distance::Current;
    }

    let parse = |version: &str| semver::Version::parse(version).ok();
    let (Some(installed), Some(latest)) = (parse(installed), parse(latest)) else {
        return Distance::Minor;
    };

    if latest <= installed {
        // A lockfile ahead of the registry happens with pre-releases and private mirrors.
        return Distance::Current;
    }
    if latest.major != installed.major {
        Distance::Major
    } else if latest.minor != installed.minor {
        Distance::Minor
    } else {
        Distance::Patch
    }
}

/// Where a registry keeps the current version, and how to ask.
fn query(package: &Package) -> Option<(String, &'static [&'static str])> {
    match package.ecosystem {
        "crates.io" => Some((
            format!("https://crates.io/api/v1/crates/{}", package.name),
            &["crate", "max_stable_version"],
        )),
        "npm" => Some((
            format!("https://registry.npmjs.org/{}/latest", package.name),
            &["version"],
        )),
        "PyPI" => Some((
            format!("https://pypi.org/pypi/{}/json", package.name),
            &["info", "version"],
        )),
        "Go" => Some((
            format!("https://proxy.golang.org/{}/@latest", package.name),
            &["Version"],
        )),
        _ => None,
    }
}

fn dig<'a>(mut value: &'a Value, path: &[&str]) -> Option<&'a str> {
    for key in path {
        value = value.get(key)?;
    }
    value.as_str()
}

async fn latest(http: &Arc<dyn HttpClient>, package: &Package) -> Option<String> {
    let (url, path) = query(package)?;

    let request = Request::get(&url)
        .header("accept", "application/json")
        // crates.io refuses a request without one, and it is only polite everywhere else.
        .header("user-agent", "Wu (https://github.com/Workspaacing/wu)")
        .timeout(REQUEST_TIMEOUT)
        .body(AsyncBody::empty())
        .ok()?;

    let mut response = http.send(request).await.ok()?;
    if !response.status().is_success() {
        // A private package, a typo, a registry that is down: none is worth failing the whole
        // report over, and a package with no answer is simply not listed.
        return None;
    }

    let mut body = Vec::new();
    response.body_mut().read_to_end(&mut body).await.ok()?;
    let value: Value = serde_json::from_slice(&body).ok()?;
    dig(&value, path).map(str::to_owned)
}

/// Asks every registry what the current version is, and keeps what has moved.
pub async fn check(http: Arc<dyn HttpClient>, packages: Vec<Package>) -> Result<Vec<Outdated>> {
    use futures::stream::StreamExt as _;

    let packages = packages
        .into_iter()
        .filter(|package| query(package).is_some())
        .take(MAX_PACKAGES)
        .collect::<Vec<_>>();

    let mut found = futures::stream::iter(packages)
        .map(|package| {
            let http = http.clone();
            async move {
                let latest = latest(&http, &package).await?;
                let distance = distance(&package.version, &latest);
                (distance != Distance::Current).then(|| Outdated {
                    ecosystem: package.ecosystem,
                    name: package.name,
                    installed: package.version,
                    latest,
                    distance,
                })
            }
        })
        .buffer_unordered(CONCURRENCY)
        .filter_map(|outcome| async move { outcome })
        .collect::<Vec<_>>()
        .await;

    // Patches first: they are the ones that can be taken in one go, and putting the work last
    // means the actionable part is not buried under it.
    found.sort_by(|a, b| a.distance.cmp(&b.distance).then_with(|| a.name.cmp(&b.name)));
    Ok(found)
}

pub fn render(outdated: &[Outdated], checked: usize) -> String {
    if outdated.is_empty() {
        return format!("All {checked} dependencies are current.\n");
    }

    let count = |distance: Distance| {
        outdated
            .iter()
            .filter(|entry| entry.distance == distance)
            .count()
    };
    let mut out = format!(
        "{} of {checked} dependencies are behind: {} patch, {} minor, {} major.\n",
        outdated.len(),
        count(Distance::Patch),
        count(Distance::Minor),
        count(Distance::Major),
    );

    let mut heading = None;
    for entry in outdated {
        if heading != Some(entry.distance) {
            heading = Some(entry.distance);
            out.push_str(match entry.distance {
                Distance::Patch => "\n--- patch: fixes, safe to take together ---\n",
                Distance::Minor => "\n--- minor: new behaviour, compatible ---\n",
                Distance::Major => "\n--- major: breaking, one at a time ---\n",
                Distance::Current => "",
            });
        }
        out.push_str(&format!(
            "  {} {} → {} ({})\n",
            entry.name, entry.installed, entry.latest, entry.ecosystem
        ));
    }
    out
}

/// The manifests worth reading, lockfiles first.
///
/// A lockfile says what is actually installed; a manifest says what was asked for. "Is this
/// outdated" is a question about the former, so when both exist the lockfile wins and the manifest
/// only fills in what it did not mention.
const MANIFESTS: [&str; 7] = [
    "Cargo.lock",
    "Cargo.toml",
    "package-lock.json",
    "package.json",
    "go.mod",
    "requirements.txt",
    "pyproject.toml",
];

pub struct DependenciesOutdatedTool;

impl crate::tool::Tool for DependenciesOutdatedTool {
    fn name(&self) -> &'static str {
        "dependencies_outdated"
    }

    fn kind(&self) -> crate::tool::ToolKind {
        crate::tool::ToolKind::Read
    }

    fn description(&self) -> &'static str {
        "List this project's dependencies that have newer releases, separated by how far behind          each one is: patch, minor, or major. This is about age, not safety — use it for upgrade          work. Vulnerabilities are a different question and are reported when a manifest changes."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }

    fn run(
        &self,
        _input: serde_json::Value,
        context: crate::tool::ToolContext,
        cx: &mut gpui::App,
    ) -> gpui::Task<Result<crate::tool::ToolOutput>> {
        use gpui::AppContext as _;

        let project = context.project.read(cx);
        let fs = project.fs().clone();
        let root = project
            .visible_worktrees(cx)
            .next()
            .map(|worktree| worktree.read(cx).abs_path().to_path_buf());
        let http = cx.http_client();

        cx.background_spawn(async move {
            let Some(root) = root else {
                anyhow::bail!("this project has no folder open, so there are no manifests to read");
            };

            // Keyed by name so a package named in both a lockfile and a manifest is asked about
            // once, with the lockfile's exact version.
            let mut packages: Vec<Package> = Vec::new();
            for file_name in MANIFESTS {
                let Ok(contents) = fs.load(&root.join(file_name)).await else {
                    continue;
                };
                for package in crate::audit::parse_manifest(file_name, &contents) {
                    if !packages
                        .iter()
                        .any(|seen| seen.name == package.name && seen.ecosystem == package.ecosystem)
                    {
                        packages.push(package);
                    }
                }
            }

            if packages.is_empty() {
                return Ok(crate::tool::ToolOutput::new(
                    "No dependency manifest was found at the root of this project.
",
                    "No manifests",
                ));
            }

            let checked = packages.len();
            let outdated = check(http, packages).await?;
            let summary = match outdated.len() {
                0 => "Everything is current".to_owned(),
                count => format!("{count} of {checked} behind"),
            };

            Ok(crate::tool::ToolOutput::new(
                render(&outdated, checked),
                summary,
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(ecosystem: &'static str, name: &str, version: &str) -> Package {
        Package {
            ecosystem,
            name: name.to_owned(),
            version: version.to_owned(),
            exact: true,
        }
    }

    #[test]
    fn how_far_behind_is_the_question_not_whether() {
        // The distinction the whole module exists for: one of these you take on a Friday, the
        // other you plan.
        assert_eq!(distance("1.0.1", "1.0.9"), Distance::Patch);
        assert_eq!(distance("1.0.1", "1.4.0"), Distance::Minor);
        assert_eq!(distance("1.0.1", "3.0.0"), Distance::Major);
        assert_eq!(distance("1.0.1", "1.0.1"), Distance::Current);
    }

    #[test]
    fn the_prefixes_every_ecosystem_writes_are_stripped() {
        // Go says `v1.8.1`, npm manifests say `^7.0.2`, Cargo says `=1.0.0`.
        assert_eq!(distance("v1.8.1", "v1.8.1"), Distance::Current);
        assert_eq!(distance("^7.0.1", "7.0.2"), Distance::Patch);
        assert_eq!(distance("=1.0.0", "2.0.0"), Distance::Major);
    }

    #[test]
    fn a_version_ahead_of_the_registry_is_not_reported_as_behind() {
        // A pre-release or a private mirror puts the lockfile in front; saying "update to an
        // older version" would be worse than saying nothing.
        assert_eq!(distance("2.0.0", "1.9.9"), Distance::Current);
    }

    #[test]
    fn a_go_pseudo_version_is_a_real_version_and_is_compared_as_one() {
        // It looks like noise but it parses: `0.0.0` with everything after the dash as a
        // pre-release. So `v0.0.0-…` against `v1.0.0` really is a major move, and saying anything
        // softer would be wrong.
        assert_eq!(
            distance("v0.0.0-20230101120000-abcdef123456", "v1.0.0"),
            Distance::Major
        );
    }

    #[test]
    fn something_that_is_not_a_version_at_all_is_not_guessed_at() {
        // `1.0` has two components and is not semver, though npm accepts it; a branch name is not
        // a version in any sense. Calling either a major would invent urgency, and calling it
        // current would hide a real difference — so it is reported as a difference and no more.
        assert_eq!(distance("1.0", "1.2"), Distance::Minor);
        assert_eq!(distance("main", "1.0.0"), Distance::Minor);

        // Identical is identical, parseable or not.
        assert_eq!(distance("abc123", "abc123"), Distance::Current);
        assert_eq!(distance("main", "main"), Distance::Current);
    }

    #[test]
    fn each_ecosystem_is_asked_where_it_actually_keeps_the_answer() {
        // Verified against the live registries: these four paths are what they return.
        let cases = [
            ("crates.io", "serde", "https://crates.io/api/v1/crates/serde", vec!["crate", "max_stable_version"]),
            ("npm", "typescript", "https://registry.npmjs.org/typescript/latest", vec!["version"]),
            ("PyPI", "requests", "https://pypi.org/pypi/requests/json", vec!["info", "version"]),
            ("Go", "github.com/gorilla/mux", "https://proxy.golang.org/github.com/gorilla/mux/@latest", vec!["Version"]),
        ];

        for (ecosystem, name, expected_url, expected_path) in cases {
            let (url, path) = query(&package(ecosystem, name, "1.0.0")).expect(ecosystem);
            assert_eq!(url, expected_url);
            assert_eq!(path.to_vec(), expected_path);
        }
    }

    #[test]
    fn an_ecosystem_with_no_registry_to_ask_is_skipped_rather_than_guessed() {
        assert!(query(&package("Maven", "com.example:thing", "1.0")).is_none());
    }

    #[test]
    fn the_answer_is_read_from_the_shape_each_registry_returns() {
        let crates_io = serde_json::json!({ "crate": { "max_stable_version": "1.0.229" } });
        assert_eq!(dig(&crates_io, &["crate", "max_stable_version"]), Some("1.0.229"));

        let npm = serde_json::json!({ "version": "7.0.2" });
        assert_eq!(dig(&npm, &["version"]), Some("7.0.2"));

        // A shape that changed underneath us yields nothing rather than nonsense.
        assert_eq!(dig(&npm, &["crate", "max_stable_version"]), None);
    }

    #[test]
    fn the_report_leads_with_what_can_be_taken_today() {
        let outdated = vec![
            Outdated {
                ecosystem: "npm",
                name: "big".into(),
                installed: "1.0.0".into(),
                latest: "3.0.0".into(),
                distance: Distance::Major,
            },
            Outdated {
                ecosystem: "npm",
                name: "small".into(),
                installed: "1.0.0".into(),
                latest: "1.0.4".into(),
                distance: Distance::Patch,
            },
        ];
        let mut sorted = outdated;
        sorted.sort_by(|a, b| a.distance.cmp(&b.distance).then_with(|| a.name.cmp(&b.name)));

        let rendered = render(&sorted, 40);
        let patch_at = rendered.find("small").expect("the patch is listed");
        let major_at = rendered.find("big").expect("the major is listed");

        assert!(patch_at < major_at, "work last, not first:\n{rendered}");
        assert!(rendered.contains("2 of 40 dependencies are behind: 1 patch, 0 minor, 1 major"));
    }

    #[test]
    fn nothing_behind_says_so_with_the_number_checked() {
        // "All current" means nothing without saying how many were looked at.
        assert_eq!(render(&[], 37), "All 37 dependencies are current.\n");
    }
}
