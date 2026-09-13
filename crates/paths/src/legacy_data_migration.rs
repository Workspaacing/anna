//! First-launch copy of the user data earlier releases kept under the Wu name.
//!
//! Each directory is copied into a staging directory next to its new location
//! and renamed into place only once the copy is complete. An interrupted launch
//! therefore never leaves a half-filled directory behind: the app would start
//! writing into it, later launches would see it as in use and never finish the
//! copy, and a database could end up next to another database's `-wal` file.
//! The next launch simply copies again. Resuming a partial copy would have to
//! tell a fully copied file from a truncated one, which a skip-if-exists rule
//! cannot do.
//!
//! The old directories are only ever read, and nothing that already exists in
//! a new directory is replaced: a new directory that has content is left alone,
//! and a rename never replaces a non-empty directory.

use std::ffi::OsStr;
use std::fmt::{self, Write as _};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result};

use crate::{
    APP_NAME, APP_NAME_LOWERCASE, LEGACY_APP_NAME, LEGACY_APP_NAME_LOWERCASE, custom_data_dir,
    platform_config_dir, platform_data_dir, platform_logs_dir, platform_state_dir,
    platform_temp_dir,
};

/// Written into each migrated directory. Later launches skip a directory that
/// has it.
pub const LEGACY_MIGRATION_MARKER_NAME: &str = ".migrated-from-wu";

/// Top-level data directories holding downloads the app fetches again when it
/// needs them.
///
/// They are not copied because language servers, Node.js and debug adapters can
/// add up to gigabytes, the copy runs before the first window opens, and a user
/// who kills a launch that looks frozen restarts the copy from scratch. Their
/// contents are also tied to where they were installed (npm `.bin` links,
/// version-stamped install directories), so a fresh download is the more
/// reliable result. The originals stay in the old directory.
const REDOWNLOADABLE_DATA_DIRECTORIES: &[&str] = &[
    "languages",
    "node",
    "prettier",
    "copilot",
    "debug_adapters",
    "remote_servers",
];

const RENAME_ATTEMPTS: u64 = 5;

/// What the first-launch migration did.
#[derive(Debug)]
pub enum LegacyDataMigration {
    /// `--user-data-dir` was given, so the platform directories were not looked at.
    SkippedForCustomDataDir,
    Ran(Vec<DirectoryMigrationReport>),
}

#[derive(Debug)]
pub struct DirectoryMigrationReport {
    /// What the directory holds, such as "config" or "data".
    pub kind: &'static str,
    pub legacy_dir: PathBuf,
    pub current_dir: PathBuf,
    pub outcome: DirectoryMigrationOutcome,
}

#[derive(Debug)]
pub enum DirectoryMigrationOutcome {
    /// There was nothing to copy, as on a fresh install.
    NoLegacyDirectory,
    /// An earlier launch already copied this directory.
    AlreadyMigrated,
    /// The new directory already had content that did not come from a
    /// migration, so nothing was copied into it.
    CurrentDirectoryInUse,
    Migrated(CopySummary),
    /// Nothing was copied into the new directory.
    Failed(anyhow::Error),
}

#[derive(Debug, Default)]
pub struct CopySummary {
    pub files_copied: usize,
    pub bytes_copied: u64,
    /// Paths relative to the old directory that were deliberately not copied.
    pub skipped: Vec<PathBuf>,
    /// Entries that could not be copied, with the reason.
    pub errors: Vec<String>,
}

/// Copies the directories earlier releases used into the current ones, once.
///
/// Must run before anything asks this crate for a directory, because those are
/// cached on first use, and after `--user-data-dir` has been applied. Does
/// nothing when a custom data directory is in effect.
pub fn migrate_legacy_user_data() -> LegacyDataMigration {
    if custom_data_dir().is_some() {
        return LegacyDataMigration::SkippedForCustomDataDir;
    }
    LegacyDataMigration::Ran(migrate_directories(legacy_migration_plan()))
}

struct PlannedMigration {
    kind: &'static str,
    legacy_dir: PathBuf,
    current_dir: PathBuf,
    skipped_top_level_entries: &'static [&'static str],
}

fn legacy_migration_plan() -> Vec<PlannedMigration> {
    let legacy_data_dir = platform_data_dir(LEGACY_APP_NAME, LEGACY_APP_NAME_LOWERCASE);
    let current_data_dir = platform_data_dir(APP_NAME, APP_NAME_LOWERCASE);
    vec![
        PlannedMigration {
            kind: "config",
            legacy_dir: platform_config_dir(LEGACY_APP_NAME, LEGACY_APP_NAME_LOWERCASE),
            current_dir: platform_config_dir(APP_NAME, APP_NAME_LOWERCASE),
            skipped_top_level_entries: &[],
        },
        PlannedMigration {
            kind: "data",
            legacy_dir: legacy_data_dir.clone(),
            current_dir: current_data_dir.clone(),
            skipped_top_level_entries: REDOWNLOADABLE_DATA_DIRECTORIES,
        },
        PlannedMigration {
            kind: "state",
            legacy_dir: platform_state_dir(LEGACY_APP_NAME, LEGACY_APP_NAME_LOWERCASE),
            current_dir: platform_state_dir(APP_NAME, APP_NAME_LOWERCASE),
            skipped_top_level_entries: &[],
        },
        PlannedMigration {
            kind: "cache",
            legacy_dir: platform_temp_dir(LEGACY_APP_NAME, LEGACY_APP_NAME_LOWERCASE),
            current_dir: platform_temp_dir(APP_NAME, APP_NAME_LOWERCASE),
            skipped_top_level_entries: &[],
        },
        PlannedMigration {
            kind: "logs",
            legacy_dir: platform_logs_dir(LEGACY_APP_NAME, &legacy_data_dir),
            current_dir: platform_logs_dir(APP_NAME, &current_data_dir),
            skipped_top_level_entries: &[],
        },
    ]
}

fn migrate_directories(plan: Vec<PlannedMigration>) -> Vec<DirectoryMigrationReport> {
    remove_covered_migrations(plan)
        .into_iter()
        .map(|migration| {
            let outcome = migrate_directory(
                &migration.legacy_dir,
                &migration.current_dir,
                migration.skipped_top_level_entries,
            );
            DirectoryMigrationReport {
                kind: migration.kind,
                legacy_dir: migration.legacy_dir,
                current_dir: migration.current_dir,
                outcome,
            }
        })
        .collect()
}

/// Drops migrations another migration already performs. On Windows the data,
/// state and cache directories are the same directory, and on Windows and Linux
/// the logs live inside the data directory; copying them separately would find
/// their destination already filled by the enclosing copy.
fn remove_covered_migrations(mut plan: Vec<PlannedMigration>) -> Vec<PlannedMigration> {
    // Stable, so of two identical directories the one planned first (and its
    // skip list) wins.
    plan.sort_by_key(|migration| migration.legacy_dir.components().count());
    let mut kept: Vec<PlannedMigration> = Vec::new();
    for migration in plan {
        let covered = kept.iter().any(|kept_migration| {
            match (
                migration.legacy_dir.strip_prefix(&kept_migration.legacy_dir),
                migration.current_dir.strip_prefix(&kept_migration.current_dir),
            ) {
                (Ok(legacy_relative), Ok(current_relative)) => legacy_relative == current_relative,
                _ => false,
            }
        });
        if !covered {
            kept.push(migration);
        }
    }
    kept
}

fn migrate_directory(
    legacy_dir: &Path,
    current_dir: &Path,
    skipped_top_level_entries: &[&str],
) -> DirectoryMigrationOutcome {
    match try_migrate_directory(legacy_dir, current_dir, skipped_top_level_entries) {
        Ok(outcome) => outcome,
        Err(error) => DirectoryMigrationOutcome::Failed(error),
    }
}

fn try_migrate_directory(
    legacy_dir: &Path,
    current_dir: &Path,
    skipped_top_level_entries: &[&str],
) -> Result<DirectoryMigrationOutcome> {
    if legacy_dir == current_dir {
        return Ok(DirectoryMigrationOutcome::NoLegacyDirectory);
    }

    // Follows a symlinked root (for example into a dotfiles repository) so its
    // contents are copied.
    match fs::metadata(legacy_dir) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(DirectoryMigrationOutcome::NoLegacyDirectory),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(DirectoryMigrationOutcome::NoLegacyDirectory);
        }
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", legacy_dir.display()));
        }
    }

    match current_directory_state(current_dir)? {
        CurrentDirectoryState::Missing | CurrentDirectoryState::Empty => {}
        CurrentDirectoryState::Migrated => return Ok(DirectoryMigrationOutcome::AlreadyMigrated),
        CurrentDirectoryState::InUse => {
            return Ok(DirectoryMigrationOutcome::CurrentDirectoryInUse);
        }
    }

    let parent = current_dir
        .parent()
        .with_context(|| format!("{} has no parent directory", current_dir.display()))?;
    let current_name = current_dir
        .file_name()
        .with_context(|| format!("{} has no directory name", current_dir.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let staging_dir = create_staging_dir(parent, current_name)?;

    let result = copy_tree(legacy_dir, &staging_dir, skipped_top_level_entries).and_then(
        |summary| {
            // A file that failed to copy may be missing or truncated in the staging
            // directory. Marking it migrated would keep that loss forever, so fail and
            // let the next launch copy everything again.
            if !summary.errors.is_empty() {
                anyhow::bail!(
                    "{} entries could not be copied: {}",
                    summary.errors.len(),
                    summary.errors.join("; ")
                );
            }
            write_marker(&staging_dir, legacy_dir, &summary)?;
            move_into_place(&staging_dir, current_dir)?;
            Ok(summary)
        },
    );

    match result {
        Ok(summary) => Ok(DirectoryMigrationOutcome::Migrated(summary)),
        Err(error) => {
            // The staging directory is this launch's own copy; the old directory
            // was only read.
            let error = match fs::remove_dir_all(&staging_dir) {
                Ok(()) => error,
                Err(cleanup_error) => error.context(format!(
                    "could not remove the partial copy at {}: {cleanup_error}",
                    staging_dir.display()
                )),
            };
            if let Ok(CurrentDirectoryState::Migrated) = current_directory_state(current_dir) {
                // Another launch finished the same migration first.
                return Ok(DirectoryMigrationOutcome::AlreadyMigrated);
            }
            Err(error)
        }
    }
}

#[derive(Debug, PartialEq)]
enum CurrentDirectoryState {
    Missing,
    Empty,
    Migrated,
    InUse,
}

fn current_directory_state(current_dir: &Path) -> Result<CurrentDirectoryState> {
    let metadata = match fs::symlink_metadata(current_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(CurrentDirectoryState::Missing);
        }
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", current_dir.display()));
        }
    };
    // A file or a symlink where the directory belongs is someone's deliberate
    // setup; leave it alone.
    if !metadata.is_dir() {
        return Ok(CurrentDirectoryState::InUse);
    }

    let marker_path = current_dir.join(LEGACY_MIGRATION_MARKER_NAME);
    match fs::symlink_metadata(&marker_path) {
        Ok(_) => return Ok(CurrentDirectoryState::Migrated),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("reading {}", marker_path.display()));
        }
    }

    let mut entries = fs::read_dir(current_dir)
        .with_context(|| format!("reading {}", current_dir.display()))?;
    if entries.next().is_none() {
        Ok(CurrentDirectoryState::Empty)
    } else {
        Ok(CurrentDirectoryState::InUse)
    }
}

fn create_staging_dir(parent: &Path, current_name: &OsStr) -> Result<PathBuf> {
    let process_id = std::process::id();
    for attempt in 0..100 {
        let mut staging_name = current_name.to_os_string();
        staging_name.push(format!(
            ".migrating-from-{LEGACY_APP_NAME_LOWERCASE}-{process_id}-{attempt}"
        ));
        let staging_dir = parent.join(staging_name);
        match fs::create_dir(&staging_dir) {
            Ok(()) => return Ok(staging_dir),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| format!("creating {}", staging_dir.display()));
            }
        }
    }
    anyhow::bail!(
        "no unused staging directory name next to {}",
        parent.join(current_name).display()
    )
}

fn copy_tree(
    source_root: &Path,
    destination_root: &Path,
    skipped_top_level_entries: &[&str],
) -> Result<CopySummary> {
    let mut summary = CopySummary::default();
    let mut pending_directories = vec![PathBuf::new()];
    while let Some(relative_dir) = pending_directories.pop() {
        let is_root = relative_dir.as_os_str().is_empty();
        let entries = match fs::read_dir(source_root.join(&relative_dir)) {
            Ok(entries) => entries,
            Err(error) if is_root => {
                return Err(error).with_context(|| format!("reading {}", source_root.display()));
            }
            Err(error) => {
                summary
                    .errors
                    .push(format!("{}: {error}", relative_dir.display()));
                continue;
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    summary
                        .errors
                        .push(format!("{}: {error}", relative_dir.display()));
                    continue;
                }
            };
            let relative_path = relative_dir.join(entry.file_name());
            if is_root
                && skipped_top_level_entries
                    .iter()
                    .any(|skipped| entry.file_name() == *skipped)
            {
                summary.skipped.push(relative_path);
                continue;
            }

            let destination = destination_root.join(&relative_path);
            // `DirEntry::file_type` does not follow symlinks, so a link to a
            // directory is recreated as a link instead of being descended into.
            let result = entry.file_type().and_then(|file_type| {
                if file_type.is_dir() {
                    fs::create_dir(&destination)?;
                    pending_directories.push(relative_path.clone());
                } else if file_type.is_file() {
                    summary.bytes_copied += fs::copy(entry.path(), &destination)?;
                    summary.files_copied += 1;
                } else if file_type.is_symlink() {
                    copy_symlink(&entry.path(), &destination)?;
                } else {
                    // Sockets and other special files (like the CLI socket) cannot be
                    // copied; the app recreates the ones it needs.
                    summary.skipped.push(relative_path.clone());
                }
                Ok(())
            });
            if let Err(error) = result {
                summary
                    .errors
                    .push(format!("{}: {error}", relative_path.display()));
            }
        }
    }
    Ok(summary)
}

fn copy_symlink(source: &Path, destination: &Path) -> io::Result<()> {
    let target = fs::read_link(source)?;
    #[cfg(unix)]
    return std::os::unix::fs::symlink(&target, destination);
    #[cfg(windows)]
    return if fs::metadata(source).is_ok_and(|metadata| metadata.is_dir()) {
        std::os::windows::fs::symlink_dir(&target, destination)
    } else {
        std::os::windows::fs::symlink_file(&target, destination)
    };
    #[cfg(not(any(unix, windows)))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!(
            "cannot recreate the symbolic link to {} on this platform",
            target.display()
        ),
    ));
}

fn write_marker(staging_dir: &Path, legacy_dir: &Path, summary: &CopySummary) -> Result<()> {
    let copied_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since_epoch| since_epoch.as_secs());
    let mut contents = format!(
        "{APP_NAME} copied this directory from {LEGACY_APP_NAME} on its first launch. \
         The {LEGACY_APP_NAME} directory was left in place as a backup.\n\
         Copied from: {}\n\
         Copied at (Unix time): {copied_at}\n\
         Files copied: {}\n\
         Bytes copied: {}\n",
        legacy_dir.display(),
        summary.files_copied,
        summary.bytes_copied,
    );
    for skipped in &summary.skipped {
        writeln!(contents, "Not copied: {}", skipped.display())?;
    }
    for error in &summary.errors {
        writeln!(contents, "Could not copy: {error}")?;
    }
    let marker_path = staging_dir.join(LEGACY_MIGRATION_MARKER_NAME);
    fs::write(&marker_path, contents).with_context(|| format!("writing {}", marker_path.display()))
}

fn move_into_place(staging_dir: &Path, current_dir: &Path) -> Result<()> {
    // Windows refuses to rename onto an existing directory, even an empty one.
    // `remove_dir` only removes a directory that is still empty.
    if current_directory_state(current_dir)? == CurrentDirectoryState::Empty {
        fs::remove_dir(current_dir)
            .with_context(|| format!("removing the empty {}", current_dir.display()))?;
    }

    let mut attempt = 1;
    loop {
        match fs::rename(staging_dir, current_dir) {
            Ok(()) => return Ok(()),
            Err(error) => {
                // Antivirus and indexing software briefly hold files that were just
                // written, which makes renaming their directory fail on Windows. A
                // destination that appeared in the meantime is never retried.
                let destination_missing =
                    matches!(current_directory_state(current_dir), Ok(CurrentDirectoryState::Missing));
                if attempt >= RENAME_ATTEMPTS || !destination_missing {
                    return Err(error).with_context(|| {
                        format!(
                            "moving {} to {}",
                            staging_dir.display(),
                            current_dir.display()
                        )
                    });
                }
                std::thread::sleep(Duration::from_millis(200 * attempt));
                attempt += 1;
            }
        }
    }
}

impl fmt::Display for DirectoryMigrationReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let kind = self.kind;
        let legacy_dir = self.legacy_dir.display();
        let current_dir = self.current_dir.display();
        match &self.outcome {
            DirectoryMigrationOutcome::NoLegacyDirectory => write!(
                formatter,
                "{kind} directory: no {LEGACY_APP_NAME} directory at {legacy_dir} to migrate"
            ),
            DirectoryMigrationOutcome::AlreadyMigrated => write!(
                formatter,
                "{kind} directory: {current_dir} was already migrated from {legacy_dir}"
            ),
            DirectoryMigrationOutcome::CurrentDirectoryInUse => write!(
                formatter,
                "{kind} directory: not migrating {legacy_dir} because {current_dir} already has \
                 content; the {LEGACY_APP_NAME} data is still in {legacy_dir}"
            ),
            DirectoryMigrationOutcome::Migrated(summary) => {
                write!(
                    formatter,
                    "{kind} directory: copied {} files ({} bytes) from {legacy_dir} to {current_dir}",
                    summary.files_copied, summary.bytes_copied
                )?;
                if !summary.skipped.is_empty() {
                    let skipped = summary
                        .skipped
                        .iter()
                        .map(|path| path.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    write!(formatter, "; not copied: {skipped}")?;
                }
                if !summary.errors.is_empty() {
                    write!(
                        formatter,
                        "; could not copy: {}",
                        summary.errors.join("; ")
                    )?;
                }
                Ok(())
            }
            DirectoryMigrationOutcome::Failed(error) => write!(
                formatter,
                "{kind} directory: migrating {legacy_dir} to {current_dir} failed, nothing was \
                 copied and the {LEGACY_APP_NAME} data is still in {legacy_dir}: {error:#}"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn write_file(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// Every file under `root` and its contents, keyed by its Unix-style path
    /// relative to `root`.
    fn read_tree(root: &Path) -> BTreeMap<String, String> {
        let mut files = BTreeMap::new();
        let mut pending_directories = vec![root.to_path_buf()];
        while let Some(directory) = pending_directories.pop() {
            for entry in fs::read_dir(&directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending_directories.push(path);
                } else {
                    let relative_path = path
                        .strip_prefix(root)
                        .unwrap()
                        .components()
                        .map(|component| component.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/");
                    files.insert(relative_path, fs::read_to_string(&path).unwrap());
                }
            }
        }
        files
    }

    fn populate_legacy_data_dir(legacy_dir: &Path) {
        for (path, contents) in [
            ("settings.json", r#"{ "theme": "One Dark" }"#),
            ("keymap.json", "[]"),
            ("AGENTS.md", "Be brief."),
            ("themes/custom.json", "{}"),
            ("db/0-stable/db.sqlite", "database"),
            ("db/0-stable/db.sqlite-wal", "write-ahead log"),
            ("extensions/installed/html/extension.toml", "id = \"html\""),
            ("logs/Wu.log", "old log"),
            ("languages/rust-analyzer/rust-analyzer", "binary"),
            ("node/bin/node", "binary"),
        ] {
            write_file(&legacy_dir.join(path), contents);
        }
        fs::create_dir_all(legacy_dir.join("snippets")).unwrap();
    }

    fn directory_names(directory: &Path) -> Vec<String> {
        let mut names = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    #[test]
    fn test_fresh_install_has_nothing_to_migrate() {
        let temp_dir = tempfile::tempdir().unwrap();
        let current_dir = temp_dir.path().join("Anna");

        let outcome = migrate_directory(
            &temp_dir.path().join("Wu"),
            &current_dir,
            REDOWNLOADABLE_DATA_DIRECTORIES,
        );

        assert!(
            matches!(outcome, DirectoryMigrationOutcome::NoLegacyDirectory),
            "{outcome:?}"
        );
        assert!(!current_dir.exists());
    }

    #[test]
    fn test_migration_copies_user_data_and_leaves_the_old_directory_untouched() {
        let temp_dir = tempfile::tempdir().unwrap();
        let legacy_dir = temp_dir.path().join("Wu");
        let current_dir = temp_dir.path().join("Anna");
        populate_legacy_data_dir(&legacy_dir);
        let legacy_files = read_tree(&legacy_dir);

        let outcome = migrate_directory(&legacy_dir, &current_dir, REDOWNLOADABLE_DATA_DIRECTORIES);

        let DirectoryMigrationOutcome::Migrated(mut summary) = outcome else {
            panic!("expected a migration, got {outcome:?}");
        };
        assert!(summary.errors.is_empty(), "{:?}", summary.errors);
        summary.skipped.sort();
        assert_eq!(
            summary.skipped,
            vec![PathBuf::from("languages"), PathBuf::from("node")]
        );
        assert_eq!(read_tree(&legacy_dir), legacy_files);

        let mut migrated_files = read_tree(&current_dir);
        let marker = migrated_files
            .remove(LEGACY_MIGRATION_MARKER_NAME)
            .expect("the migrated directory should have a marker");
        assert!(marker.contains(&legacy_dir.display().to_string()), "{marker}");
        let expected_files = legacy_files
            .into_iter()
            .filter(|(path, _)| !path.starts_with("languages/") && !path.starts_with("node/"))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(migrated_files, expected_files);
        assert_eq!(summary.files_copied, expected_files.len());
        assert!(current_dir.join("snippets").is_dir());
        assert_eq!(
            directory_names(temp_dir.path()),
            vec!["Anna".to_string(), "Wu".to_string()],
            "no staging directory should be left behind"
        );
    }

    #[test]
    fn test_migration_never_writes_into_a_directory_that_already_has_content() {
        let temp_dir = tempfile::tempdir().unwrap();
        let legacy_dir = temp_dir.path().join("Wu");
        let current_dir = temp_dir.path().join("Anna");
        populate_legacy_data_dir(&legacy_dir);
        write_file(&current_dir.join("settings.json"), "Anna's own settings");

        let outcome = migrate_directory(&legacy_dir, &current_dir, REDOWNLOADABLE_DATA_DIRECTORIES);

        assert!(
            matches!(outcome, DirectoryMigrationOutcome::CurrentDirectoryInUse),
            "{outcome:?}"
        );
        assert_eq!(
            read_tree(&current_dir),
            BTreeMap::from([(
                "settings.json".to_string(),
                "Anna's own settings".to_string()
            )])
        );
        assert_eq!(
            directory_names(temp_dir.path()),
            vec!["Anna".to_string(), "Wu".to_string()],
            "no staging directory should be left behind"
        );
    }

    #[test]
    fn test_migration_fills_an_existing_empty_directory() {
        let temp_dir = tempfile::tempdir().unwrap();
        let legacy_dir = temp_dir.path().join("Wu");
        let current_dir = temp_dir.path().join("Anna");
        populate_legacy_data_dir(&legacy_dir);
        fs::create_dir_all(&current_dir).unwrap();

        let outcome = migrate_directory(&legacy_dir, &current_dir, &[]);

        assert!(
            matches!(outcome, DirectoryMigrationOutcome::Migrated(_)),
            "{outcome:?}"
        );
        assert_eq!(
            fs::read_to_string(current_dir.join("settings.json")).unwrap(),
            r#"{ "theme": "One Dark" }"#
        );
    }

    #[test]
    fn test_second_run_is_a_no_op() {
        let temp_dir = tempfile::tempdir().unwrap();
        let legacy_dir = temp_dir.path().join("Wu");
        let current_dir = temp_dir.path().join("Anna");
        populate_legacy_data_dir(&legacy_dir);
        let first_outcome = migrate_directory(&legacy_dir, &current_dir, &[]);
        assert!(
            matches!(first_outcome, DirectoryMigrationOutcome::Migrated(_)),
            "{first_outcome:?}"
        );

        write_file(&current_dir.join("settings.json"), "changed in Anna");
        write_file(&legacy_dir.join("settings.json"), "changed in Wu");
        write_file(&legacy_dir.join("tasks.json"), "created in Wu");
        let migrated_files = read_tree(&current_dir);

        let second_outcome = migrate_directory(&legacy_dir, &current_dir, &[]);

        assert!(
            matches!(second_outcome, DirectoryMigrationOutcome::AlreadyMigrated),
            "{second_outcome:?}"
        );
        assert_eq!(read_tree(&current_dir), migrated_files);
    }

    #[test]
    fn test_an_interrupted_copy_is_redone_and_its_leftovers_are_kept() {
        let temp_dir = tempfile::tempdir().unwrap();
        let legacy_dir = temp_dir.path().join("Wu");
        let current_dir = temp_dir.path().join("Anna");
        populate_legacy_data_dir(&legacy_dir);
        // A launch killed mid-copy leaves its staging directory and no new directory.
        let stale_staging_dir = temp_dir.path().join("Anna.migrating-from-wu-1-0");
        write_file(&stale_staging_dir.join("settings.json"), "partial");

        let outcome = migrate_directory(&legacy_dir, &current_dir, &[]);

        assert!(
            matches!(outcome, DirectoryMigrationOutcome::Migrated(_)),
            "{outcome:?}"
        );
        assert_eq!(
            fs::read_to_string(current_dir.join("db/0-stable/db.sqlite-wal")).unwrap(),
            "write-ahead log"
        );
        assert_eq!(
            read_tree(&stale_staging_dir),
            BTreeMap::from([("settings.json".to_string(), "partial".to_string())])
        );
    }

    #[cfg(unix)]
    #[test]
    fn test_symlinks_are_recreated_instead_of_followed() {
        let temp_dir = tempfile::tempdir().unwrap();
        let legacy_dir = temp_dir.path().join("wu");
        let current_dir = temp_dir.path().join("anna");
        populate_legacy_data_dir(&legacy_dir);
        std::os::unix::fs::symlink("settings.json", legacy_dir.join("linked.json")).unwrap();

        let outcome = migrate_directory(&legacy_dir, &current_dir, &[]);

        assert!(
            matches!(outcome, DirectoryMigrationOutcome::Migrated(_)),
            "{outcome:?}"
        );
        assert_eq!(
            fs::read_link(current_dir.join("linked.json")).unwrap(),
            PathBuf::from("settings.json")
        );
    }

    #[test]
    fn test_directories_inside_another_migrated_directory_are_not_migrated_twice() {
        let planned = |kind, legacy_dir: &str, current_dir: &str, skipped| PlannedMigration {
            kind,
            legacy_dir: PathBuf::from(legacy_dir),
            current_dir: PathBuf::from(current_dir),
            skipped_top_level_entries: skipped,
        };
        let plan = vec![
            planned("config", "/roaming/Wu", "/roaming/Anna", &[]),
            planned(
                "data",
                "/local/Wu",
                "/local/Anna",
                REDOWNLOADABLE_DATA_DIRECTORIES,
            ),
            planned("state", "/local/Wu", "/local/Anna", &[]),
            planned("cache", "/local/Wu", "/local/Anna", &[]),
            planned("logs", "/local/Wu/logs", "/local/Anna/logs", &[]),
        ];

        let kept = remove_covered_migrations(plan);

        assert_eq!(
            kept.iter().map(|migration| migration.kind).collect::<Vec<_>>(),
            vec!["config", "data"]
        );
        assert_eq!(
            kept[1].skipped_top_level_entries,
            REDOWNLOADABLE_DATA_DIRECTORIES
        );
    }

    #[test]
    fn test_custom_user_data_dir_skips_the_migration() {
        let temp_dir = tempfile::tempdir().unwrap();
        let custom_dir = temp_dir.path().join("custom");
        crate::set_custom_data_dir(custom_dir.to_str().unwrap())
            .expect("the custom data directory must be set, or the real directories would be migrated");
        assert!(crate::custom_data_dir().is_some());

        assert!(matches!(
            migrate_legacy_user_data(),
            LegacyDataMigration::SkippedForCustomDataDir
        ));
    }
}
