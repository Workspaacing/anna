# Wu — Settings, Keymaps, Actions, Themes, Tasks, Snippets

Repo: `C:/Users/USER/Documents/wu-main` (fork of Zed, upstream pin in `UPSTREAM_VERSION` = `01acd0ee8e906dd0ec8b526fe08da94444a5e2af`).
Everything below is READ-ONLY analysis; no project files were modified.

---

## 0. Executive summary of the non-obvious bits

1. **Settings schema types no longer live in the consuming crates.** They all live in
   `crates/settings_content/`. Consuming crates define a *runtime* struct + `impl Settings::from_settings(&SettingsContent)`.
2. **`Settings::register(cx)` is essentially dead.** Registration is now automatic via
   `#[derive(settings::RegisterSetting)]` -> `inventory::submit!` -> `SettingsStore::load_settings_types()`.
   Only 10 call sites of `::register(cx)` remain, all inside `#[cfg(test)]` blocks in `git_ui`/`git_ui_core`.
3. **`SettingsSources` no longer exists.** Layering is done *before* the typed structs are built, by
   `MergeFrom` on `SettingsContent`. `from_settings` receives one already-merged `&SettingsContent`.
4. **`script/update-json-schemas` has NOTHING to do with Wu's settings schema.** It only re-downloads
   `tsconfig.json` / `package.json` from SchemaStore. Wu's settings/keymap/tasks/theme schemas are
   generated **at runtime** from `schemars` derives and served over a `wu://schemas/...` URI to the
   JSON language server. **There is no schema file to regenerate and commit.**
5. **Task variables were NOT renamed.** They are still `$ZED_FILE`, `$ZED_WORKTREE_ROOT`, ... —
   `ZED_VARIABLE_NAME_PREFIX = "ZED_"` in `crates/task/src/task.rs:254`.
6. Project-local config dir is `.wu/` (with `.zed/` kept as a legacy fallback, and `.vscode/` for imports).
7. The default keymap has already been re-tuned to be VS Code-like; the `keymaps/*/vscode.json`
   base keymaps are now just small *delta overlays*.

---

## 1. `assets/` inventory (what ships in the binary)

Top-level: `C:/Users/USER/Documents/wu-main/assets/`

| Dir | Files | Notes |
|---|---|---|
| `assets/badge/` | 1 | `v0.json` — shields.io badge definition (not embedded) |
| `assets/fonts/` | 10 | 2 families, see below |
| `assets/icons/` | 191 top-level `.svg` + `LICENSES` | UI icon set |
| `assets/icons/file_icons/` | 81 | file-type icons |
| `assets/icons/knockouts/` | 6 | `dot_bg/fg`, `triangle_bg/fg`, `x_bg/fg` |
| `assets/images/` | 4 | `wu_icon.png`, `wu_logo.svg`, `screenshot-dark.png`, `screenshot-light.png` |
| `assets/keymaps/` | 19 | see section 3 |
| `assets/settings/` | 9 | see below |
| `assets/themes/` | 10 | 4 theme families + licenses |

**There is no `assets/sounds/` and no `assets/prompts/` directory** (both exist upstream in Zed; Wu strips
collab audio and the agent prompt templates).

### Fonts
- `assets/fonts/lilex/Lilex-{Regular,Italic,Bold,BoldItalic}.ttf` + `OFL.txt` — the monospace font; `.ZedMono` aliases to it.
- `assets/fonts/ibm-plex-sans/IBMPlexSans-{Regular,Italic,SemiBold,SemiBoldItalic}.ttf` + `license.txt` — UI font; `.ZedSans` aliases to it.

### Themes (bundled)
- `assets/themes/one/one.json` (One Dark / One Light) + `LICENSE`
- `assets/themes/ayu/ayu.json` (Ayu Dark / Light / Mirage) + `LICENSE`
- `assets/themes/gruvbox/gruvbox.json` (6 variants) + `LICENSE`
- `assets/themes/catppuccin/catppuccin.json` (Latte / Frappe / Macchiato / Mocha) + `LICENSE`
- `assets/themes/LICENSES` — aggregated
- `assets/themes/.gitkeep`

Default theme in `assets/settings/default.json:9-14`:
```jsonc
"theme": { "mode": "system", "light": "Catppuccin Latte", "dark": "Catppuccin Mocha" },
"icon_theme": "Wu (Default)",
```

### `assets/settings/`
| File | Loaded by |
|---|---|
| `default.json` (2378 lines) | `settings::default_settings()` — `crates/settings/src/settings.rs:133` |
| `default_semantic_token_rules.json` | `settings::default_semantic_token_rules()` — `settings.rs:137` |
| `initial_user_settings.json` | template written to `~/.config/Wu/settings.json` on first run |
| `initial_local_settings.json` | template for `.wu/settings.json` |
| `initial_server_settings.json` | remote/server settings template |
| `initial_tasks.json` | template for `tasks.json` (fully commented reference) |
| `initial_local_debug_tasks.json`, `initial_debug_tasks.json` | debug.json templates |
| `initial_worktree_setup_tasks.json` | tasks with `"hooks": ["create_worktree"]` |

`default.json` begins with `"$schema": "wu://schemas/settings"`.

### Embedding
Two separate `RustEmbed` roots — this matters:
- `crates/assets/src/assets.rs:7-16` — `Assets` embeds `fonts/**`, `icons/**`, `images/**`, `themes/**`, `*.md`.
- `crates/settings/src/settings.rs:120-125` — `SettingsAssets` embeds `settings/*` and `keymaps/*`.

`Assets::load_fonts` (`crates/assets/src/assets.rs:40`) auto-registers every `.ttf` under `fonts/`, so
dropping a new `.ttf` in `assets/fonts/<family>/` is all that is needed to make it selectable.

---

## 2. The settings system end-to-end

### 2.1 `crates/settings_content` — the schema layer

This crate holds **only serde/schemars/MergeFrom "content" types** (all fields `Option<T>`), no gpui, no runtime logic.

| File | Contents |
|---|---|
| `src/settings_content.rs` (1223) | root `SettingsContent`, `UserSettingsContent`, `SettingsProfile`, `ReleaseChannelOverrides`, `PlatformOverrides`, `HideMouseMode`, `ReduceMotionMode`, `PixelSetting`, `InstrumentationSettingsContent` |
| `src/editor.rs` (1192) | `EditorSettingsContent` and friends |
| `src/project.rs` (905) | `ProjectSettingsContent`, `WorktreeSettingsContent`, LSP/DAP maps |
| `src/language.rs` (1331) | `AllLanguageSettingsContent`, `LanguageSettingsContent` |
| `src/theme.rs` (1469) | `ThemeSettingsContent`, `ThemeStyleContent`, `ThemeColorsContent`, `StatusColorsContent`, `HighlightStyleContent`, `PlayerColorContent`, `AccentContent`, `ThemeSelection`, `FontFamilyName`, `FontSize`, `ThemeName`, `IconThemeName`, `UiDensity`, `BufferLineHeight` |
| `src/workspace.rs` (1137) | `WorkspaceSettingsContent`, dock/tab/panel settings |
| `src/terminal.rs` (569) | `TerminalSettingsContent` |
| `src/title_bar.rs`, `src/extension.rs` | small sections |
| `src/merge_from.rs` (173) | the `MergeFrom` trait + blanket impls (documents the merge semantics) |
| `src/fallible_options.rs` (194) | `parse_json`, `deserialize`, `flattened_deserialize!` macro |
| `src/action.rs` (290) | `ActionName`, `ActionWithArguments`, `CommandAliasTarget` (used for `command_aliases` setting + keymap schema) |
| `tests/flatten_collision_probe.rs` | test asserting no key collision between named fields and flattened sections |

**Root shape** (`settings_content.rs:167-289`): six `#[serde(flatten)]` sections
(`project: ProjectSettingsContent`, `theme: Box<ThemeSettingsContent>`, `extension: ExtensionSettingsContent`,
`workspace: WorkspaceSettingsContent`, `editor: EditorSettingsContent`, `remote: RemoteSettingsContent`)
plus roughly 35 named `Option<...>` fields (`file_finder`, `git_panel`, `tabs`, `tab_bar`, `status_bar`,
`activity_bar`, `preview_tabs`, `auto_update`, `base_keymap`, `debugger`, `diagnostics`, `git`,
`global_lsp_settings`, `image_viewer`, `markdown_preview`, `hide_mouse`, `log`, `line_indicator_format`,
`outline_panel`, `project_panel`, `node`, `proxy`, `reduce_motion`, `server_url`, `credentials_url`,
`session`, `terminal`, `title_bar`, `modeline_lines`, `instrumentation`, ...).

**`UserSettingsContent`** (`settings_content.rs:404-420`) wraps it and adds:
- flattened `release_channel_overrides: ReleaseChannelOverrides` -> JSON keys `dev`, `nightly`, `preview`, `stable`
- flattened `platform_overrides: PlatformOverrides` -> JSON keys `macos`, `linux`, `windows`
- `profiles: IndexMap<String, SettingsProfile>`

**Custom `Deserialize`** — `flattened_deserialize!` (`fallible_options.rs:66-111`) generates a hand-rolled
`Deserialize` that pulls named fields out of the JSON object one-by-one (so a bad value for one key only
nulls that key and records an error, instead of failing the whole file), then hands the remainder to
each flattened section. Every named field must appear in the `options:`/`defaults:` list of that macro,
or the generated struct literal will not compile.

`#[with_fallible_options]` (from `settings_macros`) adds
`#[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::fallible_options::deserialize")]`
to every `Option<T>` field automatically.

### 2.2 `crates/settings` — the store

`crates/settings/src/settings_store.rs` (3178 lines) defines the `Settings` trait
(`settings_store.rs:60-124`). Required method: **`fn from_settings(content: &SettingsContent) -> Self`**.
Provided methods: `register(cx)` (legacy), `get(path, cx)`, `get_global(cx)`, `try_get(cx)`,
`try_read_global(cx, f)`, `override_global(value, cx)`. Associated const `PRESERVED_KEYS`.

- `settings_store.rs:74` docstring: *"This function **should** panic if default values are missing, and
  you should add a default to default.json for documentation."* That is why the idiomatic body is
  `content.foo.unwrap()`.
- `SettingsKey` (`settings_store.rs:48`) with `const KEY` / `const FALLBACK_KEY` still exists but is
  essentially unused (only re-exported from `settings.rs:51`).

**Registration** — `settings_store.rs:129` has `inventory::collect!(RegisteredSetting)`;
`load_settings_types()` (`:419`) iterates the inventory at `SettingsStore` construction.
The `RegisterSetting` derive (`crates/settings_macros/src/settings_macros.rs:85-105`) emits the
`inventory::submit!`. **Consequence: the crate defining the setting must actually be linked into the
binary** — hence the `pub fn init() {}` no-op trick in `wu_actions` and `menu`.

**Layering / merge order** — `recompute_values` (`settings_store.rs:1313-1360`):

```
default.json
  <- extension_settings
  <- global_settings.json      (config_dir()/global_settings.json)
  <- user settings.json        (skipped entirely if the active profile has base == "default")
      <- release_channel_overrides[dev|nightly|preview|stable]
      <- platform_overrides[macos|linux|windows]
  <- active settings profile
  <- server settings           (remote)
  <- project settings (.wu/settings.json), deepest directory wins
```

Precedence is also encoded in `impl Ord for SettingsFile` (`settings_store.rs:187`), with variants
`Default < Global < User < Server < Project`.

Merge semantics (`settings_content/src/merge_from.rs:1-15`): maps/structs merge deeply; a `None` option
is ignored; `Vec` and scalars overwrite. Escape hatches: `ExtendingVec`, `ExtendingSet`, `SaturatingBool`.

**Reading**

- `MySettings::get_global(cx)` — global value
- `MySettings::get(Some(SettingsLocation { worktree_id, path }), cx)` — project-local value
- `settings::SettingsStore::global(cx).merged_settings()` — raw merged content

There is **no** free function `settings::get::<T>(cx)` in this tree.

**Observing**: `cx.observe_global::<SettingsStore>(|cx| { ... }).detach();` — canonical example
`crates/wu/src/wu.rs:1731-1738` (`init_reduce_motion`).

**Writing**

- `settings::update_settings_file(fs, cx, |content, cx| { ... })` — `crates/settings/src/settings_file.rs:269`
- `settings::update_settings_file_with_completion(...)` — `settings_file.rs:277`, returns a completion receiver
- Both delegate to `SettingsStore::update_settings_file` (`settings_store.rs:616`), which performs a
  *surgical* text edit via `settings_json::update_value_in_json_text` so comments and formatting survive.

**File watching**: `SettingsStore::watch_settings_files` (`settings_store.rs:353`) watches
`paths::settings_file()` and `paths::global_settings_file()`. Wired up in `crates/wu/src/wu.rs:1740`.

### 2.3 `crates/settings_macros` — what the derives generate

`crates/settings_macros/src/settings_macros.rs`:

- `#[derive(MergeFrom)]` (`:22`) — field-wise `self.f.merge_from(&other.f)`; for enums it is a
  whole-value overwrite; panics for unions. It emits `impl crate::merge_from::MergeFrom`, so it is
  **only usable inside the `settings_content` crate**.
- `#[derive(RegisterSetting)]` (`:85`) — emits
  `settings::private::inventory::submit! { RegisteredSetting { settings_value, from_settings, id } }`.
  Requires `gpui` in the crate dependencies.
- `#[with_fallible_options]` attribute macro (`:109`) — adds the fallible serde attributes to every
  `Option<T>` field.

### 2.4 Other settings crates

- `crates/settings_json/` (2652 lines) — tree-sitter-based JSON/JSONC surgical editing:
  `update_value_in_json_text`, `replace_value_in_json_text`,
  `append_top_level_array_value_in_json_text`, `replace_top_level_array_value_in_json_text`,
  `parse_json_with_comments`, `infer_json_indent_size`. Used by both the settings and keymap writers.
- `crates/settings/src/vscode_import.rs` (1096) — `VsCodeSettings` into `SettingsContent`, using
  **exhaustive struct literals** (`settings_content()` at `:171`, `editor_settings_content()` at `:222`).
- `crates/settings/src/editorconfig_store.rs` (395) — `.editorconfig` support.
- `crates/settings/src/granted_write_path.rs`, `base_keymap_setting.rs`,
  `content_into_gpui.rs` (the `IntoGpui` trait, content types into gpui types),
  `editable_setting_control.rs`.

### 2.5 Config file locations (`crates/paths/src/paths.rs`)

`APP_NAME = "Wu"` (`paths.rs:19`). `config_dir()` is `%APPDATA%\Wu` on Windows, XDG config on Linux.

| Purpose | Path |
|---|---|
| User settings | `config_dir()/settings.json` (`:299`) |
| Global settings | `config_dir()/global_settings.json` (`:305`) |
| Settings backup | `config_dir()/settings_backup.json` (`:311`) |
| Keymap | `config_dir()/keymap.json` (`:317`), backup `keymap_backup.json` (`:323`) |
| Tasks | `config_dir()/tasks.json` (`:329`) |
| Debug scenarios | `config_dir()/debug.json` (`:335`) |
| Themes | `config_dir()/themes/` (`:393`) |
| Snippets | `config_dir()/snippets/` (`:399`) |
| Prompts | `config_dir()/prompts/` (`:407`) |
| Global agent file | `config_dir()/AGENTS.md` (`:344`) |
| Project settings | `.wu/settings.json` (`:532`), legacy `.zed/settings.json` (`:540`) |
| Project tasks | `.wu/tasks.json` (`:547`), legacy `.zed/tasks.json` (`:555`), VS Code `.vscode/tasks.json` (`:562`) |
| Project debug | `.wu/debug.json` (`:578`), legacy `.zed/debug.json` (`:586`) |

`paths::resolve_local_config_path` (`:519`) picks `.wu` over `.zed` unless only `.zed` exists.

---

## 3. RECIPE A — Add a new user setting, step by step

Derived from a real trace of `reduce_motion` (a Wu-era addition), plus `hide_mouse` and
`instrumentation.performance_profiler.enabled`.

### Step 1 — Declare the value type (only if it is a new enum/struct)

File: `crates/settings_content/src/settings_content.rs` (or `editor.rs` / `theme.rs` / `project.rs` /
`workspace.rs` / `terminal.rs`, depending on section).

```rust
/// Doc comment here - this becomes the JSON-schema description shown in the editor.
///
/// Default: off
#[derive(Copy, Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq,
         JsonSchema, MergeFrom, strum::VariantArray, strum::VariantNames)]
#[serde(rename_all = "snake_case")]
pub enum ReduceMotionMode { On, #[default] Off }
```

`strum::VariantArray` + `VariantNames` are what let the settings-UI dropdown enumerate options.
Reference: `crates/settings_content/src/settings_content.rs:140-166`.

### Step 2 — Add the field to the content struct

Same file, for example on `SettingsContent`:

```rust
/// Doc comment - user facing.
///
/// Default: off
pub reduce_motion: Option<ReduceMotionMode>,
```

(`settings_content.rs:259`.) Always `Option<T>`; `#[with_fallible_options]` on the struct handles the
serde attributes.

### Step 3 — Register the field in `flattened_deserialize!`

Same file, below the struct:

```rust
fallible_options::flattened_deserialize!(SettingsContent {
    sections: { project, theme, extension, workspace, editor, remote },
    options: { /* ... */ reduce_motion /* ... */ },
    defaults: {},
});
```

(`settings_content.rs:318-331`.) **Skipping this is a compile error**, but the error points at the macro
rather than at your field.

### Step 4 — Write the default into `assets/settings/default.json`

```jsonc
// Whether to reduce non-essential motion in the UI ...
"reduce_motion": "off",
```

(`assets/settings/default.json:274`.) This is mandatory in practice, because `from_settings`
implementations call `.unwrap()`. Keep the comment: `default.json` doubles as the reference doc
(`wu: open default settings`).

### Step 5 — Fix `vscode_import.rs`

`crates/settings/src/vscode_import.rs:171-218` builds `SettingsContent { ... }` **exhaustively**.
Add either a real mapping or `field: None`:

```rust
reduce_motion: self.read_enum("workbench.reduceMotion", |s| match s {
    "on"  => Some(ReduceMotionMode::On),
    "off" => Some(ReduceMotionMode::Off),
    _     => None,
}),
```

If you add to `EditorSettingsContent`, `GutterContent`, etc., the same applies to their builder fn.
This is the number-one "why does it not compile" surprise.

### Step 6 — Define or extend the runtime `Settings` type in the consuming crate

```rust
use settings::{RegisterSetting, Settings};

#[derive(Copy, Clone, Debug, RegisterSetting)]
struct ReduceMotionSetting(settings::ReduceMotionMode);

impl Settings for ReduceMotionSetting {
    fn from_settings(content: &settings::SettingsContent) -> Self {
        Self(content.reduce_motion.unwrap_or_default())
    }
}
```

(`crates/wu/src/wu.rs:1722-1729`.) For a nested section the idiom is
`content.image_viewer.clone().unwrap().unit.unwrap()`
(`crates/image_viewer/src/image_viewer_settings.rs:15-17`).
**Do NOT call `Settings::register(cx)`** — the derive handles it. Make sure the crate is actually
linked (add a `pub fn init() {}` and call it from `wu` if the crate would otherwise be dead-stripped).

### Step 7 — Consume it

```rust
let v = ReduceMotionSetting::get_global(cx).0;
cx.observe_global::<SettingsStore>(|cx| { /* react */ }).detach();
```

Project-scoped read: `MySettings::get(Some(SettingsLocation { worktree_id, path }), cx)`.

### Step 8 — Add it to the GUI settings editor (optional but expected)

`crates/settings_ui/src/page_data.rs` — find the right page fn (`general_page`, `appearance_page`,
`editor_page`, `terminal_page`, `developer_page`, ...) and push:

```rust
SettingsPageItem::SettingItem(SettingItem {
    title: "Reduce Motion",
    description: "Whether to reduce non-essential motion ...",
    field: Box::new(SettingField {
        json_path: Some("reduce_motion"),
        pick:  |c| c.reduce_motion.as_ref(),
        write: |c, value, _| { c.reduce_motion = value; },
    }),
    metadata: None,
    files: USER,   // or USER | PROJECT - crates/settings_ui/src/settings_ui.rs:1461-1462
}),
```

(`crates/settings_ui/src/page_data.rs:1117-1126`.)
If the field type is new, also register a renderer in `init_renderers`
(`crates/settings_ui/src/settings_ui.rs:445-500`):

```rust
.add_basic_renderer::<settings::ReduceMotionMode>(render_dropdown)
```

Available renderers: `render_toggle_button`, `render_text_field`, `render_dropdown`,
`render_font_picker`, `render_number_field`, plus the theme and icon-theme pickers.
**If you skip Step 8 entirely, the setting still works from JSON** — the settings UI simply will not
show it. Types listed without a renderer fall back to `UnimplementedSettingField`, which renders an
"Edit in settings.json" button (`settings_ui.rs:446-470`).

### Step 9 — Project-level (`.wu/settings.json`) opt-in

Only fields reachable from `ProjectSettingsContent` (`crates/settings_content/src/project.rs:41-78`) —
`all_languages`, `worktree` (both flattened), `lsp`, `dap`, `terminal`, `load_direnv`,
`git_hosting_providers` — are accepted in `.wu/settings.json`. `SettingsStore::set_local_settings`
parses that file as `ProjectSettingsContent` (`crates/settings/src/settings_store.rs:1060-1063`), so a
user-only top-level key there is a parse error. If the setting must be project-overridable it has to
live under `ProjectSettingsContent` and its own `flattened_deserialize!` (`project.rs:72-78`).

### Step 10 — Schema regeneration: nothing to run

The user-settings schema is `SettingsStore::json_schema()` (`settings_store.rs:1247`), generated from
the `schemars` derive on `UserSettingsContent` at runtime and served as `wu://schemas/settings` by
`crates/json_schema_store/src/json_schema_store.rs`. Just rebuild and restart.
`script/update-json-schemas` is **not** for this (see section 6).

### Files touched, in order

1. `crates/settings_content/src/<section>.rs` — value type + field
2. `crates/settings_content/src/<section>.rs` — `flattened_deserialize!` list
3. `assets/settings/default.json` — default value + doc comment
4. `crates/settings/src/vscode_import.rs` — exhaustive literal
5. `crates/<consumer>/src/<x>_settings.rs` — `#[derive(RegisterSetting)]` + `impl Settings`
6. consumer call sites (`get_global` / `observe_global`)
7. `crates/settings_ui/src/page_data.rs` — `SettingItem`
8. `crates/settings_ui/src/settings_ui.rs` — `add_basic_renderer::<T>` (new types only)
9. `crates/settings_content/src/project.rs` — only if project-scoped

---

## 4. Actions and keymaps

### 4.1 Action naming

`crates/gpui/src/action.rs`:

- `actions!(namespace, [Foo, Bar])` (`:24-39`) expands to unit structs with
  `#[derive(Clone, PartialEq, Default, Debug, gpui::Action)]` and `#[action(namespace = namespace)]`.
  The name becomes `"namespace::Foo"` (`crates/gpui_macros/src/derive_action.rs:113-117`).
- Doc comments on the action become the schema description shown by the JSON LSP, the command palette
  and the keymap editor (`.rules:121`).
- `#[derive(Action)]` for actions with data:

```rust
#[derive(Clone, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = wu)]
#[serde(deny_unknown_fields)]
pub struct OpenBrowser { pub url: Arc<str> }
```

  (`crates/wu_actions/src/lib.rs:16-21`.)
- `#[action(...)]` options (`action.rs:71-88`): `namespace = x`, `name = "X"`, `no_json`,
  `no_register`, `deprecated_aliases = ["old::Name"]`, `deprecated = "message"`.
- Deprecated aliases in practice: `crates/wu_actions/src/lib.rs:40-62`
  (`wu_actions::OpenSettingsEditor` -> `wu::OpenSettings`, and similar). Registering an alias that
  collides with a real registered action panics at `App` creation (`action.rs:298-322`).
- `#[serde(deny_unknown_fields)]` is preferred over the global `DefaultDenyUnknownFields` transform for
  actions, so that a bad action argument fails just that binding rather than the whole keymap
  (`crates/settings/src/keymap_file.rs:603-606`).

Where actions live: `crates/wu_actions/src/lib.rs` (736 lines: the `wu::` namespace plus submodule
namespaces such as `wu_actions::settings_profile_selector::Toggle`), `crates/menu/src/menu.rs`
(`menu::`), and inline `actions!(...)` in each feature crate.

### 4.2 Keymap JSON format

`crates/settings/src/keymap_file.rs:56-100`:

```jsonc
[
  {
    "context": "Editor && mode == full",     // KeyBindingContextPredicate
    "use_key_equivalents": true,             // macOS non-QWERTY positional mapping
    "unbind": { "ctrl-k": "some::Action" },  // parsed BEFORE bindings in the same section
    "bindings": {
      "ctrl-shift-p": "command_palette::Toggle",
      "alt-enter": ["picker::ConfirmInput", { "secondary": false }],
      "ctrl-x": null                          // null means NoAction
    }
  }
]
```

- Keystroke syntax: modifiers `ctrl` `alt` `shift` `fn` `cmd` `super` `win`, joined with `-`;
  multi-key chords separated by whitespace, e.g. `"ctrl-k ctrl-o"`.
- Later bindings at the same context depth win. Unknown section keys land in `unrecognized_fields`
  and are reported without killing the file.
- Context predicates (`crates/gpui/src/keymap/context.rs:175-200`): `Identifier`,
  `Equal(k, v)` written `mode == full`, `NotEqual`, `Not` written `!X`, `And` written `X && Y`,
  `Or` written `X || Y`, `Descendant` written `X > Y`. Real examples from
  `assets/keymaps/specific-overrides.json`: `"(Picker && with_preview) > Editor"` and
  `"FileFinder || (FileFinder > Picker > Editor) || (FileFinder > Picker > menu)"`.

### 4.3 Keymap files that ship

`assets/keymaps/`:

- `default-macos.json` (1244), `default-linux.json` (1201), `default-windows.json` (1193) —
  selected at compile time by `DEFAULT_KEYMAP_PATH` (`crates/settings/src/settings.rs:141-148`).
- `specific-overrides.json` (50) and `specific-overrides-macos.json` (54) —
  `SPECIFIC_OVERRIDES_KEYMAP_PATH` (`settings.rs:159-163`). Loaded **after** base keymaps but
  **before** the user keymap, so they beat the Atom/VSCode/JetBrains overlays. The file header says to
  only add here when absolutely needed and to cross-reference from the default keymap.
- `initial.json` (15) — template written to `config_dir()/keymap.json`.
- `macos/{atom,cursor,emacs,jetbrains,sublime_text,textmate,vscode}.json`
- `linux/{atom,cursor,emacs,jetbrains,sublime_text,vscode}.json`
- **There is no `keymaps/windows/` directory.** On Windows the base-keymap overlays come from
  `keymaps/linux/*` (`crates/settings/src/base_keymap_setting.rs:110-122`, the
  `#[cfg(not(target_os = "macos"))]` arm). `TextMate` returns `None` on non-macOS.

`assets/keymaps/linux/vscode.json:2-4` states it explicitly: the Wu default keymap is close to the VS
Code one, so the overlay only contains the bindings where the two diverge. Confirmed by
`assets/keymaps/default-windows.json`: `ctrl-p` -> `file_finder::Toggle`, `ctrl-b` ->
`workspace::ToggleLeftDock`, ctrl-backtick -> `terminal_panel::Toggle`, `ctrl-shift-f` ->
`pane::DeploySearch`, `ctrl-k ctrl-o` -> `workspace::Open`.

### 4.4 Load order at runtime

`crates/wu/src/wu.rs:1930-1952` (`load_default_keymap`):

1. `DEFAULT_KEYMAP_PATH` with `KeybindSource::Default`
2. `base_keymap.asset_path()` if any, with `KeybindSource::Base`
3. `SPECIFIC_OVERRIDES_KEYMAP_PATH` with `KeybindSource::Default`

then the user `keymap.json` on top (`handle_keymap_file_changes`, `wu.rs:1749`).
`BaseKeymap::None` short-circuits all of it.

**`base_keymap` default is `"Zed"`** (`assets/settings/default.json:27`), rendered in the UI as
"Wu (Default)" (`base_keymap_setting.rs:60` and `:74`). The `BaseKeymapContent` doc comment in
`crates/settings_content/src/settings_content.rs:206-212` says "Default: VSCode" — **that doc comment
is stale** relative to `default.json` and to the `#[default] Zed` variant.

### 4.5 `script/check-keymaps`

`C:/Users/USER/Documents/wu-main/script/check-keymaps` — pure `git grep`, two rules:

1. Fails if the literal `cmd-` appears in any file under `assets/keymaps/` **except**
   `default-macos.json`, `specific-overrides-macos.json`, and `macos/*.json`.
2. Fails if `super-`, `win-`, or `fn-` appear **anywhere** under `assets/keymaps/`
   (the message says these are currently not used).

It uses `git grep`, so it only sees tracked files. **It is not wired into CI** — `.github/workflows/`
contains only `release.yml` and `upstream-sync.yml`, neither of which runs it. It is a manual dev script.

### 4.6 RECIPE B — Add a new action plus a default keybinding

1. **Declare the action** in the owning crate:

```rust
actions!(
    my_feature,
    [
        /// Doc comment shown in the command palette, the keymap editor and the JSON schema.
        DoTheThing,
    ]
);
```

   or with data:

```rust
#[derive(Clone, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = my_feature)]
#[serde(deny_unknown_fields)]
pub struct DoTheThing { pub loudly: bool }
```

   If you are renaming an existing action, add
   `#[action(deprecated_aliases = ["my_feature::OldName"])]`.

2. **If the namespace is brand new**, add it to the sorted `expected_namespaces` list in
   `crates/wu/src/wu.rs:5330-5444` (`test_action_namespaces`). That test also asserts **every** action
   has a namespace, so a bare `actions!([Foo])` fails it.

3. **Handle it** — `element.on_action(cx.listener(...))`, `workspace.register_action(...)`, or
   `cx.on_action(|_: &DoTheThing, cx| ...)` in the crate `init(cx)`.

4. **Bind it** in the three default keymaps:
   - `assets/keymaps/default-windows.json`
   - `assets/keymaps/default-linux.json`
   - `assets/keymaps/default-macos.json` (the only file allowed to use `cmd-`)

   Put it under the right `"context"` block; add `"use_key_equivalents": true` if the surrounding
   section does. If the binding must beat the Atom/JetBrains/VSCode overlays, put it in
   `assets/keymaps/specific-overrides.json` or `specific-overrides-macos.json` instead and leave a
   comment in the default keymap pointing there.

5. **Run `script/check-keymaps`** (bash) before committing keymap edits.

6. **`crates/keymap_editor` — nothing to do.** The in-app keymap UI is driven entirely by
   `cx.all_action_names()`, `cx.action_documentation()` and
   `cx.deprecated_actions_to_preferred_actions()`
   (`crates/keymap_editor/src/keymap_editor.rs:801-802, 1339-1340, 1390, 2479-2483`), with
   human-readable names from `command_palette::humanize_action_name` (`:1663`). The command palette is
   the same; `crates/command_palette_hooks` only maintains an opt-in *hide* list.

7. **Schema — nothing to regenerate.** `wu://schemas/keymap` is built from the action inventory
   (`KeymapFile::generate_json_schema_from_inventory`, `crates/settings/src/keymap_file.rs:622`) and
   from the live app (`generate_json_schema_for_registered_actions`, `:602`). Per-action argument
   schemas are served at `wu://schemas/action/<namespace>__<Name>` and are also file-associated to a
   synthetic `<namespace>__<Name>.json` filename
   (`crates/json_schema_store/src/json_schema_store.rs:492-502`).

8. Optional: a `KeyBindingValidator` can be registered via
   `inventory::submit!(KeyBindingValidatorRegistration(...))` (`keymap_file.rs:28-44`) to reject
   nonsensical bindings for a specific action type.

### Files touched, in order

1. `crates/<feature>/src/<feature>.rs` — `actions!` or `#[derive(Action)]`
2. `crates/wu/src/wu.rs` — `expected_namespaces` (new namespace only)
3. `crates/<feature>/src/...` — handler registration
4. `assets/keymaps/default-windows.json`
5. `assets/keymaps/default-linux.json`
6. `assets/keymaps/default-macos.json`
7. `assets/keymaps/specific-overrides*.json` (only when it must beat base keymaps)
8. `assets/keymaps/{macos,linux}/<editor>.json` (only to change a base-keymap overlay)

---

## 5. Themes

### 5.1 Crate map

| Crate | Role |
|---|---|
| `crates/theme` | runtime `Theme`, `ThemeFamily`, `ThemeColors`, `StatusColors`, `PlayerColors`, `AccentColors`, `SystemColors`, `IconTheme`, `ThemeRegistry`, `LoadThemes` |
| `crates/theme_settings` | `ThemeSettings` (the `Settings` impl), `ThemeFamilyContent`/`ThemeContent` (`src/schema.rs:25-37`), content-to-runtime refinement, bundled and user theme loading |
| `crates/settings_content/src/theme.rs` | **`ThemeStyleContent`, `ThemeColorsContent`, `StatusColorsContent`, `HighlightStyleContent`, `PlayerColorContent`, `AccentContent`** — the actual JSON schema types |
| `crates/syntax_theme` | `SyntaxTheme` — `Vec<HighlightStyle>` plus a `BTreeMap<capture_name, index>` |
| `crates/theme_importer` | CLI: VS Code theme into Wu theme JSON |
| `crates/theme_selector` | theme and icon-theme pickers |
| `crates/theme_extension` | `ExtensionThemeProxy` so extensions can ship themes |
| `crates/schema_generator` | CLI that prints the `theme` / `icon_theme` / `project` JSON schemas |

### 5.2 Registry and loading

`crates/theme/src/registry.rs` — `ThemeRegistry` (global via `GlobalThemeRegistry`), an `RwLock` over
`{ themes, icon_themes, extensions_loaded }`. API: `insert_theme_families`, `insert_themes`,
`remove_user_themes`, `list_names`, `list`, `get`, `list_icon_themes`, `get_icon_theme`,
`load_icon_theme`. The Zed fallback themes are inserted at construction (`registry.rs:110-113`, from
`crates/theme/src/fallback_themes.rs`).

Loading paths:

- **Bundled**: `theme_settings::load_bundled_themes`
  (`crates/theme_settings/src/theme_settings.rs:202-221`) — `registry.assets().list("themes/")`,
  `serde_json::from_slice::<ThemeFamilyContent>`, then `refine_theme_family`.
- **User**: `crates/wu/src/main.rs:1621-1662` — reads every file in `paths::themes_dir()`
  (`%APPDATA%\Wu\themes`), creating the directory if missing, then `load_user_theme` and `reload_theme`.
  Uses `serde_json_lenient`, so comments and trailing commas are fine.
- **Extensions**: `crates/theme_extension/src/theme_extension.rs` via `ExtensionThemeProxy`.

Theme selection and live reload: `theme_settings::init` (`theme_settings.rs:68-144`) observes
`SettingsStore` and calls `reload_theme` / `reload_icon_theme` when `theme`, `icon_theme`,
`experimental.theme_overrides`, `theme_overrides`, or any font-size setting changes.
Fallbacks: unknown theme name falls back to `default_theme(appearance)` then `DEFAULT_DARK_THEME`
(`theme_settings.rs:147-172`), and the error is only logged once extensions have loaded.

### 5.3 Theme JSON format

`assets/themes/<family>/<family>.json`:

```jsonc
{
  "$schema": "https://zed.dev/schema/themes/v0.2.0.json",
  "name": "One",
  "author": "Zed Industries",
  "themes": [
    { "name": "One Dark", "appearance": "dark",
      "style": {
        "border": "#464b57ff", "background": "#3b414dff",
        "accents": ["#8839ef"],
        "players": [ { "cursor": "#..", "selection": "#..", "background": "#.." } ],
        "syntax": { "keyword": { "color": "#..", "font_style": "italic", "font_weight": 700 } }
      }
    }
  ]
}
```

- Colors are `#rrggbbaa` strings parsed by `theme::try_parse_color` (`crates/theme/src/schema.rs:17-29`).
- `style` is `ThemeStyleContent` (`crates/settings_content/src/theme.rs:529-548`); colors are
  `ThemeColorsContent` (`:575-1110`, roughly 250 keys); status colors are `StatusColorsContent`
  (`:1157-1298`); syntax entries are
  `HighlightStyleContent { color, background_color, font_style, font_weight }` (`:1114-1134`).
- The deprecated key `scrollbar_thumb.background` logs a warning on load
  (`crates/theme_settings/src/theme_settings.rs:243-252`).

### 5.4 Syntax highlighting names

`crates/syntax_theme/src/syntax_theme.rs` — `SyntaxTheme::new(Vec<(String, HighlightStyle)>)`.
The `String` keys are the tree-sitter capture names from each language `highlights.scm`
(`extensions/*/languages/*/highlights.scm` and `crates/languages/src/*/highlights.scm`).
`style_for_name(name)` does the lookup; `HighlightId` is an index into the vec. Theme authors just
write whatever capture names the grammars use (`keyword`, `string`, `variable.special`,
`punctuation.bracket`, and so on) under `"style": { "syntax": { ... } }`. There is no fixed allow-list.
Semantic-token rules are seeded from `assets/settings/default_semantic_token_rules.json`.

### 5.5 Font and typography settings — `ThemeSettings`

Content type: `ThemeSettingsContent` (`crates/settings_content/src/theme.rs:178-247`):
`ui_font_{size,family,fallbacks,features,weight}`,
`buffer_font_{family,fallbacks,size,weight,features}`, `buffer_line_height`,
`git_commit_buffer_font_size`,
`markdown_preview_{font_family,code_font_family,font_size,theme}`, `theme`, `icon_theme`,
`unstable.ui_density`, `unnecessary_code_fade`, `experimental.theme_overrides`, and
`theme_overrides` (a per-theme map).
Runtime type: `theme_settings::ThemeSettings`, read with `ThemeSettings::get_global(cx)`.
Defaults are `.ZedMono` (Lilex) and `.ZedSans` (IBM Plex Sans) —
`assets/settings/default.json:31` and `:59`. Font-family enumeration for the schema comes from
`cx.text_system().all_font_names()` plus `crates/theme/src/font_family_cache.rs`.

### 5.6 Add or modify a theme

**Bundled theme**

1. Create `assets/themes/<family>/<family>.json` with `$schema`, `name`, `author`, `themes[]`.
2. Add `assets/themes/<family>/LICENSE` and append to `assets/themes/LICENSES`.
3. Nothing to register — `load_bundled_themes` globs `themes/` from the embedded assets
   (`assets/themes/**` is already in `crates/assets/src/assets.rs:12`).
4. If it should become the default, edit `assets/settings/default.json:9-14` and the constants in
   `crates/theme/src/theme.rs` / `fallback_themes.rs`.
5. `crates/wu/src/wu.rs:5449` (`test_bundled_settings_and_themes`) loads every bundled theme and asserts
   `theme.name` equals the registry key and that `DEFAULT_DARK_THEME` exists.

**User theme** — drop the JSON into `%APPDATA%\Wu\themes\`.

**Import a VS Code theme**

```sh
cargo run -p theme_importer -- path/to/vscode-theme.json --output assets/themes/x/x.json
```

`crates/theme_importer/src/main.rs:53-130`; conversion in `src/vscode/converter.rs` (451 lines) and
`src/vscode/syntax.rs` (310 lines). It stamps `"$schema": "https://zed.dev/schema/themes/v0.2.0.json"`.
`--warn-on-missing` reports values the VS Code theme did not supply.

**Emit the theme JSON schema**

```sh
cargo run -p schema_generator -- theme        # ThemeFamilyContent
cargo run -p schema_generator -- icon_theme   # IconThemeFamilyContent
cargo run -p schema_generator -- project      # ProjectSettingsContent
```

(`crates/schema_generator/src/main.rs`.) Output goes to stdout or to `--output`. Nothing in the repo
consumes the result; it exists for publishing to a docs site.

### 5.7 Icon themes

The default icon theme is **Rust code, not JSON**: `crates/theme/src/icon_theme.rs` (456 lines) with a
static table of pairs like `("astro", "icons/file_icons/astro.svg")` starting at `:315`. Custom icon
themes come from extensions via `IconThemeFamilyContent` (`crates/theme/src/icon_theme_schema.rs`).
UI icons: `crates/icons/src/icons.rs` — `enum IconName` with `#[strum(serialize_all = "snake_case")]`
and `path()` returning `icons/{file_stem}.svg` (`:203-206`). Two tests enforce the mapping both ways:
`test_all_icons_exist` and `test_no_dangling_icons` (`icons.rs:217-245`). **Adding an SVG to
`assets/icons/` without adding an `IconName` variant fails the test suite, and vice versa.**

---

## 6. JSON schemas — `crates/json_schema_store` and `script/update-json-schemas`

`crates/json_schema_store/src/json_schema_store.rs` (644 lines). URI prefix: `wu://schemas/`.

**Static or lazily cached** (`resolve_static_schema`, `:167-222`):

| URI | Source |
|---|---|
| `wu://schemas/tsconfig` | `src/schemas/tsconfig.json` (checked in, from SchemaStore) |
| `wu://schemas/package_json` | `src/schemas/package.json` (checked in, from SchemaStore) |
| `wu://schemas/tasks` | `task::TaskTemplates::generate_json_schema()` |
| `wu://schemas/snippets` | `snippet_provider::format::VsSnippetsFile::generate_json_schema()` |
| `wu://schemas/jsonc` | `generate_jsonc_schema()` |
| `wu://schemas/keymap` | `settings::KeymapFile::generate_json_schema_from_inventory()` |
| `wu://schemas/action/<ns>__<Name>` | per-action argument schema, cached in `ACTION_SCHEMA_CACHE` |
| `wu://schemas/zed_inspector_style` | debug builds only |

**Dynamic** (`resolve_dynamic_schema`, `:224-395`, cached in `DYNAMIC_SCHEMA_CACHE`):
`wu://schemas/settings` (needs fonts, themes, icon themes, languages, LSP adapters, action names,
documentation and deprecations), `wu://schemas/settings/lsp/<adapter>/initialization_options`,
`wu://schemas/settings/lsp/<adapter>/settings`, `wu://schemas/project_settings`,
`wu://schemas/debug_tasks`, `wu://schemas/keymap`, `wu://schemas/tasks`.
Cache invalidation: `notify_schema_changed` fires on `ExtensionsInstalledChanged` and on `DapRegistry`
changes (`:100-136`), then pushes `notify_schemas_changed` to every live `LspStore`.

**File associations** (`all_schema_file_associations`, `:406-503`) map globs to schema URIs:
`settings.json`, `.wu/settings.json` plus `.zed/settings.json`, `keymap.json`, `tasks.json` plus
`.wu/tasks.json` plus `.zed/tasks.json`, `debug.json` plus `.wu/debug.json` plus `.zed/debug.json`,
`snippets/*.json`, `tsconfig.json`, `package.json`, the JSONC globs, and one entry per action.
**Note: theme JSON files have NO local association** — theme files rely on the remote
`https://zed.dev/schema/themes/v0.2.0.json` URL in their `$schema` key.

**`script/update-json-schemas`** (`C:/Users/USER/Documents/wu-main/script/update-json-schemas`):

```sh
script/update-json-schemas [schemastore-commit]
```

It changes into `crates/json_schema_store/src/schemas`, resolves a SchemaStore commit through the
GitHub API, curls `tsconfig.json` and `package.json`, rewrites `json.schemastore.org` to
`www.schemastore.org`, and prints a changelog snippet. **That is all it does.** It requires `curl`,
`jq` and network access. It is irrelevant to Wu settings, keymap, task, snippet and theme schemas.

---

## 7. `crates/settings_ui` — the GUI settings editor

- `src/settings_ui.rs` (6004 lines) — the `SettingsWindow`, navbar, `SettingField<T>` and
  `AnySettingField`, the `SettingFieldRenderer` global, `init_renderers` (`:445-500`), and the
  `USER` / `PROJECT` `FileMask` constants (`:1461-1462`).
- `src/page_data.rs` (8834 lines) — the declarative page tree. `settings_data(cx)` (`:52-67`) returns
  `general_page`, `appearance_page`, `keymap_page`, `editor_page`, `languages_and_tools_page`,
  `search_and_files_page`, `window_and_layout_page`, `panels_page`, `debugger_page`, `terminal_page`,
  `version_control_page`, `network_page`, `developer_page`.
- `src/components/` — `dropdown`, `font_picker`, `icon_theme_picker`, `input_field`, `number_field`,
  `section_items`, `theme_picker`.

**Does a new setting have to be registered here?** No — it works from JSON regardless. But it is
invisible in the GUI editor until you add a `SettingItem` with `json_path` / `pick` / `write`
(Recipe A, Step 8). There is **no test enforcing coverage**, so it is easy to forget.
`SettingField` also drives the "which file is this set in" indicator and "reset to default", via
`SettingsStore::get_value_from_file` and `raw_default_settings()` (`settings_ui.rs:189-215`).

Actions: the `settings_editor::*` namespace plus `wu::OpenSettings`, `wu::OpenSettingsFile`,
`wu::OpenProjectSettings`, `OpenSettingsAt` and `OpenSettingsPage` (`settings_ui.rs:394-442`).

---

## 8. Tasks — `crates/task` and `crates/tasks_ui`

### Format

`crates/task/src/task_template.rs:24-81` — `TaskTemplate`:
`label`, `command`, `args`, `env`, `cwd`, `use_new_terminal`, `allow_concurrent_runs`,
`reveal` (`always` / `no_focus` / `never`), `reveal_target` (`dock` / `center`),
`hide` (`never` / `always` / `on_success`), `tags`,
`shell` (`system`, or an object with `program`, or `with_arguments` with `program` and `args`),
`show_summary`, `show_command`, `save` (`all` / `current` / `none`), `hooks`.

`hooks` is a Wu addition: `TaskHook::CreateWorktree`, JSON value `"create_worktree"` with alias
`"create_git_worktree"` (`task_template.rs:93-98`). Template file for it:
`assets/settings/initial_worktree_setup_tasks.json`.
`TaskTemplates(pub Vec<TaskTemplate>)` has `FILE_NAME = "tasks.json"` and `generate_json_schema()`
(`task_template.rs:141-155`). The fully commented reference is `assets/settings/initial_tasks.json`.

### Variables — still `$ZED_*`

`crates/task/src/task.rs:154-283`. `ZED_VARIABLE_NAME_PREFIX = "ZED_"` (`:254`), custom prefix
`ZED_CUSTOM_` (`:255`). Full set:
`ZED_FILE`, `ZED_FILENAME`, `ZED_RELATIVE_FILE`, `ZED_RELATIVE_DIR`, `ZED_DIRNAME`, `ZED_STEM`,
`ZED_WORKTREE_ROOT`, `ZED_SYMBOL`, `ZED_RUNNABLE_SYMBOL`, `ZED_SELECTED_TEXT`, `ZED_LANGUAGE`,
`ZED_ROW`, `ZED_COLUMN`, `ZED_PICK_PID`, `ZED_MAIN_GIT_WORKTREE`, `ZED_GIT_SHA`, `ZED_GIT_SHA_SHORT`,
`ZED_GIT_REPOSITORY_NAME`, `ZED_GIT_REPOSITORY_PATH`, `ZED_GIT_REF`, `ZED_CUSTOM_<NAME>`.
The `${ZED_FILE:default_value}` default syntax is supported (`task_template.rs:336`).
**They were NOT renamed to `$WU_*`.**

### Files and sources

- Global: `%APPDATA%\Wu\tasks.json`
- Project: `.wu/tasks.json` (legacy `.zed/tasks.json`, plus `.vscode/tasks.json` import via
  `crates/task/src/vscode_format.rs`)
- Debug: `%APPDATA%\Wu\debug.json`, `.wu/debug.json` (`crates/task/src/debug_format.rs`,
  `vscode_debug_format.rs`)
- Loader: `crates/task/src/static_source.rs` (`StaticSource` plus `TrackedFile<TaskTemplates>` over a
  watched-file channel). `SettingsStore::set_local_settings` explicitly **rejects**
  `LocalSettingsKind::Tasks` and `LocalSettingsKind::Debug`
  (`crates/settings/src/settings_store.rs:1028-1046`) — tasks do not flow through the settings store.
- UI: `crates/tasks_ui/src/tasks_ui.rs` (`init` at `:101`) and `src/modal.rs`.
- The repo own tasks: `C:/Users/USER/Documents/wu-main/.wu/tasks.json`
  (`./script/clippy`, `cargo run --profile release-fast`).

---

## 9. Snippets — `crates/snippet` and `crates/snippet_provider`

- **Format**: VS Code snippet JSON. `crates/snippet_provider/src/format.rs:69-78`:

```jsonc
{ "My Snippet": { "prefix": "ms", "body": ["l1", "l2"], "description": "..." } }
```

  `prefix`, `body` and `description` each accept a string or an array of strings
  (`ListOrDirect`, `:38-52`).
- **Locations**: `paths::snippets_dir()` is `%APPDATA%\Wu\snippets\`. The file stem is the language
  name; `snippets.json` is the global all-languages file
  (`crates/snippet_provider/src/lib.rs:27-33`). Project-local snippet directories are also supported
  through `SnippetProvider::watch_directory`.
- **Body parsing**: `crates/snippet/src/snippet.rs` — `Snippet::parse` handles `$1`,
  `${1:default}`, choice syntax with pipes, and `$0` as the final tabstop.
- **Schema**: `wu://schemas/snippets`, associated to `<snippets_dir>/*.json`
  (`crates/json_schema_store/src/json_schema_store.rs:456-465`).
- Extensions can ship snippets: `crates/snippet_provider/src/extension_snippet.rs`.
- UI: `crates/snippets_ui`.

---

## 10. Settings profiles — `crates/settings_profile_selector`

A named bundle of setting overrides you can toggle at runtime (presentation mode, pairing mode, etc.).
Declared in the user settings file:

```jsonc
{
  "profiles": {
    "Presenting": {
      "base": "user",
      "settings": { "buffer_font_size": 22, "ui_font_size": 20 }
    }
  }
}
```

- Types: `SettingsProfile` and `ProfileBase` — `crates/settings_content/src/settings_content.rs:376-402`.
  `base` is `"user"` (default: apply on top of the user settings) or `"default"` (apply on top of Wu
  defaults, ignoring user customizations entirely).
- The active profile is a gpui global `ActiveSettingsProfileName(String)`
  (`crates/settings/src/settings.rs:58-61`);
  `SettingsStore::observe_active_settings_profile_name` (`settings_store.rs:338`) recomputes on change.
- Applied in `recompute_values` after the user layer and before server settings
  (`settings_store.rs:1320-1338`); `base: "default"` suppresses the whole user layer including
  release-channel and OS overrides.
- Picker: `crates/settings_profile_selector/src/settings_profile_selector.rs`, action
  `wu_actions::settings_profile_selector::Toggle` (`:11`). Names come from
  `SettingsStore::configured_settings_profiles()` (`settings_store.rs:507`); the first entry is `None`,
  meaning no profile.

---

## 11. Footguns

1. **`vscode_import.rs` will break your build.** `SettingsContent`, `EditorSettingsContent`,
   `GutterContent` and friends are built with exhaustive struct literals in
   `crates/settings/src/vscode_import.rs`. Every new field needs an entry there (`None` is fine).
2. **`flattened_deserialize!` must list your new field.** Miss it and the macro-generated
   `Deserialize` impl fails to compile with a confusing missing-field error at the macro site.
3. **`from_settings` is expected to panic on a missing default.** `content.foo.unwrap()` is the house
   style, so **forgetting to add the key to `assets/settings/default.json` is a startup panic**, not a
   silent fallback. Use `.unwrap_or_default()` only when you genuinely want no `default.json` entry.
4. **`Settings::register(cx)` is a red herring.** The 10 remaining call sites are all in test modules
   (`crates/git_ui/src/git_ui.rs:1372-1376`, `crates/git_ui_core/src/*.rs`). Adding it in production
   code is harmless but pointless; *omitting* `#[derive(RegisterSetting)]` causes a runtime panic:
   "unregistered setting type <T>" (`settings_store.rs:448`).
5. **Dead-code stripping eats action and setting registrations.** If nothing in the binary references a
   crate, its `inventory::submit!` calls never run. That is why `crates/wu_actions/src/lib.rs:13` and
   `crates/menu/src/menu.rs:10` have `pub fn init() {}` no-ops that `main` calls.
6. **The `base_keymap` doc comment is stale.** `crates/settings_content/src/settings_content.rs:206-212`
   says "Default: VSCode"; the actual default is `"Zed"` (`assets/settings/default.json:27`, and
   `#[default] Zed` in `crates/settings/src/base_keymap_setting.rs:16`). "Zed" is displayed as
   "Wu (Default)".
7. **There is no `assets/keymaps/windows/`.** Windows silently uses the `linux/` base-keymap overlays.
   A Windows-only base-keymap tweak has to go into `linux/*.json` (which then also affects Linux) or
   into `assets/keymaps/default-windows.json`.
8. **`script/check-keymaps` is not in CI** and uses `git grep`, so untracked keymap edits are invisible
   to it. `super-`, `win-` and `fn-` are banned everywhere; `cmd-` is banned outside the macOS files.
9. **`.wu/settings.json` only accepts `ProjectSettingsContent`.** Putting `theme` or `ui_font_size`
   there is a parse/schema error. The accepted subset is `all_languages` and `worktree` (both
   flattened) plus `lsp`, `dap`, `terminal`, `load_direnv`, `git_hosting_providers`.
10. **Task variables are `ZED_*`, not `WU_*`.** Do not "fix" them; `.zed/tasks.json` compatibility and
    the `runnables.scm` ecosystem depend on it.
11. **`Vec` merges by overwrite, not append.** A user setting a one-element array replaces the whole
    default array. Use `ExtendingVec` or `ExtendingSet` for additive behavior
    (`crates/settings_content/src/merge_from.rs`).
12. **Adding an icon SVG without an `IconName` variant fails `test_no_dangling_icons`**
    (`crates/icons/src/icons.rs:231`), and vice versa (`test_all_icons_exist`, `:217`).
13. **Adding an action in a new namespace fails `test_action_namespaces`**
    (`crates/wu/src/wu.rs:5330`). It also fails on any action declared without a namespace.
14. **Theme files have no local JSON schema.** They point at
    `https://zed.dev/schema/themes/v0.2.0.json`; there is no `wu://schemas/theme` file association in
    `crates/json_schema_store/src/json_schema_store.rs:406-503`. If you add fields to
    `ThemeStyleContent`, no local editor validation picks them up.
15. **Settings UI coverage is unenforced.** No test asserts that every `SettingsContent` field appears
    in `page_data.rs`, so new settings silently stay GUI-invisible.
16. **A bad value in `settings.json` does not fail the whole file** (by design, see
    `fallible_options`); it nulls that one key and surfaces an error notification. Do not rely on
    strict parsing.
17. **`assets/*.json` are JSONC** — comments and trailing commas everywhere. `.gitattributes` marks
    `*.json` as `linguist-language=JSON-with-Comments`; the repo `.wu/settings.json` maps
    `"JSONC": ["**/assets/**/*.json", "renovate.json"]`. Never run a strict JSON formatter on them.
    `.prettierrc` sets `printWidth: 120`.
18. **`SettingsAssets` and `Assets` are two different embed roots.** Settings and keymaps live in
    `crates/settings/src/settings.rs:120-125`; fonts, icons, images and themes live in
    `crates/assets/src/assets.rs:7-16`. Adding a new asset category means editing the right one.
19. **`default_settings()` is parsed as `UserSettingsContent`, not `SettingsContent`**
    (`settings_store.rs:901-907`), so `default.json` may itself contain `dev`/`stable`/`macos`/
    `windows` override blocks, and they are merged before anything else.

---

## 12. What to re-run after changing X

| You changed | Re-run / re-do |
|---|---|
| A field in `crates/settings_content/**` | Rebuild. Fix `vscode_import.rs`. Add the default to `assets/settings/default.json`. Nothing else: the settings schema is runtime-generated. |
| `assets/settings/default.json` | Rebuild (it is `RustEmbed`-embedded via `SettingsAssets`). Restart the app. |
| An action (`actions!` or `#[derive(Action)]`) | Rebuild. `cargo test -p wu test_action_namespaces` if the namespace is new. Keymap and action schemas regenerate at runtime. |
| Any file in `assets/keymaps/**` | `script/check-keymaps` (bash, needs git). Rebuild. |
| A theme JSON in `assets/themes/**` | Rebuild. `cargo test -p wu test_bundled_settings_and_themes`. |
| `ThemeStyleContent` / `ThemeColorsContent` in `settings_content/src/theme.rs` | Rebuild; then `cargo run -p schema_generator -- theme` if you publish the theme schema externally. Update every bundled theme JSON that used a renamed key. |
| `IconThemeFamilyContent` | `cargo run -p schema_generator -- icon_theme` |
| `ProjectSettingsContent` | `cargo run -p schema_generator -- project` (only if publishing); otherwise just rebuild. |
| `TaskTemplate` / `DebugTaskFile` | Rebuild. Schema is `wu://schemas/tasks` and `debug_tasks`, runtime-generated. Consider updating the docs in `assets/settings/initial_tasks.json`. |
| `VsCodeSnippet` / `VsSnippetsFile` | Rebuild; `wu://schemas/snippets` is runtime-generated. |
| An SVG in `assets/icons/` | Add or remove the matching `IconName` variant in `crates/icons/src/icons.rs`; `cargo test -p icons`. |
| A `.ttf` in `assets/fonts/` | Rebuild (auto-loaded by `Assets::load_fonts`). |
| SchemaStore-sourced `tsconfig.json` / `package.json` | `script/update-json-schemas [commit]` — needs curl, jq and network; it rewrites the two files under `crates/json_schema_store/src/schemas/`. |
| A `settings_ui` page entry | Rebuild; `cargo test -p settings_ui` (navbar snapshot tests). |
| Anything under `crates/settings_content` | `cargo test -p settings_content` (`flatten_collision_probe`). |
| Formatting of any `assets/**/*.json` | `script/prettier` (the repo uses Prettier for JSON/JSONC/MD/YAML). |

CI note: `.github/workflows/` contains only `release.yml` and `upstream-sync.yml`. Neither runs tests,
clippy, nor `check-keymaps` — **all of the above are local/manual gates.**

---

## 13. Quick file index

```
assets/settings/default.json                       # every default, with docs
assets/settings/initial_*.json                     # first-run templates
assets/keymaps/default-{macos,linux,windows}.json  # the shipped keymap
assets/keymaps/specific-overrides{,-macos}.json    # beats base keymaps
assets/keymaps/{macos,linux}/<editor>.json         # base-keymap overlays
assets/themes/<family>/<family>.json               # bundled themes
crates/settings_content/src/settings_content.rs    # root SettingsContent / UserSettingsContent
crates/settings_content/src/fallible_options.rs    # flattened_deserialize! + tolerant parsing
crates/settings_content/src/merge_from.rs          # merge semantics
crates/settings_content/src/theme.rs               # ThemeStyleContent and friends
crates/settings/src/settings.rs                    # crate root, asset accessors, keymap paths
crates/settings/src/settings_store.rs              # Settings trait, SettingsStore, layering, schema
crates/settings/src/settings_file.rs               # update_settings_file, file watching
crates/settings/src/keymap_file.rs                 # KeymapFile format + keymap schema
crates/settings/src/base_keymap_setting.rs         # BaseKeymap enum to asset paths
crates/settings/src/vscode_import.rs               # VS Code settings import (exhaustive literals)
crates/settings_macros/src/settings_macros.rs      # MergeFrom / RegisterSetting / with_fallible_options
crates/settings_json/src/settings_json.rs          # comment-preserving JSON edits
crates/settings_ui/src/page_data.rs                # GUI settings page tree
crates/settings_ui/src/settings_ui.rs              # SettingField, renderers, USER/PROJECT masks
crates/json_schema_store/src/json_schema_store.rs  # wu://schemas/* server + file associations
crates/schema_generator/src/main.rs                # theme/icon_theme/project schema CLI
crates/gpui/src/action.rs                          # actions! macro, Action trait
crates/gpui_macros/src/derive_action.rs            # #[action(...)] attribute parsing
crates/gpui/src/keymap/context.rs                  # KeyBindingContextPredicate
crates/wu_actions/src/lib.rs                       # wu:: namespace actions
crates/keymap_editor/src/keymap_editor.rs          # in-app keymap UI (no registration needed)
crates/theme/src/registry.rs                       # ThemeRegistry
crates/theme/src/icon_theme.rs                     # default icon theme (Rust, not JSON)
crates/theme_settings/src/theme_settings.rs        # theme init, load_bundled_themes, reload_theme
crates/theme_settings/src/schema.rs                # ThemeFamilyContent / ThemeContent
crates/theme_importer/src/main.rs                  # VS Code theme importer CLI
crates/syntax_theme/src/syntax_theme.rs            # capture name to HighlightStyle
crates/task/src/task.rs                            # VariableName / ZED_ prefix
crates/task/src/task_template.rs                   # TaskTemplate JSON format
crates/snippet_provider/src/format.rs              # VS Code snippet JSON format
crates/settings_profile_selector/src/...           # profile picker
crates/paths/src/paths.rs                          # every config path
crates/icons/src/icons.rs                          # IconName to assets/icons/*.svg
script/check-keymaps                               # cmd-/super-/win-/fn- linter
script/update-json-schemas                         # SchemaStore tsconfig/package.json refresh only
.wu/settings.json, .wu/tasks.json                  # the repo own project config
```

### App init order (`crates/wu/src/main.rs`)

```
settings::init(cx)                                  # main.rs:480
zlog_settings::init(cx)                             # :481
wu::watch_settings_files(fs, cx)                    # :482
handle_keymap_file_changes(rx, watcher, cx)         # :483
theme_settings::init(LoadThemes::All(Assets), cx)   # :604
snippet_provider::init(cx)                          # :613
tasks_ui::init(cx)                                  # :636
settings_profile_selector::init(cx)                 # :655
settings_ui::init(cx)                               # :663
keymap_editor::init(cx)                             # :664
json_schema_store::init(cx)                         # :667
load_user_themes_in_background(fs, cx)              # :712
```
