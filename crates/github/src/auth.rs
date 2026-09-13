//! Getting a token, and keeping it somewhere that is not a file.
//!
//! There are two ways in, and the order matters. Most people who would use this already have the
//! GitHub CLI signed in, so asking it for its token turns connecting into one click with nothing
//! to paste and nothing to lose. Pasting a personal access token is the fallback, and the only
//! option for anyone without the CLI.
//!
//! What there deliberately is not is an OAuth flow. It would need a client id and secret shipped
//! inside the binary, where neither is secret, and a callback server listening on localhost — a
//! meaningful amount of machinery and attack surface to replace a button that already works.
//!
//! The token never reaches a settings file, the database, or a log. It goes to the OS credential
//! store, the same place Cowork keeps provider keys.

use anyhow::{Context as _, Result, bail};
use gpui::{App, AppContext as _, Task};

/// Where the token lives in the OS credential store.
///
/// Keyed by host so a future GitHub Enterprise connection does not overwrite this one.
pub const CREDENTIAL_URL: &str = "github://api.github.com";

/// Reads the token the GitHub CLI is already holding.
///
/// `gh auth token` prints it and nothing else, which is why this is a subprocess rather than an
/// attempt to read the CLI's own storage: on Windows that storage is the Credential Manager under
/// a key the CLI owns, on macOS the Keychain, on Linux whatever `gh` decided. Asking `gh` is the
/// one method that is right on all three.
pub async fn from_cli() -> Result<String> {
    let output = util::command::new_command("gh")
        .args(["auth", "token"])
        .output()
        .await;

    let output = match output {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "The GitHub CLI is not installed, or is not on this machine's PATH. Install it \
                 from cli.github.com, or paste a token instead."
            )
        }
        Err(error) => return Err(error).context("running the GitHub CLI"),
    };

    if !output.status.success() {
        let complaint = String::from_utf8_lossy(&output.stderr);
        bail!("{}", describe_cli_failure(&complaint));
    }

    let token = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if token.is_empty() {
        bail!("The GitHub CLI returned an empty token. Try `gh auth login` and connect again.");
    }
    Ok(token)
}

/// Turns the CLI's complaint into something worth reading.
///
/// `gh` writes a helpful paragraph to stderr, but it is written for a terminal — it suggests
/// commands, wraps, and repeats itself. The one case worth recognising is not being signed in,
/// because the remedy is a single command the user can be told.
fn describe_cli_failure(stderr: &str) -> String {
    let lowered = stderr.to_lowercase();
    if lowered.contains("not logged in") || lowered.contains("no oauth token") {
        return "The GitHub CLI is installed but not signed in. Run `gh auth login`, then connect \
                again."
            .to_owned();
    }

    let first_line = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("the GitHub CLI failed without saying why");
    format!("The GitHub CLI could not provide a token: {first_line}")
}

/// Rejects a pasted value that cannot be a token, before spending a request on it.
///
/// The check is deliberately loose. GitHub has changed its token formats several times — classic
/// hex, `ghp_`, `gho_`, the longer `github_pat_` — and a strict allow-list would reject the next
/// one. So this only catches the mistakes people actually make: pasting a URL, pasting a username,
/// or pasting something with a space in it.
pub fn looks_like_a_token(candidate: &str) -> Result<()> {
    let candidate = candidate.trim();

    if candidate.is_empty() {
        bail!("Paste a token first.");
    }
    if candidate.contains(char::is_whitespace) {
        bail!("That has a space in it, so it is not a token — check what was copied.");
    }
    if candidate.contains("://") || candidate.starts_with("github.com") {
        bail!("That looks like a URL. A token is the long string from the token settings page.");
    }
    if candidate.len() < 20 {
        bail!("That is too short to be a GitHub token.");
    }
    Ok(())
}

/// Saves the token to the OS credential store.
pub fn store(token: &str, login: &str, cx: &App) -> Task<Result<()>> {
    cx.write_credentials(CREDENTIAL_URL, login, token.as_bytes())
}

/// The saved token, when there is one.
///
/// A missing entry is not an error — it is the ordinary state before anyone has connected — so it
/// comes back as `None` rather than a failure to be reported.
pub fn stored(cx: &App) -> Task<Result<Option<String>>> {
    let read = cx.read_credentials(CREDENTIAL_URL);
    cx.background_spawn(async move {
        let Some((_login, token)) = read.await? else {
            return Ok(None);
        };
        let token = String::from_utf8(token)
            .context("the stored GitHub token is not text; disconnect and connect again")?;
        Ok(Some(token))
    })
}

/// Forgets the token.
pub fn forget(cx: &App) -> Task<Result<()>> {
    cx.delete_credentials(CREDENTIAL_URL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_being_signed_in_is_answered_with_the_command_that_fixes_it() {
        // What `gh auth token` writes when there is no session.
        let stderr = "gh: To use GitHub CLI in a GitHub Actions workflow, set the GH_TOKEN \
                      environment variable.\nnot logged in to any hosts";

        let described = describe_cli_failure(stderr);
        assert!(described.contains("gh auth login"), "{described}");
    }

    #[test]
    fn any_other_complaint_is_passed_on_rather_than_swallowed() {
        let described = describe_cli_failure("could not connect to github.com\n");
        assert!(described.contains("could not connect"), "{described}");
    }

    #[test]
    fn an_empty_complaint_still_produces_a_sentence() {
        let described = describe_cli_failure("   \n\n");
        assert!(!described.is_empty());
        assert!(described.contains("without saying why"), "{described}");
    }

    #[test]
    fn the_token_formats_github_actually_issues_are_all_accepted() {
        // Every shape GitHub has used, including the one this machine's CLI holds.
        for token in [
            "ghp_0123456789abcdefghijklmnopqrstuvwxyz",
            "gho_0123456789abcdefghijklmnopqrstuvwxyz",
            "ghs_0123456789abcdefghijklmnopqrstuvwxyz",
            "github_pat_11ABCDEFG0abcdefghijkl_0123456789abcdefghijklmnopqrstuvwxyz",
            "0123456789abcdef0123456789abcdef01234567",
        ] {
            assert!(looks_like_a_token(token).is_ok(), "rejected {token}");
        }
    }

    #[test]
    fn the_mistakes_people_actually_make_are_caught_before_a_request() {
        let url = looks_like_a_token("https://github.com/settings/tokens").unwrap_err();
        assert!(url.to_string().contains("URL"), "{url}");

        let spaced = looks_like_a_token("ghp_abc def").unwrap_err();
        assert!(spaced.to_string().contains("space"), "{spaced}");

        assert!(looks_like_a_token("").is_err());
        assert!(looks_like_a_token("   ").is_err());
        assert!(looks_like_a_token("devconnecting1").is_err());
    }

    #[test]
    fn surrounding_whitespace_is_not_a_reason_to_refuse() {
        // Copying from a terminal brings a newline with it.
        assert!(looks_like_a_token("  ghp_0123456789abcdefghijklmnopqrstuvwxyz\n").is_ok());
    }
}
