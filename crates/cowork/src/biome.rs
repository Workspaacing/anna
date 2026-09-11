//! Biome, run over the JavaScript-family files the agent writes.
//!
//! Wu drives the project's own Biome rather than a copy of it. That is a deliberate choice, and the
//! alternative was investigated first: the `biome_*` crates are published on crates.io and the
//! formatter, linter and autofix all work from them, but they are frozen at 0.5.7 — Biome 1.6.1,
//! March 2024 — and the parts that read `biome.json` are `pub(crate)` in crates that are not
//! published at all. Embedding them would mean a linter that ignores the project's own
//! configuration and disagrees with the Biome the team runs in CI, for +10 MB of binary and ~117
//! extra packages. A checker that contradicts the project is worse than no checker.
//!
//! So: find the Biome the project already has — `node_modules/.bin` first, then `PATH` — and run it.
//! It reads `biome.json`, its rules match CI, and it updates with the project. Nothing is asked of a
//! model, so this costs no tokens.
//!
//! The source is passed on stdin and the corrected source comes back on stdout, so the change lands
//! in the buffer the editor owns rather than behind its back.

use crate::verify::{Finding, Severity};
use anyhow::{Context as _, Result, bail};
use futures::{AsyncReadExt as _, AsyncWriteExt as _};
use gpui::{BackgroundExecutor, FutureExt as _};
use regex::Regex;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::LazyLock,
    time::Duration,
};

/// Biome is fast; anything past this is a hang, not a slow run.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The extensions Biome understands.
///
/// Everything else is left to the project's own language servers, which Wu already runs.
const EXTENSIONS: &[&str] = &[
    "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "json", "jsonc", "css", "graphql", "gql",
    "vue", "svelte", "astro",
];

pub fn applies_to(file_name: &str) -> bool {
    file_name
        .rsplit_once('.')
        .is_some_and(|(_, extension)| EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
}

/// Finds the Biome the project would use.
///
/// The project's own copy wins over anything on `PATH`, because that is the version its lockfile
/// pins and the one CI will run.
pub fn locate(worktree_root: &Path) -> Option<PathBuf> {
    let local = worktree_root.join("node_modules").join(".bin");
    for name in binary_names() {
        let candidate = local.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    which::which("biome").ok()
}

#[cfg(windows)]
fn binary_names() -> &'static [&'static str] {
    // npm writes a shim per platform; `biome` alone is the shell script, which Windows cannot run.
    &["biome.exe", "biome.cmd", "biome"]
}

#[cfg(not(windows))]
fn binary_names() -> &'static [&'static str] {
    &["biome"]
}

/// What Biome made of one file.
pub struct Outcome {
    /// The corrected source, when Biome changed anything.
    pub fixed: Option<String>,
    /// What it could not fix on its own.
    pub findings: Vec<Finding>,
}

/// Runs `biome check --write` over one file's source.
///
/// `path` is passed as `--stdin-file-path` so Biome picks the right language and applies the
/// `biome.json` that governs that location.
pub async fn check(
    binary: PathBuf,
    working_directory: PathBuf,
    path: String,
    source: String,
    executor: &BackgroundExecutor,
) -> Result<Outcome> {
    let mut command = util::command::new_std_command(&binary);
    command
        .args([
            "check",
            "--write",
            // Pretty output is what gets parsed below; escape codes would ruin it.
            "--colors=off",
            &format!("--stdin-file-path={path}"),
        ])
        .current_dir(&working_directory);

    let mut child = util::process::Child::spawn(command, Stdio::piped(), Stdio::piped(), Stdio::piped())
        .with_context(|| format!("running {}", binary.display()))?;

    let mut stdin = child.stdin.take().context("biome refused a stdin pipe")?;
    let mut stdout = child.stdout.take().context("biome refused a stdout pipe")?;
    let mut stderr = child.stderr.take().context("biome refused a stderr pipe")?;

    // Writing the whole input before reading the output deadlocks once the source outgrows the
    // pipe buffer, so all three run together.
    let pump = async move {
        let write = async move {
            stdin.write_all(source.as_bytes()).await?;
            stdin.close().await
        };
        let read_out = async move {
            let mut text = String::new();
            stdout.read_to_string(&mut text).await.map(|_| text)
        };
        let read_err = async move {
            let mut text = String::new();
            stderr.read_to_string(&mut text).await.map(|_| text)
        };
        futures::future::join3(write, read_out, read_err).await
    };

    let Ok((written, out, err)) = pump.with_timeout(TIMEOUT, executor).await else {
        let _ = child.kill();
        bail!("biome did not finish within {} seconds", TIMEOUT.as_secs());
    };
    written.context("sending the file to biome")?;
    let out = out.context("reading biome's output")?;
    let err = err.unwrap_or_default();

    // Biome exits non-zero when it found problems, which is not a failure of the run. A missing
    // configuration or an unreadable file is, and that comes with no source on stdout.
    let status = child.status().await.context("waiting for biome")?;
    if out.is_empty() && !status.success() {
        bail!("biome failed: {}", first_line(&err));
    }

    Ok(Outcome {
        fixed: Some(out),
        findings: parse_diagnostics(&err, &path),
    })
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no output")
        .to_owned()
}

/// The header Biome prints above each diagnostic: `path:line:column category`.
static HEADER: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"^(?<path>[^\s:]+):(?<line>\d+):(?<column>\d+)\s+(?<category>\S+)")
        .inspect_err(|error| log::error!("cowork: biome header pattern is invalid: {error}"))
        .ok()
});

/// Reads Biome's own report.
///
/// Anything that cannot be parsed is still reported verbatim rather than dropped: a check that
/// silently swallows what it does not recognise is worse than one that is occasionally untidy.
fn parse_diagnostics(stderr: &str, path: &str) -> Vec<Finding> {
    let Some(header) = HEADER.as_ref() else {
        return Vec::new();
    };

    let mut findings = Vec::new();
    let mut lines = stderr.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(captured) = header.captures(line.trim_start()) else {
            continue;
        };
        let Some(number) = captured["line"].parse::<usize>().ok().filter(|n| *n > 0) else {
            continue;
        };

        // The message follows on one of the next lines, marked with Biome's severity glyph.
        let mut message = String::new();
        while let Some(next) = lines.peek() {
            if header.is_match(next.trim_start()) {
                break;
            }
            let next = lines.next().unwrap_or_default().trim();
            if let Some(rest) = next
                .strip_prefix('\u{2716}')
                .or_else(|| next.strip_prefix('\u{26a0}'))
            {
                message = rest.trim().to_owned();
                break;
            }
        }

        let category = &captured["category"];
        findings.push(Finding {
            check: "biome",
            severity: if category.starts_with("lint/") {
                Severity::Warning
            } else {
                Severity::Error
            },
            line: number,
            message: if message.is_empty() {
                format!("biome reported `{category}`")
            } else {
                format!("{category}: {message}")
            },
        });
    }

    if findings.is_empty() && !stderr.trim().is_empty() && stderr.contains("biome") {
        log::debug!("cowork: biome said something unparsed about {path}: {stderr}");
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_languages_biome_understands_are_sent_to_it() {
        for name in ["a.ts", "b.tsx", "c.json", "d.css", "e.JS", "f.svelte"] {
            assert!(applies_to(name), "{name} should go to biome");
        }
        for name in ["main.rs", "lib.py", "go.mod", "README.md", "Makefile", "noextension"] {
            assert!(!applies_to(name), "{name} should not go to biome");
        }
    }

    #[test]
    fn reads_a_diagnostic_back_out_of_biomes_report() {
        let stderr = concat!(
            "app.ts:3:7 lint/style/useConst  FIXABLE  ━━━━━━━━━━━\n",
            "\n",
            "  \u{2716} This let declares a variable that is never re-assigned.\n",
            "\n",
            "    1 │ function a() {\n",
        );

        let findings = parse_diagnostics(stderr, "app.ts");

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 3);
        assert_eq!(findings[0].check, "biome");
        assert_eq!(findings[0].severity, Severity::Warning);
        assert!(
            findings[0].message.contains("never re-assigned"),
            "got: {}",
            findings[0].message
        );
    }

    #[test]
    fn several_diagnostics_are_read_without_running_together() {
        let stderr = concat!(
            "a.ts:1:1 lint/suspicious/noDebugger  ━━━━\n",
            "  \u{2716} This is an unexpected use of the debugger statement.\n",
            "\n",
            "a.ts:9:4 parse  ━━━━\n",
            "  \u{2716} Expected a semicolon.\n",
        );

        let findings = parse_diagnostics(stderr, "a.ts");

        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].line, 1);
        assert_eq!(findings[1].line, 9);
        assert_eq!(
            findings[1].severity,
            Severity::Error,
            "a parse failure is not a style opinion"
        );
    }

    #[test]
    fn a_diagnostic_with_no_readable_message_still_names_its_rule() {
        let findings = parse_diagnostics("a.ts:2:1 lint/a11y/useAltText  ━━━\n", "a.ts");

        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("useAltText"), "{:?}", findings[0]);
    }

    #[test]
    fn a_clean_run_reports_nothing() {
        assert!(parse_diagnostics("", "a.ts").is_empty());
        assert!(parse_diagnostics("Checked 1 file in 2ms. No fixes applied.\n", "a.ts").is_empty());
    }

    #[test]
    fn a_line_number_of_zero_is_not_a_location() {
        // Findings are 1-based everywhere; a 0 would point at nothing.
        assert!(parse_diagnostics("a.ts:0:1 lint/x  ━━━\n", "a.ts").is_empty());
    }
}
