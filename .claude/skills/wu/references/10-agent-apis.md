# Agent-facing APIs: editing, diagnostics, processes

Verified against this repo on 2026-09-11 by reading the source, not from Zed memory. Every entry
carries a `path:line`. These are the APIs `crates/cowork` is built on; they are where a tool that
touches the user's project has to go.

**Read `02-fork-delta.md` first.** Several of these diverge from Zed in ways that produce
code which looks right and does not compile.

## Async context shapes

| Call | Returns |
| --- | --- |
| `Entity::update(cx, f)` on `AsyncApp` | the closure's value, **not** `Result` |
| `Entity::read_with(cx, f)` on `AsyncApp` | the closure's value, **not** `Result` |
| `AsyncApp::update(f)` | the closure's value, **not** `Result` |
| `WeakEntity::update(cx, f)` | `Result<R>` — this is the one that can fail |

`Context::listener` / `Context::processor` take `&self`. `Context::spawn` is inherent;
`background_spawn` and `new` come from the `AppContext` trait, re-exported by `gpui::prelude` and
therefore by `ui::prelude::*`. `detach_and_log_err` comes from `gpui::TaskExt`.
`AsyncApp::subscribe` returns a bare `Subscription` (`crates/gpui/src/app/async_context.rs:173`).

## Editing a file

```rust
Project::open_buffer(path: impl Into<ProjectPath>, cx: &mut App) -> Task<Result<Entity<Buffer>>>
```
`crates/project/src/project.rs:2407`. Note `&mut App`, not `&mut Context<Self>`.
`open_buffer_with_lsp` (`:2422`) is `#[cfg(feature = "test-support")]` — unusable in a normal build.

```rust
Buffer::edit<I, S, T>(edits: I, autoindent: Option<AutoindentMode>, cx: &mut Context<Self>)
    -> Option<clock::Lamport>
where I: IntoIterator<Item = (Range<S>, T)>, S: ToOffset, T: Into<Arc<str>>
```
`crates/language/src/buffer.rs:2749`. **Plain `usize` byte offsets work** —
`impl ToOffset for usize` at `crates/text/src/text.rs:3418`. The newtype offsets
(`MultiBufferOffset`, `crates/multi_buffer/src/multi_buffer.rs:225`) are MultiBuffer-only.

```rust
Buffer::set_text<T: Into<Arc<str>>>(text: T, cx: &mut Context<Self>) -> Option<clock::Lamport>
```
`crates/language/src/buffer.rs:2723` — clears autoindent requests, then edits `0..len`.

```rust
Project::save_buffer(&self, buffer: Entity<Buffer>, cx: &mut Context<Project>) -> Task<Result<()>>
```
`crates/project/src/project.rs:2568`.

### Creating a file that does not exist

```rust
Project::create_entry(path: impl Into<ProjectPath>, is_directory: bool, cx: &mut Context<Self>)
    -> Task<Result<CreatedEntry>>
```
`crates/project/src/project.rs:2003`. **It takes no content** — it forwards `None` to
`Worktree::create_entry` (`crates/worktree/src/worktree.rs:957`), whose local arm calls
`fs.write(...)` at `:1793`. `RealFs::write` creates the parent directories itself
(`crates/fs/src/fs.rs:990-993`), so a nested path needs no preparation.

### Resolving a path the model supplied

```rust
Project::find_project_path(&self, path: impl AsRef<Path>, cx: &App) -> Option<ProjectPath>
```
`crates/project/src/project.rs:4190`. **It returns `None` for a relative path that does not exist
yet**, unless the path is prefixed with a worktree's root name (second pass, `:4234-4245`). A tool
that creates files needs its own fallback — see `resolve_for_create` in `crates/cowork/src/tool.rs`,
which resolves against the single visible worktree and refuses to guess when there are several.

`RelPath` lives in `crates/path/src/rel_path.rs` (not `crates/util`). Useful:
`file_name() -> Option<&str>` (`:153`), `as_unix_str() -> &str` (`:269`),
`as_std_path() -> &Path` (`:279`), `display(PathStyle) -> Cow<str>` (`:258`).
`Worktree::abs_path() -> Arc<Path>` at `crates/worktree/src/worktree.rs:864`.

## Diagnostics

```rust
BufferSnapshot::diagnostics_in_range<'a, T: ToOffset, O: FromAnchor>(
    &'a self, search_range: Range<T>, reversed: bool,
) -> impl 'a + Iterator<Item = DiagnosticEntryRef<'a, O>>
```
`crates/language/src/buffer.rs:5167`. **The turbofish is mandatory** — `O` is not inferable.
`diagnostics_in_range::<usize, Point>(0..snapshot.len(), false)` gives row/column locations.
`impl FromAnchor` exists for `Anchor`, `Point`, `PointUtf16`, `usize`
(`crates/text/src/text.rs:3553-3574`).

Two traps in `Diagnostic` (`crates/language/src/diagnostic.rs:240`):

- `severity` is **`lsp::DiagnosticSeverity`**, not the settings enum
  `project::DiagnosticSeverity` (`crates/project/src/project_settings.rs:181`). `language` does
  **not** re-export it; depend on `lsp` and use `lsp::DiagnosticSeverity::{ERROR, WARNING}`.
- `message` is **not a `String`** — it is `DiagnosticMessage`
  (`crates/language/src/diagnostic.rs:13`). Use `.as_str()`.

There is **no `Buffer::diagnostics`**; the buffer-level accessor is
`Buffer::buffer_diagnostics(for_server: Option<LanguageServerId>) -> Vec<&DiagnosticEntry<Anchor>>`
(`crates/language/src/buffer.rs:1984`).

Diagnostics arrive **asynchronously after an edit**, so reading them immediately reports the state
before the change. Wait for `BufferEvent::DiagnosticsUpdated` (`crates/language/src/buffer.rs:340`,
emitted at `:3262`) with a timeout — and skip the wait entirely when
`buffer.language().is_none()`, since nothing will ever analyse it. Coarser signals:
`project::Event::DiagnosticsUpdated` (`crates/project/src/project.rs:319`) and
`project::Event::DiskBasedDiagnosticsFinished` (`:316`).

Nothing in Anna formats diagnostics as plain text for a non-UI consumer. The only
string-producing code is `crates/diagnostics/src/diagnostic_renderer.rs:87`, which is private and
emits Markdown with `file://#diagnostic-…` anchors.

## Formatting

```rust
Project::format(
    buffers: HashSet<Entity<Buffer>>, target: LspFormatTarget, push_to_history: bool,
    trigger: lsp_store::FormatTrigger, cx: &mut Context<Project>,
) -> Task<Result<ProjectTransaction>>
```
`crates/project/src/project.rs:3319`. `FormatTrigger { Save, Manual }` at
`crates/project/src/lsp_store.rs:236`; `LspFormatTarget { Buffers, Ranges(..) }` at `:241`.

`format_with_prettier` is `pub(super)` (`crates/project/src/prettier_store.rs:736`) — go through
`Project::format`. Known gap: the external-formatter path **silently skips range formatting**
(`crates/project/src/lsp_store.rs:1884`), so "format selection" is a no-op for externally formatted
languages.

## Running a process

**Do not call `smol::process::Command::new` directly.** `clippy.toml` bans
`std::process::Command::{spawn,output,status,stdin,stdout,stderr}`, and `crates/git/clippy.toml:19`
additionally bans `smol::process::Command::new` inside that crate.

- `util::command::new_command(program) -> util::command::Command` —
  `crates/util/src/command.rs:16`. Adds `CREATE_NO_WINDOW` on Windows. Has
  `output()` (`:117`), `spawn()` (`:113`), `status()` (`:121`), `kill_on_drop()` (`:108`).
- `util::command::new_std_command(program) -> std::process::Command` — re-exported at
  `crates/util/src/command.rs:14` from `crates/gpui_util/src/lib.rs:21`.
- **`util::process::Child::spawn(command: std::process::Command, stdin, stdout, stderr)`** —
  `crates/util/src/process.rs:34` (unix, `setsid`) / `:57` (windows, job object). Use this when
  descendants must be reaped: on Windows it assigns the child to a
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` job, on unix it kills the process group.
  `output(self)` at `:106`, `kill(&mut self)` at `:115`/`:124`. It `Deref`s to
  `smol::process::Child`, so `child.stdout.take()` works.

**Never write all of stdin before reading stdout** — that deadlocks once the payload outgrows the
pipe buffer. `futures::future::join3(write, read_out, read_err)`.

### Shells

`crates/util/src/shell.rs`: `Shell { System, Program, WithArguments }` (`:10`),
`ShellKind` (`:54`, 11 variants), `get_system_shell()` (`:71`).
`ShellBuilder` (`crates/util/src/shell_builder.rs:8`): `new(&Shell, is_windows)` (`:20`),
`non_interactive()` (`:36`), `redirect_stdin_to_dev_null()` (`:71`),
`build_smol_command(Option<String>, &[String])` (`:192`), `build_std_command(..)` (`:204`).
With **empty args the command string is passed verbatim**, which is what you want for a raw command
line. Prefer these over hand-assembling, because they handle `cmd.exe`'s quoting.

Do not confuse `util::shell::Shell` with the settings enum at
`crates/settings_content/src/terminal.rs:225`.

### Project environment

There is no `Project::exec`, no `EnvironmentStore`, no `get_directory_environment`.
`Project::environment() -> &Entity<ProjectEnvironment>` (`crates/project/src/project.rs:1700`);
`ProjectEnvironment::directory_environment(Arc<Path>, &mut App) -> Shared<Task<Option<HashMap<..>>>>`
(`crates/project/src/environment.rs:141`). Memoized and `Shared`, so repeated calls are cheap.

`Project::exec_in_shell(command: String, cx) -> Task<Result<smol::process::Command>>`
(`crates/project/src/terminals.rs:517`) does everything you would want — resolves the directory
environment, applies `settings.env`, handles the remote case — and returns an **unspawned** command.
It currently has **zero callers**.

Under `test-support`, `get_cli_environment` returns an empty map
(`crates/project/src/environment.rs:71`), so tests get no PATH from the project environment.

### Timeouts

`smol::Timer::after` is banned. Use:

```rust
gpui::FutureExt::with_timeout(self, timeout: Duration, executor: &BackgroundExecutor)
    -> WithTimeout<Self>   // Output = Result<T::Output, Timeout>
```
`crates/gpui/src/util.rs:64`, re-exported at `crates/gpui/src/gpui.rs:165`. Underneath:
`BackgroundExecutor::timer(Duration) -> Task<()>` (`crates/gpui/src/executor.rs:187`).

## Terminals the user can see

`TerminalKind` does not exist here. The equivalent is `terminal::TerminalMode`
(`crates/terminal/src/terminal.rs:935`) with `interactive()` (`:948`),
`interactive_with_completion()` (`:953`), `task(SpawnInTerminal)` (`:958`).

There is no `Terminal::spawn`. Entry points, highest first:

- `Workspace::spawn_in_terminal(SpawnInTerminal, &mut Window, cx) -> Task<Option<Result<ExitStatus>>>`
  — `crates/workspace/src/tasks.rs:222`. Returns `Task::ready(None)` with no `TerminalProvider`.
- `TerminalPanel::spawn_task(&SpawnInTerminal, window, cx)` — `crates/terminal_view/src/terminal_panel.rs:610`.
- **`Project::create_terminal_task(SpawnInTerminal, cx) -> Task<Result<Entity<Terminal>>>`** —
  `crates/project/src/terminals.rs:64`. Needs **no `Window`**; this is the one for an async path.
- `Terminal::wait_for_completed_task(&App) -> Task<Option<ExitStatus>>` — `crates/terminal/src/terminal.rs:3158`.

`SpawnInTerminal` (`crates/task/src/task.rs:42`) derives `Default`.

Headless terminal grids are 100x6 and cannot be resized without a `Window`, so long lines hard-wrap
— **a `.output()` capture is more faithful than a headless terminal** for anything a model reads.

## Crate edges

`tooling/xtask/src/workspace.rs:23` lists 12 forbidden dependency edges, enforced by
`cargo test -p xtask`. None of them involve `cowork`. Dev-dependencies are exempt.
