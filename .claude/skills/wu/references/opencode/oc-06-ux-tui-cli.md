# OpenCode UX Research: TUI / CLI / IDE / Web / Share / Keybinds / Themes

Research target: the **user experience** surface of OpenCode, for the team building **Cowork**, an AI panel inside a Zed fork (Rust/GPUI).

Sources fetched 2026-09-11:
- https://opencode.ai/docs/tui/
- https://opencode.ai/docs/cli/
- https://opencode.ai/docs/ide/
- https://opencode.ai/docs/web/
- https://opencode.ai/docs/share/
- https://opencode.ai/docs/keybinds/
- https://opencode.ai/docs/themes/
- https://opencode.ai/docs/permissions/ (added — the TUI page documents approval UX only by reference)

**Methodological note, important.** OpenCode's TUI prose page is thin: it documents slash commands and config, but says almost nothing about visual layout, message rendering, or the approval dialog. The **default keybinds map on /docs/keybinds/ is the authoritative inventory of what the TUI can actually do** — every bindable action implies a feature. Much of section 1.1 below is reconstructed from that map, and I flag which claims are prose-documented vs. keybind-inferred. Where the docs are silent on pixels (exact frame layout, spinner design, token counters), I say so rather than invent it.

---

# Part 1 — How OpenCode does it

## 1.1 TUI — the full interaction model

### Entry and session bootstrap

- `opencode` starts the TUI for the current directory; `opencode /path/to/project` targets another. (prose)
- The TUI is a **client of a server**, not a monolith. `opencode serve` runs headless; `opencode attach <url>` points a TUI at a running backend; `opencode web` serves a browser UI over the same backend. A TUI and a web UI can attach to the same server "sharing the same sessions and state." This client/server split is the most consequential architecture decision on this list — it is what makes the CLI, web, IDE and ACP surfaces cheap.
- Startup flags reach into session state directly: `--continue`/`-c` (resume last), `--session <id>`, `--fork` (branch the resumed session instead of appending), `--prompt` (seed first message), `--model provider/model`, `--agent`, `--auto`.

### Screen regions (inferred from keybinds; not drawn in the docs)

The bindable actions imply these regions exist:

| Region | Evidence |
|---|---|
| Sidebar | `sidebar_toggle: "<leader>b"` |
| Message viewport with its own scrollbar | `scrollbar_toggle`, `messages_page_up/down`, `messages_half_page_up/down`, `messages_line_up/down`, `messages_first`, `messages_last` |
| Status / detail view | `status_view: "<leader>s"` |
| Composer (multi-line input) | full readline binding set, `input_newline` |
| Modal dialogs | `dialog.select.*`, `dialog.prompt.*`, `dialog.mcp.*`, `dialog.plugins.*` |
| Autocomplete popup over the composer | `prompt.autocomplete.*` |
| Permission prompt (own overlay, expandable to fullscreen) | `permission.prompt.fullscreen: "ctrl+f"` |
| Which-key hint overlay | 10 `which_key_*` bindings |
| Command palette | `command_list: "ctrl+p"` |

### Message rendering

- Markdown is themed at fine grain — the theme schema has **14 dedicated markdown slots**: `markdownText`, `markdownHeading`, `markdownLink`, `markdownLinkText`, `markdownCode`, `markdownCodeBlock`, `markdownBlockQuote`, `markdownEmph`, `markdownStrong`, `markdownHorizontalRule`, `markdownListItem`, `markdownListEnumeration`, `markdownImage`, `markdownImageText`. Plus **9 syntax slots** for code inside fences (`syntaxComment`, `syntaxKeyword`, `syntaxFunction`, `syntaxVariable`, `syntaxString`, `syntaxNumber`, `syntaxType`, `syntaxOperator`, `syntaxPunctuation`).
- Per-message affordances: `messages_copy` (`<leader>y`) copies a message; `messages_next` / `messages_previous` / `messages_last_user` move a **message-level cursor** — messages are addressable objects, not a text blob.
- Toggles that change transcript density, all user-bindable:
  - `session_toggle_timestamps` — timestamps on/off
  - `messages_toggle_conceal` (`<leader>h`) — conceal/reveal long or sensitive content
  - `app_toggle_paste_summary` — a large paste collapses to a summary chip instead of dumping into the transcript
  - `app_toggle_file_context` — show/hide the attached-file context block
  - `app_toggle_animations`
- Username display in chat messages is toggleable, persisted across restarts, discovered via the command palette ("username" / "hide username"). (prose)

### Thinking / reasoning blocks

- `/thinking` toggles **visibility** of reasoning blocks. The docs are explicit: "This command only controls whether thinking blocks are displayed - it does not enable or disable the model's reasoning capabilities." Also bindable as `display_thinking`. `opencode run --thinking` shows them non-interactively.
- Reasoning **effort** is a separate axis: `variant_cycle` (`ctrl+t`) cycles model variants, `variant_list` opens the picker. "How hard should it think" is a one-keystroke inline control, not a settings page.

### Tool calls and their output

- Tool calls render **inline in the conversation as first-class items**, collapsed by default, details expandable:
  - `/details` — "Toggle tool execution details" (prose), bindable as `tool_details`
  - `session_toggle_generic_tool_output` — a **second, separate** toggle for generic tool output, which implies tools have **bespoke renderers** (read, edit, bash, grep…) with a generic fallback for the rest
- `!`-prefixed messages run a shell command and "the output appears as a tool result in the conversation" — user-run shell and agent-run shell share one rendering path. (prose)
- Subagent work is not inlined into one flat log; it becomes a **child session** you navigate into (see below).

### Diff display

- `diff_style` config: `"auto"` (adapts to terminal width — side-by-side when wide, stacked when narrow) or `"stacked"` (always single-column). Default `"auto"`. **Responsive diff layout is a first-class setting**, not a hardcoded choice.
- `app_toggle_diffwrap` — toggle line wrapping inside diffs at runtime.
- The theme schema devotes **13 slots** to diffs alone: `diffAdded`, `diffRemoved`, `diffContext`, `diffHunkHeader`, `diffHighlightAdded`, `diffHighlightRemoved`, `diffAddedBg`, `diffRemovedBg`, `diffContextBg`, `diffLineNumber`, `diffAddedLineNumberBg`, `diffRemovedLineNumberBg`. The `diffHighlight*` pair implies **intra-line (word-level) highlighting** layered on line-level add/remove; the `*LineNumberBg` pair implies a **gutter with its own coloring**. This is a real diff viewer, not colored text.

### Approving an action (the permission flow)

Documented on /docs/permissions/, surfaced in the TUI:

- Every rule resolves to `"allow"` (runs silently), `"ask"` (prompts), or `"deny"` (blocks).
- Permission keys: `read`, `edit` (covers edit/write/patch), `glob`, `grep`, `bash`, `task` (subagents), `skill`, `lsp`, `question`, `webfetch`, `websearch`, `external_directory`, `doom_loop` (same tool call repeated 3+ times).
- Each key matches against tool input: `edit`/`read` match file path, `bash` matches **parsed commands**, `webfetch` matches URL, `task` matches subagent type, `skill` matches skill name.
- Granular object syntax, **last matching rule wins**:

```json
{
  "permission": {
    "bash": { "*": "ask", "git *": "allow", "npm *": "allow", "rm *": "deny", "grep *": "allow" },
    "edit": { "*": "deny", "packages/web/src/content/docs/*.mdx": "allow" }
  }
}
```

- Patterns: `*` = zero or more chars, `?` = exactly one, everything else literal; `~` / `$HOME` expand at pattern start.
- Defaults are permissive: most keys default to `"allow"`; **`doom_loop` and `external_directory` default to `"ask"`**; `.env` is denied by default while `.env.example` is allowed.
- **The prompt itself offers three outcomes: `once`, `always` (approve future matching requests this session), `reject`.** Critically: "Tools suggest safe pattern whitelists for the `always` option" — the dialog proposes the generalization (e.g. `git *`) rather than making the user author a glob.
- `permission.prompt.fullscreen` (`ctrl+f`) — expand the prompt to fullscreen so a long command or large diff can actually be read before approving.
- **`--auto` / YOLO mode**: `opencode --auto`, `opencode run --auto "Refactor this module"` auto-approves everything not explicitly denied; explicit `"deny"` rules still hold; **the TUI displays an `auto` indicator while active.** A persistent, visible mode badge — not a silent setting.
- Per-agent permission overrides, in JSON or agent markdown frontmatter:

```yaml
---
description: Code review without edits
mode: subagent
permission:
  edit: deny
  bash: ask
  webfetch: deny
---
```

- `external_directory` deserves separate note: it gates any tool call touching paths outside the project, defaults to `ask`, and composes with `edit`/`read` rules.

### Interrupt / cancel

- **`session_interrupt: "escape"`** — one unmodified key, no leader, no confirmation. Stops the running turn.
- `input_clear: "ctrl+c"` clears the composer; `app_exit: "ctrl+c,ctrl+d,<leader>q"` exits. `ctrl+c` is context-sensitive rather than a kill switch, and Escape carries interruption.
- `terminal_suspend: "ctrl+z"` (forced to `none` on Windows — native terminals lack POSIX suspend).

### Attaching files and @-mentions

- `@` in the composer opens **fuzzy file search over the working directory**; the selected file's content is auto-added to the conversation. Documented example: `"How is auth handled in @packages/functions/src/api/index.ts?"`
- **Named reference roots**: configured references also appear in `@` autocomplete — `@alias` adds a whole reference root as context, `@alias/` browses and autocompletes files inside it. So @-mention is a namespaced context system, not merely a file picker.
- Autocomplete popup has its own bindings: `prompt.autocomplete.prev` (`up,ctrl+p`), `.next` (`down,ctrl+n`), `.hide` (`escape`), `.select` (`return`), `.complete` (`tab`).
- Non-interactive attachment: `opencode run --file`/`-f`.
- `app_toggle_file_context` hides/shows the resulting context block so attachments don't drown the transcript.

### Session switching, forking, and the session tree

- `/sessions` (aliases `/resume`, `/continue`), `<leader>l` — list and switch.
- `/new` (alias `/clear`), `<leader>n` — new session.
- `session_rename` (`ctrl+r`), `session_delete` (`ctrl+d`), `session_copy`, `session_move`, `session_fork`.
- **`session_timeline` (`<leader>g`)** — a timeline view of the session. Undocumented in prose, but paired with undo/redo this reads as a checkpoint browser.
- **Parent/child session tree navigation**, bare arrow keys, no leader:
  - `session_child_first: "<leader>down"`
  - `session_child_cycle: "right"` / `session_child_cycle_reverse: "left"`
  - `session_parent: "up"`

  Subagent runs become navigable child sessions you descend into and pop out of.
- `app_toggle_session_directory_filter` — filter the session list to the current directory, or show all.
- `workspace_set` (experimental, `OPENCODE_EXPERIMENTAL_WORKSPACES`).

### Undo / redo — VCS-backed, turn-level

- `/undo` (`<leader>u`, `messages_undo`): "Removes the most recent user message, all subsequent responses, **and any file changes**."
- `/redo` (`<leader>r`, `messages_redo`): only available after `/undo`.
- Both **use Git internally** to manage file changes. This is the strongest safety affordance in the product: one keystroke rewinds the conversation and the working tree together.

### Context management

- `/compact` (alias `/summarize`, `<leader>c`) — compact the current session. Auto-compaction exists, disabled via `OPENCODE_DISABLE_AUTOCOMPACT`.
- `status_view` (`<leader>s`) — status surface. `opencode stats` reports tokens/cost/tool usage, so the equivalent is presumably surfaced here.

### Composer quality-of-life

- Full emacs/readline editing: `ctrl+a`/`ctrl+e` line home/end, `ctrl+k`/`ctrl+u` kill to end/start, `ctrl+w` delete word back, `alt+f`/`alt+b` word motions, `ctrl+shift+d` delete line, shift-variants of every motion for selection, separate **visual-line vs logical-line** home/end (`alt+a`/`alt+e` vs `ctrl+a`/`ctrl+e`) for wrapped text, `super+a` select all, `input_undo`/`input_redo` scoped to the composer.
- Multi-line submit discipline: `input_submit: "return"`, `input_newline: "shift+return,ctrl+return,alt+return,ctrl+j"` — four newline bindings because terminals disagree about modifier reporting.
- `history_previous` / `history_next` (`up`/`down`) — shell-style prompt history.
- **Prompt stash**: `prompt_stash`, `prompt_stash_pop`, `prompt_stash_list`, `stash_delete` — park a half-written prompt, do something else, pop it back.
- `/editor` (`<leader>e`) — compose in `$EDITOR` (`vim`, `nano`, `code --wait`; GUI editors need `--wait`). `prompt_editor_context_clear` clears carried editor context.
- `prompt_skills` — skills picker from the composer.
- `input_paste` is an **object-form binding** `{"key": "ctrl+v", "preventDefault": false}` so the terminal's native paste still works alongside it.

### Discoverability

- **Command palette `ctrl+p`** (`command_list`) is the primary discovery surface — it also exposes persistent settings like username display.
- **Which-key overlay** — 10 bindings (`which_key_toggle: "ctrl+alt+k"`, layout toggle, pending toggle, group prev/next, scroll up/down, page up/down, home/end). After the leader key, pending completions display in scrollable groups.
- `/help` dialog, `docs_open`, `tips_toggle` (`<leader>h`).

### Attention / notifications

`attention` config block — desktop notifications **and sounds** for the events `question`, `permission`, `error`, `done`, `subagent_done`, plus `default`. Fires when the terminal is blurred. `volume` 0–1 (default 0.4), `sound_pack` (default `opencode.default`), per-event sound file overrides. `attention.enabled` defaults to `false`.

### Models, agents, providers, MCP — all inline pickers

- `model_list` (`<leader>m`), `model_provider_list` (`ctrl+a`), `/connect` + `provider_connect` (add a provider and its API key from inside the TUI, no config editing).
- **`model_favorite_toggle` (`ctrl+f`)**, `model_cycle_favorite` / reverse, and `model_cycle_recent` (`f2`) / reverse (`shift+f2`). Favorites and MRU, both cyclable without opening a dialog.
- `agent_list` (`<leader>a`), **`agent_cycle: "tab"` / `agent_cycle_reverse: "shift+tab"`** — switching agent is a single Tab press in the composer.
- `mcp_list` with `dialog.mcp.toggle: "space"` — enable/disable MCP servers interactively.
- `plugin_manager`, `plugin_install`, `plugins.toggle: "space"`, `dialog.plugins.install: "shift+i"`.
- `console_org_switch`.

### Complete slash-command list (prose-documented)

| Command | Keybind | Description |
|---|---|---|
| `/connect` | — | Add a provider; select and enter API keys |
| `/compact` (`/summarize`) | `ctrl+x c` | Compact the current session |
| `/details` | — | Toggle tool execution details |
| `/editor` | `ctrl+x e` | Open external editor to compose a message |
| `/exit` (`/quit`, `/q`) | `ctrl+x q` | Exit OpenCode |
| `/export` | `ctrl+x x` | Export conversation to Markdown, open in editor |
| `/help` | — | Show the help dialog |
| `/init` | — | Guided setup for creating/updating `AGENTS.md` |
| `/models` | `ctrl+x m` | List available models |
| `/new` (`/clear`) | `ctrl+x n` | Start a new session |
| `/redo` | `ctrl+x r` | Redo a previously undone message |
| `/sessions` (`/resume`, `/continue`) | `ctrl+x l` | List and switch sessions |
| `/share` | — | Share current session |
| `/themes` | `ctrl+x t` | List available themes |
| `/thinking` | — | Toggle thinking/reasoning block visibility |
| `/undo` | `ctrl+x u` | Undo last message, revert file changes |
| `/unshare` | — | Unshare current session |

### Full `tui.json`

```json
{
  "$schema": "https://opencode.ai/tui.json",
  "theme": "opencode",
  "leader_timeout": 2000,
  "keybinds": { "leader": "ctrl+x", "command_list": "ctrl+p" },
  "scroll_speed": 3,
  "scroll_acceleration": { "enabled": false },
  "diff_style": "auto",
  "cursor": { "style": "block", "blinking": true },
  "mouse": true,
  "attention": {
    "enabled": true, "notifications": true, "sound": true,
    "volume": 0.4, "sound_pack": "opencode.default",
    "sounds": { "error": "./sounds/error.mp3" }
  }
}
```

| Option | Description | Default |
|---|---|---|
| `theme` | UI theme | (varies) |
| `leader_timeout` | ms to wait after leader key | `2000` |
| `keybinds` | merged with built-in defaults | (built-in) |
| `scroll_acceleration.enabled` | macOS-style acceleration; overrides `scroll_speed` | `false` |
| `scroll_speed` | min `0.001`, decimals allowed | `3` |
| `diff_style` | `"auto"` (adapts to width) or `"stacked"` | `"auto"` |
| `cursor.style` | `block` / `underline` / `line` / `default` | `"block"` |
| `cursor.blinking` | no effect when style is `default` | `true` |
| `mouse` | mouse capture in the TUI | `true` |
| `attention.enabled` | all notifications + sounds | `false` |
| `attention.notifications` | terminal-mediated desktop notifications | `true` |
| `attention.sound` | attention sounds | `true` |
| `attention.volume` | 0–1 | `0.4` |
| `attention.sound_pack` | sound pack id | `"opencode.default"` |
| `attention.sounds` | override `default`/`question`/`permission`/`error`/`done`/`subagent_done` | (varies) |

---

## 1.2 CLI — every command and flag

### `tui` (default when no subcommand)

`opencode [project]`

`--continue`/`-c`, `--session`/`-s <id>`, `--fork`, `--prompt`, `--model`/`-m <provider/model>`, `--agent`, `--auto`, `--port`, `--hostname`, `--mdns`, `--mdns-domain`, `--cors`

### `run` — non-interactive / scripted

```
opencode run [message..]
opencode run Explain the use of context in Go
opencode run --attach http://localhost:4096 "Explain async/await"
```

`--command`, `--continue`/`-c`, `--session`/`-s`, `--fork`, `--share`, `--model`/`-m`, `--agent`, `--file`/`-f`, **`--format` (default or `json`)**, `--title`, `--attach <url>`, `--username`/`-u`, `--password`/`-p`, `--dir`, `--port` (random default), `--variant`, `--thinking`, `--auto`

Notable: `run` can either spin its own local server on a random port, or attach to an existing one; `--format json` is the machine-readable path; `--auto` is how you get unattended execution in CI.

### `serve` — headless API server

`opencode serve` — `--port`, `--hostname`, `--mdns`, `--mdns-domain`, `--cors`

### `web` — headless server + browser UI

`opencode web` — same flags as `serve`

### `attach` — point a TUI at a running backend

```
opencode attach [url]
opencode attach http://10.20.30.40:4096
```
`--dir`, `--continue`/`-c`, `--session`/`-s`, `--fork`, `--username`/`-u`, `--password`/`-p`

### `acp` — Agent Client Protocol server

`opencode acp` — `serve` flags plus `--cwd`. **This is how OpenCode plugs into third-party editors** (ACP is the protocol Zed itself speaks for external agents — directly relevant to Cowork).

### `agent`

- `opencode agent create` — `--path`, `--description`, `--mode` (`all`|`primary`|`subagent`), `--permissions` (comma-separated from `bash, read, edit, glob, grep, webfetch, task, todowrite, websearch, lsp, skill`), `--model`/`-m`. Non-interactive use requires all of `--path`, `--description`, `--mode`, `--permissions`.
- `opencode agent list`

### `auth`

- `opencode auth login` — `--provider`/`-p`, `--method`/`-m`
- `opencode auth list` / `ls`
- `opencode auth logout`

### `models`

```
opencode models
opencode models anthropic
opencode models --refresh
```
`--refresh` (update cached model list), `--verbose` (include metadata such as costs)

### `mcp`

`mcp add`, `mcp list`/`ls`, `mcp auth [name]`, `mcp auth list`/`ls`, `mcp logout [name]`, `mcp debug <name>` (debug OAuth connection issues)

### `github`

- `opencode github install` — set up the GitHub agent in a repo
- `opencode github run` — execute in Actions; `--event` (mock event), `--token` (PAT)

### `session`

- `opencode session list` — `--max-count`/`-n`, **`--format` (`table` or `json`)**
- `opencode session delete <sessionID>`

### `stats`

`opencode stats` — `--days N`, `--tools N`, `--models [N]`, `--project <name>`. Token usage and cost, tool breakdown, model breakdown.

### `export` / `import`

- `opencode export [sessionID]` — session as JSON; **`--sanitize` redacts sensitive data**
- `opencode import session.json` / `opencode import https://opncd.ai/s/abc123` — round-trips from a share URL

### `plugin` / `plug`

`opencode plugin <module>` — `--global`/`-g`, `--force`/`-f`

### `pr`

`opencode pr <number>` — fetch and checkout a GitHub PR, then run OpenCode on it

### `db`, `debug`

`opencode db [query]`, `opencode db path` — `--format` (`json` or `tsv`). `opencode debug [command]`.

### `upgrade` / `uninstall`

- `opencode upgrade [version]` — `--method`/`-m` (`curl`, `npm`, `pnpm`, `bun`, `brew`)
- `opencode uninstall` — `--keep-config`/`-c`, `--keep-data`/`-d`, `--dry-run`, `--force`/`-f`

### Global flags

`--help`/`-h`, `--version`/`-v`, `--print-logs` (logs to stderr), `--log-level` (`DEBUG|INFO|WARN|ERROR`), `--pure` (run without external plugins)

### Environment variables (selected, UX-relevant)

- Config: `OPENCODE_CONFIG`, `OPENCODE_TUI_CONFIG`, `OPENCODE_CONFIG_DIR`, `OPENCODE_CONFIG_CONTENT` (inline JSON), `OPENCODE_PERMISSION` (inline JSON permissions)
- Server/auth: `OPENCODE_SERVER_PASSWORD` (enables basic auth), `OPENCODE_SERVER_USERNAME` (default `opencode`)
- Behavior: `OPENCODE_AUTO_SHARE`, `OPENCODE_DISABLE_AUTOUPDATE`, `OPENCODE_DISABLE_PRUNE`, `OPENCODE_DISABLE_TERMINAL_TITLE`, `OPENCODE_DISABLE_DEFAULT_PLUGINS`, `OPENCODE_DISABLE_LSP_DOWNLOAD`, `OPENCODE_DISABLE_AUTOCOMPACT`, `OPENCODE_DISABLE_MOUSE`, `OPENCODE_DISABLE_MODELS_FETCH`
- Claude Code interop: `OPENCODE_DISABLE_CLAUDE_CODE` (don't read `.claude`), `OPENCODE_DISABLE_CLAUDE_CODE_PROMPT` (don't read `~/.claude/CLAUDE.md`), `OPENCODE_DISABLE_CLAUDE_CODE_SKILLS` (don't load `.claude/skills`). **They deliberately read a competitor's config format.**
- Misc: `OPENCODE_GIT_BASH_PATH` (Windows), `OPENCODE_ENABLE_EXA`, `OPENCODE_ENABLE_PARALLEL`, `OPENCODE_CLIENT` (client identifier, default `cli`), `OPENCODE_MODELS_URL`
- Experimental: `OPENCODE_EXPERIMENTAL` plus ~16 specific flags including `PLAN_MODE`, `BACKGROUND_SUBAGENTS`, `WORKSPACES`, `LSP_TOOL`, `FILEWATCHER`, `SCOUT` (scout subagent), `EVENT_SYSTEM`, `NATIVE_LLM`, `ICON_DISCOVERY`, `DISABLE_COPY_ON_SELECT`, `BASH_DEFAULT_TIMEOUT_MS`, `OUTPUT_TOKEN_MAX`

---

## 1.3 IDE — the editor integration

This is the thinnest of the seven pages, and the finding is itself significant: **OpenCode's editor story is a terminal, not a panel.**

- **Supported**: "VS Code, Cursor, or any IDE that supports a terminal." Named forks: Cursor, Windsurf, VSCodium.
- **Install**: automatic — run `opencode` in the integrated terminal and "the extension installs automatically." Or manually from the Extension Marketplace. Requires the editor's CLI in PATH: `code`, `cursor`, `windsurf`, `codium`.
- **What the extension actually does** — three things, no more:
  1. Launches/focuses OpenCode in a terminal
  2. "Automatically share[s] your current selection or tab with OpenCode" — the editor pushes selection + active file into the agent's context
  3. Inserts file references in the form **`@File#L37-42`** (path plus line range)
- **Keybinds it adds**:
  - `Cmd+Esc` / `Ctrl+Esc` — open OpenCode, or focus the existing terminal
  - `Cmd+Shift+Esc` / `Ctrl+Shift+Esc` — start a new session
  - `Cmd+Option+K` / `Alt+Ctrl+K` — insert file reference
- **What the editor does NOT contribute** (not documented, and structurally hard for a terminal-hosted agent): diagnostics/LSP errors from the editor's own language servers, applying diffs into the editor's native diff view, gutter decorations, multibuffer review, cursor position, or open-tab set beyond the active one.
- The `acp` command is the more interesting integration path — ACP is the protocol Zed already uses for external agents, so OpenCode can be driven as an ACP agent rather than as a terminal.

**Read for Cowork:** this is OpenCode's weakest surface and Cowork's structural advantage. A native GPUI panel inside a Zed fork can contribute everything the terminal cannot — real multibuffer diff review, LSP diagnostics as context, gutter decorations, click-to-navigate, the open-tab set, and edit application through the editor's own buffer/undo system.

---

## 1.4 Web — the browser UI

- `opencode web` starts a local server on `127.0.0.1` at a random port and opens the default browser. Pitched as "the same powerful AI coding experience without needing a terminal."
- Shows: a homepage listing **active sessions** with a way to start new ones, and a **"See Servers"** view of connected servers and their status.
- Config:

| Setting | Command | Detail |
|---|---|---|
| Port | `opencode web --port 4096` | default: random free port |
| Hostname | `opencode web --hostname 0.0.0.0` | default `127.0.0.1` (localhost only); `0.0.0.0` exposes to the network |
| mDNS | `opencode web --mdns` | advertises as `opencode.local`; `--mdns-domain myproject.local` |
| CORS | `opencode web --cors https://example.com` | allow additional origins for custom frontends |
| Auth | `OPENCODE_SERVER_PASSWORD=secret opencode web` | basic auth; username defaults to `opencode`, override with `OPENCODE_SERVER_USERNAME` |

- Coexists with a TUI on the same backend:
```
opencode web --port 4096
opencode attach http://localhost:4096
```
"sharing the same sessions and state."
- **Security**: "If `OPENCODE_SERVER_PASSWORD` is not set, the server will be unsecured" — acceptable for local use only. Combined with `--hostname 0.0.0.0` and `--cors`, this is an easy footgun.
- The docs don't detail the web UI's in-session rendering, so I can't claim parity with the TUI's tool/diff rendering.

---

## 1.5 Share — session sharing and its privacy surface

- **How**: `/share` "generates a unique URL that'll be copied to your clipboard." Format: `opncd.ai/s/<share-id>`.
- **What gets uploaded**: conversation history, full message exchanges and AI responses, and session metadata — synced to OpenCode's servers. Given that tool calls and diffs are part of the conversation, **shared sessions carry file contents and command output**, which is the real exposure.
- **Hosting**: OpenCode's servers by default; enterprise can self-host on their own infrastructure.
- **Retention**: shares are **publicly accessible indefinitely** until manually unshared. The docs advise against sharing conversations containing "proprietary code or confidential data" and recommend reviewing content before sharing.
- **Unshare**: `/unshare` "remove[s] the share link and delete[s] the data related to the conversation."
- **Three modes** in `opencode.json`:

```json
{ "$schema": "https://opencode.ai/config.json", "share": "manual" }
```
```json
{ "$schema": "https://opencode.ai/config.json", "share": "auto" }
```
```json
{ "$schema": "https://opencode.ai/config.json", "share": "disabled" }
```

`manual` is the default (explicit `/share` required); `auto` shares every new conversation automatically (also `OPENCODE_AUTO_SHARE`); `disabled` blocks sharing entirely and is **enforceable team-wide by committing the config to Git**.
- **Enterprise controls**: disabled entirely for security compliance, or restricted to SSO-authenticated users.
- Related: `opencode export --sanitize` redacts sensitive data for local export, and `opencode import <share-url>` pulls a shared session back into a local one.

---

## 1.6 Keybinds — the config schema

Config lives in `tui.json` under `keybinds`, merged with built-in defaults (you override individual entries, you don't replace the map).

**Leader key.** Default `ctrl+x`, to avoid terminal conflicts. Press leader, then the next key within `leader_timeout` (default `2000` ms). "Some navigation keybinds intentionally do not use the leader key by default."

**Binding value forms:**
- String, single or comma-separated alternatives: `"command_name": "ctrl+x"` / `"ctrl+x,alt+y"`
- Array: `"messages_copy": ["<leader>y", "ctrl+shift+c"]`
- Object, supporting `key`, `event`, `preventDefault`, `fallthrough`:
```json
"input_paste": { "key": "ctrl+v", "preventDefault": false }
```

**Disabling:** `"none"` or `false` — e.g. `"session_compact": "none"`.

**Windows:** `input_undo` defaults to `ctrl+z,ctrl+-,super+z` when not explicitly configured; `terminal_suspend` is forced to `none`.

**Full default map** (~160 actions — the single best inventory of TUI capability):

```json
{
  "$schema": "https://opencode.ai/tui.json",
  "leader_timeout": 2000,
  "keybinds": {
    "leader": "ctrl+x",
    "app_exit": "ctrl+c,ctrl+d,<leader>q",
    "app_debug": "none",
    "app_console": "none",
    "app_heap_snapshot": "none",
    "app_toggle_animations": "none",
    "app_toggle_file_context": "none",
    "app_toggle_diffwrap": "none",
    "app_toggle_paste_summary": "none",
    "app_toggle_session_directory_filter": "none",
    "command_list": "ctrl+p",
    "help_show": "none",
    "docs_open": "none",
    "editor_open": "<leader>e",
    "theme_list": "<leader>t",
    "theme_switch_mode": "none",
    "theme_mode_lock": "none",
    "sidebar_toggle": "<leader>b",
    "scrollbar_toggle": "none",
    "status_view": "<leader>s",
    "session_export": "<leader>x",
    "session_copy": "none",
    "session_move": "none",
    "session_new": "<leader>n",
    "session_list": "<leader>l",
    "session_timeline": "<leader>g",
    "session_fork": "none",
    "session_rename": "ctrl+r",
    "session_delete": "ctrl+d",
    "session_share": "none",
    "session_unshare": "none",
    "session_interrupt": "escape",
    "session_compact": "<leader>c",
    "session_toggle_timestamps": "none",
    "session_toggle_generic_tool_output": "none",
    "session_child_first": "<leader>down",
    "session_child_cycle": "right",
    "session_child_cycle_reverse": "left",
    "session_parent": "up",
    "stash_delete": "ctrl+d",
    "model_provider_list": "ctrl+a",
    "model_favorite_toggle": "ctrl+f",
    "model_list": "<leader>m",
    "model_cycle_recent": "f2",
    "model_cycle_recent_reverse": "shift+f2",
    "model_cycle_favorite": "none",
    "model_cycle_favorite_reverse": "none",
    "mcp_list": "none",
    "provider_connect": "none",
    "console_org_switch": "none",
    "agent_list": "<leader>a",
    "agent_cycle": "tab",
    "agent_cycle_reverse": "shift+tab",
    "variant_cycle": "ctrl+t",
    "variant_list": "none",
    "messages_page_up": "pageup,ctrl+alt+b",
    "messages_page_down": "pagedown,ctrl+alt+f",
    "messages_line_up": "ctrl+alt+y",
    "messages_line_down": "ctrl+alt+e",
    "messages_half_page_up": "ctrl+alt+u",
    "messages_half_page_down": "ctrl+alt+d",
    "messages_first": "ctrl+g,home",
    "messages_last": "ctrl+alt+g,end",
    "messages_next": "none",
    "messages_previous": "none",
    "messages_last_user": "none",
    "messages_copy": "<leader>y",
    "messages_undo": "<leader>u",
    "messages_redo": "<leader>r",
    "messages_toggle_conceal": "<leader>h",
    "tool_details": "none",
    "display_thinking": "none",
    "prompt_submit": "none",
    "prompt_editor_context_clear": "none",
    "prompt_skills": "none",
    "prompt_stash": "none",
    "prompt_stash_pop": "none",
    "prompt_stash_list": "none",
    "workspace_set": "none",
    "input_clear": "ctrl+c",
    "input_paste": { "key": "ctrl+v", "preventDefault": false },
    "input_submit": "return",
    "input_newline": "shift+return,ctrl+return,alt+return,ctrl+j",
    "input_move_left": "left,ctrl+b",
    "input_move_right": "right,ctrl+f",
    "input_move_up": "up",
    "input_move_down": "down",
    "input_select_left": "shift+left",
    "input_select_right": "shift+right",
    "input_select_up": "shift+up",
    "input_select_down": "shift+down",
    "input_line_home": "ctrl+a",
    "input_line_end": "ctrl+e",
    "input_select_line_home": "ctrl+shift+a",
    "input_select_line_end": "ctrl+shift+e",
    "input_visual_line_home": "alt+a",
    "input_visual_line_end": "alt+e",
    "input_select_visual_line_home": "alt+shift+a",
    "input_select_visual_line_end": "alt+shift+e",
    "input_buffer_home": "home",
    "input_buffer_end": "end",
    "input_select_buffer_home": "shift+home",
    "input_select_buffer_end": "shift+end",
    "input_delete_line": "ctrl+shift+d",
    "input_delete_to_line_end": "ctrl+k",
    "input_delete_to_line_start": "ctrl+u",
    "input_backspace": "backspace,shift+backspace",
    "input_delete": "ctrl+d,delete,shift+delete",
    "input_undo": "ctrl+-,super+z",
    "input_redo": "ctrl+.,super+shift+z",
    "input_word_forward": "alt+f,alt+right,ctrl+right",
    "input_word_backward": "alt+b,alt+left,ctrl+left",
    "input_select_word_forward": "alt+shift+f,alt+shift+right",
    "input_select_word_backward": "alt+shift+b,alt+shift+left",
    "input_delete_word_forward": "alt+d,alt+delete,ctrl+delete",
    "input_delete_word_backward": "ctrl+w,ctrl+backspace,alt+backspace",
    "input_select_all": "super+a",
    "history_previous": "up",
    "history_next": "down",
    "dialog.select.prev": "up,ctrl+p",
    "dialog.select.next": "down,ctrl+n",
    "dialog.select.page_up": "pageup",
    "dialog.select.page_down": "pagedown",
    "dialog.select.home": "home",
    "dialog.select.end": "end",
    "dialog.select.submit": "return",
    "dialog.prompt.submit": "return",
    "dialog.mcp.toggle": "space",
    "prompt.autocomplete.prev": "up,ctrl+p",
    "prompt.autocomplete.next": "down,ctrl+n",
    "prompt.autocomplete.hide": "escape",
    "prompt.autocomplete.select": "return",
    "prompt.autocomplete.complete": "tab",
    "permission.prompt.fullscreen": "ctrl+f",
    "plugins.toggle": "space",
    "dialog.plugins.install": "shift+i",
    "terminal_suspend": "ctrl+z",
    "terminal_title_toggle": "none",
    "tips_toggle": "<leader>h",
    "plugin_manager": "none",
    "plugin_install": "none",
    "which_key_toggle": "ctrl+alt+k",
    "which_key_layout_toggle": "ctrl+alt+shift+k",
    "which_key_pending_toggle": "ctrl+alt+shift+p",
    "which_key_group_previous": "ctrl+alt+left,ctrl+alt+[",
    "which_key_group_next": "ctrl+alt+right,ctrl+alt+]",
    "which_key_scroll_up": "ctrl+alt+up,ctrl+alt+p",
    "which_key_scroll_down": "ctrl+alt+down,ctrl+alt+n",
    "which_key_page_up": "ctrl+alt+pageup",
    "which_key_page_down": "ctrl+alt+pagedown",
    "which_key_home": "ctrl+alt+home",
    "which_key_end": "ctrl+alt+end"
  }
}
```

Design notes worth stealing: actions are **namespaced by surface** (`app_`, `session_`, `messages_`, `input_`, `prompt_`, `model_`, `agent_`, `which_key_`, plus dotted context scopes `dialog.select.*`, `prompt.autocomplete.*`, `permission.prompt.*`). Many actions ship as `"none"` — **the action exists and is discoverable in the palette, but is unbound by default**, keeping the default keymap small while the capability surface stays large.

---

## 1.7 Themes — the config schema

**Built-in themes:** `system`, `tokyonight`, `everforest`, `ayu`, `catppuccin`, `catppuccin-macchiato`, `gruvbox`, `kanagawa`, `nord`, `matrix`, `one-dark` (plus `opencode` as the default seen in `tui.json`).

**Setting one:** `/theme` command (or `/themes`, `<leader>t`), or in `tui.json`:
```json
{ "$schema": "https://opencode.ai/tui.json", "theme": "tokyonight" }
```

**Discovery precedence** (later overrides earlier):
1. Built-in themes
2. `~/.config/opencode/themes/*.json`
3. `.opencode/themes/*.json` at project root
4. `.opencode/themes/*.json` in the current directory

**The `system` theme** "generates a custom gray scale based on your terminal's background color" and "leverages standard ANSI colors (0-15) for syntax highlighting," using `none` values to preserve terminal defaults. Auto-adaptation rather than a fixed palette.

**Color value forms:**
- Hex: `"#ffffff"`
- ANSI index: `3` (0–255)
- Reference to a `defs` entry: `"primary"`
- Light/dark variant pair: `{"dark": "#000", "light": "#fff"}`
- Terminal default: `"none"`

**Schema shape** — a `defs` palette plus a `theme` map of semantic roles, each role taking any of the value forms above:

```json
{
  "$schema": "https://opencode.ai/theme.json",
  "defs": { "colorName": "#hexvalue" },
  "theme": {
    "primary": {"dark": "", "light": ""},
    "secondary": {...}, "accent": {...},
    "error": {...}, "warning": {...}, "success": {...}, "info": {...},
    "text": {...}, "textMuted": {...},
    "background": {...}, "backgroundPanel": {...}, "backgroundElement": {...},
    "border": {...}, "borderActive": {...}, "borderSubtle": {...},

    "diffAdded": {...}, "diffRemoved": {...}, "diffContext": {...}, "diffHunkHeader": {...},
    "diffHighlightAdded": {...}, "diffHighlightRemoved": {...},
    "diffAddedBg": {...}, "diffRemovedBg": {...}, "diffContextBg": {...},
    "diffLineNumber": {...}, "diffAddedLineNumberBg": {...}, "diffRemovedLineNumberBg": {...},

    "markdownText": {...}, "markdownHeading": {...}, "markdownLink": {...}, "markdownLinkText": {...},
    "markdownCode": {...}, "markdownCodeBlock": {...}, "markdownBlockQuote": {...},
    "markdownEmph": {...}, "markdownStrong": {...}, "markdownHorizontalRule": {...},
    "markdownListItem": {...}, "markdownListEnumeration": {...},
    "markdownImage": {...}, "markdownImageText": {...},

    "syntaxComment": {...}, "syntaxKeyword": {...}, "syntaxFunction": {...}, "syntaxVariable": {...},
    "syntaxString": {...}, "syntaxNumber": {...}, "syntaxType": {...},
    "syntaxOperator": {...}, "syntaxPunctuation": {...}
  }
}
```

**Terminal requirement:** truecolor (24-bit). Verify with `echo $COLORTERM` (expect `truecolor` or `24bit`); force with `COLORTERM=truecolor`.

**Runtime mode controls:** `theme_switch_mode` and `theme_mode_lock` keybinds — switch between the light/dark variants of the current theme, and pin one so it stops following the system.

**Read for Cowork:** the split is instructive — roughly 15 UI roles vs **36 content roles** (13 diff + 14 markdown + 9 syntax). The chrome is a small part of the theme; almost all of it is about rendering agent output legibly. Cowork inherits Zed's theme system, so the work is *mapping* panel elements onto existing Zed theme roles (and reusing Zed's tree-sitter highlighting and diff colors) rather than inventing a palette.

---

# Part 2 — Gap vs Cowork today

Cowork today: **thread list, search box, model picker, plain chat view with streamed markdown, no tool calls.**

| Capability | OpenCode | Cowork | Gap |
|---|---|---|---|
| Thread/session list | `/sessions`, `<leader>l`, directory filter | Have | Minor: filtering, rename, delete, fork |
| Search | (no documented transcript search) | Have | **Cowork is ahead** |
| Model picker | List + favorites + MRU cycling + provider list + in-app provider connect | Have (plain picker) | Favorites, recents, `f2` cycling, add-provider flow |
| Streamed markdown | 14 themed markdown roles + 9 syntax roles | Have (plain) | Syntax highlighting fidelity, code block treatment |
| **Tool calls in transcript** | First-class, collapsible, per-tool renderers, generic fallback | **None** | **Total** |
| **Diff rendering** | Responsive side-by-side/stacked, word-level highlight, gutter, wrap toggle | **None** | **Total** |
| **Permission / approval** | ask/allow/deny, once/always/reject, suggested patterns, fullscreen, `--auto` badge, per-agent rules | **None** | **Total** |
| **Interrupt / cancel** | `escape`, unmodified, no confirm | **None** | **Total** |
| **Undo / redo of a turn** | `/undo`+`/redo`, Git-backed, reverts files *and* messages | **None** | **Total** |
| **@-mentions / file attach** | Fuzzy search, content injection, named reference roots, `@alias/` browse | **None** | **Total** |
| Slash commands / palette | 17 commands, `ctrl+p` palette, which-key overlay | None | Total |
| Agent picker | `<leader>a`, `tab` to cycle | None | Total |
| Reasoning effort / variants | `ctrl+t` cycles variants | None | Total |
| Thinking block display | `/thinking` toggle (display only) | None | Total |
| Subagent / child sessions | Parent/child tree, arrow-key navigation | None | Total |
| Session timeline / checkpoints | `session_timeline` `<leader>g` | None | Total |
| Compaction / context status | `/compact`, autocompact, `status_view`, `stats` | None | Total |
| Shell passthrough | `!cmd` renders as tool result | None | Total |
| Prompt history / stash | `up`/`down` history, stash/pop/list | None | Total |
| External editor compose | `/editor` `<leader>e` | N/A — Cowork *is* the editor | **Cowork is ahead** |
| Export | `/export` to Markdown, `export --sanitize` to JSON | None | Moderate |
| Notifications when unfocused | `attention`: desktop notify + sound per event type | None | Total |
| Rebindable keymap | ~160 namespaced actions, leader key, which-key | None | Total (but Zed's keymap system is free) |
| Theming | 51-role schema, 4-level file precedence, light/dark variants | Inherits Zed theme | Need diff/tool roles mapped |
| Headless / scripted | `run --format json`, `serve`, `attach`, ACP | None | Strategic, not UX |
| Editor context contribution | Selection + active tab only, `@File#L37-42` | None | **Cowork's structural advantage** |
| Web UI | `opencode web`, shares state with TUI | N/A | Not a near-term concern |
| Sharing | `/share` to `opncd.ai`, 3 modes, enterprise disable | None | Low priority; note privacy design |

**Three structural observations:**

1. **Cowork's "no tool calls at all" is not one gap, it is the trunk of the tree.** Tool calls, diff rendering, permission prompts, interrupt, undo, and child sessions are all downstream of having a tool-call event model in the transcript. Fix the data model once and six features become possible.

2. **OpenCode is a terminal app fighting to reach the editor. Cowork starts inside it.** OpenCode's whole IDE integration is "launch a terminal, push the selection, insert `@File#L37-42`." A GPUI panel in a Zed fork can do native multibuffer diff review, LSP diagnostics as context, gutter decorations, click-to-navigate, and edits applied through the editor's own buffer/undo stack. Do not reimplement OpenCode's terminal-shaped compromises.

3. **OpenCode's client/server split is why it has four front-ends.** If Cowork ever wants headless runs, CI, or a web view, the session/tool-event model should be transport-shaped from the start. Zed already speaks ACP, which is also OpenCode's editor-integration escape hatch.

---

# Part 3 — Prioritized UX features Cowork is missing

Ordered most valuable first. Tiers reflect that everything in Tier 1 is table stakes — an agent panel without these is not competitive regardless of model quality.

## Tier 1 — the panel is not credible without these

1. **Tool calls as first-class transcript items.** An event model where a turn is a sequence of (text | thinking | tool_call | tool_result) parts, rendered as collapsible cards: tool name, key argument as the summary line, status (pending/running/success/error), duration, collapsed-by-default output with expand. Add a global expand/collapse toggle (`/details`) and a *separate* toggle for generic/low-value output. Ship bespoke renderers for the tools that matter — read, edit/write, bash, grep/search — with a generic JSON fallback. **Everything below depends on this.**

2. **Interrupt / cancel.** A single unmodified `Escape` (or a visible Stop button) that halts the running turn, keeps partial output in the transcript, and returns focus to the composer. Cheap to build, and its absence is felt on literally every long turn.

3. **Permission / approval flow.** A prompt with **once / always / reject**, where "always" is offered as a *suggested pattern* (e.g. "allow all `cargo *`") rather than a glob the user must write. Must be expandable to show the full command or full diff before deciding. Back it with a config schema (`allow`/`ask`/`deny` per tool, with path/command pattern matching, last-match-wins) and a visible **auto-approve mode badge** in the panel header. Default `.env` to denied. This is the difference between an agent users let touch their repo and one they don't.

4. **Inline diff rendering for edits — and in Zed, better than inline.** At minimum: word-level intra-line highlighting, hunk headers, gutter line numbers, add/remove/context backgrounds, a wrap toggle, and a responsive side-by-side/stacked switch on panel width. But the Zed-native win is **routing proposed edits into a multibuffer review** with accept/reject per hunk, which no terminal agent can do.

5. **@-mentions with fuzzy file search.** Type `@`, fuzzy-search the worktree, insert a reference, inject content. Then go past OpenCode: mention symbols (via Zed's LSP/outline), selections, diagnostics, and the current diff — not just file paths. Add named reference roots (`@docs`, `@docs/...`) for curated context. Show attached context as removable chips with a hide toggle so it doesn't drown the transcript.

6. **Turn-level undo, VCS-backed.** One action that removes the last user message plus all subsequent responses **and reverts the file changes** those responses made, with redo. OpenCode does this with Git snapshots. In Zed you can do it better by also using buffer-level undo for unsaved edits. This is the highest-trust-per-line-of-code feature on the list.

## Tier 2 — what makes it feel like a real tool

7. **Slash commands + a command palette entry for every panel action.** A command registry the panel owns, discoverable by typing `/` in the composer and from Zed's own palette. Seed it with: new, clear/compact, export, undo/redo, details, thinking, agent, model, share/disable. Namespace actions (`cowork::Interrupt`, `cowork::ToggleToolDetails`) so they land in Zed's keymap system for free — that single decision buys you OpenCode's entire rebindable-keymap story.

8. **Agent picker with single-key cycling.** OpenCode binds `tab`/`shift+tab` to cycle agents from the composer. Combine with per-agent permission profiles (a read-only "review" agent vs. a full-access "build" agent) — that pairing is what makes agent switching meaningful rather than cosmetic.

9. **Reasoning-effort / model-variant control inline.** One keystroke (`ctrl+t` in OpenCode) to cycle effort, separate from the model picker. Plus **model favorites and recents** with cycling, so the picker isn't a scroll through 40 models every time.

10. **Context/status surface.** Tokens used, context window remaining, session cost, elapsed time — plus `/compact` and automatic compaction with a visible notice when it fires. Users need to know when they are about to fall off the context cliff.

11. **Thinking blocks, collapsed by default.** With a display toggle that is clearly labeled as *display only*, distinct from reasoning effort. OpenCode is careful about this distinction and it avoids real confusion.

12. **Notification when the agent needs you and the panel is unfocused.** OpenCode fires desktop notifications *and* sounds, with distinct events for question / permission / error / done / subagent_done. Permission-needed is the critical one — an agent silently blocked on approval behind another window is a dead agent.

13. **Subagent runs as navigable child sessions.** Rather than interleaving subagent output into the parent transcript, make them collapsible child threads you can descend into. Keeps the main transcript readable as the agent count grows.

14. **Composer quality-of-life.** Prompt history (up/down), multi-line with an unambiguous submit/newline split, large-paste-collapses-to-chip, image/file drop, and a draft stash. Individually small, collectively the difference between a text box and a composer.

## Tier 3 — differentiators and long-tail

15. **Editor-native context contribution — Cowork's actual moat.** Auto-attach the current selection and active tab; contribute LSP diagnostics, the symbol under cursor, and the current git diff as first-class context types; click any file/line in the transcript to jump there; decorate the gutter where the agent edited. OpenCode cannot do any of this from a terminal.

16. **Session timeline / checkpoint browser.** A scrubber over the session's snapshots — jump back to any prior state of conversation-and-worktree, not just undo the last turn. Natural extension of #6.

17. **Session fork.** Branch a session at a point to try a different approach without losing the original. Pairs with the timeline.

18. **Shell passthrough from the composer.** `!cargo test` runs and renders as a tool result in the same transcript, so the user's own commands and the agent's share one history the model can see.

19. **Export and share.** `/export` to Markdown for a PR description or bug report; JSON export with a `--sanitize` redaction pass. If you ever add hosted sharing, copy OpenCode's three-mode config (`manual`/`auto`/`disabled`) with **`disabled` committable to the repo for team-wide enforcement** — and be more conservative than they are about indefinite public retention, since shared sessions carry file contents and command output.

20. **Theme roles for agent content.** Map diff (add/remove/context/hunk-header/word-highlight/gutter) and tool-card states onto Zed's existing theme system rather than inventing colors. Mostly reuse, but needs doing deliberately or the panel will look bolted on.

21. **Headless parity.** A scriptable entry point with JSON output, so the same session engine serves CI and automation. Architectural rather than UX, but if the tool-event model is designed transport-shaped from the start it is nearly free later — and retrofitting it is not.
