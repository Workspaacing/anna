//! What a command would do, as distinct from whether it is a command.
//!
//! The permission prompt used to fire for every shell command, which made it useless in the way
//! every over-eager confirmation is useless: asked whether `ls` may run, four times in a minute, a
//! person stops reading the dialog and starts clicking through it — and then clicks through the one
//! that mattered. A prompt that interrupts constantly does not make anyone safer; it trains them
//! not to look.
//!
//! So the question asked here is not "is this a command" but "could I undo this". Reading a
//! directory is as safe as reading a file, and the app already lets the agent read files. Writing
//! one is visible in the gutter and reversible with undo. Deleting one, pushing a branch, or
//! publishing a package is none of those things, and that is the line worth interrupting for.
//!
//! Being wrong in one direction is much worse than the other, so anything not recognised is
//! treated as changing something rather than as harmless — and the separate check for commands
//! that reach outside the project still forces a prompt regardless of what is decided here.

/// How far a command can be taken back.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Consequence {
    /// Reads, and changes nothing. `ls`, `git status`, `cargo check`.
    Harmless,
    /// Changes the working tree in a way the editor, git, or a rerun can undo. `mkdir`,
    /// `npm install`, `cargo build`.
    Reversible,
    /// Cannot be taken back from inside this app: it destroys, publishes, or leaves the machine.
    Irreversible,
}

/// What running `command` would amount to.
pub fn classify(command: &str) -> Consequence {
    // Every segment of a compound command counts: `ls && rm -rf /tmp` is not a listing.
    command
        .split(|character| matches!(character, ';' | '|' | '&' | '\n'))
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(classify_one)
        .max()
        .unwrap_or(Consequence::Reversible)
}

fn classify_one(segment: &str) -> Consequence {
    let mut words = segment.split_whitespace().skip_while(|word| {
        // `FOO=bar cmd` — the assignment is not the program.
        word.contains('=') && !word.starts_with(['"', '\'', '-'])
    });

    let Some(program) = words.next() else {
        return Consequence::Reversible;
    };
    let program = program
        .trim_matches(['"', '\''])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .trim_end_matches(".exe");

    // Running something as another user is beyond anything this app can reason about.
    if matches!(program, "sudo" | "doas" | "runas" | "su") {
        return Consequence::Irreversible;
    }

    let arguments = words.collect::<Vec<_>>();
    let subcommand = arguments
        .iter()
        .find(|word| !word.starts_with('-'))
        .copied()
        .unwrap_or_default();

    match program {
        // --- destroys things ---------------------------------------------------------------------
        "rm" | "rmdir" | "del" | "erase" | "rd" | "shred" | "dd" | "mkfs" | "format" | "fdisk"
        | "diskpart" | "kill" | "taskkill" | "shutdown" | "reboot" | "chmod" | "chown"
        | "icacls" | "takeown" => Consequence::Irreversible,

        // PowerShell spells them differently and they are no gentler for it.
        "Remove-Item" | "Clear-Content" | "Stop-Process" | "Stop-Computer" => {
            Consequence::Irreversible
        }

        // --- git, where the subcommand is the whole story --------------------------------------
        "git" => match subcommand {
            "status" | "log" | "diff" | "show" | "branch" | "remote" | "ls-files" | "rev-parse"
            | "describe" | "blame" | "shortlog" | "tag" | "stash" | "config" | "cat-file"
            | "ls-remote" | "whatchanged" | "reflog" => Consequence::Harmless,
            // A push is visible to other people; a hard reset and a clean destroy uncommitted work.
            "push" | "clean" | "filter-branch" | "filter-repo" => Consequence::Irreversible,
            "reset" | "checkout" | "restore" | "switch" if discards(&arguments) => {
                Consequence::Irreversible
            }
            _ => Consequence::Reversible,
        },

        "cargo" => match subcommand {
            "check" | "clippy" | "test" | "tree" | "metadata" | "search" | "doc" | "bench"
            | "verify-project" => Consequence::Harmless,
            "publish" | "yank" | "owner" | "login" | "install" | "uninstall" => {
                Consequence::Irreversible
            }
            _ => Consequence::Reversible,
        },

        "npm" | "pnpm" | "yarn" | "bun" => match subcommand {
            "test" | "ls" | "list" | "view" | "outdated" | "audit" | "why" | "info" | "ping"
            | "doctor" => Consequence::Harmless,
            "publish" | "unpublish" | "deprecate" | "adduser" | "login" | "token" => {
                Consequence::Irreversible
            }
            _ => Consequence::Reversible,
        },

        "gh" => match subcommand {
            "browse" | "status" | "search" => Consequence::Harmless,
            // `gh api` is a GET until it is not.
            "api" if !arguments.iter().any(|word| is_writing_method(word)) => {
                Consequence::Harmless
            }
            _ => Consequence::Irreversible,
        },

        "docker" | "podman" | "kubectl" | "helm" | "terraform" | "aws" | "gcloud" | "az" => {
            // These reach a machine that is not this one, and this app cannot tell which.
            Consequence::Irreversible
        }

        // --- reads ---------------------------------------------------------------------------------
        "ls" | "dir" | "cat" | "type" | "head" | "tail" | "wc" | "grep" | "rg" | "find" | "fd"
        | "pwd" | "cd" | "echo" | "which" | "where" | "whoami" | "stat" | "file" | "du" | "df"
        | "tree" | "diff" | "sort" | "uniq" | "cut" | "jq" | "date" | "env" | "printenv"
        | "hostname" | "uname" | "ps" | "top" | "basename" | "dirname" | "realpath" | "md5sum"
        | "sha256sum" | "Get-Content" | "Get-ChildItem" | "Select-String" | "Test-Path" => {
            Consequence::Harmless
        }

        // `sed -n` prints; `sed -i` rewrites the file in place.
        "sed" => {
            if arguments.iter().any(|word| word.starts_with("-i")) {
                Consequence::Reversible
            } else {
                Consequence::Harmless
            }
        }

        // A version check is not a build.
        "node" | "python" | "python3" | "deno" | "tsc" | "go" | "rustc" | "java" | "php"
        | "ruby" => {
            if arguments
                .iter()
                .any(|word| matches!(*word, "--version" | "-v" | "--help" | "-h" | "--noEmit"))
            {
                Consequence::Harmless
            } else {
                Consequence::Reversible
            }
        }

        // Fetching is reading; posting is not.
        "curl" | "wget" | "http" | "Invoke-WebRequest" => {
            if arguments.iter().any(|word| is_writing_method(word)) {
                Consequence::Irreversible
            } else {
                Consequence::Harmless
            }
        }

        // Anything unrecognised changed something, as far as this is concerned. A project's own
        // script is the common case and it is treated as an ordinary change — the check for
        // commands that reach outside the project is what guards the rest.
        _ => Consequence::Reversible,
    }
}

/// Whether a git invocation throws away work rather than moving a pointer.
fn discards(arguments: &[&str]) -> bool {
    arguments
        .iter()
        .any(|word| matches!(*word, "--hard" | "--force" | "-f" | "--" | "."))
}

/// Whether an HTTP flag turns a fetch into a write.
fn is_writing_method(word: &str) -> bool {
    matches!(
        word,
        "-X" | "--request"
            | "POST"
            | "PUT"
            | "DELETE"
            | "PATCH"
            | "-d"
            | "--data"
            | "--data-raw"
            | "-F"
            | "--form"
            | "-T"
            | "--upload-file"
            | "-Method"
    )
}

#[cfg(test)]
mod tests {
    use super::Consequence::*;
    use super::*;

    #[test]
    fn looking_at_things_does_not_interrupt_anyone() {
        // The complaint this exists for: being asked whether `ls` may run.
        for command in [
            "ls",
            "ls -la src",
            "cat package.json",
            "grep -rn TODO src",
            "rg --files",
            "git status --short",
            "git log --oneline -20",
            "git diff HEAD~1",
            "cargo check",
            "cargo clippy --all-targets",
            "cargo test -p cowork",
            "npm test",
            "node --version",
            "tsc --noEmit",
            "pwd",
        ] {
            assert_eq!(classify(command), Harmless, "{command}");
        }
    }

    #[test]
    fn destroying_things_always_interrupts() {
        for command in [
            "rm -rf target",
            "rm file.txt",
            "del build",
            "Remove-Item -Recurse dist",
            "git push origin main",
            "git push --force",
            "git reset --hard HEAD~3",
            "git clean -fdx",
            "npm publish",
            "cargo publish",
            "sudo anything at all",
            "chmod 777 .",
        ] {
            assert_eq!(classify(command), Irreversible, "{command}");
        }
    }

    #[test]
    fn ordinary_work_is_a_change_rather_than_a_catastrophe() {
        for command in [
            "mkdir -p src/lib",
            "npm install",
            "npm run build",
            "cargo build",
            "cargo fmt",
            "node scripts/seed.js",
            "git commit -m 'x'",
            "git add .",
            "git checkout -b feature",
            "./scripts/whatever.sh",
        ] {
            assert_eq!(classify(command), Reversible, "{command}");
        }
    }

    #[test]
    fn a_compound_command_is_judged_by_its_worst_part() {
        // The whole point of splitting: hiding a deletion behind a listing must not work.
        assert_eq!(classify("ls && rm -rf /tmp/x"), Irreversible);
        assert_eq!(classify("cargo check; cargo build"), Reversible);
        assert_eq!(classify("git status | grep modified"), Harmless);
        assert_eq!(classify("npm test && npm publish"), Irreversible);
    }

    #[test]
    fn an_environment_prefix_is_not_the_program() {
        assert_eq!(classify("RUST_LOG=debug cargo test"), Harmless);
        assert_eq!(classify("CI=1 FORCE=1 rm -rf dist"), Irreversible);
    }

    #[test]
    fn a_path_to_a_program_is_still_that_program() {
        assert_eq!(classify("/bin/rm -rf x"), Irreversible);
        assert_eq!(classify("C:\\Windows\\System32\\del.exe x"), Irreversible);
        assert_eq!(classify("./node_modules/.bin/tsc --noEmit"), Harmless);
    }

    #[test]
    fn git_checkout_is_only_dangerous_when_it_throws_work_away() {
        // `git checkout -b` makes a branch; `git checkout -- .` discards every uncommitted edit.
        assert_eq!(classify("git checkout -b feature"), Reversible);
        assert_eq!(classify("git checkout -- ."), Irreversible);
        assert_eq!(classify("git restore --force src"), Irreversible);
    }

    #[test]
    fn fetching_is_reading_and_posting_is_not() {
        assert_eq!(classify("curl https://example.com/a.json"), Harmless);
        assert_eq!(classify("curl -X POST https://example.com"), Irreversible);
        assert_eq!(classify("curl -d @body.json https://example.com"), Irreversible);
        assert_eq!(classify("wget https://example.com/file"), Harmless);
    }

    #[test]
    fn sed_is_decided_by_whether_it_writes_back() {
        assert_eq!(classify("sed -n '1,20p' file"), Harmless);
        assert_eq!(classify("sed -i 's/a/b/' file"), Reversible);
    }

    #[test]
    fn an_unrecognised_program_is_assumed_to_change_something() {
        // Never `Harmless`: being wrong that way is the expensive direction.
        assert_eq!(classify("some-tool --flag"), Reversible);
        assert_eq!(classify(""), Reversible);
        assert_eq!(classify("   "), Reversible);
    }

    #[test]
    fn reaching_another_machine_is_treated_as_beyond_recall() {
        // This app cannot tell a staging cluster from production, so it does not guess.
        for command in ["kubectl delete pod x", "docker rm -f c", "terraform apply", "aws s3 rm s3://b"] {
            assert_eq!(classify(command), Irreversible, "{command}");
        }
    }

    #[test]
    fn the_ordering_lets_the_worst_case_win() {
        // `max()` over segments only works because the severity order is the derive order.
        assert!(Harmless < Reversible);
        assert!(Reversible < Irreversible);
    }
}
