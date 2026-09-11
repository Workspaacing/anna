> **Windows prerequisites (verified 2026-09-11, on this machine).**
> `cmake` is a hard requirement for the *entire* workspace, not just the extension crates:
> `tree-sitter` -> `wasmtime-c-api-impl`, whose build script spawns `cmake` directly. Without it
> every build fails at `failed to spawn 'cmake': program not found`. `script/install-cmake` handles
> only macOS/Linux and errors out on Windows; use `winget install --id Kitware.CMake --exact`.
> `script/install-rustup.ps1` downloads the **i686-pc-windows-gnu** rustup-init, which sets a 32-bit
> GNU default host; fetch the `x86_64-pc-windows-msvc` installer and pass
> `--default-host x86_64-pc-windows-msvc` instead. MSVC Build Tools 2022 + the Windows SDK are
> required but do not need to be on `PATH`.

# Wu — Build System, Toolchain, Lints, CI & Upstream Sync

Repo root: `C:/Users/USER/Documents/wu-main`
Project: **Wu**, a hard fork of Zed (`crates/wu` replaces upstream `crates/zed`).
Current version: `crates/wu/Cargo.toml` -> `version = "1.0.6"`.
Release channel in tree: `crates/wu/RELEASE_CHANNEL` -> `dev`.
Upstream pin: `UPSTREAM_VERSION` -> `01acd0ee8e906dd0ec8b526fe08da94444a5e2af` (a Zed commit SHA, not a tag).

---

## 0. TL;DR

| Want to… | Run |
|---|---|
| Fast type-check | `cargo check -p <crate> --all-targets` |
| Build the app (dev) | `cargo build` (default member is `crates/wu`) |
| Run the app | `cargo run --profile release-fast` |
| Lint (the real gate) | `pwsh script/clippy.ps1` (Windows) / `./script/clippy` (bash) |
| Format Rust | `cargo fmt --all` |
| Test | `cargo test -p <crate>` or `cargo nextest run -p <crate>` |
| Custom (dylint) lints | `cargo dylint --all -- --workspace` |
| Workspace hygiene | `cargo xtask package-conformity`, `cargo xtask licenses`, `cargo test -p xtask` |

**Do not run a full-workspace `cargo build`/`clippy` casually** — ~170 member crates, a GPU editor. Scope with `-p <crate>` while iterating.

---

## 1. Toolchain

`rust-toolchain.toml` (root):

```toml
[toolchain]
channel = "1.97.1"
profile = "minimal"
components = [ "rustfmt", "clippy", "rust-analyzer", "rust-src" ]
targets = [
    "wasm32-wasip2",              # extensions
    "wasm32-unknown-unknown",     # gpui on the web
    "x86_64-unknown-linux-musl",  # remote server
]
```

- Pinned **stable 1.97.1**. `rustup show` in the repo root installs it (that is literally what CI does).
- Edition **2024** everywhere (`[workspace.package] edition = "2024"`; `rustfmt.toml` = `edition = "2024"` + `style_edition = "2024"`, nothing else).
- Second, separate toolchain: `tooling/lints/rust-toolchain.toml` pins **`nightly-2026-03-21`** with `llvm-tools-preview`, `rustc-dev`, `rust-src`. Only for the dylint lint library.
- Windows rustup bootstrap: `script/install-rustup.ps1` (respects `CARGO_HOME`/`RUSTUP_HOME`; downloads `rustup-init.exe` if `cargo` is absent).

### `.cargo/config.toml`

```toml
[build]
jobs = 4
rustflags = ["-C", "symbol-mangling-version=v0", "--cfg", "tokio_unstable"]

[alias]
xtask        = "run --package xtask --"
perf-test    = ["test", "--profile", "release-fast", ...]   # tests under the `perf` runner, --cfg perf_enabled
perf-compare = ["run", "--profile", "release-fast", "-p", "perf", ..., "--", "compare"]

[target.'cfg(target_os = "windows")']
rustflags = [
  "--cfg", "windows_slim_errors",     # windows::core::Error 16B -> 4B
  "-C", "target-feature=+crt-static", # required to link livekit on Windows
]

[env]
MACOSX_DEPLOYMENT_TARGET = "10.15.7"
```

Encoded gotchas:
- `jobs = 4` caps parallelism globally regardless of core count. Deliberate.
- **Target-specific `rustflags` REPLACE `build.rustflags`, they do not merge.** The `aarch64-apple-darwin` block re-lists `v0` mangling + `tokio_unstable` for exactly that reason, with an explanatory comment. The Windows block does *not* re-list them, so Windows builds get neither `v0` mangling nor `tokio_unstable`.
- `+crt-static` on Windows: any native dep must also be static; mismatches appear as link errors.
- `cargo xtask <task>` works because of the alias.

### `.config/nextest.toml`
- Default `slow-timeout = { period = "60s", terminate-after = 1 }` — **any test over 60s is killed**.
- Overrides raise it to 300s for known-slow tests (`test_rainbow_bracket_highlights`, `test_wrapped_invisibles_drawing`, `test_basic_following`, `test_random_diagnostics_blocks`, `extension_host::test_extension_store_with_test_extension`, several `editor`/`vim` randomized tests).
- `package(db)` runs in test-group `sequential-db-tests` with `max-threads = 1`.
- Priority overrides run the slowest tests first (`worktree::test_random_worktree_changes` at priority 100).
- Several overrides name crates that **no longer exist in this fork** (`collab`, `language_model`, `vim`). Harmless, but not a source of truth.

---

## 2. Exact commands to run (Windows/PowerShell notes inline)

### Format
```powershell
cargo fmt --all              # write
cargo fmt --all -- --check   # verify only
```
Config is only edition/style_edition, so plain rustfmt defaults are correct.

### Check / build
```powershell
cargo check -p editor --all-targets      # iterate on one crate
cargo check --workspace --all-targets    # slow, full
cargo build                              # dev profile; default-members = ["crates/wu"]
cargo build --profile release-fast       # release codegen + full debuginfo, lto off
cargo run --profile release-fast         # how the project itself runs the editor (.wu/tasks.json)
cargo build --profile dbg                # dev + debug="full" when you need a debugger
```

### Lint - THIS IS THE REAL GATE
`.rules` says explicitly: *"Use `./script/clippy` instead of `cargo clippy`."*

Both `script/clippy` (bash) and `script/clippy.ps1` (PowerShell) run:

```
cargo clippy [<your args>] --workspace --release --all-targets --all-features -- --deny warnings
```

`--workspace` is added only when you did NOT pass `-p`/`--package`.

Windows:
```powershell
pwsh script/clippy.ps1              # whole workspace
pwsh script/clippy.ps1 -p editor    # single crate - do this while iterating
```
Git Bash:
```bash
./script/clippy
./script/clippy -p editor
```

Facts:
- `--release --all-targets --all-features` compiles the *release* profile of every target incl. tests, benches, examples. First run: minutes to tens of minutes. The root `Cargo.toml` says so inline: *"Running ./script/clippy can take several minutes"*.
- `-- --deny warnings`: any clippy warning is an error.
- The bash version, when `$GITHUB_ACTIONS` is unset, additionally runs each of these only if the binary is on PATH (silently exits 0 if not):
  - `cargo shear --locked --deny-warnings` (unused-dependency detection)
  - `typos --config typos.toml`
  - `buf lint crates/proto/proto` and `buf format --diff --exit-code crates/proto/proto`
- `script/clippy.ps1` runs NONE of those extras - it is clippy-only (it also prints your PATH entries first). No parity between the two.

### Test
There is no `script/test`. Use cargo directly:
```powershell
cargo test -p <crate>
cargo nextest run -p <crate>     # honours .config/nextest.toml timeouts/groups
cargo test -p xtask              # runs the FORBIDDEN_DEPENDENCIES graph test (see 4d)
cargo test --workspace           # very slow; avoid unless asked
```

### Perf harness
```
cargo perf-test        # test --profile release-fast --all-features, runner = cargo run -p perf --release, --cfg perf_enabled
cargo perf-compare     # run -p perf --profile release-fast -- compare
```
Results land in `.perf-runs/` (gitignored). `tooling/perf` is measurement, not a lint.

### Prettier (JSON / Markdown / YAML)
`script/prettier` (bash only, needs `pnpm`):
```bash
./script/prettier            # --check
./script/prettier --write    # fix
```
Pins `prettier@3.5.0` via `pnpm dlx`. Scope is exactly two things: `assets/settings/default.json` (with `--parser=jsonc`) and everything under `docs/`.
`.prettierrc` = `{ "printWidth": 120 }`.
No PowerShell equivalent. On Windows use Git Bash, or invoke directly:
```powershell
pnpm dlx prettier@3.5.0 assets/settings/default.json --parser=jsonc --write
```

### Keymap check
`script/check-keymaps` (bash, uses `git grep`). Enforces:
1. No `cmd-` outside `assets/keymaps/default-macos.json`, `assets/keymaps/specific-overrides-macos.json`, `assets/keymaps/macos/*.json`.
2. No `super-`, `win-`, or `fn-` anywhere under `assets/keymaps/`.

Run after touching any keymap JSON. Requires a git repo (see section 7 trap 1).

### Shellcheck
`script/shellcheck-scripts [error|warning]` (default `error`) - finds every `script/*` with a sh/bash/dash shebang and runs `shellcheck -x -S <mode> -C`. Run after editing any bash script.

### JSON schemas
`script/update-json-schemas [schemastore-commit]` - pulls `tsconfig.json` and `package.json` schemas from SchemaStore into `crates/json_schema_store/src/schemas/`, rewriting `https://json.schemastore.org` to `https://www.schemastore.org`. Needs `curl` + `jq`. Prints a ready-made changelog blurb. Only run when explicitly asked.

### Licenses
- `script/generate-licenses [outfile]` (bash) / `script/generate-licenses.ps1` (PowerShell) writes `assets/licenses.md` (gitignored via `/assets/*licenses.*`). Pins `cargo-about@0.8.2`, installs it if missing. Config `script/licenses/zed-licenses.toml`, template `script/licenses/template.md.hbs`. `ALLOW_MISSING_LICENSES=1` downgrades failures.
- Accepted licenses (`script/licenses/zed-licenses.toml`): Apache-2.0, MIT, MIT-0, Apache-2.0 WITH LLVM-exception, MPL-2.0, BSD-3-Clause, BSD-2-Clause, ISC, CC0-1.0, NCSA, Unicode-3.0, OpenSSL, Zlib, BSL-1.0, bzip2-1.0.6, CDLA-Permissive-2.0. AGPL is explicitly excluded. A new dep outside this list breaks bundling.
- `cargo xtask licenses` - every workspace crate must have a `LICENSE-APACHE` or `LICENSE-GPL` symlink; reports "is not a symlink" / "Missing license: <crate>".

### Dependency graph
`script/crate-dep-graph` runs `cargo depgraph --workspace-only --offline --root=zed,cli,collab ... | dot -Tsvg > target/crate-graph.html`. STALE - `zed` and `collab` do not exist here. Fix roots to `wu,cli` if you need it.

### Bundling / packaging (30-300 min jobs; do not run unprompted)

| Script | Platform | Output / notes |
|---|---|---|
| `script/bundle-mac [arch]` | macOS | `Wu-aarch64.dmg`, `Wu.dSYM.zip`, `target/wu-remote-server-macos-*.gz`. Signing/notarization only if `MACOS_CERTIFICATE` / `APPLE_NOTARIZATION_*` present, else ad-hoc signed. Identity is `Farshed`. |
| `script/bundle-linux` | Linux | `target/release/wu-linux-<arch>.tar.gz` + remote-server gz. `--flatpak` flag. |
| `script/bundle-freebsd` | FreeBSD | tar.gz |
| `script/bundle-windows.ps1 -Architecture x86_64 or aarch64 [-Install]` | Windows | see below |
| `script/install-linux` | Linux | bundle-linux + install into `~/.local` |
| `script/install.sh` / `script/uninstall.sh` | Linux | end-user installers |

`script/bundle-windows.ps1` (the Windows-relevant one):
- Auto-enters the VS 2022 dev shell via `Launch-VsDevShell.ps1` if found.
- Channel from `crates/wu/RELEASE_CHANNEL`; version from `crates/wu/Cargo.toml` unless `$env:RELEASE_VERSION` is set.
- Staging dir `<repo>/inno/<arch>` (gitignored `/inno`).
- Builds `wu`, `cli`, `auto_update_helper`, then `explorer_command_injector` (with `--features stable --no-default-features` on the stable channel), then `remote_server`, all `--release --target <arch>-pc-windows-msvc`.
- Requires: Windows 10/11 SDK (`makeappx.exe`), Inno Setup 6 at `C:\Program Files (x86)\Inno Setup 6\ISCC.exe`, and network access (downloads AGS_SDK v6.3.0 and Microsoft ConPTY v1.23.13503.0).
- Only `stable` and `dev` channels are supported; anything else errors and exits 1.
- App identity strings must stay in sync with `crates/release_channel/src/lib.rs` (`app_identifier()`) and `crates/wu/src/wu/windows_only_instance.rs` (mutex `Wu-Editor-Stable-Instance-Mutex`).

### Other scripts, quick reference
- `script/linux`, `script/freebsd` - install OS build deps (apt/dnf/pacman/pkg); `script/remote-server` - installs `clang` on Debian; `script/install-cmake` - up-to-date CMake.
- `script/download-wasi-sdk` - fetches WASI SDK v25 into `./target/wasi-sdk` for extension builds.
- `script/bootstrap` / `script/bootstrap.ps1` - collab-server only: installs `sqlx-cli 0.7.2`, minio, foreman, then cd `crates/collab` and creates DBs. `crates/collab` does not exist in this fork, so both are dead. Do not run them.
- `script/cargo` - Node wrapper adding `--timings` to `build|check|run|test`; it no-ops unless `git remote -v` mentions `zed-industries/zed`, so it is inert in Wu. `script/cargo --init` installs a shell alias (has a PowerShell branch). Not needed.
- `script/new-crate <name> [apache|gpl]` - see section 3.
- `script/get-crate-version <crate>` / `.ps1` - reads a version from `cargo metadata` (bash one needs `jq`).
- `script/memory-benchmark` (macOS only, compares Wu vs Zed RSS), `script/metal-debug` (macOS), `script/histogram` + `script/analyze_highlights.py` (Python), `script/import-themes` (`cargo run -p theme_importer`), `script/verify-macos-document-icon`.
- `script/lib/blob-store.sh` - DigitalOcean Spaces upload helpers sourced by the bundle scripts; needs `DIGITALOCEAN_SPACES_*`, no-ops locally.

---

## 3. Workspace / Cargo conventions to follow

### Layout
- `resolver = "2"`, ~170 members: everything under `crates/*`, plus `extensions/{glsl,html,proto,test-extension}`, plus `tooling/perf` and `tooling/xtask`.
- `default-members = ["crates/wu"]` - a bare `cargo build`/`cargo run` targets the editor only.
- `[workspace.package] publish = false`, `edition = "2024"`.
- `tooling/lints` is deliberately EXCLUDED from the workspace (it declares its own empty `[workspace]` table and pins nightly).
- Members list and on-disk `crates/*` dirs are currently in perfect sync (verified) - keep it that way when adding/removing crates.

### Adding a dependency - the rule
1. Add it to `[workspace.dependencies]` in the root `Cargo.toml` with version/features/git-rev.
2. In the member crate write `foo.workspace = true`, or `foo = { workspace = true, features = ["x"], optional = true }`.
3. NEVER write a bare `foo = "1.2"` in a member crate. `cargo xtask package-conformity` prints `<dep> is being used as a non-workspace dependency: <crates>` for every violation.
   - Exempt: anything under `extensions/`, and the `zed_extension_api` package.
   - Known pre-existing violations you will see: `crates/wu/Cargo.toml` has `spin = "0.10.0"` and `mimalloc = { version = "0.1", optional = true }` inline. Do not add more.
4. Keep `[workspace.dependencies]` alphabetically sorted - both the internal-crate block and the external-crate block are maintained that way.
5. Internal crates go in the first block as `name = { path = "crates/name" }`. A few carry extra defaults that must be preserved: `gpui`, `gpui_linux`, `gpui_macos`, `gpui_platform`, `gpui_windows` all use `default-features = false`; `collections` additionally pins `version = "0.1.0"`.

### Version pinning conventions
- Most external deps use loose caret ranges (`anyhow = "1.0.86"`).
- Exact pins (`=`) where ABI matters: `cocoa = "=0.26.0"`, `objc2-foundation = { version = "=0.3.2", ... }`.
- Git deps are always pinned by `rev`, never by branch: `alacritty_terminal`, `async-pipe`, `async-tar`, `dap-types`, `lsp-types`, `tree-sitter` (`rev = "43623ec9..."`), the whole `pet-*` family, `proptest`, `rodio`, several `tree-sitter-*` grammars, and `reqwest` (repackaged as `zed-reqwest`).
- Two load-bearing comments in the root manifest:
  - Above `reqwest`: "WARNING: If you change this, you must also publish a new version of zed-reqwest to crates.io".
  - Above the `[patch.crates-io]` `tree-sitter-language` entry: it exists to unify `LanguageFn` between crates.io grammar crates and the git `tree-sitter`. Bumping the `tree-sitter` rev requires bumping this patch rev in lockstep or grammar constants become type-incompatible with `tree_sitter::Language`.

### `[patch.crates-io]` - forked deps

```
tree-sitter-language -> github.com/tree-sitter/tree-sitter        @ 43623ec9...
async-process        -> github.com/zed-industries/async-process   @ 0b6d6713...
async-task           -> github.com/smol-rs/async-task             @ b4486cd7...
calloop              -> github.com/zed-industries/calloop         (NO rev)
notify, notify-types -> github.com/zed-industries/notify          @ 0890bbb8...
```

`calloop` is the only unpinned patch. `Cargo.lock` is what stabilizes it - do not `cargo update -p calloop` without intent.

### Profiles

| Profile | Key settings |
|---|---|
| `dev` | `split-debuginfo = "unpacked"`, `incremental = true`, `codegen-units = 16`, **`debug = 0`** (no debuginfo by default) |
| `dev.build-override` | mirrors dev so build scripts / proc macros are not compiled twice (~400 crates) |
| `dbg` | `inherits = "dev"`, `debug = "full"` - use this when you need a debugger ("debug" is a reserved profile name, hence `dbg`) |
| `release` | `debug = "line-tables-only"`, `lto = "thin"`, `codegen-units = 16` |
| `release.package` | `codegen-units = 1` for `gpui`, `editor`, `language`, `rope`, `sum_tree`, `text` (editor-perf critical) |
| `release-fast` | `inherits = "release"`, `debug = "full"`, `lto = false`, `codegen-units = 16` - the day-to-day run profile |
| `dev.package` | `opt-level = 3` for proc-macro crates (`gpui_macros`, `derive_refineable`, `settings_macros`, `sqlez_macros`, `ui_macros`, `util_macros`, `quote`, `syn`, `proc-macro2`) and for `tree-sitter`, `taffy`, `resvg`, `wasmtime`, `cranelift-codegen`, `wasmtime-environ`, `wasmtime-internal-cranelift`, `serde_json`; `codegen-units = 1` for ~25 single-file crates |

If you add a new proc-macro crate, add it inside the `# proc-macros start` / `# proc-macros end` block with `opt-level = 3`.

### Adding a new crate
`script/new-crate <crate_name> [apache|agpl|gpl]` (bash; no PowerShell equivalent):
- Default license is GPL-3.0-or-later; pass a flag containing "apache" for Apache-2.0. AGPL is rejected outright: "New first-party crates cannot use AGPL. Use GPL or Apache."
- Crate name must match `^[a-z0-9_]+$` (lowercase + underscores).
- It SYMLINKS the license: `ln -sf ../../LICENSE-GPL crates/<name>/LICENSE-GPL`.
- It emits a `Cargo.toml` with `edition.workspace = true`, `publish.workspace = true`, explicit `license = "..."`, `[lints] workspace = true`, and `[lib] path = "src/<crate_name>.rs"`.
- It does NOT add the crate to `[workspace] members` - do that manually, and also add `name = { path = "crates/name" }` to `[workspace.dependencies]`.

Reinforced by `.rules`:
- "Never create files with `mod.rs` paths - prefer `src/some_module.rs`."
- "prefer specifying the library root path in `Cargo.toml` using `[lib] path = "...rs"` instead of the default `lib.rs`" (e.g. `gpui.rs`, `wu.rs`).

Every member crate MUST have `[lints] workspace = true`, or `cargo xtask package-conformity` prints `"<pkg>" is not using workspace lints`.

---

## 4. Custom lints and what they forbid

### 4a. `[workspace.lints]` in the root `Cargo.toml`

```toml
[workspace.lints.rust]
unexpected_cfgs = { level = "allow" }

[workspace.lints.clippy]
dbg_macro                       = "deny"
todo                            = "deny"
declare_interior_mutable_const  = "deny"
redundant_clone                 = "deny"
disallowed_methods              = "deny"

style = { level = "allow", priority = -1 }   # whole style group off, on purpose

type_complexity          = "allow"
let_underscore_future    = "allow"
single_range_in_vec_init = "allow"
too_many_arguments       = "allow"
large_enum_variant       = "allow"
nonminimal_bool          = "allow"
```

What this means in practice:
- `dbg!(...)` and `todo!(...)` are HARD ERRORS. Strip every `dbg!` before finishing.
- `redundant_clone` is denied - a `.clone()` the compiler can prove unnecessary fails `script/clippy`.
- `disallowed_methods` is denied, so the `clippy.toml` list below produces errors, not warnings.
- The entire clippy `style` group is ALLOWED. Do not "fix style nits" - that is a documented policy decision, explained inline in `Cargo.toml` (running clippy is slow, so style churn is not worth blocking shipping).

### 4b. `clippy.toml` - disallowed methods (each one is a deny)

| Forbidden | Replacement / reason |
|---|---|
| `std::process::Command::{spawn, output, status}` | `smol::process::Command::*` - blocks the thread for an unknown duration |
| `std::process::Command::{stdin, stdout, stderr}` | `smol::process::Command::*` - `smol::process::Command::from()` does not preserve stdio config |
| `smol::Timer::after` | `gpui::BackgroundExecutor::timer` - `smol::Timer` introduces non-determinism in tests |
| `serde_json::from_reader` | `serde_json::from_slice` (reader parsing is much slower) |
| `serde_json_lenient::from_reader` | `serde_json_lenient::from_slice` |
| `cocoa::foundation::NSString::alloc` | the `ns_string()` helper - NSString must be autoreleased |

Escape hatch actually used in this repo (tooling and build scripts only):
```rust
#![allow(clippy::disallowed_methods, reason = "tooling is exempt")]
```
seen in `crates/wu/build.rs` and every `tooling/xtask/src/tasks/*.rs`.

Other `clippy.toml` settings: `allow-private-module-inception = true`, `avoid-breaking-exported-api = false`, `ignore-interior-mutability = ["agent_ui::context::AgentContextKey"]` (stale - `crates/agent_ui` is gone), and a fully commented-out `disallowed-types` block, so `std::collections::HashMap` is NOT currently banned in favour of `collections::HashMap`.

### 4c. Custom dylint lints - `tooling/lints/`

Registered in the root `Cargo.toml`:
```toml
[workspace.metadata.dylint]
libraries = [{ path = "tooling/lints" }]
```
so `cargo dylint --all` finds them with no `--path` argument.

One-time setup (slow first build):
```
cargo install cargo-dylint dylint-link       # version 6 or later
cd tooling/lints && rustup toolchain install # nightly-2026-03-21 + rustc-dev/rust-src/llvm-tools
```
`tooling/lints/.cargo/config.toml` sets `linker = "dylint-link"` for `cfg(all())`.

Run:
```
cargo dylint --all -- --workspace          # whole repo
cargo dylint --all -- -p project_panel     # one crate
tooling/lints/single-lint blocking_io_on_foreground -p project_panel   # one lint
```
`single-lint` exists because Dylint loads the whole library at once: it sets `DYLINT_RUSTFLAGS="-A warnings --force-warn <lint>"` (a plain `-W` does not re-enable a driver-registered lint after `-A warnings`) and cleans the targeted package first (`DYLINT_RUSTFLAGS` is not part of Cargo's fingerprint, so a stale cache would silently replay).

THE SIX LINTS AND EXACTLY WHAT THEY FORBID (all declared at Warn level; they are NOT part of `script/clippy`, but they encode the team's real rules and are the fastest way to learn what this codebase considers a bug):

1. `shared_string_from_str_literal` - `SharedString::from("lit")`, `SharedString::new("lit")`, `<SharedString as From<_>>::from("lit")`, or `"lit".into()` where the inferred target is `SharedString`.
   Fix: `SharedString::new_static("lit")`. Machine-applicable suggestion. Escalated message when the literal exceeds 23 bytes (SmolStr inline cap) because then every call site heap-allocates an `Arc<str>`.

2. `async_block_without_await` - an `async { ... }` / `async move { ... }` whose body has no `.await` at its own nesting level (an `.await` in a nested async block does not count).
   Fix: remove the `async`, or you are missing an `.await`. Exempt: async blocks inside trait-method bodies, where the trait signature forces the shape.

3. `entity_update_in_render` - `Entity::update` / `WeakEntity::update` called synchronously inside `Render::render` or `RenderOnce::render` where the closure returns `()` or `Result<()>` (i.e. mutation, not a read).
   Rationale: `render` must be a pure function of state; mutating mid-render causes inconsistent UI or infinite render loops.

4. `notify_in_render` - `cx.notify()` on a GPUI context type inside `Render::render`.
   Rationale: every render pass would schedule another render pass.

5. `owned_string_into_shared` - `String::from("lit").into()`, `"lit".to_string().into()`, `"lit".to_owned().into()` where the destination is `SharedString`, `Arc<str>`, `Rc<str>`, or `Cow<'_, str>`.
   Rationale: two allocations and two copies where zero are needed.

6. `blocking_io_on_foreground` - a large explicit list of blocking std calls invoked from a function that takes a synchronous GPUI context (`&App`, `&mut App`, `&Context<T>`, `&mut Context<T>`, `&mut Window`) or directly inside a `render` method. The flagged set:
   - `std::fs::{read, read_to_string, write, read_dir, read_link, metadata, symlink_metadata, set_permissions, canonicalize, create_dir, create_dir_all, remove_file, remove_dir, remove_dir_all, copy, rename, hard_link}`
   - `std::fs::File::{open, create, create_new}` plus methods `{sync_all, sync_data, set_len, metadata, try_clone, set_permissions}`
   - `std::thread::sleep`
   - `std::path::Path::{metadata, symlink_metadata, read_link, read_dir, exists, try_exists, is_file, is_dir, is_symlink, canonicalize}`
   - `std::net::{TcpStream::connect, TcpStream::connect_timeout, TcpListener::bind, UdpSocket::bind}`, plus `TcpStream::peek`, `TcpListener::{accept, incoming}`, `UdpSocket::{send, send_to, recv, recv_from, peek, peek_from}`
   - `Command::{output, status, spawn}`, `Child::{wait, wait_with_output}`
   - `Mutex::lock`, `RwLock::{read, write}`, `Condvar::{wait, wait_timeout, wait_while}`, `Barrier::wait`, `mpsc::Receiver::{recv, recv_timeout}`, `SyncSender::send`
   Fix: move the work to `cx.background_spawn(...)` or an async fs API. NOTE: a plain `Path::exists()` in a render path is a lint hit - this catches people constantly.

The lint library has UI tests (`tooling/lints/ui/*.rs` + matching `.stderr`) run by `cargo test` inside `tooling/lints`, with `dylint_testing` built with the `deny_warnings` feature so nightly drift surfaces as a failing test. `tooling/lints/test_fixture/` holds a mini `gpui` crate the UI tests build against.

### 4d. `tooling/xtask` - workspace-integrity gates

`cargo xtask <subcommand>`:

| Subcommand | What it does |
|---|---|
| `clippy [--fix] [-p PKG]` | same as `script/clippy` but in Rust: `--workspace --release --all-targets --all-features -- --deny warnings` |
| `licenses` | every workspace crate must have a `LICENSE-APACHE`/`LICENSE-GPL` symlink |
| `package-conformity` | every crate must use `[lints] workspace = true`; every dep must be `workspace = true` (extensions exempt) |
| `publish-gpui` | publishes GPUI and its deps to crates.io |
| `wsl-sandbox-tests [--require-enforced] [--release]` | Windows-only; drives `windows_wsl::wrap_invocation`. BROKEN HERE - references a `sandbox` crate and `script/test-wsl-sandbox.ps1`, neither of which exists in this fork |
| `setup-webrtc [--force] [--triple T] [--no-cargo-config]` | downloads the pinned `webrtc-sys` release into `.webrtc-sys/` and sets `LK_CUSTOM_WEBRTC` in `~/.cargo/config.toml` |
| `web-examples [--release] [--port N] [--no-serve]` | builds and serves `crates/gpui/examples` for wasm |

HIDDEN BUT IMPORTANT: `tooling/xtask/src/workspace.rs` contains `#[test] fn no_forbidden_dependencies_between_feature_crates()`. It BFS-searches the non-dev dependency graph from `cargo metadata` and FAILS if any of these edges exist directly or transitively:

```
agent_ui         -> git_ui           file_finder    -> project_panel
git_ui           -> agent_ui         git_ui         -> search
open_path_prompt -> project_panel    picker         -> editor
project_panel    -> git_ui           project_panel  -> search
search           -> git_ui           search         -> project_panel
sidebar          -> git_ui           title_bar      -> git_ui
```

Reason given in the test: such edges chain large UI crates and serialize the build, hurting incremental compile times. Dev-dependencies are exempt. Run `cargo test -p xtask` after any cross-crate dependency change. The correct fix is always to extract shared code into a lower-level crate, never to merge the two crates.

### 4e. `tooling/perf` - its own much stricter lint set

`tooling/perf/Cargo.toml` opts that crate (only) into: `clippy::all` / `pedantic` / `style` = warn, `missing_docs` = warn, `missing_docs_in_private_items` = warn, and:
- DENY: `as_underscore`, `allow_attributes`, `allow_attributes_without_reason`
- FORBID: `let_underscore_must_use`, `undocumented_unsafe_blocks`, `missing_safety_doc`

If you edit `tooling/perf`: document everything including private items, give every `unsafe` block a `// SAFETY:` comment, and never use a bare `#[allow]` - a `reason = "..."` is mandatory (and `#[expect]` is covered by the same deny).

---

## 5. CI gates - what will fail your PR

There are exactly TWO workflows. Full `.github` inventory:
```
.github/workflows/release.yml
.github/workflows/upstream-sync.yml
.github/pull_request_template.md
.github/release/body.md
.github/ISSUE_TEMPLATE/{10_bug_report,11_crash_report,config}.yml
.github/DISCUSSION_TEMPLATE/feature-requests.yml
```

### THERE IS NO PR/PUSH CI
No build, test, clippy, fmt, prettier, keymap, shellcheck or typos job runs on pull requests or on pushes to `main`. Nothing mechanical will catch a broken build before merge. This inverts the usual advice: because CI will not catch it, you MUST run `script/clippy` (at minimum scoped to the crates you touched) and `cargo fmt --all` locally before declaring done. Human review is the only gate.

### `.github/workflows/release.yml` - triggered by `push: tags: v*`
Job graph: `check_version` -> (`bundle_mac`, `bundle_linux`, `bundle_windows`) -> `release`.
Env: `CARGO_TERM_COLOR=always`, `CARGO_INCREMENTAL=0`. Permissions: `contents: write`.

1. `check_version` (ubuntu-24.04) - THE ONLY AUTOMATED CORRECTNESS GATE IN THE REPO:
   ```sh
   version=$(sed -n 's/^version = "\(.*\)"/\1/p' crates/wu/Cargo.toml | head -1)
   [ "v${version}" = "${GITHUB_REF_NAME}" ] || exit 1
   ```
   The git tag must exactly equal `v` + the `version` in `crates/wu/Cargo.toml`. Bump `crates/wu/Cargo.toml` before tagging.
2. `bundle_mac` (macos-14, timeout 300 min): `rustup show`, `brew install lld`, `echo stable > crates/wu/RELEASE_CHANNEL`, optional provisioning profile from `secrets.MACOS_PROVISIONING_PROFILE`, then `script/bundle-mac aarch64-apple-darwin`. Uploads `Wu-aarch64.dmg` + `wu-remote-server-macos-aarch64.gz`, and separately `Wu.dSYM.zip`. `if-no-files-found: error`.
3. `bundle_linux` (matrix: ubuntu-22.04 x86_64, ubuntu-22.04-arm aarch64; 300 min; `CC=clang CXX=clang++`): `./script/linux`, `rustup show`, `echo stable > crates/wu/RELEASE_CHANNEL`, `./script/bundle-linux`.
4. `bundle_windows` (windows-2022, 300 min): `rustup show`, `Set-Content -Path crates/wu/RELEASE_CHANNEL -Value stable -NoNewline`, then `script/bundle-windows.ps1 -Architecture x86_64` under `pwsh`.
5. `release` (ubuntu-24.04): downloads all artifacts merged, deletes `*.dSYM.zip`, then `softprops/action-gh-release@v2` with `body_path: .github/release/body.md` and `generate_release_notes: true`.

All three bundle jobs OVERWRITE `crates/wu/RELEASE_CHANNEL` to `stable` at build time; the file is `dev` in the tree.

### `.github/workflows/upstream-sync.yml` - cron `0 6 * * *` + `workflow_dispatch`
Permissions `contents: write`, `pull-requests: write`; concurrency group `upstream-sync`, `cancel-in-progress: false`.
1. Reads `UPSTREAM_VERSION`, `git ls-remote --tags` against `zed-industries/zed`, filters `^v[0-9]+\.[0-9]+\.[0-9]+$`, `sort -V | tail -1` -> newest stable tag. Skips if already synced or if branch `sync/<tag>` already exists.
2. `git switch -c sync/<tag>`, runs `script/upstream-sync <tag> | tee sync.log`. EXIT-CODE CONTRACT: 0 = clean, 2 = committed with conflict markers, >2 = hard failure (job aborts).
3. Pushes and opens a PR titled `Sync Zed <tag>` (clean) or `Sync Zed <tag> (needs resolution)`; body embeds `sync.log` and ends with `Release Notes:` / `- N/A`.

### PR conventions reviewers enforce (`.github/pull_request_template.md` + `.rules`)
- Sections: Objective / Solution / Testing / Self-Review Checklist / (optional) Showcase, then a final `Release Notes:` section.
- Release notes: exactly one bullet, blank line after the heading:
  ```
  Release Notes:

  - N/A
  ```
  or `- Added ...` / `- Fixed ...` / `- Improved ...` for user-facing changes.
- PR TITLE: imperative, correctly capitalized, NO conventional-commit prefixes (`fix:`, `feat:`, `docs:`), NO trailing punctuation. Optionally prefix with a crate name: `git_ui: Add history view`.
- Self-review checklist explicitly includes "Unsafe blocks (if any) have justifying comments".

### The `.rules` README tripwire - FLAGGING THIS EXPLICITLY
`CLAUDE.md` and `AGENTS.md` each contain only the string `.rules`, pointing at the root `.rules` file. That file contains a rule labelled "HARD RULE": when modifying any source files, prepend to `README.md`:

```
> [!IMPORTANT]
> Remove this line to confirm you've reviewed this PR before submitting.
```

and it says never to remove those lines yourself. `README.md` in the working tree does NOT currently contain them. This is a human-in-the-loop tripwire authored inside the repo, not a mechanical check, and it means editing a file (`README.md`) unrelated to the change at hand - surface it to the user and confirm before acting on it.

Other `.rules` items that affect build/lint outcomes:
- "Use `./script/clippy` instead of `cargo clippy`."
- Never `unwrap()`; propagate with `?`. Never `let _ = fallible()`; use `.log_err()` or explicit `match` / `if let Err(...)`.
- No `mod.rs`; use `[lib] path = "..."` and descriptive root filenames.
- In GPUI tests prefer `cx.background_executor().timer(d).await` over `smol::Timer::after` (also enforced by `clippy.toml`).
- Rules hygiene: do NOT edit `.rules` inline during feature work - propose additions under a "Suggested .rules additions" heading in the PR description instead. Crate-specific rules belong in that crate's own `.rules` file.

---

## 6. Upstream sync - how it works

`UPSTREAM_VERSION` holds the last-synced Zed point. It is currently a bare SHA (`01acd0ee8e906dd0ec8b526fe08da94444a5e2af`), not a `vX.Y.Z` tag; `script/upstream-sync` handles both (`ref_for()` maps `v[0-9]*` to `refs/tags/...`, anything else is used raw).

`script/upstream-sync <zed-tag-or-sha>`:
- NO Zed history ever enters this repo. It shallow-fetches (`--depth=1 --no-tags`) the base and target trees into `refs/upstream/{base,target}`, builds two synthetic commits with `git commit-tree`, runs `git merge-tree --write-tree --name-only`, and commits the result with HEAD as the ONLY parent.
- Preconditions: git >= 2.38 and a clean working tree (`git status --porcelain --untracked-files=no` must be empty), else exit 1.
- Ordering guard: compares `crates/zed/Cargo.toml` versions at base vs target, because stable branches fork from main before their tag commit is made so commit dates cannot order them. If target predates base it exits 0 with "nothing to sync".
- DROPPED_PREFIXES - upstream additions under these are discarded and listed at the end:
  `.agents/ .cloudflare/ .factory/ .github/ .wezel/ .zed/ assets/prompts/ assets/sounds/ ci/ docs/ extensions/ legal/ nix/ tooling/ crates/extension_api/wit/ Dockerfile compose.yml Procfile default.nix flake.lock flake.nix shell.nix livekit.yaml lychee.toml renovate.json REVIEWERS.conl GEMINI.md CONTRIBUTING.md CODE_OF_CONDUCT.md .mailmap .git-blame-ignore-revs`
  PLUS: any `crates/<x>/...` whose crate dir does not exist in HEAD is dropped. That is the mechanism by which crates Wu deleted stay deleted.
- REVIEW_PREFIXES = `crates/zed/` - exempt from dropping, because `crates/zed` became `crates/wu`. Rename detection maps most edits across; whatever falls through lands here and needs a human to port or discard.
- PINNED_FILES = `crates/wu/RELEASE_CHANNEL` - Wu's version always wins.
- `UPSTREAM_VERSION` is rewritten to the new target in the same commit.
- Commit message: `Sync Zed <target>` + blank line + `Upstream range: <base>..<target>`.
- Exit codes: 0 clean, 2 = some files committed WITH CONFLICT MARKERS (they are listed; resolve on the branch and `git commit --amend`), 1 = hard failure.
- Built-in hint it prints: "If Cargo.lock is listed: `git checkout --theirs Cargo.lock && cargo check`".

Resolving a sync PR: check out `sync/<tag>`, `git grep -n '<<<<<<<'`, resolve, `cargo check --workspace`, `./script/clippy`, then amend and force-push.

---

## 7. Traps / gotchas

1. THIS WORKING COPY IS NOT A GIT REPOSITORY. `C:/Users/USER/Documents/wu-main/.git` does not exist; `git rev-parse --is-inside-work-tree` fails. Consequences:
   - `script/check-keymaps` (uses `git grep`) cannot run.
   - `script/upstream-sync` cannot run.
   - `crates/wu/build.rs` degrades gracefully: it tries `git rev-parse HEAD` and, on failure, leaves the commit SHA unset. You can inject `ZED_COMMIT_SHA=<sha>` instead. The build still works.
   - `script/install-linux` and `script/cargo`'s `isZedRepo()` also degrade.
2. NO PR CI. Nothing catches a broken build for you. Run `pwsh script/clippy.ps1 -p <crate>` and `cargo fmt --all` yourself.
3. `script/clippy` uses `--release --all-targets --all-features`. Code that compiles under `cargo check` can still fail here: feature-gated code, `#[cfg(test)]` code, benches, examples, and `test-support` paths all get compiled. Always scope with `-p` while iterating; a full run is a coffee break at minimum.
4. `script/clippy.ps1` is NOT equivalent to `script/clippy`. The PowerShell version skips `cargo shear`, `typos`, and `buf`. Do not assume parity.
5. `typos.toml` DOES NOT EXIST, yet bash `script/clippy` runs `typos --config typos.toml` whenever `typos` is on PATH -> spurious failure. Likewise there is NO `deny.toml`, NO `.editorconfig`, and NO `typos`/`cargo-deny` config anywhere in the repo, despite those being common in Zed-family repos.
6. MOST SCRIPTS ARE BASH-ONLY. PowerShell equivalents exist only for: `clippy`, `bootstrap`, `generate-licenses`, `get-crate-version`, `install-rustup`, `bundle-windows`. Everything else (`check-keymaps`, `prettier`, `new-crate`, `shellcheck-scripts`, `update-json-schemas`, `crate-dep-graph`, `upstream-sync`, all other bundle scripts, `download-wasi-sdk`, `install-cmake`, `linux`, `freebsd`, `remote-server`) needs Git Bash or WSL.
7. `script/new-crate` creates a SYMLINK (`ln -sf ../../LICENSE-GPL`). On Windows this needs Developer Mode or an elevated shell, and Git Bash needs `MSYS=winsymlinks:nativestrict`. If the symlink degrades to a plain copy, `cargo xtask licenses` reports "is not a symlink".
8. DEAD / STALE scripts and config - do not trust these blindly:
   - `script/bootstrap`, `script/bootstrap.ps1` - target `crates/collab`, which does not exist here.
   - `script/crate-dep-graph` - `--root=zed,cli,collab`, all wrong for this fork.
   - `cargo xtask wsl-sandbox-tests` - references a `sandbox` crate and `script/test-wsl-sandbox.ps1`, neither present.
   - `clippy.toml`'s `ignore-interior-mutability` names `agent_ui::context::AgentContextKey`; `crates/agent_ui` is gone.
   - `.wu/settings.json` sets `RUST_DEFAULT_PACKAGE_RUN: "zed"` (should be `wu`) and excludes `crates/agent/src/tools/evals/fixtures`, a path that no longer exists.
   - `.config/nextest.toml` overrides reference `collab`, `vim`, `language_model` packages that do not exist.
   - `script/prettier` and `script/new-crate` still print "the Zed repo" in their messages.
9. `profile.dev` sets `debug = 0`. A default `cargo build` gives you NO debuginfo. Use `--profile dbg` (dev + `debug = "full"`) for a debugger, or `--profile release-fast`.
10. Windows `rustflags` REPLACE rather than extend. The `[target.'cfg(target_os = "windows")']` block in `.cargo/config.toml` drops `-C symbol-mangling-version=v0` and `--cfg tokio_unstable` from `[build] rustflags`. If something depends on `tokio_unstable` on Windows, that is why it breaks. (The `aarch64-apple-darwin` block re-lists them precisely to avoid this; Windows does not.)
11. `jobs = 4` in `.cargo/config.toml` throttles the entire build regardless of your core count.
12. `--all-features` in `script/clippy` enables `wu`'s `visual-tests`, `inspector`, `tracy`, `track-project-leak`, and `test-support` features, which pull in `gpui/test-support`, `gpui/leak-detection`, `ztracing/tracy`, `dep:image`, `dep:tempfile`, etc. A change that is fine for the default build can break under the feature union.
13. Adding a git dependency without a `rev` breaks convention (`calloop` is the sole exception, and only accidentally). Always pin by rev.
14. `Cargo.lock` IS COMMITTED (339 KB) and `.wu/settings.json` marks `**/*.lock` read-only in the editor. Do not hand-edit; let cargo regenerate it, and prefer `cargo update -p <one-crate>` over a blanket `cargo update`.
15. `.gitignore` hides generated artifacts you might otherwise think are missing: `/assets/*licenses.*`, `/crates/theme/schemas/theme.json`, `/inno`, `/crates/wu/test_fixtures/visual_tests/`, `.perf-runs`, `.webrtc-sys/`, `**/target`, `**/cargo-target`, `.claude/settings.local.json`, `/node_modules/`, `/script/node_modules`.
16. `.gitattributes` forces `crates/wu/resources/windows/zed.sh` to LF (`text eol=lf`) - it becomes `bin/wu` inside the Windows bundle and must not get CRLF. It also marks `*.json linguist-language=JSON-with-Comments`.
17. TRAILING COMMAS AND COMMENTS IN THIS REPO'S JSON ARE INTENTIONAL (it is JSONC - see `.wu/tasks.json` and `.wu/settings.json`). Do not "fix" them, and do not run stock `prettier --parser=json` over `assets/**`; `script/prettier` passes `--parser=jsonc` for `assets/settings/default.json` specifically.
18. The dylint lints need a nightly toolchain and a slow first build (`cargo dylint` downloads `nightly-2026-03-21` plus `rustc-dev`). Do not kick that off casually mid-task.
19. `cargo shear` (run by bash `script/clippy` locally) fails on unused deps - if you remove the last use of a dependency, remove it from that crate's `Cargo.toml` too.
20. `buf` lint/format targets `crates/proto/proto` (which does exist: `app.proto`, `buffer.proto`, `core.proto`, `debugger.proto`, `download.proto`, `git.proto`, `image.proto`, `lsp.proto`, `task.proto`, plus `buf.yaml`). If you touch a `.proto`, run `buf format -w crates/proto/proto` and `buf lint crates/proto/proto`.
21. There is no `.github/workflows/ci.yml`, no `docs/src/development*` page in this fork, and `README.md` just points at Zed's development docs. Do not expect fork-specific build documentation to exist.

---

## 8. Files worth opening directly

| Path | Why |
|---|---|
| `C:/Users/USER/Documents/wu-main/Cargo.toml` | workspace members, `[workspace.dependencies]`, `[patch.crates-io]`, profiles, `[workspace.lints]`, `[workspace.metadata.dylint]` |
| `C:/Users/USER/Documents/wu-main/.cargo/config.toml` | rustflags, aliases (`xtask`, `perf-test`, `perf-compare`), per-target overrides |
| `C:/Users/USER/Documents/wu-main/clippy.toml` | disallowed methods (denied) |
| `C:/Users/USER/Documents/wu-main/rust-toolchain.toml` | 1.97.1 + wasm/musl targets |
| `C:/Users/USER/Documents/wu-main/rustfmt.toml` | edition/style_edition 2024 only |
| `C:/Users/USER/Documents/wu-main/.config/nextest.toml` | 60s default test timeout + per-test overrides |
| `C:/Users/USER/Documents/wu-main/.rules` | the coding rules every agent session is expected to follow (CLAUDE.md / AGENTS.md just contain the string `.rules`) |
| `C:/Users/USER/Documents/wu-main/script/clippy` and `script/clippy.ps1` | the lint entrypoints |
| `C:/Users/USER/Documents/wu-main/script/upstream-sync` | the entire fork-sync algorithm, incl. DROPPED_PREFIXES |
| `C:/Users/USER/Documents/wu-main/script/bundle-windows.ps1` | Windows packaging, SDK / Inno Setup prerequisites |
| `C:/Users/USER/Documents/wu-main/script/new-crate` | crate scaffolding + license policy |
| `C:/Users/USER/Documents/wu-main/tooling/lints/README.md` and `tooling/lints/src/lib.rs` | the six custom lints |
| `C:/Users/USER/Documents/wu-main/tooling/xtask/src/workspace.rs` | FORBIDDEN_DEPENDENCIES test |
| `C:/Users/USER/Documents/wu-main/tooling/xtask/src/tasks/package_conformity.rs` | workspace-dep + workspace-lints enforcement |
| `C:/Users/USER/Documents/wu-main/tooling/perf/Cargo.toml` | the strict per-crate lint set |
| `C:/Users/USER/Documents/wu-main/.github/workflows/release.yml` | the only automated version gate |
| `C:/Users/USER/Documents/wu-main/.github/workflows/upstream-sync.yml` | nightly Zed sync automation |
| `C:/Users/USER/Documents/wu-main/.wu/tasks.json` | the two tasks the project itself defines: `./script/clippy`, `cargo run --profile release-fast` |
