//! Checks that run over what the agent changed, natively.
//!
//! Every check here is compiled into Wu. None of them asks a model anything, so running them costs
//! nothing per turn; only the findings are sent onward, and that is the entire point — an agent
//! that is told it leaked a key or broke a lint rule fixes it on the next step.
//!
//! Deliberately not here: CodeQL, which is a proprietary binary whose license does not permit
//! embedding and which needs a compiled database per run, and Dependabot, which is a Ruby service
//! GitHub hosts. Their jobs — static analysis and dependency advisories — are covered by the
//! language servers Wu already runs and by the RustSec database respectively.

use crate::{audit, cowork_settings::VerificationSettings};
use collections::HashSet;
use futures::channel::oneshot;
use gpui::{AsyncApp, Entity, FutureExt as _};
use http_client::HttpClient;
use language::{Buffer, BufferEvent, Point};
use regex::Regex;
use std::{sync::Arc, sync::LazyLock, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub check: &'static str,
    pub severity: Severity,
    /// 1-based, to match how every compiler and editor reports a line.
    pub line: usize,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct VerificationReport {
    pub findings: Vec<Finding>,
}

impl VerificationReport {
    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }

    /// Rendered for the model, worst first, so a truncated report still leads with what matters.
    pub fn to_model(&self, path: &str) -> String {
        if self.findings.is_empty() {
            return String::new();
        }

        let mut findings = self.findings.clone();
        findings.sort_by(|left, right| {
            right
                .severity
                .cmp(&left.severity)
                .then(left.line.cmp(&right.line))
        });

        let mut out = format!("Checks on {path} reported {} issue", findings.len());
        if findings.len() != 1 {
            out.push('s');
        }
        out.push_str(":\n");
        for finding in findings.iter().take(20) {
            let label = match finding.severity {
                Severity::Error => "error",
                Severity::Warning => "warning",
            };
            out.push_str(&format!(
                "  {}:{} {label} [{}] {}\n",
                path, finding.line, finding.check, finding.message
            ));
        }
        if findings.len() > 20 {
            out.push_str(&format!("  … and {} more\n", findings.len() - 20));
        }
        out
    }
}

/// How long to wait for the language servers to react to an edit before reporting what they have.
///
/// Diagnostics arrive asynchronously, so reading them the instant a write lands would report the
/// state before the change. Waiting forever is worse: a project with no language server for the file
/// would hang the turn.
const DIAGNOSTICS_TIMEOUT: Duration = Duration::from_millis(2500);

/// Checks that must pass before the agent's text is allowed to reach the project.
///
/// Only the secret scan runs here. The rest describe the result of a change and cannot be answered
/// until it has landed; a leaked credential is the one thing that is cheaper to stop than to undo.
pub fn gate(settings: &VerificationSettings, contents: &str) -> Vec<Finding> {
    if settings.secret_scan {
        scan_secrets(contents)
    } else {
        Vec::new()
    }
}

/// Checks that read the state of the project after the agent changed it.
///
/// Every enabled check runs; none of them can fail the call, because a broken check must not cost
/// the user a turn. What it cannot determine, it does not report.
pub async fn inspect(
    settings: VerificationSettings,
    http: Arc<dyn HttpClient>,
    buffer: Entity<Buffer>,
    file_name: String,
    contents: String,
    cx: &mut AsyncApp,
) -> VerificationReport {
    let mut findings = Vec::new();

    if settings.diagnostics {
        findings.extend(diagnostics(&buffer, cx).await);
    }

    if settings.dependency_audit {
        let packages = audit::parse_manifest(&file_name, &contents);
        if !packages.is_empty() {
            match audit::advisories(http, &packages).await {
                Ok(advisories) => findings.extend(audit::findings(&advisories, &contents)),
                // An advisory service that is unreachable is not the user's problem to solve
                // mid-turn, and saying so to the model would only waste its next step.
                Err(error) => log::warn!("cowork: could not check dependencies: {error:#}"),
            }
        }
    }

    VerificationReport { findings }
}

/// Waits for the language servers to catch up with the edit, then reports what they found.
async fn diagnostics(buffer: &Entity<Buffer>, cx: &mut AsyncApp) -> Vec<Finding> {
    // Nothing will ever analyse a file whose language Wu does not know, so there is nothing to
    // wait for. Without this every write to a `.txt` or a `.env` would stall for the full timeout.
    if buffer.read_with(cx, |buffer, _| buffer.language().is_none()) {
        return Vec::new();
    }

    let (sender, receiver) = oneshot::channel();
    let mut sender = Some(sender);
    let subscription = cx.subscribe(buffer, move |_, event: &BufferEvent, _| {
        if matches!(event, BufferEvent::DiagnosticsUpdated)
            && let Some(sender) = sender.take()
        {
            let _ = sender.send(());
        }
    });

    let executor = cx.background_executor().clone();
    // Either outcome is fine: the servers answered, or there are none and what is already on the
    // buffer is the whole truth.
    let _ = receiver.with_timeout(DIAGNOSTICS_TIMEOUT, &executor).await;
    drop(subscription);

    buffer.read_with(cx, |buffer, _| {
        let snapshot = buffer.snapshot();
        let mut findings = Vec::new();
        for entry in snapshot.diagnostics_in_range::<usize, Point>(0..snapshot.len(), false) {
            // Hints and information are editor affordances, not problems with the change.
            let severity = match entry.diagnostic.severity {
                lsp::DiagnosticSeverity::ERROR => Severity::Error,
                lsp::DiagnosticSeverity::WARNING => Severity::Warning,
                _ => continue,
            };
            let source = entry.diagnostic.source.as_deref().unwrap_or("lsp");
            findings.push(Finding {
                check: source_check(source),
                severity,
                line: entry.range.start.row as usize + 1,
                message: entry.diagnostic.message.as_str().to_owned(),
            });
        }
        findings
    })
}

/// Names the check after the tool that produced the diagnostic, so the report says `clippy` or
/// `biome` rather than a uniform `lsp` the user cannot act on.
fn source_check(source: &str) -> &'static str {
    match source {
        source if source.contains("clippy") => "clippy",
        source if source.contains("biome") => "biome",
        source if source.contains("eslint") => "eslint",
        source if source.contains("rustc") => "rustc",
        _ => "diagnostics",
    }
}

/// A credential pattern with a name the user will recognise.
struct SecretPattern {
    name: &'static str,
    /// Patterns that can match the same text share a family, and only the first one to match a
    /// line reports. `sk-ant-…` satisfies the OpenAI pattern too; it is still an Anthropic key.
    family: &'static str,
    regex: &'static str,
}

/// Patterns chosen for precision rather than coverage.
///
/// Every one of these has a fixed prefix or a structural marker, so a hit is almost certainly a real
/// credential. Generic "long random string" detection is handled separately, behind an entropy gate
/// and an assignment to a secret-sounding name, because on its own it fires constantly on hashes,
/// base64 assets and minified code.
static SECRET_PATTERNS: &[SecretPattern] = &[
    SecretPattern {
        name: "AWS access key id",
        family: "aws",
        regex: r"\b(?:AKIA|ASIA|ABIA|ACCA)[0-9A-Z]{16}\b",
    },
    SecretPattern {
        name: "GitHub token",
        family: "github",
        regex: r"\bgh[pousr]_[A-Za-z0-9]{36,255}\b",
    },
    // Before the OpenAI pattern, which `sk-ant-…` also satisfies.
    SecretPattern {
        name: "Anthropic API key",
        family: "sk-prefixed key",
        regex: r"\bsk-ant-[A-Za-z0-9_-]{24,}\b",
    },
    SecretPattern {
        name: "OpenAI API key",
        family: "sk-prefixed key",
        regex: r"\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}\b",
    },
    SecretPattern {
        name: "Google API key",
        family: "google",
        regex: r"\bAIza[0-9A-Za-z_-]{35}\b",
    },
    SecretPattern {
        name: "Slack token",
        family: "slack",
        regex: r"\bxox[baprs]-[0-9A-Za-z-]{10,}\b",
    },
    SecretPattern {
        name: "Stripe secret key",
        family: "stripe",
        regex: r"\b(?:sk|rk)_live_[0-9a-zA-Z]{24,}\b",
    },
    SecretPattern {
        name: "private key block",
        family: "private key",
        regex: r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP )?PRIVATE KEY(?: BLOCK)?-----",
    },
    SecretPattern {
        name: "JSON Web Token",
        family: "jwt",
        regex: r"\beyJ[A-Za-z0-9_-]{10,}\.eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
    },
];

/// Name, family, compiled pattern — in declaration order, which is what makes the family rule
/// deterministic.
static COMPILED_PATTERNS: LazyLock<Vec<(&'static str, &'static str, Regex)>> = LazyLock::new(|| {
    SECRET_PATTERNS
        .iter()
        .filter_map(|pattern| {
            Regex::new(pattern.regex)
                .inspect_err(|error| {
                    log::error!("cowork: secret pattern `{}` is invalid: {error}", pattern.name)
                })
                .ok()
                .map(|regex| (pattern.name, pattern.family, regex))
        })
        .collect()
});

/// `NAME = "value"` where the name sounds like a credential and the value looks random.
static ASSIGNMENT: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b([A-Za-z0-9_]*(?:secret|passwd|password|token|api[_-]?key|access[_-]?key|private[_-]?key|credential)[A-Za-z0-9_]*)\s*[:=]\s*["']([^"'\s]{16,})["']"#,
    )
    .inspect_err(|error| log::error!("cowork: secret assignment pattern is invalid: {error}"))
    .ok()
});

/// Shannon entropy in bits per character.
///
/// On its own this is a poor test: a short English phrase with few repeated letters scores above
/// 3.5 too. It is one condition of several in [`looks_random`], which is the predicate that
/// actually decides.
fn entropy(value: &str) -> f64 {
    if value.is_empty() {
        return 0.0;
    }
    let mut counts = [0usize; 256];
    let mut total = 0usize;
    for byte in value.bytes() {
        counts[byte as usize] += 1;
        total += 1;
    }
    counts
        .iter()
        .filter(|count| **count > 0)
        .map(|count| {
            let p = *count as f64 / total as f64;
            -p * p.log2()
        })
        .sum()
}

/// Entropy is the last guard, not the first, so it is set low deliberately.
///
/// The conditions in [`looks_random`] already exclude prose, property paths, slugs, placeholders
/// and anything short. What is left for this number to catch is a long value that repeats itself,
/// and a real credential never does. Set higher, it starts missing genuine keys: a 32-character
/// lowercase hex key — the shape half the industry issues — scores around 3.4.
const ENTROPY_THRESHOLD: f64 = 3.0;

/// The shortest credential worth guessing at. Shorter values are too often ordinary strings.
const MIN_SECRET_LENGTH: usize = 20;

/// A dotted identifier such as `process.env.API_KEY` or `settings.auth.token`.
///
/// These are how a *correctly* written program reaches a secret, so flagging them would punish
/// exactly the code the check is trying to encourage.
static PROPERTY_PATH: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)+$")
        .inspect_err(|error| log::error!("cowork: property-path pattern is invalid: {error}"))
        .ok()
});

/// Whether a literal looks like a generated credential rather than a sentence, a path or a name.
///
/// Every condition here exists because of a false positive it removes: whitespace rules out prose,
/// the property-path rule keeps `process.env.API_KEY` quiet, the character-class rule rules out
/// slugs and sentences, and the length floor rules out short words that happen to score well.
fn looks_random(value: &str) -> bool {
    if value.len() < MIN_SECRET_LENGTH
        || value.chars().any(char::is_whitespace)
        || is_placeholder(value)
    {
        return false;
    }
    if PROPERTY_PATH
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(value))
    {
        return false;
    }

    let classes = [
        value.chars().any(|c| c.is_ascii_lowercase()),
        value.chars().any(|c| c.is_ascii_uppercase()),
        value.chars().any(|c| c.is_ascii_digit()),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();

    classes >= 2 && entropy(value) >= ENTROPY_THRESHOLD
}

/// Values that look random but are not secrets.
fn is_placeholder(value: &str) -> bool {
    let lowered = value.to_ascii_lowercase();
    const MARKERS: [&str; 8] = [
        "example",
        "changeme",
        "placeholder",
        "your-",
        "your_",
        "xxxxx",
        "redacted",
        "dummy",
    ];
    MARKERS.iter().any(|marker| lowered.contains(marker))
}

/// Scans text for credentials.
///
/// This is run over what the agent wrote, before it is saved, because the cheapest moment to catch
/// a key is before it exists on disk — once written it is in the editor's undo history, possibly in
/// a backup, and shortly in a commit.
pub fn scan_secrets(text: &str) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut reported: HashSet<(usize, &'static str)> = HashSet::default();

    for (index, line) in text.lines().enumerate() {
        let line_number = index + 1;

        for (name, family, regex) in COMPILED_PATTERNS.iter() {
            if regex.is_match(line) && reported.insert((line_number, family)) {
                findings.push(Finding {
                    check: "secrets",
                    severity: Severity::Error,
                    line: line_number,
                    message: format!("looks like a {name}; move it to an environment variable"),
                });
            }
        }

        let Some(assignment) = ASSIGNMENT.as_ref() else {
            continue;
        };
        for capture in assignment.captures_iter(line) {
            let (Some(name), Some(value)) = (capture.get(1), capture.get(2)) else {
                continue;
            };
            let value = value.as_str();
            if !looks_random(value) {
                continue;
            }
            if reported.insert((line_number, "assignment")) {
                findings.push(Finding {
                    check: "secrets",
                    severity: Severity::Error,
                    line: line_number,
                    message: format!(
                        "`{}` is assigned a high-entropy literal; move it to an environment variable",
                        name.as_str()
                    ),
                });
            }
        }
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(text: &str) -> Vec<String> {
        scan_secrets(text)
            .into_iter()
            .map(|finding| finding.message)
            .collect()
    }

    #[test]
    fn finds_credentials_with_a_recognisable_shape() {
        let findings = scan_secrets(concat!(
            "const a = \"AKIAIOSFODNN7EXAMPLE\";\n",
            "token: ghp_0123456789abcdefghijklmnopqrstuvwxyzAB\n",
            "key = \"AIzaSyA1234567890abcdefghijklmnopqrstuv\"\n",
            "-----BEGIN RSA PRIVATE KEY-----\n",
        ));

        assert_eq!(findings.len(), 4, "got: {findings:#?}");
        assert!(findings.iter().all(|f| f.severity == Severity::Error));
        assert_eq!(findings[0].line, 1);
        assert_eq!(findings[3].line, 4);
    }

    #[test]
    fn reports_the_line_a_credential_is_on() {
        let findings = scan_secrets("fn main() {}\n\nlet k = \"sk-ant-0123456789abcdefghijklmnop\";\n");

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 3);
    }

    #[test]
    fn a_random_looking_assignment_to_a_secret_name_is_caught() {
        let found = messages("API_KEY = \"f3Kq9vZ2xLpR7wN4mB8tY6sJ1cH5dG0a\"\n");

        assert_eq!(found.len(), 1);
        assert!(found[0].contains("API_KEY"), "got: {found:?}");
    }

    #[test]
    fn placeholders_and_low_entropy_values_are_left_alone() {
        // These are what a README, a template and a test fixture look like. Flagging them trains
        // the user to ignore the check.
        let quiet = [
            "password = \"changeme\"\n",
            "api_key = \"your-api-key-here\"\n",
            "SECRET_TOKEN = \"example-value-for-docs\"\n",
            "let secret = \"aaaaaaaaaaaaaaaaaaaaaaaa\";\n",
        ];

        for source in quiet {
            assert!(
                scan_secrets(source).is_empty(),
                "should not have flagged: {source}"
            );
        }
    }

    #[test]
    fn ordinary_code_produces_nothing() {
        let source = concat!(
            "use std::collections::HashMap;\n",
            "pub fn tokenize(input: &str) -> Vec<String> {\n",
            "    input.split_whitespace().map(str::to_owned).collect()\n",
            "}\n",
        );

        assert!(scan_secrets(source).is_empty());
    }

    #[test]
    fn a_generated_credential_looks_random_and_ordinary_text_does_not() {
        assert!(looks_random("f3Kq9vZ2xLpR7wN4mB8tY6sJ1cH5dG0a"));
        // Hex is only two character classes and scores poorly on entropy, but it is what a great
        // many services issue. Assigned to a name like `api_key`, it is a key.
        assert!(looks_random("d41d8cd98f00b204e9800998ecf8427e"));

        // Each of these would be a false positive under a bare entropy threshold.
        assert!(!looks_random("the quick brown fox jumps"), "prose scores high per character");
        assert!(!looks_random("process.env.STRIPE_KEY"), "this is how you *avoid* hardcoding");
        assert!(!looks_random("aaaaaaaaaaaaaaaaaaaaaaaa"), "no entropy");
        assert!(!looks_random("shortbutrandom1A"), "too short to guess at");
        assert!(!looks_random("this-is-a-long-kebab-slug"), "one character class");
    }

    #[test]
    fn reading_a_secret_from_the_environment_is_never_flagged() {
        // The fix the check recommends must not itself trip the check.
        let quiet = [
            "API_KEY = process.env.API_KEY\n",
            "api_key: str = os.environ.get(\"API_KEY\")\n",
            "let token = std::env::var(\"GITHUB_TOKEN\")?;\n",
        ];

        for source in quiet {
            assert!(scan_secrets(source).is_empty(), "should not have flagged: {source}");
        }
    }

    #[test]
    fn an_anthropic_key_is_not_also_reported_as_an_openai_key() {
        // `sk-ant-…` satisfies both patterns; the more specific name is the right one.
        let findings = scan_secrets("k = \"sk-ant-api03-0123456789abcdefghijklmnop\"\n");

        assert_eq!(findings.len(), 1, "got: {findings:#?}");
        assert!(findings[0].message.contains("Anthropic"), "got: {}", findings[0].message);
    }

    #[test]
    fn one_finding_per_pattern_per_line() {
        // A line repeating the same key must not produce one finding per occurrence.
        let findings = scan_secrets("a=\"AKIAIOSFODNN7EXAMPLE\" b=\"AKIAIOSFODNN7EXAMPLE\"\n");

        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn the_model_facing_report_leads_with_the_worst() {
        let report = VerificationReport {
            findings: vec![
                Finding {
                    check: "lint",
                    severity: Severity::Warning,
                    line: 2,
                    message: "unused import".into(),
                },
                Finding {
                    check: "secrets",
                    severity: Severity::Error,
                    line: 9,
                    message: "looks like a GitHub token".into(),
                },
            ],
        };

        let rendered = report.to_model("src/a.rs");
        let error_at = rendered.find("error").expect("the error should be listed");
        let warning_at = rendered.find("warning").expect("the warning should be listed");

        assert!(error_at < warning_at, "errors come first:\n{rendered}");
        assert!(rendered.contains("src/a.rs:9"));
    }

    #[test]
    fn an_empty_report_renders_to_nothing() {
        assert!(VerificationReport::default().to_model("a.rs").is_empty());
    }

    fn settings(secret_scan: bool) -> VerificationSettings {
        VerificationSettings {
            format: true,
            diagnostics: true,
            secret_scan,
            dependency_audit: true,
        }
    }

    #[test]
    fn the_gate_stops_a_credential_before_it_is_written() {
        let leaked = "const KEY = \"AKIAIOSFODNN7EXAMPLE\";";

        assert_eq!(gate(&settings(true), leaked).len(), 1);
    }

    #[test]
    fn turning_the_secret_scan_off_turns_the_gate_off() {
        // The user's choice has to actually mean something, or the toggle is decoration.
        let leaked = "const KEY = \"AKIAIOSFODNN7EXAMPLE\";";

        assert!(gate(&settings(false), leaked).is_empty());
    }

    #[test]
    fn no_check_is_enabled_means_no_work() {
        let none = VerificationSettings {
            format: false,
            diagnostics: false,
            secret_scan: false,
            dependency_audit: false,
        };

        assert!(!none.any_enabled());
        assert!(settings(true).any_enabled());
    }

    #[test]
    fn a_diagnostic_is_attributed_to_the_tool_that_produced_it() {
        assert_eq!(source_check("clippy"), "clippy");
        assert_eq!(source_check("biome"), "biome");
        assert_eq!(source_check("rustc"), "rustc");
        assert_eq!(source_check("some-other-server"), "diagnostics");
    }
}
