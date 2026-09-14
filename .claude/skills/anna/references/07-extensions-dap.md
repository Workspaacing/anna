# 07 — Extension system & DAP stack in Anna

Paths are relative to the repo root unless stated.

## 1. How the extension system works

### Runtime
- **Wasmtime component model + WASI p2.** Engine at `crates/extension_host/src/wasm_host.rs:456` (`wasm_engine`): `wasm_component_model(true)`, incremental compilation cache, and **epoch interruption** with a 100 ms ticker so a misbehaving extension cannot block the executor inside `Future::poll`. Each store gets `set_epoch_deadline(1)` + `epoch_deadline_async_yield_and_update(1)`.
- Guest target is **`wasm32-wasip2`** (`crates/extension/src/extension_builder.rs:28`).
- Extension calls run on a **tokio** runtime via `gpui_tokio::Tokio::spawn`; compile/parse runs on gpui's background executor.

### Sandbox
`WasmHost::build_wasi_ctx` (`crates/extension_host/src/wasm_host.rs:~635`):
- Work dir `<data_dir>/extensions/work/<extension-id>` is preopened **read-write** twice — as `"."` and at its absolute path. `PWD` is set to it. `inherit_stdio()` is on, so extension `println!` goes to Anna's stdout.
- Guest `chdir` is stubbed to `NOTSUP` (`crates/extension_api/src/extension_api.rs:276`).
- Path escapes blocked by `WasmHost::writeable_path_from_extension` (lexical normalize + canonicalize nearest existing ancestor).

### WIT interface definitions
`crates/extension_api/wit/since_v0.<X>.<Y>/` — one full copy of the world per API version:
`since_v0.0.1`, `0.0.4`, `0.0.6`, `0.1.0`, `0.2.0`, `0.3.0`, `0.4.0`, `0.5.0`, `0.6.0`, `0.8.0`.

Files per dir: `extension.wit` (the world), `common.wit`, `github.wit`, `http-client.wit`, `lsp.wit`, `nodejs.wit`, `platform.wit`, `process.wit`, `dap.wit` (0.6.0+), plus a `settings.rs` copied into `OUT_DIR` by `crates/extension_host/build.rs` and `include!`d.

There is **no `since_v0.7.0` directory**, but `MAX_VERSION` of the 0.6.0 module is `0.7.0` (`crates/extension_host/src/wasm_host/wit/since_v0_6_0.rs:12`), so a `zed_extension_api = "0.7.0"` extension binds against the 0.6.0 world. `extensions/html` and `extensions/proto` depend on exactly that.

### API versioning / backward compat
- Host side: `crates/extension_host/src/wasm_host/wit.rs` has `mod since_v0_0_1 ... since_v0_8_0`, each a separate `wasmtime::component::bindgen!` against its own WIT dir, plus per-version conversion shims. `Extension::instantiate_async` picks the highest module whose `MIN_VERSION <= version`. Old versions stay compatible by never being edited.
- The guest's API version is stamped into a custom wasm section `zed:api-version` (6 big-endian bytes, written by `crates/extension_api/build.rs`, read by `parse_wasm_extension_version` in `crates/extension/src/extension.rs:159`). `strip_custom_sections` preserves `zed:api-version`, `name`, `component-type:*`, `dylink.0`.
- **Release-channel gate** (`wit.rs:57`, `wasm_api_version_range`): `Dev` → max `0.8.0`; `Stable` → max `0.7.0`. `authorize_access_to_unreleased_wasm_api_version` errors on Stable. `crates/anna/RELEASE_CHANNEL` contains `dev` in-tree; CI overwrites it with `stable` (`.github/workflows/release.yml:35,91,111`).
- Manifest schema version: `CURRENT_SCHEMA_VERSION = SchemaVersion(1)` (`crates/extension_host/src/extension_host.rs:104`); schema_version 0 = legacy `extension.json`.

### Capabilities / permissions
Two layers, but both are checked **only for `process:exec`**:
- Manifest-declared: `ExtensionManifest::capabilities` (`crates/extension/src/extension_manifest.rs:112`), kinds `process:exec`, `download_file`, `npm:install` (`crates/extension/src/capabilities.rs`). Wildcards: `*` = one arg, `**` = rest.
- Host-granted: `ExtensionSettings::granted_capabilities` from `granted_extension_capabilities`. **Defaults are fully permissive** — `assets/settings/default.json:1834-1838` grants `process:exec */**`, `download_file */**`, `npm:install *`.
- `CapabilityGranter` (`crates/extension_host/src/capability_granter.rs`): `grant_exec` checks **manifest AND settings**; `grant_download_file` and `grant_npm_install_package` check **settings only**.

## 2. `extensions/test-extension` — canonical minimal example

Files: `extension.toml`, `Cargo.toml`, `README.md`, `LICENSE-APACHE`, `src/test_extension.rs`, `languages/gleam/{config.toml,highlights.scm,indents.scm,outline.scm}`.

`extension.toml` required fields (`id`, `name`, `version`, `schema_version`; `description`, `authors`, `repository` required by the packaging CLI):

```toml
id = "test-extension"
name = "Test Extension"
description = "An extension for use in tests."
version = "0.1.0"
schema_version = 1
authors = ["..."]
repository = "https://github.com/zed-industries/zed"

[language_servers.gleam]
name = "Gleam LSP"
language = "Gleam"

[grammars.gleam]
repository = "https://github.com/gleam-lang/tree-sitter-gleam"
commit = "8432ffe32ccd360534837256747beb5b1c82fca1"

[[capabilities]]
kind = "process:exec"
command = "echo"
args = ["hello from a child process!"]
```

`Cargo.toml` must have `[lib] crate-type = ["cdylib"]` and depend on `zed_extension_api`. test-extension uses the **path** dep `../../crates/extension_api` (always in-tree v0.8.0), which is why it only loads on the Dev channel.

Layout auto-discovered by `populate_defaults` (`crates/extension/src/extension_builder.rs:645`): `languages/<lang>/config.toml` + `.scm` files, `themes/*.json`, `icon_themes/*.json`, `snippets.json`, `debug_adapter_schemas/<adapter>.json`, and (v0 only) `grammars/*.toml`. A `Cargo.toml` present means `lib.kind = Rust`.

`src/test_extension.rs` exercises `zed::register_extension!`, `Extension::new/language_server_command/label_for_completion`, `current_platform()`, rel+abs filesystem writes, `process::Command`, `latest_github_release`, `download_file` with `GzipTar`, `set_language_server_installation_status`.

Test entry point: `crates/extension_host/src/extension_store_test.rs:765` `test_extension_store_with_test_extension` uses `RealFs`, `CARGO_MANIFEST_DIR/../../extensions/test-extension`, `ExtensionStore::install_dev_extension(...)` (real `cargo build --target wasm32-wasip2` + real wasi-sdk grammar compile), stubs GitHub via `FakeHttpClient`. Slow and network/toolchain dependent.

## 3. Bundled extensions

All three are workspace members (`Cargo.toml:173-176`) but are **not compiled into the Anna binary** — they are `cdylib` wasm crates published to the registry and installed at runtime. No bundling step exists in `script/bundle-mac`, `script/bundle-linux`, `script/bundle-windows.ps1`, and `assets/` contains no extensions.

| Extension | Demonstrates | API dep |
|---|---|---|
| `extensions/glsl` | grammar (`theHamsta/tree-sitter-glsl`) + 5 query files + LSP adapter that tries `worktree.which("glsl_analyzer")`, else downloads a GitHub release zip | `zed_extension_api = "0.1.0"` |
| `extensions/html` | grammar + 6 query files (incl. `overrides.scm`) + **npm-based** LSP (`npm_package_latest_version` / `npm_install_package` of `@zed-industries/vscode-langservers-extracted`) + `language_ids` | `zed_extension_api = "0.7.0"` |
| `extensions/proto` | grammar + queries (incl. `textobjects.scm`) + **three** alternative language servers in one extension (`buf`, `protobuf-language-server`, `protols`) under `src/language_servers/` | `zed_extension_api = "0.7.0"` |

`extensions/.gitignore` is `grammars` — cloned/compiled grammar checkouts are build artifacts.

`assets/settings/default.json:1828` sets `"auto_install_extensions": { "html": true }`, so a fresh install **auto-downloads the `html` extension on first run**.

## 4. Extension API surface: supported vs removed

**Supported** (manifest keys, `crates/extension/src/extension_manifest.rs:84`):
- `languages` (config.toml + tree-sitter queries) — `ExtensionLanguageProxy`
- `grammars` (git repo + rev, compiled to wasm) — `ExtensionGrammarProxy`
- `language_servers` (+ `language_ids`, `code_action_kinds`) — `ExtensionLanguageServerProxy`
- `themes`, `icon_themes` — `ExtensionThemeProxy`
- `snippets` — `ExtensionSnippetProxy` (`crates/snippet_provider/src/extension_snippet.rs`)
- `debug_adapters` (+ `schema_path`), `debug_locators` — `ExtensionDebugAdapterProviderProxy`
- `capabilities`
- Host imports available to guests: `download-file`, `make-file-executable`, `get-settings`, `set-language-server-installation-status`, `github`, `http-client` (incl. streaming), `nodejs`, `process`, `platform`, `dap` (`resolve-tcp-template`), plus `worktree` / `project` / `key-value-store` resources.

**Present but dead in Anna:**
- `language_model_providers` in the manifest + `ExtensionLanguageModelProviderProxy` (`extension_host_proxy.rs:406`). **Nothing calls `register_language_model_provider_proxy`**, and `crates/extension_cli/src/main.rs:469` hard-rejects manifests using it (`LanguageModelProvidersUnsupported`).
- `suggest-docs-packages` / `index-docs` WIT exports exist in every WIT version and in the `Extension` trait, but have **no host consumer**.

**Stripped relative to Zed** (verified: zero hits for `slash-command`, `slash_command`, `context-server`, `context_server`, `SlashCommand`, `ContextServer` in `crates/extension*`, `crates/extension_api/wit/**`, `extensions/`):
- Slash commands, context servers / MCP, agent servers, indexed docs providers. No `agent`, `assistant`, `language_model`, `slash_command`, `context_server`, `indexed_docs` crates exist.

Only surviving trace: `cloud_api_types::ExtensionProvides` (`crates/cloud_api_types/src/extension.rs:36-64`) keeps `ContextServers`, `AgentServers`, `SlashCommands`, `IndexedDocsProviders` marked deprecated with an `is_deprecated()` helper, because the remote registry still returns them. `crates/extensions_ui/src/extensions_ui.rs:1343-1349` filters them (plus `Grammars`) out of the category buttons.

## 5. `crates/extension_host` — discovery, install, network

Layout under `paths::extensions_dir()` = `<data_dir>/extensions` (`crates/paths/src/paths.rs:369`):
- `installed/<id>/` — extension content (dev extensions are **symlinks** to the source dir)
- `work/<id>/` — WASI preopened rw dir
- `staging/` — tempdir for atomic unpack
- `build/` — `ExtensionBuilder` cache, incl. `build/wasi-sdk/`
- `index.json` — cached `ExtensionIndex`, loaded synchronously at startup (`extension_host.rs:406`), rebuilt only if mtime-stale or unparsable

**Discovery/reload**: `ExtensionStore::new` starts an fs watcher on `installed/` (`FS_WATCH_LATENCY` 100 ms) pushing ids into `reload_tx`; a loop debounces `RELOAD_DEBOUNCE_DURATION` (200 ms) then `rebuild_extension_index` → `extensions_updated` (diffs old/new index, unloads then loads themes/icon themes/languages/grammars/snippets/LSPs/DAP adapters/locators). `ReloadExtensions` action registered in `extension_host::init`.

**Dev extensions**: `install_dev_extension(path)` (`extension_host.rs:1087`) loads the manifest, uninstalls any non-dev extension with the same id, compiles with `CompileExtensionOptions::dev()`, then **symlinks** `installed/<id>` to the source dir. `rebuild_dev_extension` recompiles in place. UI actions: `anna::InstallDevExtension`, `anna::RebuildDevExtension { extension_id }` (`crates/extensions_ui/src/extensions_ui.rs:48,57`).

### Network — Anna points at Zed's registry
`fetch_extensions_from_api` (`extension_host.rs:781`) and the install/upgrade endpoints use `HttpClientWithUrl::build_zed_api_url` (`crates/http_client/src/http_client.rs:277`), mapping base `https://zed.dev` to **`https://api.zed.dev`**. The base comes from `ClientSettings::server_url` (`crates/client/src/client.rs:37`), default `"server_url": "https://zed.dev"` (`assets/settings/default.json:2248`), overridable via `ZED_SERVER_URL`.

Endpoints:
- `GET https://api.zed.dev/extensions?max_schema_version=&filter=&provides=`
- `GET https://api.zed.dev/extensions/updates?...&ids=<comma list of installed ids>` — **leaks the list of installed extension ids**
- `GET https://api.zed.dev/extensions/{id}`
- `GET https://api.zed.dev/extensions/{id}/download` and `/{id}/{version}/download`

Plus non-registry network from extension code paths: `https://api.github.com/...`, the npm registry via `NodeRuntime`, arbitrary `download-file` URLs, and `https://github.com/WebAssembly/wasi-sdk/releases/download/wasi-sdk-25/` (`crates/extension/src/extension_builder.rs:36`).

`SUPPRESSED_EXTENSIONS` (`extension_host.rs:110`) is a denylist filtered out of API responses *and* actively uninstalled locally: `snippets`, `ruff`, `ty`, `basedpyright`, `basher`, plus ACP agents `opencode`, `mistral-vibe`, `auggie`, `stakpak`, `codebuddy`, `autohand-acp`, `corust-agent`, `factory-droid`, `qqcode`.

**Remote/SSH**: `crates/extension_host/src/headless_host.rs` (`HeadlessExtensionStore`) mirrors extensions to remote servers; only extensions with language servers / debug adapters / locators are synced (`ExtensionManifest::remote_load`). Retry/backoff at `extension_host.rs:78-83`.

## 6. `crates/extension_cli` — packaging

Binary name **`zed-extension`** (`crates/extension_cli/Cargo.toml:11`). Not subcommand-based; three required flags (`crates/extension_cli/src/main.rs:27`):

```
zed-extension --source-dir <ext dir> --output-dir <out> --scratch-dir <build cache>
```

Pipeline (`main.rs:41-155`): load `extension.toml` → `ExtensionBuilder::compile_extension` (release) → `validate_extension_manifest` → `validate_extension_features` → `test_grammars` (loads each `.wasm` into a `tree_sitter::WasmStore`) → `test_languages` (compiles every `.scm`) → `test_themes` → `test_snippets` → `test_debug_adapter_schemas` → copy resources into `<out>/archive/` → `tar -czvf archive.tar.gz -C archive .` → write `<out>/manifest.json`.

Validation rules that bite: description required and **strictly longer than the name**; at least one non-empty author; `repository` must parse as a URL with a host; themes and icon themes must be the **only** feature in their extension; `language_model_providers` rejected outright.

There is **no publish/upload step**. Publishing is the registry's job; Anna has no publishing pipeline of its own.

## 7. `crates/grammars` — built-in grammars

`crates/grammars/src/grammars.rs` embeds `src/` via `rust_embed` (excluding `*.rs`) and exposes `native_grammars()`, `load_config(name)`, `load_queries(name)`, `get_file(path)`. Each language has `crates/grammars/src/<name>/config.toml` plus its `.scm` files. 23 dirs: bash, c, cpp, css, diff, gitcommit, go, gomod, gowork, javascript, jsdoc, json, jsonc, markdown, markdown-inline, python, regex, rust, tsx, typescript, yaml, zed-keybind-context.

Grammars are **statically linked Rust crates** (`tree-sitter-*`), all optional behind the `load-grammars` feature. Registered at `crates/languages/src/lib.rs:60` via `languages.register_native_grammars(grammars::native_grammars())`.

**WASI SDK** is only for *extension* grammars:
- In-app / CLI: `ExtensionBuilder::install_wasi_sdk_if_needed` (`crates/extension/src/extension_builder.rs:492`) honours `$WASI_SDK_PATH`, else downloads wasi-sdk 25 into `<cache_dir>/wasi-sdk`, shelling out to `tar`.
- `script/download-wasi-sdk` puts it in `./target/wasi-sdk`. **Referenced by nothing** — no CI job, no other script. It also does not export `WASI_SDK_PATH`, so running it alone does not make the builder use it.

Extension grammar compilation (`compile_grammar`, `extension_builder.rs:293`): `git init` + `git fetch --depth 1 origin <rev>` + `git checkout <rev>` into `<ext>/grammars/<name>/`, then
`clang -fPIC -shared -Os -Wl,--export=tree_sitter_<name> -o <name>.wasm -I src src/parser.c [src/scanner.c]`, skipped if the `.wasm` is newer than sources (`file_newer_than_deps`).

### Adding a grammar

**(a) Built-in / native:**
1. Add `tree-sitter-<lang>` to root `[workspace.dependencies]`.
2. Add it as `optional = true` in `crates/grammars/Cargo.toml` **and** to the `load-grammars` feature list.
3. Add `("<name>", tree_sitter_<lang>::LANGUAGE.into())` to `native_grammars()` (`crates/grammars/src/grammars.rs:17`).
4. Create `crates/grammars/src/<name>/config.toml` (see `crates/grammars/src/rust/config.toml` for the full field set) plus `highlights.scm`, `outline.scm`, `indents.scm`, `injections.scm`, `brackets.scm`, `overrides.scm`, `textobjects.scm` as needed.
5. Add a `LanguageInfo { name: "<name>", adapters, context, ... }` entry to `built_in_languages` in `crates/languages/src/lib.rs:90+`.

**(b) Extension-provided:** add `[grammars.<snake_case_name>]` with `repository`, `commit` and optional `path` to `extension.toml`. The name **must be snake_case** or the builder bails. Queries live in `languages/<lang>/*.scm`.

## 8. `language_extension` / `theme_extension` — host-side glue

- `crates/language_extension/src/language_extension.rs`: `init(lsp_access, proxy, language_registry)` registers one `LanguageServerRegistryProxy` as the grammar proxy, language proxy, **and** language-server proxy. `LspAccess` enum (`ViaLspStore` / `ViaWorkspaces` / `Noop`) is how the extension host reaches live `LspStore`s to restart servers on reload; Anna uses `ViaWorkspaces` (`crates/anna/src/main.rs:549`).
- `crates/language_extension/src/extension_lsp_adapter.rs` (765 lines) wraps a wasm extension as a `LspAdapter`.
- `crates/theme_extension/src/theme_extension.rs`: `ThemeRegistryProxy` bridging to `theme::ThemeRegistry` + `theme_settings::{load_user_theme, reload_theme, reload_icon_theme}`.

Init order in `crates/anna/src/main.rs`: `extension::init` (508) → `debug_adapter_extension::init` (544) → `language_extension::init` (549) → `extension_host::init` (596) → `theme_extension::init` (606) → `snippet_provider::init` (617) → `extensions_ui::init` (665). **Proxies must be registered before `extension_host::init`**, otherwise the proxy getters silently no-op.

## 9. `crates/extensions_ui`

`extensions_ui.rs` (1418 lines) is the `ExtensionsPage` workspace item, opened by `anna::Extensions` with an optional `ExtensionCategoryFilter`. Fuzzy filter over installed + remote, category buttons from `ExtensionProvides::iter()` minus deprecated/`Grammars`, `InstallDevExtension` (directory picker), `RebuildDevExtension`, version picker (`extension_version_selector.rs`), cards (`components/extension_card.rs`), and "feature upsells" linking to **`https://zed.dev/docs/...`** (lines 1015-1068) — stale links into Zed's docs. `extension_suggest.rs` maps ~50 file extensions to registry ids and shows an install notification.

## 10. DAP stack

- **`crates/dap`** — DAP client. `client.rs`, `transport.rs` (`StdioTransport` / `TcpTransport`, chosen at `transport.rs:94-98` from `DebugAdapterBinary.connection`), `adapters.rs` (`DebugAdapter` trait, `DapDelegate`, `DebugAdapterName`, `DebugTaskDefinition`, `DebugAdapterBinary`, `download_adapter_from_github`), `registry.rs` (`DapRegistry` global), `debugger_settings.rs`, `proto_conversions.rs`, `inline_value.rs`. `dap_types` re-exported wholesale.
- **`crates/dap_adapters`** — 5 built-ins registered in `dap_adapters::init` (`crates/anna/src/main.rs:595`): **CodeLLDB**, **Debugpy**, **JavaScript** (js-debug), **Delve**, **GDB**, plus `fake-adapter` under `test-support`. Binaries download lazily into `paths::debug_adapters_dir()`.
- **`crates/debug_adapter_extension`** — `DebugAdapterRegistryProxy` implements `ExtensionDebugAdapterProviderProxy`; `ExtensionDapAdapter` reads the extension's JSON schema at registration time and forwards `get_binary` / `config_from_zed_format` / `request_kind` into wasm; `ExtensionLocatorAdapter`.
- **`crates/debugger_ui`** — `DebugPanel` (loaded in `crates/anna/src/anna.rs:647`), `session/running/{stack_frame_list,variable_list,console,module_list,breakpoint_list,loaded_source_list,memory_view}`, `attach_modal`, `new_process_modal`, `persistence`, plus a large `src/tests/` suite.
- **`crates/debugger_tools`** — `dap_log.rs`, the DAP protocol log viewer (gated by `log_dap_communications` / `format_dap_log_messages`).

**Configuration & startup.** Scenarios are `task::DebugScenario { adapter, label, build?, config (flattened JSON), tcp_connection? }` (`crates/task/src/debug_format.rs:265`), grouped in a `DebugTaskFile` (bare JSON array). Sources:
- Project: **`.anna/debug.json`** (`crates/paths/src/paths.rs`), legacy fallbacks **`.wu/debug.json`** then **`.zed/debug.json`**, and **`.vscode/launch.json`** import (`:594`, `crates/task/src/vscode_debug_format.rs`).
- Global: `<config_dir>/debug.json` (`paths::debug_scenarios_file()`, `:335`).
- JSON schema generated from the live `DapRegistry` and bound to those filenames in `crates/json_schema_store/src/json_schema_store.rs:381,451-453`.

Launch flow: `DebugPanel::start_session(scenario, task_context, ...)` (`crates/debugger_ui/src/debugger_panel.rs:175`) → `DapRegistry::global(cx).adapter(&scenario.adapter)` → `dap_store.new_session(...)` → adapter `get_binary` (download if needed) → transport. Locators (`crates/project/src/debugger/locators/{cargo,go,node,python}.rs` + extension-provided) turn a build task into a filled-in `DebugScenario`.

**`debug.plist`** (repo root) is a macOS entitlements plist with only `com.apple.security.get-task-allow = true`. **Nothing references it** — `script/bundle-mac` signs with `crates/anna/resources/anna.entitlements`. Orphan inherited from Zed.

## 11. Tests & fixtures

- `crates/extension_host/src/extension_store_test.rs` (4222 lines): `test_load_plugin_queries`, `test_extension_store` (FakeFs + synthetic trees), `test_extension_store_with_test_extension` (real build), ~30 remote/headless sync tests.
- `crates/extension/src/extension_manifest.rs` tests: `allow_exec` wildcards, `build_debug_adapter_schema_path`, Windows path separators.
- `crates/extension/src/extension_builder.rs` tests: `file_newer_than_deps`, snippet-path defaulting.
- `crates/extension_host/src/capability_granter.rs` tests: manifest-vs-host capability interaction.
- `crates/extension_cli/src/main.rs:~680-860`: manifest/feature validation unit tests.
- `crates/extension_host/benches/extension_compilation_benchmark.rs`.
- Debugger: `crates/debugger_ui/src/tests/*` driven by `dap::FakeAdapter`.

## 12. Footguns

1. **Anna talks to `api.zed.dev`.** Extension browse/install/auto-update all go there, and `/extensions/updates` sends the installed-extension id list. `"auto_install_extensions": {"html": true}` triggers a download on first run. To honour the "no telemetry" promise, change `extension_host.rs:781,951,996` + `http_client.rs:277`.
2. **Release-channel gate on the API version.** In-tree `RELEASE_CHANNEL` is `dev`, so local builds accept v0.8.0; a release build caps at v0.7.0 and refuses v0.8.0 with a confusing error. `extensions/test-extension` uses the path dep, so v0.8.0, so it **will not load in a stable build**.
3. **Capability asymmetry.** `download_file` and `npm:install` are gated only by settings-level `granted_extension_capabilities`, which defaults to `*`/`**`. Declaring them in `extension.toml` is decorative. Only `process:exec` is double-checked.
4. **Never edit an existing `wit/since_vX.Y.Z/` dir or its `since_vX_Y_Z.rs` bindings** — that silently breaks every published extension pinned to that version. Add a new dir, a new host module, a new arm in `Extension::instantiate_async`, bump `zed_extension_api` + `MIN/MAX_VERSION`, and note it in `crates/extension_api/PENDING_CHANGES.md`.
5. **`crates/extension_host/build.rs` copies `../extension_api/wit/**/*.rs` into `OUT_DIR`.** A new WIT dir without its `settings.rs` fails with an obscure `include!` error.
6. **Dev extensions are symlinks.** The fs watcher then fires on your working tree; deleting the source leaves a dangling symlink. On Windows, symlink creation may need Developer Mode.
7. **Grammar names must be snake_case** or `compile_extension` bails; the export symbol is `tree_sitter_<name>`.
8. **Grammar recompiles are mtime-based** and only look at `src/parser.c` / `src/scanner.c`. A `scanner.cc` (C++) grammar is silently not compiled.
9. **wasi-sdk download is ~100 MB** and shells out to `tar` and `git`; `script/download-wasi-sdk` writes to `./target/wasi-sdk`, which the builder does **not** look at unless you export `WASI_SDK_PATH`.
10. **Dead API surface**: `language_model_providers` and `suggest-docs-packages`/`index-docs`. Do not build on either.
11. **`SUPPRESSED_EXTENSIONS` actively uninstalls** matching extensions on every reload.
12. **Proxy registration order matters.** `ExtensionHostProxy` methods no-op when the sub-proxy is unset, so registering after `extension_host::init` yields a silent "installed but nothing happened".
13. **`debug.plist` is an orphan**; `crates/anna/resources/anna.entitlements` signs macOS builds.
14. **`extensions/README.md`, the extension_api README/compat table, and the `extensions_ui` upsell links still say "Zed"** and point at zed.dev — stale docs, not behaviour.
15. `Cargo.toml` lists the four `extensions/*` crates as workspace members with `crate-type = ["cdylib"]`; a plain `cargo build --workspace` builds them for the **host** target, which is not what you want.
