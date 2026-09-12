//! Asking before the agent does something the editor cannot take back.
//!
//! Reading and writing files needs no broker: every change lands in a buffer the editor owns, so it
//! shows up in the open editor, in the git gutter and in the undo history. Running a command is
//! different — nothing here can undo `rm -rf`, a `git push`, or an `npm publish` — so the shell tool
//! goes through this.
//!
//! One request is outstanding at a time, because the turn loop runs tools in sequence. A request
//! that is never answered blocks that turn and nothing else.

use crate::consequence::Consequence;
use crate::cowork_settings::CoworkSettings;
use collections::HashSet;
use futures::channel::oneshot;
use gpui::{Context, EventEmitter, SharedString};
use settings::Settings as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Just this once.
    Once,
    /// This and anything else in the same scope, until Wu is restarted.
    Always,
    Reject,
}

impl Decision {
    pub fn is_allowed(self) -> bool {
        matches!(self, Decision::Once | Decision::Always)
    }
}

/// Something the agent wants to do and may not do unasked.
#[derive(Clone, Debug)]
pub struct PermissionRequest {
    /// The tool asking, for the card's icon and for the log.
    pub tool: &'static str,
    /// One line: what it wants to do.
    pub title: SharedString,
    /// The specifics the user judges — for a command, the command itself.
    pub detail: SharedString,
    /// What running it would amount to, which decides whether it is worth an interruption.
    pub consequence: Consequence,
    /// Whether this must be asked even when the user has turned on approving everything.
    ///
    /// Approving everything is a promise about *this project*: the user made it so an agent could
    /// work without being interrupted about files they had already chosen to open. A command that
    /// reaches outside those folders is not what they agreed to, so it is asked anyway — the
    /// blanket approval is deliberately not blanket at the project boundary.
    pub always_ask: bool,
    /// What "always" would cover.
    ///
    /// For a command this is the program name, so allowing `cargo` once does not also allow `rm`.
    /// It is shown on the button, because a permission whose reach the user cannot see is not one
    /// they can give meaningfully.
    pub scope: String,
}

pub enum PermissionEvent {
    Changed,
}

pub struct PermissionBroker {
    pending: Option<(PermissionRequest, oneshot::Sender<Decision>)>,
    /// Scopes the user allowed for the session. Deliberately not persisted: a standing grant that
    /// outlives the window is one the user will have forgotten giving.
    granted: HashSet<String>,
}

impl EventEmitter<PermissionEvent> for PermissionBroker {}

impl Default for PermissionBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl PermissionBroker {
    pub fn new() -> Self {
        Self {
            pending: None,
            granted: HashSet::default(),
        }
    }

    /// Asks the user, returning a receiver the caller awaits.
    ///
    /// Resolves immediately when the scope was already allowed for the session, or when the user
    /// has turned approval off entirely. A dropped sender resolves to a rejection, so a thread
    /// closed mid-question denies rather than hanging.
    pub fn request(
        &mut self,
        request: PermissionRequest,
        cx: &mut Context<Self>,
    ) -> oneshot::Receiver<Decision> {
        let (sender, receiver) = oneshot::channel();

        // Reading something is not worth an interruption. The prompt exists for changes that
        // cannot be seen or taken back, and firing it for `ls` and `git status` is how a person
        // learns to click through the dialog without reading it — which costs exactly the one
        // that mattered. A command that reaches outside the project is still asked about, whatever
        // it does, because leaving the project is itself the thing being judged.
        if request.consequence == Consequence::Harmless && !request.always_ask {
            let _ = sender.send(Decision::Always);
            return receiver;
        }

        // Both shortcuts are skipped for a request that leaves the project: neither the
        // setting nor an earlier "always" for this program was given with that in view.
        if !request.always_ask {
            if CoworkSettings::get_global(cx).auto_approve {
                let _ = sender.send(Decision::Always);
                return receiver;
            }
            if self.granted.contains(&request.scope) {
                let _ = sender.send(Decision::Always);
                return receiver;
            }
        }

        // A request that arrives while another is open replaces it, and the displaced one is
        // refused rather than left to hang.
        if let Some((_, displaced)) = self.pending.take() {
            let _ = displaced.send(Decision::Reject);
        }

        self.pending = Some((request, sender));
        cx.emit(PermissionEvent::Changed);
        cx.notify();
        receiver
    }

    pub fn resolve(&mut self, decision: Decision, cx: &mut Context<Self>) {
        let Some((request, sender)) = self.pending.take() else {
            return;
        };

        if decision == Decision::Always {
            self.granted.insert(request.scope);
        }
        let _ = sender.send(decision);
        cx.emit(PermissionEvent::Changed);
        cx.notify();
    }

    /// Refuses whatever is outstanding, for when the user interrupts the turn.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.pending.is_some() {
            self.resolve(Decision::Reject, cx);
        }
    }

    pub fn pending(&self) -> Option<&PermissionRequest> {
        self.pending.as_ref().map(|(request, _)| request)
    }

    pub fn is_granted(&self, scope: &str) -> bool {
        self.granted.contains(scope)
    }
}

/// The program a command line runs, which is what "always allow" applies to.
///
/// Three things have to be right or the grant covers something other than the user thinks:
/// environment prefixes are stepped over, so `FOO=1 cargo test` is scoped to `cargo`; a quoted
/// program is read to its closing quote, because that is the only way to write a path containing
/// spaces; and the directory is dropped, so `/usr/bin/git` and `git` are one permission rather
/// than two.
pub fn command_scope(command: &str) -> String {
    let mut rest = command.trim_start();
    while let Some(word) = rest.split_whitespace().next() {
        if word.contains('=') && !word.starts_with(['"', '\'']) {
            rest = rest[word.len()..].trim_start();
        } else {
            break;
        }
    }

    let program = match (rest.chars().next(), rest.get(1..)) {
        (Some(quote @ ('"' | '\'')), Some(quoted)) => quoted.split(quote).next().unwrap_or(quoted),
        _ => rest.split_whitespace().next().unwrap_or(""),
    };

    if program.is_empty() {
        return "command".to_owned();
    }
    program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_is_the_program_being_run() {
        assert_eq!(command_scope("cargo test -p cowork"), "cargo");
        assert_eq!(command_scope("npm run build"), "npm");
        assert_eq!(command_scope("  git   status  "), "git");
    }

    #[test]
    fn a_scope_steps_over_environment_prefixes() {
        // Otherwise allowing `RUST_LOG=debug cargo test` would grant the scope `RUST_LOG=debug`,
        // which covers nothing and asks again every time.
        assert_eq!(command_scope("RUST_LOG=debug cargo test"), "cargo");
        assert_eq!(command_scope("A=1 B=2 make"), "make");
    }

    #[test]
    fn a_scope_ignores_where_the_program_lives() {
        // `/usr/bin/git` and `git` are the same permission to give.
        assert_eq!(command_scope("/usr/bin/git push"), "git");
        assert_eq!(command_scope(r"C:\tools\git.exe status"), "git.exe");
    }

    #[test]
    fn a_quoted_program_keeps_the_spaces_in_its_path() {
        // Splitting on whitespace first scoped this to `Program`, which grants nothing the user
        // meant and asks again on the next command.
        assert_eq!(
            command_scope(r#""C:\Program Files\Git\git.exe" status"#),
            "git.exe"
        );
        assert_eq!(command_scope("'/opt/my tools/run' --once"), "run");
    }

    #[test]
    fn an_empty_command_still_has_a_scope() {
        // It will be refused for being empty, but it must not panic on the way there.
        assert_eq!(command_scope(""), "command");
        assert_eq!(command_scope("   "), "command");
    }

    #[test]
    fn allowing_one_program_does_not_allow_another() {
        // The whole point of the scope: `cargo` must never imply `rm`.
        assert_ne!(command_scope("cargo test"), command_scope("rm -rf /"));
    }

    #[test]
    fn a_decision_says_plainly_whether_it_permits() {
        assert!(Decision::Once.is_allowed());
        assert!(Decision::Always.is_allowed());
        assert!(!Decision::Reject.is_allowed());
    }
}
