//! Known-vulnerability checking for dependency manifests the agent edits.
//!
//! This is the part of Dependabot that is worth having inside an editor. Dependabot itself cannot be
//! embedded — it is a service GitHub hosts, and its worker is Ruby running one container per
//! ecosystem — but the advisory data it draws on is public. [OSV](https://osv.dev) aggregates the
//! same sources: RustSec for crates.io, the GitHub Advisory Database for npm, Maven, NuGet,
//! Packagist, RubyGems and Go, and PyPA for PyPI.
//!
//! So when an agent adds or bumps a dependency, Anna asks OSV about it and hands any advisory straight
//! back to the agent, which can pin above the fixed version on its next step. No model is consulted,
//! so the check costs nothing per turn.
//!
//! The only thing that leaves the machine is a list of package names and versions from the manifest
//! that was just edited. The check is off with `cowork.verification.dependency_audit: false`.

use crate::verify::{Finding, Severity};
use anyhow::{Context as _, Result};
use futures::AsyncReadExt as _;
use http_client::{AsyncBody, HttpClient, Request};
use semver::{Version, VersionReq};
use serde_json::{Value, json};
use std::sync::Arc;

const OSV_BATCH_URL: &str = "https://api.osv.dev/v1/querybatch";
const OSV_QUERY_URL: &str = "https://api.osv.dev/v1/query";

/// How many packages one manifest may contribute.
///
/// A lockfile can name thousands; a report that long helps nobody and the request would be refused.
const MAX_PACKAGES: usize = 250;

/// How many packages we fetch full advisory details for.
const MAX_DETAILED: usize = 10;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Package {
    /// An OSV ecosystem name, spelled exactly as OSV spells it.
    pub ecosystem: &'static str,
    pub name: String,
    /// A concrete version from a lockfile, or the requirement written in a manifest.
    pub version: String,
    /// Whether `version` is the version that will actually be used, rather than a requirement that
    /// a range of versions satisfies.
    pub exact: bool,
}

/// Recognises a manifest by file name and reads the packages out of it.
///
/// Returns nothing for a file that is not a manifest, which is the common case and the reason this
/// is cheap to call after every edit.
pub fn parse_manifest(file_name: &str, contents: &str) -> Vec<Package> {
    let mut packages = match file_name {
        "Cargo.toml" => cargo_manifest(contents),
        "Cargo.lock" => cargo_lock(contents),
        "package.json" => package_json(contents),
        "package-lock.json" => package_lock_json(contents),
        "requirements.txt" => requirements_txt(contents),
        "pyproject.toml" => pyproject_toml(contents),
        "go.mod" => go_mod(contents),
        _ => Vec::new(),
    };

    packages.sort_by(|left, right| left.name.cmp(&right.name));
    packages.dedup();
    packages.truncate(MAX_PACKAGES);
    packages
}

fn cargo_manifest(contents: &str) -> Vec<Package> {
    let Ok(document) = contents.parse::<toml::Table>() else {
        return Vec::new();
    };

    let mut packages = Vec::new();
    let mut collect = |table: Option<&Value>| {
        let Some(table) = table.and_then(Value::as_object) else {
            return;
        };
        for (name, spec) in table {
            // A path or git dependency has no published version to ask about.
            if spec.get("path").is_some() || spec.get("git").is_some() {
                continue;
            }
            let version = spec
                .as_str()
                .or_else(|| spec.get("version").and_then(Value::as_str));
            if let Some(version) = version {
                packages.push(Package {
                    ecosystem: "crates.io",
                    name: name.clone(),
                    version: version.to_owned(),
                    exact: false,
                });
            }
        }
    };

    // `toml::Table` and `serde_json::Value` describe the same shapes here, and going through JSON
    // means one traversal helper rather than two.
    let Ok(document) = serde_json::to_value(&document) else {
        return Vec::new();
    };
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        collect(document.get(section));
    }
    if let Some(targets) = document.get("target").and_then(Value::as_object) {
        for target in targets.values() {
            for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
                collect(target.get(section));
            }
        }
    }
    // A workspace root declares the versions every member inherits.
    if let Some(workspace) = document.get("workspace") {
        collect(workspace.get("dependencies"));
    }

    packages
}

fn cargo_lock(contents: &str) -> Vec<Package> {
    let Ok(document) = contents.parse::<toml::Table>() else {
        return Vec::new();
    };
    let Some(entries) = document.get("package").and_then(toml::Value::as_array) else {
        return Vec::new();
    };

    entries
        .iter()
        .filter_map(|entry| {
            // A member of the workspace has no `source`; it is not a published package.
            entry.get("source")?;
            Some(Package {
                ecosystem: "crates.io",
                name: entry.get("name")?.as_str()?.to_owned(),
                version: entry.get("version")?.as_str()?.to_owned(),
                exact: true,
            })
        })
        .collect()
}

fn package_json(contents: &str) -> Vec<Package> {
    let Ok(document) = serde_json::from_str::<Value>(contents) else {
        return Vec::new();
    };

    let mut packages = Vec::new();
    for section in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        let Some(table) = document.get(section).and_then(Value::as_object) else {
            continue;
        };
        for (name, requirement) in table {
            let Some(requirement) = requirement.as_str() else {
                continue;
            };
            // `file:`, `link:`, `workspace:` and git specifiers name no registry version.
            if requirement.contains(':') || requirement.contains('/') {
                continue;
            }
            packages.push(Package {
                ecosystem: "npm",
                name: name.clone(),
                version: requirement.to_owned(),
                exact: false,
            });
        }
    }
    packages
}

fn package_lock_json(contents: &str) -> Vec<Package> {
    let Ok(document) = serde_json::from_str::<Value>(contents) else {
        return Vec::new();
    };
    let Some(entries) = document.get("packages").and_then(Value::as_object) else {
        return Vec::new();
    };

    entries
        .iter()
        .filter_map(|(path, entry)| {
            // Keys are install paths; the package name is what follows the last `node_modules/`.
            // The root project is keyed by the empty string and has no name here.
            let name = path.rsplit_once("node_modules/")?.1;
            Some(Package {
                ecosystem: "npm",
                name: name.to_owned(),
                version: entry.get("version")?.as_str()?.to_owned(),
                exact: true,
            })
        })
        .collect()
}

/// Splits a PEP 508 requirement such as `requests>=2.0,<3` into its name and its specifier.
fn python_requirement(line: &str) -> Option<Package> {
    let line = line.split('#').next()?.trim();
    if line.is_empty() || line.starts_with('-') {
        return None;
    }
    // Environment markers (`; python_version < "3.9"`) and extras are not versions.
    let line = line.split(';').next()?.trim();
    let split = line.find(['=', '>', '<', '~', '!'])?;
    let (name, specifier) = line.split_at(split);
    let name = name.split('[').next()?.trim();
    if name.is_empty() {
        return None;
    }

    let version = specifier.trim_start_matches(['=', '>', '<', '~', '!']).trim();
    Some(Package {
        ecosystem: "PyPI",
        name: name.to_owned(),
        version: version.split(',').next().unwrap_or(version).trim().to_owned(),
        // `==` pins one version; everything else is a range.
        exact: specifier.trim_start().starts_with("=="),
    })
}

fn requirements_txt(contents: &str) -> Vec<Package> {
    contents.lines().filter_map(python_requirement).collect()
}

fn pyproject_toml(contents: &str) -> Vec<Package> {
    let Ok(document) = contents.parse::<toml::Table>() else {
        return Vec::new();
    };

    let mut packages = Vec::new();
    if let Some(dependencies) = document
        .get("project")
        .and_then(toml::Value::as_table)
        .and_then(|project| project.get("dependencies"))
        .and_then(toml::Value::as_array)
    {
        packages.extend(
            dependencies
                .iter()
                .filter_map(toml::Value::as_str)
                .filter_map(python_requirement),
        );
    }
    packages
}

fn go_mod(contents: &str) -> Vec<Package> {
    let mut packages = Vec::new();
    let mut in_block = false;

    for line in contents.lines() {
        let line = line.split("//").next().unwrap_or(line).trim();
        if line == "require (" {
            in_block = true;
            continue;
        }
        if in_block && line == ")" {
            in_block = false;
            continue;
        }

        let entry = if in_block {
            line
        } else if let Some(rest) = line.strip_prefix("require ") {
            rest.trim()
        } else {
            continue;
        };

        let Some((name, version)) = entry.split_once(char::is_whitespace) else {
            continue;
        };
        let version = version.trim();
        if !version.starts_with('v') {
            continue;
        }
        packages.push(Package {
            ecosystem: "Go",
            name: name.trim().to_owned(),
            version: version.to_owned(),
            exact: true,
        });
    }
    packages
}

/// One published advisory against one package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Advisory {
    pub id: String,
    pub package: String,
    pub summary: String,
    /// The lowest version known to carry the fix, when the record names one.
    pub fixed: Option<String>,
}

impl Advisory {
    fn message(&self) -> String {
        match &self.fixed {
            Some(fixed) => format!(
                "{} is affected by {}: {} Fixed in {fixed}.",
                self.package, self.id, self.summary
            ),
            None => format!(
                "{} is affected by {}: {} No fixed version is published yet.",
                self.package, self.id, self.summary
            ),
        }
    }
}

/// Asks OSV about a manifest's packages.
///
/// Two round trips at most: one batch query that says which packages have anything at all against
/// them, then one detail query per affected package. Packages with no advisory — nearly all of
/// them — cost a single shared request.
pub async fn advisories(http: Arc<dyn HttpClient>, packages: &[Package]) -> Result<Vec<Advisory>> {
    if packages.is_empty() {
        return Ok(Vec::new());
    }

    let queries = packages.iter().map(osv_query).collect::<Vec<_>>();
    let batch = post_json(&http, OSV_BATCH_URL, json!({ "queries": queries })).await?;

    let Some(results) = batch.get("results").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };

    // The response is index-aligned with the queries, and an unaffected package comes back as an
    // empty object rather than an empty list.
    let affected = results
        .iter()
        .enumerate()
        .filter(|(_, result)| {
            result
                .get("vulns")
                .and_then(Value::as_array)
                .is_some_and(|vulns| !vulns.is_empty())
        })
        .filter_map(|(index, _)| packages.get(index))
        .take(MAX_DETAILED)
        .collect::<Vec<_>>();

    let mut advisories = Vec::new();
    for package in affected {
        let detail = post_json(&http, OSV_QUERY_URL, osv_query(package)).await?;
        let Some(vulns) = detail.get("vulns").and_then(Value::as_array) else {
            continue;
        };
        for vuln in vulns {
            if let Some(advisory) = read_advisory(vuln, package) {
                advisories.push(advisory);
            }
        }
    }

    advisories.sort_by(|left, right| left.id.cmp(&right.id));
    advisories.dedup();
    Ok(advisories)
}

fn osv_query(package: &Package) -> Value {
    let mut query = json!({
        "package": { "name": package.name, "ecosystem": package.ecosystem },
    });
    // A requirement is not a version, and sending one makes OSV answer about a version that does
    // not exist. Asking without one returns every advisory for the package, which `read_advisory`
    // then narrows using the requirement.
    if package.exact {
        query["version"] = json!(package.version);
    }
    query
}

fn read_advisory(vuln: &Value, package: &Package) -> Option<Advisory> {
    let id = vuln.get("id")?.as_str()?.to_owned();
    let summary = vuln
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("no summary published")
        .trim_end_matches('.')
        .to_owned();
    let fixed = fixed_version(vuln, &package.name);

    // OSV filtered by version already for an exact package; for a requirement, drop anything the
    // requirement cannot reach. `>= 1.2` against a fix in 1.1 is not a finding.
    if !package.exact
        && let (Some(fixed), Some(lowest)) = (fixed.as_deref(), lowest_allowed(&package.version))
        && Version::parse(fixed).is_ok_and(|fixed| lowest >= fixed)
    {
        return None;
    }

    Some(Advisory {
        id,
        package: package.name.clone(),
        summary: format!("{summary}."),
        fixed,
    })
}

/// The highest `fixed` event OSV lists for this package, which is the version to pin at or above.
fn fixed_version(vuln: &Value, name: &str) -> Option<String> {
    let affected = vuln.get("affected")?.as_array()?;
    affected
        .iter()
        .filter(|entry| {
            entry
                .pointer("/package/name")
                .and_then(Value::as_str)
                .is_none_or(|affected| affected == name)
        })
        .filter_map(|entry| entry.get("ranges")?.as_array())
        .flatten()
        .filter_map(|range| range.get("events")?.as_array())
        .flatten()
        .filter_map(|event| event.get("fixed")?.as_str())
        .max_by(|left, right| match (Version::parse(left), Version::parse(right)) {
            (Ok(left), Ok(right)) => left.cmp(&right),
            _ => left.cmp(right),
        })
        .map(str::to_owned)
}

/// The lowest version a requirement admits.
///
/// `^1.2.3`, `1.2.3`, `>=1.2.3` and `~1.2.3` all bottom out at 1.2.3; that is the version to compare
/// against a published fix.
fn lowest_allowed(requirement: &str) -> Option<Version> {
    let requirement = VersionReq::parse(requirement).ok()?;
    let comparator = requirement.comparators.first()?;
    Some(Version::new(
        comparator.major,
        comparator.minor.unwrap_or(0),
        comparator.patch.unwrap_or(0),
    ))
}

async fn post_json(http: &Arc<dyn HttpClient>, url: &str, body: Value) -> Result<Value> {
    let body = serde_json::to_vec(&body).context("serializing the advisory query")?;
    let request = Request::post(url)
        .header("content-type", "application/json")
        .body(AsyncBody::from(body))
        .with_context(|| format!("building the request to {url}"))?;

    let mut response = http
        .send(request)
        .await
        .with_context(|| format!("asking {url} about this manifest's dependencies"))?;

    let mut bytes = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut bytes)
        .await
        .context("reading the advisory response")?;

    anyhow::ensure!(
        response.status().is_success(),
        "{url} returned {}",
        response.status()
    );
    serde_json::from_slice(&bytes).context("parsing the advisory response")
}

/// Turns advisories into findings against the manifest.
///
/// A vulnerable dependency is a warning rather than an error: unlike a leaked key it does not have
/// to be fixed before the file can be written, and sometimes there is no fixed version to move to.
pub fn findings(advisories: &[Advisory], contents: &str) -> Vec<Finding> {
    advisories
        .iter()
        .map(|advisory| Finding {
            check: "dependencies".into(),
            severity: Severity::Warning,
            line: line_of(contents, &advisory.package).unwrap_or(1),
            message: advisory.message(),
        })
        .collect()
}

/// Finds the line a package is named on, so the finding points somewhere useful.
fn line_of(contents: &str, name: &str) -> Option<usize> {
    contents
        .lines()
        .position(|line| line.contains(name))
        .map(|index| index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(packages: &[Package]) -> Vec<&str> {
        packages.iter().map(|package| package.name.as_str()).collect()
    }

    #[test]
    fn a_file_that_is_not_a_manifest_is_skipped_entirely() {
        assert!(parse_manifest("main.rs", "fn main() {}").is_empty());
        assert!(parse_manifest("README.md", "# hello").is_empty());
    }

    #[test]
    fn malformed_manifests_produce_nothing_rather_than_failing() {
        // The agent is mid-edit; a half-written manifest must not break the turn.
        assert!(parse_manifest("Cargo.toml", "[dependencies").is_empty());
        assert!(parse_manifest("package.json", "{\"dependencies\":").is_empty());
    }

    #[test]
    fn reads_cargo_dependencies_in_both_spellings() {
        let packages = parse_manifest(
            "Cargo.toml",
            concat!(
                "[dependencies]\n",
                "serde = \"1.0\"\n",
                "tokio = { version = \"1.35\", features = [\"full\"] }\n",
                "[dev-dependencies]\n",
                "insta = \"1.0\"\n",
            ),
        );

        assert_eq!(names(&packages), ["insta", "serde", "tokio"]);
        assert!(packages.iter().all(|package| !package.exact));
        assert!(packages.iter().all(|p| p.ecosystem == "crates.io"));
    }

    #[test]
    fn local_and_git_cargo_dependencies_have_no_published_version_to_ask_about() {
        let packages = parse_manifest(
            "Cargo.toml",
            concat!(
                "[dependencies]\n",
                "gpui = { path = \"../gpui\" }\n",
                "tree-sitter = { git = \"https://github.com/x/y\", version = \"0.20\" }\n",
                "serde = \"1.0\"\n",
            ),
        );

        assert_eq!(names(&packages), ["serde"]);
    }

    #[test]
    fn a_cargo_lockfile_gives_exact_versions_and_omits_workspace_members() {
        let packages = parse_manifest(
            "Cargo.lock",
            concat!(
                "[[package]]\nname = \"cowork\"\nversion = \"0.1.0\"\n\n",
                "[[package]]\nname = \"serde\"\nversion = \"1.0.210\"\n",
                "source = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
            ),
        );

        assert_eq!(names(&packages), ["serde"], "a member is not a published package");
        assert_eq!(packages[0].version, "1.0.210");
        assert!(packages[0].exact);
    }

    #[test]
    fn reads_npm_dependencies_and_skips_non_registry_specifiers() {
        let packages = parse_manifest(
            "package.json",
            concat!(
                "{\"dependencies\":{\"react\":\"^18.2.0\",\"local\":\"file:../local\"},",
                "\"devDependencies\":{\"vitest\":\"1.0.0\"}}",
            ),
        );

        assert_eq!(names(&packages), ["react", "vitest"]);
        assert!(packages.iter().all(|package| package.ecosystem == "npm"));
    }

    #[test]
    fn an_npm_lockfile_names_packages_by_their_install_path() {
        let packages = parse_manifest(
            "package-lock.json",
            concat!(
                "{\"packages\":{",
                "\"\":{\"name\":\"root\",\"version\":\"1.0.0\"},",
                "\"node_modules/react\":{\"version\":\"18.2.0\"},",
                "\"node_modules/a/node_modules/b\":{\"version\":\"2.0.0\"}",
                "}}",
            ),
        );

        assert_eq!(names(&packages), ["b", "react"], "the root entry is not a dependency");
        assert!(packages.iter().all(|package| package.exact));
    }

    #[test]
    fn reads_python_requirements_with_their_pins() {
        let packages = parse_manifest(
            "requirements.txt",
            concat!(
                "# a comment\n",
                "requests==2.31.0\n",
                "urllib3>=1.26,<2\n",
                "django[argon2]~=4.2\n",
                "-r other.txt\n",
                "\n",
            ),
        );

        assert_eq!(names(&packages), ["django", "requests", "urllib3"]);
        let requests = packages.iter().find(|p| p.name == "requests").unwrap();
        assert_eq!(requests.version, "2.31.0");
        assert!(requests.exact, "`==` pins one version");
        let urllib3 = packages.iter().find(|p| p.name == "urllib3").unwrap();
        assert!(!urllib3.exact, "a range is not a pin");
    }

    #[test]
    fn reads_go_requirements_in_block_and_single_line_form() {
        let packages = parse_manifest(
            "go.mod",
            concat!(
                "module example.com/m\n\n",
                "go 1.21\n\n",
                "require github.com/pkg/errors v0.9.1\n\n",
                "require (\n",
                "\tgolang.org/x/net v0.17.0 // indirect\n",
                ")\n",
            ),
        );

        assert_eq!(names(&packages), ["github.com/pkg/errors", "golang.org/x/net"]);
        assert!(packages.iter().all(|package| package.exact));
    }

    #[test]
    fn a_lockfiles_worth_of_packages_is_capped() {
        let mut lock = String::new();
        for index in 0..(MAX_PACKAGES + 50) {
            lock.push_str(&format!(
                "[[package]]\nname = \"crate{index:04}\"\nversion = \"1.0.0\"\nsource = \"registry\"\n\n"
            ));
        }

        assert_eq!(parse_manifest("Cargo.lock", &lock).len(), MAX_PACKAGES);
    }

    #[test]
    fn an_exact_version_is_sent_to_osv_but_a_requirement_is_not() {
        let pinned = osv_query(&Package {
            ecosystem: "npm",
            name: "react".into(),
            version: "18.2.0".into(),
            exact: true,
        });
        assert_eq!(pinned["version"], "18.2.0");

        let ranged = osv_query(&Package {
            ecosystem: "crates.io",
            name: "serde".into(),
            version: "^1.0".into(),
            exact: false,
        });
        assert!(
            ranged.get("version").is_none(),
            "a requirement is not a version OSV can match"
        );
        assert_eq!(ranged["package"]["ecosystem"], "crates.io");
    }

    fn vuln(fixed: &str) -> Value {
        json!({
            "id": "RUSTSEC-2020-0001",
            "summary": "Something is wrong",
            "affected": [{
                "package": { "name": "thing" },
                "ranges": [{ "events": [{ "introduced": "0.0.0" }, { "fixed": fixed }] }],
            }],
        })
    }

    #[test]
    fn an_advisory_reports_the_version_to_move_to() {
        let package = Package {
            ecosystem: "crates.io",
            name: "thing".into(),
            version: "0.1.0".into(),
            exact: true,
        };

        let advisory = read_advisory(&vuln("1.2.0"), &package).expect("should be reported");

        assert_eq!(advisory.fixed.as_deref(), Some("1.2.0"));
        assert!(advisory.message().contains("Fixed in 1.2.0"), "{}", advisory.message());
    }

    #[test]
    fn a_requirement_that_already_excludes_the_vulnerable_range_is_not_a_finding() {
        let safe = Package {
            ecosystem: "crates.io",
            name: "thing".into(),
            version: "^1.3".into(),
            exact: false,
        };
        assert_eq!(
            read_advisory(&vuln("1.2.0"), &safe),
            None,
            "asking for 1.3 or above cannot reach a bug fixed in 1.2.0"
        );

        let vulnerable = Package {
            ecosystem: "crates.io",
            name: "thing".into(),
            version: "^1.0".into(),
            exact: false,
        };
        assert!(
            read_advisory(&vuln("1.2.0"), &vulnerable).is_some(),
            "asking for 1.0 or above still admits 1.0.x"
        );
    }

    #[test]
    fn the_highest_published_fix_is_the_one_to_pin_above() {
        // A record may carry several ranges; pinning below the highest fix leaves the bug in.
        let vuln = json!({
            "id": "GHSA-x",
            "affected": [{
                "package": { "name": "thing" },
                "ranges": [
                    { "events": [{ "introduced": "0" }, { "fixed": "1.9.0" }] },
                    { "events": [{ "introduced": "2.0.0" }, { "fixed": "2.3.1" }] },
                ],
            }],
        });

        assert_eq!(fixed_version(&vuln, "thing").as_deref(), Some("2.3.1"));
    }

    #[test]
    fn an_advisory_with_no_published_fix_says_so() {
        let package = Package {
            ecosystem: "npm",
            name: "thing".into(),
            version: "1.0.0".into(),
            exact: true,
        };
        let unfixed = json!({ "id": "GHSA-y", "summary": "Bad", "affected": [] });

        let advisory = read_advisory(&unfixed, &package).expect("should still be reported");

        assert_eq!(advisory.fixed, None);
        assert!(advisory.message().contains("No fixed version"), "{}", advisory.message());
    }

    #[test]
    fn a_finding_points_at_the_line_the_package_is_declared_on() {
        let manifest = "[dependencies]\nserde = \"1.0\"\nthing = \"0.1\"\n";
        let advisories = [Advisory {
            id: "RUSTSEC-2020-0001".into(),
            package: "thing".into(),
            summary: "Bad.".into(),
            fixed: Some("0.2.0".into()),
        }];

        let findings = findings(&advisories, manifest);

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 3);
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    #[test]
    fn lowest_allowed_reads_the_floor_of_a_requirement() {
        assert_eq!(lowest_allowed("^1.2.3"), Some(Version::new(1, 2, 3)));
        assert_eq!(lowest_allowed("1.2"), Some(Version::new(1, 2, 0)));
        assert_eq!(lowest_allowed(">=2.0.0"), Some(Version::new(2, 0, 0)));
        assert_eq!(lowest_allowed("not a version"), None);
    }
}
