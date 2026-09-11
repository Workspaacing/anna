---
name: wu
description: Working knowledge of the Wu editor codebase (a Rust/GPUI fork of Zed, ~983k LOC across 163 crates). Load before writing, reviewing, or navigating any code in this repo - it documents the build/lint gates, the fork's divergences from upstream Zed (which will otherwise produce non-compiling code), crate ownership, step-by-step recipes for common changes, test harness usage, and the footguns. Use for any task touching crates/, extensions/, assets/, script/ or tooling/.
---

# Wu codebase

Wu is a native code editor in Rust: a fork of [Zed](https://github.com/zed-industries/zed) that keeps
the editor core and drops collaboration, accounts and telemetry. Repo `Workspaacing/wu`, docs at
`wu.farshed.me`, version `1.0.6`, upstream pin in `UPSTREAM_VERSION` (`01acd0ee...`).

- **983k LOC / 1213 `.rs` files / 163 crates**, edition **2024**, toolchain pinned to **1.97.1**.
- Extra targets: `wasm32-wasip2` (extensions), `wasm32-unknown-unknown` (gpui web), `x86_64-unknown-linux-musl` (remote server).
- Only two release channels exist: **`Dev`** and **`Stable`**. No Preview, no Nightly.
- **This working copy is not a git repository** (no `.git`). Anything that shells out to git —
  `script/check-keymaps`, `script/upstream-sync`, build-script SHA lookup — will not work until `git init`.

## 0. Read this first: your Zed knowledge is partly wrong

This fork restructured several core APIs. Writing from upstream-Zed memory produces code that does not
compile. The high-frequency ones:

| You remember | Reality in Wu |
|---|---|
| `Task` from `gpui` | re-exported from the **`scheduler`** crate |
| `impl_actions!` / `impl_internal_actions!` | **deleted** — use `actions!` or `#[derive(Action)]` |
| `NoAction`, `Unbind` | live in the **`wu`** namespace |
| `cx.spawn(|cx| async move {...})` | takes an `AsyncFnOnce`: `cx.spawn(async move |this, cx| ...)` |
| app starts via `gpui::Application::new()` | `gpui_platform::application()` |
| `ExcerptId` in multibuffer | **does not exist.** `multi_buffer::Anchor` is `Min \| Excerpt(ExcerptAnchor) \| Max`, keyed by `PathKey` + `text::Anchor`. Excerpts are declarative: `set_excerpts_for_path` replaces all excerpts for a path |
| multibuffer offsets are `usize` | newtypes: `MultiBufferOffset(usize)` vs `BufferOffset(usize)` (+ UTF-16 variants) |
| `SettingsSources<T>` layering | **gone.** Layering happens on `SettingsContent` via `MergeFrom` before typed structs exist |
| `Settings::register(cx)` | dead code — use `#[derive(settings::RegisterSetting)]` (inventory-based) |
| `db::define_connection!` | **does not exist** — `db::static_connection!` against one shared `AppDatabase` |
| language configs in `crates/languages/` | `.scm` queries and `config.toml` live in **`crates/grammars/src/<lang>/`** |
| `ReleaseChannel::{Preview,Nightly}` | only `Dev` and `Stable` |
| blade / `gpui/wgpu` feature | Blade is gone; there is no `gpui/wgpu` feature |

New in this fork: a **`View` trait** (`crates/gpui/src/view.rs:182`). `#[derive(IntoElement)]` emits
`ViewElement<Self>`, and `Entity<T: Render>` implements `IntoElement` directly, so `.child(entity)`
works. Two views over the same entity must not be siblings.

## 1. Non-negotiable rules

From `.rules` (loaded via `CLAUDE.md`/`AGENTS.md`) plus what the lints actually enforce:

1. **Correctness and clarity over speed.**
2. **No summarizing comments.** Comment only non-obvious *why*.
3. **No `unwrap()`**, no panicking indexing. Propagate with `?`.
4. **Never `let _ =` on a fallible operation.** Use `?`, `.log_err()`, `.warn_on_err()`, or explicit `match`.
5. **Never create `mod.rs`.** Use `src/some_module.rs`. New crates declare `[lib] path = "src/<name>.rs"`.
6. **Full words in identifiers** — no `q` for `queue`.
7. **Shadow clones into async blocks** (`let x = x.clone();` inside the `spawn` block expression).
8. **Prefer existing files** over many small new ones. No creative additions beyond what was asked.
9. **Async failures must reach the UI layer** so users get feedback.
10. **`./script/clippy`, never `cargo clippy`.**
11. **HARD RULE from `.rules`:** when modifying any *source* file, first prepend to `README.md`:
    ```
    > [!IMPORTANT]
    > Remove this line to confirm you've reviewed this PR before submitting.
    ```
    Never remove those lines yourself — removal is the human author's manual confirmation step.
12. **Do not edit `.rules` during feature work.** Propose additions under a
    `Suggested .rules additions` heading in the PR description instead.

### PR hygiene
Imperative, capitalized title; no `fix:`/`feat:` prefixes; no trailing punctuation; optional
`crate_name: ` prefix. Body ends with:
```
Release Notes:

- Added ... / - Fixed ... / - Improved ...   (or `- N/A` for non-user-facing)
```

## 2. Commands

```bash
./script/clippy                      # the lint gate (bash)
```
```powershell
pwsh script/clippy.ps1 -p <crate>    # Windows equivalent
```

- Both run `cargo clippy [args] --workspace --release --all-targets --all-features -- --deny warnings`.
  The `.ps1` **skips** `cargo shear`, `typos` and `buf` that the bash version runs.
- **`cargo test` with no args only builds `crates/wu`** (`default-members`). Always `-p <crate>` or `--workspace`.
- `cargo test -p project --test integration` — `fs`, `project`, `worktree` set `[lib] test = false` and use a single integration binary.
- `cargo test -p xtask` — **hidden gate**: fails if any of 12 forbidden crate edges exist transitively
  (`picker → editor`, `project_panel → git_ui`, `search → project_panel`, ...). Fix by extracting a
  lower-level crate, never by merging.
- `cargo run --profile release-fast` — the day-to-day run profile (`dev` has `debug = 0`; use `--profile dbg` for a debugger).
- `cargo dylint --all -- -p <crate>` — the 6 custom lints (needs `nightly-2026-03-21`, pinned in `tooling/lints`).
- `script/new-crate <name> [apache]` — scaffolds a crate. You still add it to `members` and `[workspace.dependencies]` by hand.
- `.cargo/config.toml` caps `jobs = 4`.

### Build prerequisites on Windows (verified 2026-09-11)
1. **MSVC Build Tools 2022 + Windows SDK.** `link.exe` does not need to be on `PATH`; rustc finds it
   through vswhere. Check with
   `& "C:\Program Files (x86)\Microsoft Visual Studio\Installerswhere.exe" -products *`.
2. **rustup.** Do **not** use `script/install-rustup.ps1` blindly — it downloads the
   `i686-pc-windows-gnu` installer, which leaves the machine defaulting to a 32-bit GNU host. Fetch
   `https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe` and pass
   `--default-host x86_64-pc-windows-msvc`. `rustup show` inside the repo then installs 1.97.1 plus
   all four targets from `rust-toolchain.toml`.
3. **CMake — mandatory for the whole workspace, and undocumented.** `tree-sitter` pulls in
   `wasmtime-c-api-impl`, whose build script shells out to `cmake` and panics with
   `failed to spawn 'cmake': program not found` otherwise. This blocks nearly every crate, including
   pure-logic ones, because `language` is so widely depended on. `script/install-cmake` covers only
   macOS/Linux and exits with an error on Windows. Install with
   `winget install --id Kitware.CMake --exact` and put `C:\Program Files\CMakein` on `PATH`.

### CI reality
There is **no PR/push CI**. Only `.github/workflows/release.yml` (fires on `v*` tags, hard-fails unless
the tag matches `version` in `crates/wu/Cargo.toml`) and `upstream-sync.yml` (nightly cron). Local
`./script/clippy` + human review are the only gates — so run them.

### Lints that will bite
Denied clippy lints: `dbg_macro`, `todo`, `redundant_clone`, `disallowed_methods`,
`declare_interior_mutable_const`. The whole `style` group is deliberately **allowed** — do not fix style nits.

`clippy.toml` disallowed methods: `std::process::Command::{spawn,output,status,stdin,stdout,stderr}`
(use `smol::process::Command`), `smol::Timer::after` (use `gpui::BackgroundExecutor::timer`),
`serde_json[_lenient]::from_reader` (use `from_slice`), cocoa `NSString::alloc` (use `ns_string()`).

Six custom dylints in `tooling/lints/` — `cargo check` will **not** catch these:
`shared_string_from_str_literal` (use `SharedString::new_static`), `async_block_without_await`,
`entity_update_in_render`, `notify_in_render`, `owned_string_into_shared`, and
**`blocking_io_on_foreground`** (forbids ~60 std calls, including plain `Path::exists()`, inside any fn
taking `&App` / `&Context<T>` / `&mut Window`, or any `render` method).

## 3. Never do X — do Y

| Never | Instead |
|---|---|
| `std::fs` / `tokio::fs` in app code | the `fs::Fs` trait via `<dyn Fs>::global(cx)` (breaks `FakeFs` tests, SSH/WSL remotes, trash/atomic-write/UNC handling) |
| `use std::collections::{HashMap, HashSet}` | `use collections::{HashMap, HashSet}` (Fx-hashed; 284 call sites vs 17 stragglers) |
| `reqwest` directly | `cx.http_client()` |
| `println!` / `dbg!` | `log::info!` etc. — `zlog` *is* the `log::Log` impl; `zlog::scoped!` for scoped loggers |
| `unwrap()` | `?` + `anyhow::Context`, `.log_err()`, `.warn_on_err()`, `.debug_assert_ok("reason")` |
| `let _ = task` | `task.detach_and_log_err(cx)` or store it in a field |
| `smol::Timer::after` in tests | `cx.background_executor().timer(d).await` |
| hardcoded colors | `Color::*` enum or `cx.theme().colors()` / `.status()` |
| hand-rolled UI primitives | `crates/ui` components (`Button`, `Label`, `ListItem`, `Modal`, ...) |
| `std::collections`-style `Command` | `smol::process::Command` |
| a bare `zed` grep | `\bzed\b` — `Serialized`/`normalized`/`humanized` cause hundreds of false positives |

`NotifyResultExt::{notify_err, notify_app_err}` lives in `crates/workspace/src/notifications.rs:1531`,
**not** in `util`. Most of `util` is a thin re-export of `crates/gpui_util/src/lib.rs` — grep there first.

## 4. What Wu is not — do not add these back

Removed wholesale, with no crate directories at all: Zed's agent/assistant stack (`zeta`, copilot,
supermaven, `semantic_index`), collab/`call`/`channel`/livekit, telemetry/feedback, **vim and helix
modes**, journal, REPL, devcontainers, extension slash commands, context servers/MCP, agent servers,
indexed docs.

`script/upstream-sync` encodes this policy: crates absent from HEAD are **auto-dropped on every sync**.
Adding a stub crate named `agent` or `vim` would silently re-open that door.

**AI is the one exception, and it is Wu's own.** `crates/cowork` is a first-party chat surface —
a dock panel for history/search/provider status plus a workspace item per conversation. It is not
upstream Zed code and must not be reconciled against it. Its rules:
- Models come **only** from the models.dev catalog. Never hardcode a model list or a provider.
- API keys are **read from environment variables** the catalog declares, never stored, never
  prompted for, never written to settings or the database.
- Requests go straight to the chosen provider. No Wu-owned proxy, no Wu-owned endpoint.
- No tool calls, no command execution, no file reads. Adding any of those is a product decision,
  not a refactor.
See `crates/cowork/README.md` for the wire-protocol dispatch and its known limitation (providers
that are neither Anthropic nor OpenAI-compatible are not supported yet).

Vestigial but present: `client`/`rpc`/`proto` survive only as HTTP-client holder + proto plumbing for
SSH/WSL remote editing (`set_connection` is called only from tests). `remote`/`remote_connection`/
`remote_server` **are** live and supported. `reliability.rs` was gutted to local logging; panics go to
`logs_dir()/panics.log` only.

### Network calls that do exist
- **auto_update** → `api.github.com/repos/Workspaacing/wu/releases`, 12h poll, SHA-256 verified. First-party, fine.
- **extension host** → `https://api.zed.dev/extensions*`, via `server_url` default `https://zed.dev` +
  `build_zed_api_url` (`crates/http_client/src/http_client.rs:277`). The update check sends the list of
  installed extension ids, and `"auto_install_extensions": {"html": true}` downloads on first run.
  **This contradicts the README's "no telemetry" claim** — worth flagging before adding anything near it.
- **cowork** → `https://models.dev/api.json` for the model catalog (cached 24h), then the model
  provider's own endpoint for each completion. User-initiated and user-configured; no Wu endpoint
  is involved.

### Naming and branding
`paths::APP_NAME = "Wu"`; bundle ids `me.farshed.Wu{,-Dev}`; URL scheme `wu://`; project config `.wu/`
with `.zed/` fallback; actions namespaced `wu::` via `crates/wu_actions`.

**There are zero `WU_*` env vars** — everything stayed `ZED_*` (`ZED_STATELESS`, `ZED_FILE`,
`ZED_WORKTREE_ROOT`, `ZED_SERVER_URL`, `ZED_LOG`, `ZED_COMMIT_SHA`, ...). `crates/wu_env_vars` holds
exactly one const. Task variables are still `$ZED_FILE`, `$ZED_WORKTREE_ROOT`. Do not "fix" these
without checking every consumer.

Known-harmless leftovers: `ZED_URL_SCHEME = "wu"`, `zed_urls::terms_of_service`, `base_keymap` serde
value still `"Zed"` (UI shows "Wu (Default)"), a `--zed` CLI flag, packaging files named `zed.iss` /
`zed.desktop.in` / `zed.entitlements`, and `debug.plist` (an orphan; macOS signing uses
`crates/wu/resources/zed.entitlements`).

### Licensing
Per-crate, marked twice (a `LICENSE-*` file **and** the `Cargo.toml` `license` field): a permissive
**Apache-2.0 island** — `gpui*`, `util`, `collections`, `http_client*`, `path`, `sum_tree`, `watch`,
`scheduler`, `refineable`, `extension_api`, `cloud_api_types` — inside a **GPL-3.0-or-later** tree.
Never copy GPL code into an Apache crate. `script/new-crate` enforces the split (AGPL is rejected).

## 5. Where things live

| Need | Crate(s) |
|---|---|
| App entry, boot order, wiring | `crates/wu` (`src/main.rs`, `src/wu.rs`) |
| UI framework, elements, entities, tasks | `gpui`, `gpui_platform`, `gpui_{windows,macos,apple,linux,web,wgpu}`, `gpui_macros` |
| Design system / components | `ui` (`use ui::prelude::*`), `ui_input`, `icons`, `component` |
| Window shell, panes, docks, items, modals | `workspace`, `panel`, `picker`, `title_bar`, `platform_title_bar` |
| Text data model | `sum_tree` → `rope` → `text` → `language` → `multi_buffer` → `editor` |
| Language configs + `.scm` queries | **`crates/grammars/src/<lang>/`** |
| LSP adapters, built-in language registration | `languages` (`src/lib.rs:57`), `lsp`, `lsp_locations` |
| Project/files/worktrees | `project`, `worktree`, `fs`, `path`, `paths` |
| Settings schema (all `Option` "content" structs) | **`settings_content`** |
| Settings runtime, store, merge | `settings`, `settings_macros`, `settings_ui` |
| Themes | `theme`, `theme_settings`, `syntax_theme`, `theme_importer` |
| Extensions (wasm) | `extension`, `extension_api`, `extension_host`, `extension_cli`, `*_extension` |
| Debugger | `dap`, `dap_adapters`, `debugger_ui`, `debug_adapter_extension` |
| AI threads (Wu-original) | `cowork` — panel + thread item, models.dev catalog, SSE streaming |
| Local SQLite | `db`, `sqlez`, `sqlez_macros` |
| Logging | `zlog`, `zlog_settings`, `ztracing`, `etw_tracing` |
| Fuzzy matching for pickers | `fuzzy` (String) or `fuzzy_nucleo` (SharedString) — **not interchangeable** |
| Git | `git` (shells out to a real `git` binary — no git2/gitoxide), `git_ui`, `git_hosting_providers` |
| Remote dev | `remote`, `remote_connection`, `remote_server` |

Biggest crates by LOC: `editor` 157k, `project` 100k, `gpui` 84k, `workspace` 49k, `git_ui` 41k,
`ui` 27k, `language` 26k, `debugger_ui` 25k, `project_panel` 22k.

Every crate exposes one `pub fn init(cx)` called in order from `crates/wu/src/main.rs`. **Order matters** —
e.g. extension proxies must be registered before `extension_host::init`, or they silently no-op.

## 6. Recipes

### Add an editor action + keybinding
1. Declare it: `actions!(namespace, [MyAction])`, or `#[derive(Action)]` for a payload.
   Its **doc comment becomes user-visible text** in the command palette.
2. Implement the handler (editor: a fn in the relevant `crates/editor/src/*.rs` module).
3. Register it in `crates/editor/src/element.rs` — **mutating actions must go inside the
   `if !read_only` block starting at `element.rs:573`**. `register_action` (`element.rs:10355`) only
   fires in `DispatchPhase::Bubble`.
4. Add bindings to all three of `assets/keymaps/default-{windows,linux,macos}.json`.
   There is **no `assets/keymaps/windows/` overlay dir** — Windows falls back to `linux/`.
5. Possibly update the hardcoded `expected_namespaces` list in `crates/wu/src/wu.rs:5330`
   (`test_action_namespaces`).
6. Test it. Copy-template: `markdown::ToggleBlockQuote` — touch points at `actions.rs:400`,
   `markdown_actions.rs:4`, `element.rs:636`, keymap, `editor_tests.rs:42779`. With a payload:
   `ConvertToUpperCase`.

The command palette has **no registry** — it enumerates `window.available_actions(cx)`. Define the
action and make it dispatchable and it appears. `CommandPaletteFilter::{hide_namespace,
hide_action_types, show_action_types}` hides things. `crates/keymap_editor` needs nothing; it is driven
by `cx.all_action_names()`.

`script/check-keymaps` is a two-rule `git grep` linter (no `cmd-` outside macOS files; no
`super-`/`win-`/`fn-` anywhere) and is **not wired into CI**.

### Add a user setting
Nine files, in order. The two that bite:
- `flattened_deserialize!` — a missing arm is a compile error at the macro site.
- `crates/settings/src/vscode_import.rs` — builds `SettingsContent` with an **exhaustive struct literal**.

Rules:
- The *shape* goes in `crates/settings_content/` as an all-`Option` "content" struct.
- The consuming crate defines a runtime struct + `impl Settings::from_settings(&SettingsContent)`.
- Register with `#[derive(settings::RegisterSetting)]`, not `Settings::register(cx)`.
- **A default in `assets/settings/default.json` is mandatory** — `from_settings` idiomatically
  `.unwrap()`s, so a missing key is a startup panic, not a fallback.
- Layer order (`settings_store.rs:1315`, `recompute_values`): default → extension → global → user (+release-channel, +OS)
  → profile → server → `.wu/settings.json` (deepest dir wins).
- **Nothing to regenerate.** `script/update-json-schemas` only re-downloads SchemaStore's
  `tsconfig.json`/`package.json`. Wu's own schemas are generated at runtime by `json_schema_store` and
  served over `wu://schemas/...`.
- `crates/settings_ui` is optional and unenforced — skip it and the setting is JSON-only, silently.

### Add a modal
`Picker<D>` already implements `ModalView` (`picker.rs:1998`), so a wrapper struct is optional.
**Copy-template: `crates/line_ending_selector/src/line_ending_selector.rs` (195 lines)** — a complete
`PickerDelegate` with all 9 required methods. `project_symbols.rs:27` shows the no-wrapper variant.
Note `PickerDelegate::name()` doubles as a persistence key.

### Add a dock panel
`Panel` requires 9 methods. The focus-handle / `activation_focus_handle` containment contract at
`crates/workspace/src/dock.rs:41-48` is the usual breakage. Templates: `dock.rs:1640-1791` (TestPanel)
and `crates/outline_panel` (init:653, load:668, serialize:914, `impl Panel`:4957). Wire it in
`crates/wu/src/main.rs` and `initialize_panels` in `crates/wu/src/wu.rs:640-678`.

### Add a workspace item (tab type)
`Item` has exactly one required method (`tab_content_text`); the rest default — but `clone_on_split`
panics unless `can_split()`, and `save`/`reload` panic unless `can_save()`. Smallest real templates:
`crates/workspace/src/theme_preview.rs:86` and `crates/svg_preview` (336 lines, whole crate).
Serializable variant: `component_preview.rs:709/:777`.

### Add a status bar item
`crates/line_ending_selector/src/line_ending_indicator.rs` (80 lines) is complete. Note
`render_right_tools` iterates `.rev()`, so the last `add_right_item` renders leftmost.

### Notify the user
Two separate systems: `Toast` / `show_toast` / `show_notification` (top-right stack) vs `StatusToast` /
`toggle_status_toast` (bottom-right `ToastLayer`, single slot). Plus `NotifyResultExt::notify_err` and
`NotifyTaskExt::detach_and_notify_err`.

### Add a tree-sitter grammar
Native: workspace dep → `optional = true` in `crates/grammars/Cargo.toml` + the `load-grammars` feature
→ entry in `native_grammars()` (`grammars.rs:17`) → `crates/grammars/src/<name>/config.toml` + `.scm`
files → `LanguageInfo` entry in `crates/languages/src/lib.rs:90+`.
Extension-provided: `[grammars.<snake_case_name>]` in `extension.toml` (**snake_case or the builder bails**).

## 7. Editor coordinates — the #1 bug source

Layers, bottom to top: buffer offset / `Point` / `PointUtf16` / `OffsetUtf16` / `Unclipped<T>` /
`text::Anchor` → multibuffer newtypes → six display layers:
`InlayPoint` → `FoldPoint` → `TabPoint` → `WrapPoint` → `BlockPoint` → `DisplayPoint`.

**Rule: down is total, up is lossy and requires a `Bias` at every layer.**

Anchors survive edits, offsets do not — hold anchors across async boundaries. Do heavy work on
`BufferSnapshot` / `EditorSnapshot` / `MultiBufferSnapshot` in the background, never on the live buffer.
Comparing anchors from different buffers panics (`anchor.rs:105`).

Debug tool worth knowing: `MultiBufferSnapshot::debug(&ranges, value)` (`multi_buffer.rs:6553`, debug
builds only) paints `Debug` output onto buffer ranges live in the editor.

## 8. Testing

Dominant convention: inline `#[cfg(test)] mod tests { use super::*; ... }` at the bottom of the source
file (329 sites). Because `mod.rs` is banned, test dirs are `src/tests.rs` + `src/tests/*.rs`.

`#[gpui::test]` accepts exactly five args: `seed=N`, `seeds(...)`, `iterations=N`, `retries=N`,
`on_failure="path"`. Params are matched by type name: `&mut TestAppContext` (repeatable for multi-client
tests), `&mut App` (sync only), `StdRng`, `BackgroundExecutor`. Also `#[gpui::property_test]`
(proptest; `StdRng` is a compile error there).

`TestScheduler` = seeded RNG queue + fake clock + `allow_parking=false`. `run_until_parked()` drains the
queue and jumps the clock to the next timer. Failure modes: "Parking forbidden. Re-run with
PENDING_TRACES=1", a 15 s hard timeout under `allow_parking`, a "Your test is not deterministic" thread
check, and entity-leak detection at teardown.

Standard init order in UI tests: `SettingsStore` → `theme_settings::init(JustBase)` → per-crate `init`
→ `workspace::init`. Verbatim boilerplate is in `references/08-testing.md`; live examples at
`editor_tests.rs:37045`, `project_tests.rs:16611`, `project_panel_tests.rs:11431`, `workspace.rs:16118`.

Utilities: `FakeFs::new(cx.executor())` + `fs.insert_tree(path!(...), json!(...)).await` (there is **no**
`insert_tree!` macro; object = dir, string = file, null = empty dir). Marked text via `util::test`:
`ˇ` cursor, `«»` range, `«ˇtext»` reversed, `«textˇ»` forward, `•` space
(`crates/util/src/test/marked_text.rs:83-112`). `EditorTestContext` / `EditorLspTestContext`,
`VisualTestContext`, `FakeLanguageServer::set_request_handler`, `language_registry.register_fake_lsp`.

~35 `test_random_*` tests use reference models + `check_invariants()`. Rerun a failure with
`SEED=N ITERATIONS=1 OPERATIONS=200`; pin the regression as `seeds(N)`. Other env vars:
`PENDING_TRACES`, `DEBUG_SCHEDULER`, `SIMPLE_TEXT`, `ZED_LOG`. There is no `insta`/snapshot crate.

Benchmarks: criterion (`harness=false`) in `benchmarks`, `rope`, `language`, `fuzzy_nucleo`,
`gpui_wgpu`, `project_panel`, `extension_host`; standalone bins `{editor,fs,project,worktree}_benchmarks`;
`cargo perf-test` / `cargo perf-compare` (needs hyperfine) for `#[perf]` tests.

## 9. Windows notes (this repo is developed on Windows)

- Renderer is **DirectX 11 + DirectComposition**. `gpui_wgpu` never compiles on Windows.
- **HLSL is only precompiled in release** — a broken shader passes `cargo check` and fails only in a
  release build.
- `VisualTestAppContext`, `VisualTestPlatform`, `wu_visual_test_runner` and headless screenshot capture
  are **macOS-gated**. `TestAppContext` / `VisualTestContext` / `#[gpui::test]` work fine.
- `.cargo/config.toml` sets `+crt-static` and `--cfg windows_slim_errors`, but the Windows `rustflags`
  block **replaces** `[build] rustflags`, dropping `v0` mangling and `--cfg tokio_unstable`.
- Use `path!` / `uri!` / `line_endings!` in every test. `root_path()` is `C:\root`. ~12 tests are
  `#[cfg(unix)]`-only (mostly symlinks).
- `AbsPathBuf::canonicalize` deliberately **never returns `\\?\`** — git and Node LSPs choke on UNC.
  `SanitizedPath` (dunce) is the general laundering type. Component matching is case-insensitive via
  `component_matches_ignore_ascii_case` (security-relevant).
- Config lives in `%APPDATA%\Wu`; data, logs and the DB under `%LOCALAPPDATA%\Wu`.
- Windows-specific code: `crates/explorer_command_injector`, `crates/etw_tracing`,
  `crates/windows_resources`, `crates/wu/src/wu/windows_only_instance.rs`.
- In `platform_title_bar`, drag and double-click are owned by the platform layer — do not add handlers
  (`platform_title_bar.rs:206`). Caption glyphs are Segoe codepoints; the close button is hardcoded
  `rgb(232,17,32)`.
- UI inspector: `shift-alt-i`.
- `script/bundle-windows.ps1` needs VS 2022, Windows SDK (`makeappx.exe`), Inno Setup 6, and network.
- **Release builds can OOM on a small machine.** The `windows` crate (0.61.x, ~500 features) at
  `opt-level=3` is the peak allocator in this tree; on 8 GB of RAM with the default `jobs = 4` it
  dies as a bare `error: could not compile \`windows\` (lib)` with no diagnostic, because rustc is
  killed rather than reporting. It is not your code — the crate is built with `--cap-lints allow`.
  Retry with `CARGO_BUILD_JOBS=1`. Debug builds are unaffected.
- Clippy lints are not profile-dependent, so `cargo clippy -p <crate> --all-targets --all-features
  -- --deny warnings` in the dev profile finds the same lints as `script/clippy.ps1` far faster; use
  it for the edit loop and keep the release run for the final gate.

## 10. Top footguns

1. **SQLite migrations are append-only, enforced destructively.** A changed migration raises
   `MigrationChangedError`, and the DB is renamed aside and **recreated empty with no prompt**.
   `should_allow_migration_change` is the only escape hatch (one real use, `ssh_connections`).
2. **Never edit an existing `crates/extension_api/wit/since_vX.Y.Z/` dir** or its host bindings — it
   silently breaks every published extension pinned to that version. Add a new version instead.
3. **Entity re-entrancy**: `entity.read()` also panics during a lease; `update_global` leases the global
   out, so a nested `cx.global::<G>()` panics.
4. **`request_lsp` returns `Ok(Default::default())`** — not an error — when no server is capable
   (`lsp_store.rs:5603`). Do not treat empty as failure.
5. **Dropped `Task`s cancel their work.** Await, `detach_and_log_err(cx)`, or store in a field.
6. **`ElementId` collisions** from `use_state` / code-location reuse cause silently wrong state. Use
   `("prefix", ix)` when lists coexist.
7. **Subscriptions are inert until the `Subscription` is stored** (typically `_subscriptions: Vec<Subscription>`).
8. **`cx.notify()` omissions** mean your view just does not repaint.
9. **Blocking IO on the foreground thread** is a dylint error, not a warning — including `Path::exists()`.
10. **Two fuzzy matchers** with non-interchangeable `StringMatch*` types.
11. **Extension dev installs are symlinks** into your working tree; the fs watcher then fires on your edits.
12. **`SUPPRESSED_EXTENSIONS`** actively uninstalls listed extensions on every reload.
13. **Stale/dead tooling**: `script/bootstrap*` and the nextest overrides target the nonexistent
    `collab`; `script/crate-dep-graph` uses `--root=zed,cli,collab`; `cargo xtask wsl-sandbox-tests`
    references a missing `sandbox` crate; `clippy.toml` mentions the removed `agent_ui`;
    `script/download-wasi-sdk` is referenced by nothing. `typos.toml`, `deny.toml` and `.editorconfig`
    do **not** exist, so bash `script/clippy` fails at its `typos` step if `typos` is on PATH.

## 11. Reference files

Deep dives live beside this file in `references/`. Read the one matching your task before editing.

| File | Covers |
|---|---|
| `references/01-build-ci.md` | Cargo/workspace conventions, every `script/`, custom lints, xtask gates, release + upstream-sync |
| `references/02-fork-delta.md` | Wu vs Zed, boot sequence with hook points, naming, licensing, removed features |
| `references/03-gpui.md` | GPUI beyond `.rules`: `View` trait, platform seam, macros, element lifecycle, focus, globals, panics |
| `references/04-editor-core.md` | Coordinate cheat sheet, `editor` module map, add-an-action recipe, test templates, 35 footguns |
| `references/05-ui-workspace.md` | Full component vocabulary, `Item`/`Panel` contracts, 6 step-by-step recipes, crate appendix |
| `references/06-settings-keymaps-themes.md` | Add-a-setting and add-an-action recipes, merge order, themes, tasks, snippets |
| `references/07-extensions-dap.md` | Wasm extension host, WIT versioning, capabilities, registry network, grammars, DAP |
| `references/08-testing.md` | `#[gpui::test]` semantics, `FakeFs`, marked text, 5 copy-paste templates, commands, benchmarks |
| `references/09-infra-crates.md` | `util`/`collections`/`fs`/`paths`/`db`/`zlog` deep dives, crate reference table, Windows paths |
