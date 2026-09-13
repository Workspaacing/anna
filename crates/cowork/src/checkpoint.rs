//! What the agent's file changes replaced, so rewinding a conversation can put the files back.
//!
//! Each `write` and `edit` call records a [`Checkpoint`] on its tool result, which is persisted with
//! the thread — so a conversation reopened in a later session can still be rewound.

use collections::HashMap;
use gpui::{AppContext as _, AsyncApp, Entity};
use language::Buffer;
use project::Project;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// The largest file whose previous contents a checkpoint keeps.
///
/// The text is stored inside the thread, and the whole thread is rewritten on every save. An agent
/// that rewrote a lockfile or a bundle would otherwise make each later save of the conversation
/// carry megabytes that are almost never needed.
pub const MAX_RECORDED_BYTES: usize = 1024 * 1024;

/// What one `write` or `edit` call did to one file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Absolute rather than a `ProjectPath`, because worktree ids are handed out afresh each time a
    /// project opens and would name nothing by the time a stored thread is rewound.
    pub abs_path: String,
    pub before: Before,
    /// A digest of the text the call left on disk, to tell whether anyone has touched it since.
    pub after_digest: u64,
}

/// A file's state before the agent changed it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "text", rename_all = "snake_case")]
pub enum Before {
    /// The agent created the file.
    Missing,
    Text(String),
    /// Larger than [`MAX_RECORDED_BYTES`], so not kept, and not restorable.
    TooLarge,
}

impl Before {
    pub fn recorded(existed: bool, text: String) -> Self {
        if !existed {
            Before::Missing
        } else if text.len() > MAX_RECORDED_BYTES {
            Before::TooLarge
        } else {
            Before::Text(text)
        }
    }
}

/// FNV-1a, 64-bit.
///
/// Chosen because its output is fixed by definition: the digest is stored in the thread and compared
/// in a later session, possibly by a later build, and `std`'s hasher promises neither. This is change
/// detection, not security — nothing here defends against a crafted collision, and nothing needs to.
pub fn digest(text: &str) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    text.bytes().fold(OFFSET_BASIS, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    })
}

/// One file to put back, merged from every checkpoint a rewind covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRestore {
    pub abs_path: String,
    /// What the file held before the agent first touched it in the rewound range.
    pub target: Before,
    /// What should be on disk now, if nobody but the agent has changed the file since.
    pub expected_digest: u64,
}

/// Merges checkpoints, given oldest first, into one restore per file, in the order the files were
/// first touched.
///
/// The earliest `before` wins because a later call's `before` is only the agent's own earlier edit,
/// and the latest digest wins because an earlier one describes text the agent has since replaced.
pub fn plan(checkpoints: impl IntoIterator<Item = Checkpoint>) -> Vec<FileRestore> {
    let mut restores: Vec<FileRestore> = Vec::new();
    let mut index_by_path: HashMap<String, usize> = HashMap::default();

    for checkpoint in checkpoints {
        match index_by_path.get(&checkpoint.abs_path) {
            Some(&index) => {
                if let Some(restore) = restores.get_mut(index) {
                    restore.expected_digest = checkpoint.after_digest;
                }
            }
            None => {
                index_by_path.insert(checkpoint.abs_path.clone(), restores.len());
                restores.push(FileRestore {
                    abs_path: checkpoint.abs_path,
                    target: checkpoint.before,
                    expected_digest: checkpoint.after_digest,
                });
            }
        }
    }

    restores
}

/// What a restore did, file by file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoreOutcome {
    pub restored: Vec<String>,
    pub removed: Vec<String>,
    /// Files left as they were, each with a reason phrased to follow "because it".
    pub skipped: Vec<(String, String)>,
}

impl RestoreOutcome {
    /// One sentence for a toast.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.restored.is_empty() {
            parts.push(format!("restored {}", files(self.restored.len())));
        }
        if !self.removed.is_empty() {
            parts.push(format!("removed {}", files(self.removed.len())));
        }
        match self.skipped.as_slice() {
            [] => {}
            [(path, reason)] => parts.push(format!(
                "left {} alone because it {reason}",
                file_name(path)
            )),
            // Several reasons do not fit in one sentence; the caller has the list.
            skipped => parts.push(format!("left {} alone", files(skipped.len()))),
        }

        let sentence = parts.join(", ");
        let mut characters = sentence.chars();
        match characters.next() {
            Some(first) => format!("{}{}.", first.to_uppercase(), characters.as_str()),
            None => "There were no file changes to undo.".to_owned(),
        }
    }
}

fn files(count: usize) -> String {
    match count {
        1 => "1 file".to_owned(),
        count => format!("{count} files"),
    }
}

fn file_name(abs_path: &str) -> String {
    Path::new(abs_path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| abs_path.to_owned())
}

/// Puts files back as the plan says, one at a time, and never fails as a whole.
///
/// A file is only overwritten while it still holds exactly what the agent left and has no unsaved
/// edits in an editor. Anything else is the user's own work, and losing it to undo the agent's would
/// be worse than leaving the agent's change in place — so it is reported instead.
pub async fn restore(
    project: Entity<Project>,
    plan: Vec<FileRestore>,
    cx: &mut AsyncApp,
) -> RestoreOutcome {
    let mut outcome = RestoreOutcome::default();
    for file in plan {
        let abs_path = file.abs_path.clone();
        match restore_file(&project, file, cx).await {
            Ok(Restored::Rewritten) => outcome.restored.push(abs_path),
            Ok(Restored::Removed) => outcome.removed.push(abs_path),
            Ok(Restored::AlreadyAbsent) => {}
            Err(reason) => outcome.skipped.push((abs_path, reason)),
        }
    }
    outcome
}

enum Restored {
    Rewritten,
    Removed,
    AlreadyAbsent,
}

/// Restores one file, or says why it was left alone.
async fn restore_file(
    project: &Entity<Project>,
    file: FileRestore,
    cx: &mut AsyncApp,
) -> Result<Restored, String> {
    let original = match file.target {
        Before::TooLarge => return Err("was too large to have been recorded".to_owned()),
        Before::Missing => None,
        Before::Text(text) => Some(text),
    };

    let located = project.read_with(cx, |project, cx| {
        let project_path = project.find_project_path(&file.abs_path, cx)?;
        let exists = project.entry_for_path(&project_path, cx).is_some();
        Some((project_path, exists))
    });
    let Some((project_path, exists)) = located else {
        return Err("is no longer inside a folder open in this project".to_owned());
    };
    if !exists {
        return match original {
            // The agent created it and something has already removed it, which is the goal.
            None => Ok(Restored::AlreadyAbsent),
            Some(_) => Err("has been deleted since".to_owned()),
        };
    }

    let buffer = project
        .update(cx, |project, cx| {
            project.open_buffer(project_path.clone(), cx)
        })
        .await
        .map_err(|error| format!("could not be opened: {error:#}"))?;

    if buffer.read_with(cx, |buffer, _| buffer.is_dirty()) {
        return Err("has unsaved edits".to_owned());
    }
    if buffer_digest(&buffer, cx).await != file.expected_digest {
        return Err("changed since the agent wrote it".to_owned());
    }

    match original {
        None => {
            // To the trash rather than deleted outright, so a rewind the user regrets can itself be
            // undone.
            let trash = project
                .update(cx, |project, cx| project.trash_file(project_path, cx))
                .ok_or_else(|| "is no longer inside a folder open in this project".to_owned())?;
            trash
                .await
                .map_err(|error| format!("could not be moved to the trash: {error:#}"))?;
            Ok(Restored::Removed)
        }
        Some(original) => {
            // One edit over the whole buffer, so it joins the undo history the way the agent's own
            // change did.
            buffer.update(cx, |buffer, cx| {
                buffer.set_text(original, cx);
            });
            project
                .update(cx, |project, cx| project.save_buffer(buffer, cx))
                .await
                .map_err(|error| format!("could not be saved: {error:#}"))?;
            Ok(Restored::Rewritten)
        }
    }
}

/// Hashed on the background for the reason `tool::buffer_text` stringifies there: a large file is
/// real work.
async fn buffer_digest(buffer: &Entity<Buffer>, cx: &mut AsyncApp) -> u64 {
    let snapshot = buffer.read_with(cx, |buffer, _| buffer.snapshot());
    cx.background_spawn(async move { digest(&snapshot.text()) })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::ToolResult;
    use fs::Fs as _;
    use serde_json::json;
    use std::{path::PathBuf, sync::Arc};

    fn checkpoint(abs_path: &str, before: Before, after: &str) -> Checkpoint {
        Checkpoint {
            abs_path: abs_path.to_owned(),
            before,
            after_digest: digest(after),
        }
    }

    async fn project_with(
        files: serde_json::Value,
        cx: &mut gpui::TestAppContext,
    ) -> (Arc<fs::FakeFs>, Entity<Project>) {
        cx.update(|cx| {
            let settings_store = settings::SettingsStore::test(cx);
            cx.set_global(settings_store);
            release_channel::init(semver::Version::new(0, 0, 0), cx);
        });

        let fs = fs::FakeFs::new(cx.executor());
        fs.insert_tree(util::path!("/project"), files).await;
        let project = Project::test(fs.clone(), [util::path!("/project").as_ref()], cx).await;
        cx.executor().run_until_parked();

        (fs, project)
    }

    /// Changes a file through a buffer and saves it, as the `write` tool does.
    async fn write(
        project: &Entity<Project>,
        relative_path: &str,
        text: &str,
        cx: &mut gpui::TestAppContext,
    ) {
        let project_path = project
            .read_with(cx, |project, cx| project.find_project_path(relative_path, cx))
            .expect("the file is in the project");
        let buffer = project
            .update(cx, |project, cx| project.open_buffer(project_path, cx))
            .await
            .expect("the file opens");
        buffer.update(cx, |buffer, cx| {
            buffer.set_text(text, cx);
        });
        project
            .update(cx, |project, cx| project.save_buffer(buffer, cx))
            .await
            .expect("the file saves");
        cx.executor().run_until_parked();
    }

    async fn run_restore(
        project: &Entity<Project>,
        plan: Vec<FileRestore>,
        cx: &mut gpui::TestAppContext,
    ) -> RestoreOutcome {
        let project = project.clone();
        let outcome = cx
            .spawn(|mut cx| async move { restore(project, plan, &mut cx).await })
            .await;
        cx.executor().run_until_parked();
        outcome
    }

    #[test]
    fn digest_is_fnv_1a() {
        // A digest that drifted would make every stored checkpoint read as changed by the user.
        assert_eq!(digest(""), 0xcbf29ce484222325);
        assert_eq!(digest("a"), 0xaf63dc4c8601ec8c);
    }

    #[test]
    fn a_plan_keeps_the_state_before_the_first_touch_and_the_digest_after_the_last() {
        let merged = plan([
            checkpoint("/project/a.rs", Before::Text("a0".into()), "a1"),
            checkpoint("/project/b.rs", Before::Missing, "b1"),
            checkpoint("/project/a.rs", Before::Text("a1".into()), "a2"),
            checkpoint("/project/b.rs", Before::Text("b1".into()), "b2"),
        ]);

        assert_eq!(
            merged,
            vec![
                FileRestore {
                    abs_path: "/project/a.rs".into(),
                    target: Before::Text("a0".into()),
                    expected_digest: digest("a2"),
                },
                FileRestore {
                    abs_path: "/project/b.rs".into(),
                    target: Before::Missing,
                    expected_digest: digest("b2"),
                },
            ]
        );
    }

    #[test]
    fn a_tool_result_stored_before_checkpoints_existed_still_loads() {
        let stored = r#"{"call_id":"c1","content":"wrote 3 lines in a.rs.","is_error":false,"path":"a.rs","diff":"@@"}"#;
        let result: ToolResult =
            serde_json::from_str(stored).expect("a thread from an earlier version must still open");
        assert_eq!(result.checkpoint, None);
        assert!(
            !serde_json::to_string(&result)
                .expect("serializes")
                .contains("checkpoint"),
            "a result with nothing to restore should not grow the stored thread"
        );

        let recorded = ToolResult {
            checkpoint: Some(checkpoint("/project/a.rs", Before::Missing, "x")),
            ..result
        };
        let reloaded: ToolResult = serde_json::from_str(
            &serde_json::to_string(&recorded).expect("serializes"),
        )
        .expect("deserializes");
        assert_eq!(reloaded.checkpoint, recorded.checkpoint);
    }

    #[test]
    fn the_summary_names_a_lone_skipped_file_and_why() {
        let outcome = RestoreOutcome {
            restored: vec!["/project/a.rs".into(), "/project/b.rs".into()],
            removed: Vec::new(),
            skipped: vec![("/project/c.rs".into(), "has unsaved edits".into())],
        };

        assert_eq!(
            outcome.summary(),
            "Restored 2 files, left c.rs alone because it has unsaved edits."
        );
        assert_eq!(
            RestoreOutcome::default().summary(),
            "There were no file changes to undo."
        );
    }

    #[gpui::test]
    async fn a_file_the_agent_edited_is_put_back_and_saved(cx: &mut gpui::TestAppContext) {
        let path = util::path!("/project/notes.txt");
        let (fs, project) = project_with(json!({ "notes.txt": "the user's text\n" }), cx).await;
        write(&project, "notes.txt", "the agent's text\n", cx).await;

        let outcome = run_restore(
            &project,
            plan([checkpoint(
                path,
                Before::Text("the user's text\n".into()),
                "the agent's text\n",
            )]),
            cx,
        )
        .await;

        assert_eq!(outcome.restored, vec![path.to_owned()]);
        assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
        assert_eq!(
            fs.load(path.as_ref()).await.expect("the file is still there"),
            "the user's text\n"
        );
    }

    #[gpui::test]
    async fn a_file_changed_after_the_agent_wrote_it_is_left_alone(cx: &mut gpui::TestAppContext) {
        let path = util::path!("/project/notes.txt");
        let (fs, project) = project_with(json!({ "notes.txt": "original\n" }), cx).await;
        write(&project, "notes.txt", "the agent's text\n", cx).await;
        write(&project, "notes.txt", "the user's newer text\n", cx).await;

        let outcome = run_restore(
            &project,
            plan([checkpoint(
                path,
                Before::Text("original\n".into()),
                "the agent's text\n",
            )]),
            cx,
        )
        .await;

        assert!(outcome.restored.is_empty(), "{:?}", outcome.restored);
        assert_eq!(
            outcome.skipped,
            vec![(
                path.to_owned(),
                "changed since the agent wrote it".to_owned()
            )]
        );
        assert_eq!(
            fs.load(path.as_ref()).await.expect("the file is still there"),
            "the user's newer text\n"
        );
    }

    #[gpui::test]
    async fn a_file_the_agent_created_goes_to_the_trash(cx: &mut gpui::TestAppContext) {
        let path = util::path!("/project/created.txt");
        let (fs, project) = project_with(
            json!({ "created.txt": "the agent's text\n", "kept.txt": "" }),
            cx,
        )
        .await;

        let outcome = run_restore(
            &project,
            plan([checkpoint(path, Before::Missing, "the agent's text\n")]),
            cx,
        )
        .await;

        assert_eq!(outcome.removed, vec![path.to_owned()]);
        assert!(outcome.skipped.is_empty(), "{:?}", outcome.skipped);
        assert!(!fs.is_file(path.as_ref()).await);
        assert_eq!(fs.trashed_paths(), vec![PathBuf::from(path)]);
        assert!(fs.is_file(util::path!("/project/kept.txt").as_ref()).await);
    }

    #[gpui::test]
    async fn a_file_with_unsaved_edits_is_left_alone(cx: &mut gpui::TestAppContext) {
        // The user is typing in the file the agent wrote and has not saved. Their text exists only
        // in the buffer, so putting the original back would destroy it with no way to recover.
        let path = util::path!("/project/notes.txt");
        let (fs, project) = project_with(json!({ "notes.txt": "original\n" }), cx).await;
        write(&project, "notes.txt", "the agent's text\n", cx).await;

        let project_path = project
            .read_with(cx, |project, cx| project.find_project_path("notes.txt", cx))
            .expect("the file is in the project");
        let buffer = project
            .update(cx, |project, cx| project.open_buffer(project_path, cx))
            .await
            .expect("the file opens");
        buffer.update(cx, |buffer, cx| {
            buffer.edit([(0..0, "unsaved ")], None, cx);
        });

        let outcome = run_restore(
            &project,
            plan([checkpoint(
                path,
                Before::Text("original\n".into()),
                "the agent's text\n",
            )]),
            cx,
        )
        .await;

        assert!(outcome.restored.is_empty(), "{:?}", outcome.restored);
        assert_eq!(
            outcome.skipped,
            vec![(path.to_owned(), "has unsaved edits".to_owned())]
        );
        assert_eq!(
            fs.load(path.as_ref()).await.expect("the file is still there"),
            "the agent's text\n"
        );
        assert_eq!(
            buffer.read_with(cx, |buffer, _| buffer.text()),
            "unsaved the agent's text\n"
        );
    }
}
