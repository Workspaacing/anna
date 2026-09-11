# OpenCode: MCP, LSP, Formatters, ACP — and what Cowork would need

Research notes for the Cowork AI panel (Wu editor, Rust/GPUI, Zed fork).

Sources (fetched 2026-09-11; pages last updated Sep 10, 2026):
- https://opencode.ai/docs/mcp-servers/
- https://opencode.ai/docs/lsp/
- https://opencode.ai/docs/formatters/
- https://opencode.ai/docs/acp/
- Supplementary: https://opencode.ai/docs/tools/, https://opencode.ai/docs/permissions/,
  https://agentclientprotocol.com/ (overview, initialization, session-setup, prompt-turn, tool-calls, file-system)

Local repo facts checked while writing (see "Where Cowork is today"):
- `C:\Users\USER\Documents\wu-main\crates\cowork\` — 3,338 LOC, chat-only, no tool loop
- No `agent_servers`, `acp_thread`, `agent_ui`, or `agent-client-protocol` anywhere in the tree or `Cargo.lock`

---

## 0. Where Cowork is today (baseline for every "what we'd need" below)

`crates/cowork/src/` is 8 files / 3,338 lines:

| File | LOC | Role |
|---|---|---|
| `thread.rs` | 981 | thread store (kvp-backed), metadata, history |
| `cowork_panel.rs` | 650 | dock panel, thread list |
| `thread_view.rs` | 509 | message rendering |
| `catalog.rs` | 420 | models.dev catalog, provider list |
| `provider.rs` | 418 | direct HTTP streaming to provider APIs |
| `model_selector.rs` | 270 | picker |
| `cowork.rs` / `cowork_settings.rs` | 90 | init + settings |

The decisive line is `crates/cowork/src/provider.rs:34`:

```rust
/// The subset of a streamed response Cowork renders today. Tool calls are deliberately absent:
/// nothing in the UI can execute one yet, so surfacing them would promise behavior that does not
/// exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionEvent {
    Text(String),
    Stop,
}
```

So Cowork is a **chat panel**, not an agent. There is no tool schema, no tool dispatch, no permission
prompt, no file-edit path, no diff review, no sub-agent, no MCP. Everything in this report is
greenfield for us. That matters enormously for the ACP verdict in §5.

Also note the fork **stripped** Zed's entire agent stack. Upstream Zed's `agent_servers`,
`acp_thread`, `agent_ui`, `agent2` crates and the `agent-client-protocol` Rust dependency are not
present in `wu-main` (`UPSTREAM_VERSION` = `01acd0ee8e906dd0ec8b526fe08da94444a5e2af`). We do **not**
inherit a working ACP client by virtue of being a Zed fork. We inherit the *option* to re-vendor one.

---

## 1. MCP servers

### How OpenCode does it

MCP servers live under a top-level `mcp` key in `opencode.json` / `opencode.jsonc`, keyed by a unique
server name. That name is user-facing: "You can refer to that MCP by name when prompting the LLM."

Shape of the block (verbatim):

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "name-of-mcp-server": {
      // ...
      "enabled": true,
    },
    "name-of-other-mcp-server": {
      // ...
    },
  },
}
```

#### Local / stdio

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "my-local-mcp-server": {
      "type": "local",
      // Or ["bun", "x", "my-mcp-command"]
      "command": ["npx", "-y", "my-mcp-command"],
      "enabled": true,
      "environment": {
        "MY_ENV_VAR": "my_env_var_value",
      },
    },
  },
}
```

Options (verbatim from the docs table):

| Option | Type | Required | Description |
|---|---|---|---|
| `type` | String | Y | Type of MCP server connection, must be `"local"`. |
| `command` | Array | Y | Command and arguments to run the MCP server. |
| `cwd` | String |  | Working directory for the MCP server process. Relative paths resolve from the workspace. |
| `environment` | Object |  | Environment variables to set when running the server. |
| `enabled` | Boolean |  | Enable or disable the MCP server on startup. |
| `timeout` | Number |  | Timeout in ms for fetching tools from the MCP server. Defaults to 5000 (5 seconds). |

Minimal real example:

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "mcp_everything": {
      "type": "local",
      "command": ["npx", "-y", "@modelcontextprotocol/server-everything"],
    },
  },
}
```

#### Remote / HTTP

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "my-remote-mcp": {
      "type": "remote",
      "url": "https://my-mcp-server.com",
      "enabled": true,
      "headers": {
        "Authorization": "Bearer MY_API_KEY"
      }
    }
  }
}
```

| Option | Type | Required | Description |
|---|---|---|---|
| `type` | String | Y | Type of MCP server connection, must be `"remote"`. |
| `url` | String | Y | URL of the remote MCP server. |
| `enabled` | Boolean |  | Enable or disable the MCP server on startup. |
| `headers` | Object |  | Headers to send with the request. |
| `oauth` | Object |  | OAuth authentication configuration. |
| `timeout` | Number |  | Timeout in ms for fetching tools from the MCP server. Defaults to 5000 (5 seconds). |

Note there is **no `type: "sse"`** and no separate transport field — remote is a URL plus headers.

#### Env-var interpolation

Values support `{env:VAR}` substitution, used for secrets:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "context7": {
      "type": "remote",
      "url": "https://mcp.context7.com/mcp",
      "headers": {
        "CONTEXT7_API_KEY": "{env:CONTEXT7_API_KEY}"
      }
    }
  }
}
```

#### OAuth

OpenCode auto-negotiates OAuth for remote servers. On a 401 it initiates the flow, uses
**Dynamic Client Registration (RFC 7591)** if the server supports it, and stores tokens at
`~/.local/share/opencode/mcp-auth.json`.

Pre-registered credentials:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "my-oauth-server": {
      "type": "remote",
      "url": "https://mcp.example.com/mcp",
      "oauth": {
        "clientId": "{env:MY_MCP_CLIENT_ID}",
        "clientSecret": "{env:MY_MCP_CLIENT_SECRET}",
        "scope": "tools:read tools:execute"
      }
    }
  }
}
```

Opt out (API-key servers):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "my-api-key-server": {
      "type": "remote",
      "url": "https://mcp.example.com/mcp",
      "oauth": false,
      "headers": {
        "Authorization": "Bearer {env:MY_API_KEY}"
      }
    }
  }
}
```

| Option | Type | Description |
|---|---|---|
| `oauth` | Object \| false | OAuth config object, or `false` to disable OAuth auto-detection. |
| `clientId` | String | OAuth client ID. If not provided, dynamic client registration will be attempted. |
| `clientSecret` | String | OAuth client secret, if required by the authorization server. |
| `scope` | String | OAuth scopes to request during authorization. |

CLI surface: `opencode mcp auth <name>`, `opencode mcp list`, `opencode mcp logout <name>`,
`opencode mcp auth list`, `opencode mcp debug <name>` (tests connectivity + OAuth discovery).

#### Namespacing and how tools reach the model

> "Once added, MCP tools are automatically available to the LLM alongside built-in tools."

They sit in the same flat tool namespace as built-ins (`bash`, `edit`, `read`, ...). The docs state
the naming rule explicitly:

> **Note**
> MCP server tools are registered with server name as prefix, so to disable all tools for a server
> simply use:
> `"mymcpservername_*": false`

So a tool `search` on server `sentry` is exposed to the model as `sentry_search`. There is no
per-tool description rewriting and no tool filtering at the protocol level — the server's own tool
list is spliced in wholesale.

#### Enabling / disabling — three independent levers

1. **Connection level** — `"enabled": false` on the server entry. Server is never started.
2. **Tool level, global** — the `tools` map, with glob patterns (`*` = zero or more chars,
   `?` = exactly one char, everything else literal):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "my-mcp-foo": { "type": "local", "command": ["bun", "x", "my-mcp-command-foo"] },
    "my-mcp-bar": { "type": "local", "command": ["bun", "x", "my-mcp-command-bar"] }
  },
  "tools": {
    "my-mcp*": false
  }
}
```

3. **Tool level, per agent** — disable globally, re-enable for one agent:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "mcp": {
    "my-mcp": {
      "type": "local",
      "command": ["bun", "x", "my-mcp-command"],
      "enabled": true
    }
  },
  "tools": {
    "my-mcp*": false
  },
  "agent": {
    "my-agent": {
      "tools": {
        "my-mcp*": true
      }
    }
  }
}
```

#### Permission interaction

As of **v1.1.1 the `tools` boolean config is deprecated and merged into `permission`** (still
supported for back-compat). `permission` is three-valued — `"allow"` / `"ask"` / `"deny"` — and MCP
tools are addressed by the same prefix glob:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "permission": {
    "mymcp_*": "ask"
  }
}
```

Global + per-tool, with last-matching-rule-wins pattern evaluation:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "permission": {
    "*": "ask",
    "bash": "allow",
    "edit": "deny"
  }
}
```

Granular object syntax applies the action based on tool *input*:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "permission": {
    "bash": {
      "*": "ask",
      "git *": "allow",
      "npm *": "allow",
      "rm *": "deny",
      "grep *": "allow"
    },
    "edit": {
      "*": "deny",
      "packages/web/src/content/docs/*.mdx": "allow"
    }
  }
}
```

Defaults are permissive: most permissions default to `"allow"`; `doom_loop` and
`external_directory` default to `"ask"`; `read` is `"allow"` except `.env`:

```json
{
  "permission": {
    "read": {
      "*": "allow",
      "*.env": "deny",
      "*.env.*": "deny",
      "*.env.example": "allow"
    }
  }
}
```

Full permission key list: `read`, `edit` (covers edit/write/patch), `glob`, `grep`, `bash`, `task`,
`skill`, `lsp`, `question`, `webfetch`, `websearch`, `external_directory`, `doom_loop`.

Two guard rails worth stealing outright:
- **`external_directory`** — fires whenever *any* path-taking tool touches outside the workspace.
  Supports `~` / `$HOME` expansion in patterns.
- **`doom_loop`** — fires when the same tool call repeats 3 times with identical input. Defaults to
  `"ask"`. A cheap, high-value agent-runaway circuit breaker.

An "ask" prompt offers three outcomes: **once**, **always** (for the rest of the session, matching a
*tool-supplied* pattern set — e.g. bash whitelists a safe prefix like `git status*`), **reject**.
`opencode --auto` auto-approves anything not explicitly denied; explicit `"deny"` still wins.

Org-level defaults can be served from a `.well-known/opencode` endpoint (typically
disabled-by-default, opt-in), and local config overrides remote.

The docs open with a warning we should internalize: MCP tool definitions consume context, and
"Certain MCP servers, like the GitHub MCP server, tend to add a lot of tokens and can easily exceed
the context limit."

### What Cowork would need

- **A tool registry with a flat, prefixed namespace.** Adopt `servername_toolname` verbatim — it is
  already the de facto convention (Claude Code uses `mcp__server__tool`; OpenCode's single-underscore
  form is prettier but collides more easily). Pick one and make the separator a constant.
- **An MCP client.** `rmcp` (the official Rust SDK) or equivalent, over stdio (tokio child process,
  newline-delimited JSON-RPC) and streamable HTTP. Budget the stdio path first — every MCP server
  ships a stdio mode, only some ship HTTP.
- **Settings schema.** Wu uses `settings.rs`-registered settings structs, so this becomes a
  `CoworkSettings` sub-struct. Mirror OpenCode's field names (`type`, `command`, `cwd`,
  `environment`, `enabled`, `timeout`, `url`, `headers`) so users can copy-paste configs between
  tools. Do **not** invent new names.
- **`{env:VAR}` interpolation** in settings values, or we force users to put API keys in a JSON file
  in their repo. Small resolver, but must land with v1.
- **A 5s default tool-fetch timeout**, non-fatal — a hung MCP server must not block panel startup.
- **A three-valued permission engine** (`allow`/`ask`/`deny`), pattern-matched, last-match-wins, with
  `once` / `always` / `reject` UI outcomes. This is the single most reusable design in the whole
  OpenCode config surface and it is independent of MCP — build it first, then let MCP plug into it.
- **Defer OAuth/DCR.** Real work (401 detection, RFC 7591 registration, PKCE, browser handoff, token
  store). Ship `headers` + `{env:...}` first; that covers most servers.
- **A context-cost display.** Show the user how many tokens their enabled MCP servers cost before the
  first message. OpenCode only warns in prose; we can do better cheaply.

---

## 2. LSP servers

### How OpenCode does it

> "OpenCode can integrate with Language Server Protocol (LSP) servers to use diagnostics as feedback
> for the agent."

**LSP is disabled by default.** When enabled, a server starts lazily:

> When LSP is enabled and opencode opens a file, it:
> 1. Checks the file extension against all enabled LSP servers.
> 2. Starts the appropriate LSP server if not already running.

So the primary mode is **internal, not model-facing**: OpenCode runs the language server, and after
the agent writes/edits a file the resulting diagnostics are fed back into the agent loop as feedback.
The model does not "call" LSP for this; it just receives the errors it caused.

**But there is also a model-facing LSP tool**, documented on the Tools page, not the LSP page:

> **lsp (experimental)**
> Interact with your configured LSP servers to get code intelligence features like definitions,
> references, hover info, and call hierarchy.
>
> **Note**
> This tool is only available when `OPENCODE_EXPERIMENTAL_LSP_TOOL=true` (or `OPENCODE_EXPERIMENTAL=true`).
>
> Supported operations include `goToDefinition`, `findReferences`, `hover`, `documentSymbol`,
> `workspaceSymbol`, `goToImplementation`, `prepareCallHierarchy`, `incomingCalls`, and `outgoingCalls`.

Gated by the `lsp` permission key, which the Permissions page describes as
"running LSP queries (currently non-granular)".

**Answer to the question as posed:** both. Diagnostics are internal edit-validation feedback and are
the shipped, default-on-when-enabled behavior. Hover / go-to-definition / references / call-hierarchy
are exposed as a single experimental `lsp` tool behind an env flag.

#### Config schema

Enable all built-ins:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "lsp": true
}
```

Keep built-ins enabled while adding overrides/custom servers:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "lsp": {}
}
```

Per-entry properties (verbatim). "Server entries need `command` unless they only disable a server."

| Property | Type | Description |
|---|---|---|
| `disabled` | boolean | Set this to true to disable the LSP server |
| `command` | string[] | The command to start the LSP server |
| `extensions` | string[] | File extensions this LSP server should handle |
| `env` | object | Environment variables to set when starting server |
| `initialization` | object | Initialization options to send to the LSP server |

Environment variables:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "lsp": {
    "rust": {
      "command": ["rust-analyzer"],
      "env": {
        "RUST_LOG": "debug"
      }
    }
  }
}
```

Initialization options (sent in the LSP `initialize` request):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "lsp": {
    "custom-lsp": {
      "command": ["custom-lsp-server", "--stdio"],
      "extensions": [".custom"],
      "initialization": {
        "preferences": {
          "importModuleSpecifierPreference": "relative"
        }
      }
    }
  }
}
```

Custom server:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "lsp": {
    "custom-lsp": {
      "command": ["custom-lsp-server", "--stdio"],
      "extensions": [".custom"]
    }
  }
}
```

Disable everything (after another config layer enabled it):

```json
{
  "$schema": "https://opencode.ai/config.json",
  "lsp": false
}
```

Disable one:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "lsp": {
    "typescript": {
      "disabled": true
    }
  }
}
```

`OPENCODE_DISABLE_LSP_DOWNLOAD=true` prevents automatic LSP server downloads.

#### Built-in defaults (35 servers, verbatim)

| LSP Server | Extensions | Requirements |
|---|---|---|
| astro | .astro | Auto-installs for Astro projects |
| bash | .sh, .bash, .zsh, .ksh | Auto-installs bash-language-server |
| clangd | .c, .cpp, .cc, .cxx, .c++, .h, .hpp, .hh, .hxx, .h++ | Auto-installs for C/C++ projects |
| csharp | .cs, .csx | .NET SDK installed |
| clojure-lsp | .clj, .cljs, .cljc, .edn | clojure-lsp command available |
| dart | .dart | dart command available |
| deno | .ts, .tsx, .js, .jsx, .mjs | deno command available (auto-detects deno.json/deno.jsonc) |
| elixir-ls | .ex, .exs | elixir command available |
| eslint | .ts, .tsx, .js, .jsx, .mjs, .cjs, .mts, .cts, .vue | eslint dependency in project |
| fsharp | .fs, .fsi, .fsx, .fsscript | .NET SDK installed |
| gleam | .gleam | gleam command available |
| gopls | .go | go command available |
| hls | .hs, .lhs | haskell-language-server-wrapper command available |
| jdtls | .java | Java SDK (version 21+) installed |
| julials | .jl | julia and LanguageServer.jl installed |
| kotlin-ls | .kt, .kts | Auto-installs for Kotlin projects |
| lua-ls | .lua | Auto-installs for Lua projects |
| nixd | .nix | nixd command available |
| ocaml-lsp | .ml, .mli | ocamllsp command available |
| oxlint | .ts, .tsx, .js, .jsx, .mjs, .cjs, .mts, .cts, .vue, .astro, .svelte | oxlint dependency in project |
| php intelephense | .php | Auto-installs for PHP projects |
| prisma | .prisma | prisma command available |
| pyright | .py, .pyi | pyright dependency installed |
| razor | .razor, .cshtml | .NET SDK and VS Code C# extension installed |
| ruby-lsp (rubocop) | .rb, .rake, .gemspec, .ru | ruby and gem commands available |
| rust | .rs | rust-analyzer command available |
| sourcekit-lsp | .swift, .objc, .objcpp | swift installed (xcode on macOS) |
| svelte | .svelte | Auto-installs for Svelte projects |
| terraform | .tf, .tfvars | Auto-installs from GitHub releases |
| tinymist | .typ, .typc | Auto-installs from GitHub releases |
| typescript | .ts, .tsx, .js, .jsx, .mjs, .cjs, .mts, .cts | typescript dependency in project |
| vue | .vue | Auto-installs for Vue projects |
| yaml-ls | .yaml, .yml | Auto-installs Red Hat yaml-language-server |
| zls | .zig, .zon | zig command available |

#### OpenCode's own caveat — read this one twice

> Language servers can get out of sync, use significant memory, vary by version or project, and slow
> down agent workflows. In many projects it is better to have the agent run lint, typecheck, or other
> diagnostic CLI tools directly, so errors are fed back into the agent loop without those tradeoffs.
> Document those commands in instruction files such as AGENTS.md or skills so the agent knows what to
> run.

That is the maintainers of a terminal agent telling you LSP is often not worth it. It is *not* advice
that transfers to us — see below.

### What Cowork would need

**This is the section where we have a structural advantage and should not copy OpenCode.**

OpenCode has to spawn and babysit its own language servers because it is a terminal program with no
editor. Wu **already runs them**. `crates/project/`, `crates/language/`, `crates/lsp/` maintain live
`LanguageServer` handles, an open-buffer set, and a `DiagnosticSet` per buffer, kept warm by the
user's own editing. Every cost OpenCode warns about — memory, startup latency, version skew,
out-of-sync state — we are already paying, and the servers are already in sync with the buffers the
user is looking at.

Concretely:

- **Do not build `lsp` config at all.** No `command` / `extensions` / `env` / `initialization`
  schema, no auto-download, no `OPENCODE_DISABLE_LSP_DOWNLOAD` equivalent. Wu's existing language
  registry and extension system own that. Adding a parallel LSP config would be a strict regression.
- **Post-edit diagnostics feedback is the must-have.** After the agent's edit tool applies a change,
  await the next `publish_diagnostics` for that buffer (Zed's `Project::diagnostics` /
  `DiagnosticSet`), diff against the pre-edit set, and inject *new* diagnostics into the tool result.
  This is maybe 100 lines against existing APIs. It is the single highest-leverage item on this list:
  it converts a model that writes plausible Rust into one that writes compiling Rust.
  - Debounce it. rust-analyzer is slow; gate on
    `project.language_servers_running_disk_based_diagnostics()` or a bounded timeout, and say
    "diagnostics still computing" rather than blocking the turn.
- **Ship the code-intelligence tool non-experimentally.** OpenCode hides `goToDefinition`,
  `findReferences`, `hover`, `documentSymbol`, `workspaceSymbol`, `goToImplementation`,
  `prepareCallHierarchy`, `incomingCalls`, `outgoingCalls` behind an env flag because their LSP
  substrate is unreliable. Ours isn't. `crates/call_hierarchy/` already exists in this fork. Exposing
  these as real tools is a genuine differentiator against every terminal agent: the model can
  navigate a 983k-LOC Rust codebase by symbol graph instead of by grep.
  - Design note: return results as `path:line:col` plus a few lines of context, not raw LSP
    `Location` JSON. Token budget matters.
- **Follow OpenCode on the AGENTS.md point anyway.** Diagnostics are not a typecheck. `cargo check`
  catches things rust-analyzer's incremental pass will not, and the repo's own gates (`script/`,
  clippy config) are the real contract. The agent should still be told to run them.
- **Permission key.** One coarse `lsp` permission (non-granular) is fine — these are read-only
  queries. Default `"allow"`.

---

## 3. Formatters

### How OpenCode does it

> "OpenCode can format files after they are written or edited using language-specific formatters.
> Formatters are disabled by default; enable them in your config before OpenCode will run them."

Flow:

> When OpenCode writes or edits a file and formatters are enabled, it:
> 1. Checks the file extension against all enabled formatters.
> 2. Runs the appropriate formatter command on the file.
> 3. Applies the formatting changes.
>
> This process happens in the background for enabled formatters.

Note it is **file-path based and out-of-process** — the formatter binary rewrites the file on disk,
then OpenCode picks the result back up. Not a buffer transform.

#### Config schema

```json
{
  "$schema": "https://opencode.ai/config.json",
  "formatter": true
}
```

```json
{
  "$schema": "https://opencode.ai/config.json",
  "formatter": {}
}
```

| Property | Type | Description |
|---|---|---|
| `disabled` | boolean | Set this to true to disable the formatter |
| `command` | string[] | The command to run for formatting. Required for custom formatters; optional for built-ins. |
| `environment` | object | Environment variables to set when running the formatter |
| `extensions` | string[] | File extensions this formatter should handle |

Disable all / disable one:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "formatter": false
}
```

```json
{
  "$schema": "https://opencode.ai/config.json",
  "formatter": {
    "prettier": {
      "disabled": true
    }
  }
}
```

Override a built-in + add a custom formatter. `$FILE` is the placeholder for the file path:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "formatter": {
    "prettier": {
      "command": ["npx", "prettier", "--write", "$FILE"],
      "environment": {
        "NODE_ENV": "development"
      },
      "extensions": [".js", ".ts", ".jsx", ".tsx"]
    },
    "custom-markdown-formatter": {
      "command": ["deno", "fmt", "$FILE"],
      "extensions": [".md"]
    }
  }
}
```

#### Built-in formatters (26, verbatim)

| Formatter | Extensions | Requirements |
|---|---|---|
| air | .R | air command available |
| biome | .js, .jsx, .ts, .tsx, .html, .css, .md, .json, .yaml, and more | biome.json(c) config file |
| cargofmt | .rs | cargo fmt command available |
| clang-format | .c, .cpp, .h, .hpp, .ino, and more | .clang-format config file |
| cljfmt | .clj, .cljs, .cljc, .edn | cljfmt command available |
| dart | .dart | dart command available |
| dfmt | .d | dfmt command available |
| gleam | .gleam | gleam command available |
| gofmt | .go | gofmt command available |
| htmlbeautifier | .erb, .html.erb | htmlbeautifier command available |
| ktlint | .kt, .kts | ktlint command available |
| mix | .ex, .exs, .eex, .heex, .leex, .neex, .sface | mix command available |
| nixfmt | .nix | nixfmt command available |
| ocamlformat | .ml, .mli | ocamlformat command available and .ocamlformat config file |
| ormolu | .hs | ormolu command available |
| oxfmt (Experimental) | .js, .jsx, .ts, .tsx | oxfmt dependency in package.json and an experimental env variable flag |
| pint | .php | laravel/pint dependency in composer.json |
| prettier | .js, .jsx, .ts, .tsx, .html, .css, .md, .json, .yaml, and more | prettier dependency in package.json |
| rubocop | .rb, .rake, .gemspec, .ru | rubocop command available |
| ruff | .py, .pyi | ruff command available with config |
| rustfmt | .rs | rustfmt command available |
| shfmt | .sh, .bash | shfmt command available |
| standardrb | .rb, .rake, .gemspec, .ru | standardrb command available |
| terraform | .tf, .tfvars | terraform command available |
| uv | .py, .pyi | uv command available |
| zig | .zig, .zon | zig command available |

Detection is project-aware: "When formatters are enabled, OpenCode will use prettier for matching
files if your project has prettier in package.json."

### What Cowork would need

Same shape as LSP: **we already have this and should not rebuild it.**

Zed's `format_on_save` / `formatter` settings, `Project::format()`, and the `Formatter` enum
(`LanguageServer`, `External { command, arguments }`, `Prettier`, `CodeActions`) already implement
everything in the table above, plus the LSP `textDocument/formatting` path OpenCode can't reach.

What we need is narrow:

- **Call `Project::format()` on the buffer after an agent edit**, honoring the user's existing
  `format_on_save` / language-override settings. One boolean in `CoworkSettings`
  (`format_after_edit`, default: follow `format_on_save`) rather than a whole `formatter` config tree.
- **Format the buffer, then compute the diff we show the user.** Order matters. If we show a diff and
  *then* format, the reviewed content is not the committed content. Format first, review second.
- **Ordering vs. diagnostics.** Format → save → await diagnostics → report. Formatting can itself
  change line numbers, which invalidates any diagnostic positions captured earlier.
- **Failure is non-fatal and must be reported.** A formatter that errors (bad syntax mid-edit is
  common) should surface as a note in the tool result, not abort the turn.
- **Honor `.editorconfig` and project config exactly as the editor does.** Free, because we are
  calling the editor's own path.

The one idea worth importing from OpenCode is `$FILE`-style external formatter commands for languages
Wu has no extension for — but Zed's `Formatter::External { command, arguments }` already covers it.

---

## 4. ACP (Agent Client Protocol)

### How OpenCode does it

The OpenCode docs page is short. Verbatim, the load-bearing parts:

> OpenCode supports the Agent Client Protocol or (ACP), allowing you to use it directly in compatible
> editors and IDEs.
>
> ACP is an open protocol that standardizes communication between code editors and AI coding agents.
>
> To use OpenCode via ACP, configure your editor to run the `opencode acp` command.
>
> The command starts OpenCode as an ACP-compatible subprocess that communicates with your editor over
> JSON-RPC via stdio.

**Direction of the relationship — this is the key architectural fact.** In ACP terminology:
- the **Agent** is the AI program (OpenCode). It runs as a **subprocess**.
- the **Client** is the editor (Zed, JetBrains, Neovim). It **spawns** the agent and **serves**
  requests back to it.

So "is OpenCode an ACP server that editors connect to?" — it is the **Agent** side, and it *is*
spawned by the editor rather than connected to over a socket. Both sides serve methods to each other;
it is a bidirectional JSON-RPC peer relationship over one stdio pipe, not client→server. The editor
implements the Client half, which includes methods OpenCode *calls into* (permissions, file reads,
terminals).

#### Client configurations (verbatim)

Zed — `~/.config/zed/settings.json`:

```json
{
  "agent_servers": {
    "OpenCode": {
      "type": "custom",
      "command": "opencode",
      "args": ["acp"]
    }
  }
}
```

(Or install from the Zed ACP Registry via `zed: acp registry` in the Command Palette; open a thread
with the `agent: new thread` action.)

Zed keybinding — `keymap.json`:

```json
[
  {
    "bindings": {
      "cmd-alt-o": [
        "agent::NewExternalAgentThread",
        {
          "agent": {
            "custom": {
              "name": "OpenCode",
              "command": {
                "command": "opencode",
                "args": ["acp"]
              }
            }
          }
        }
      ]
    }
  }
]
```

JetBrains — `acp.json`:

```json
{
  "agent_servers": {
    "OpenCode": {
      "command": "/absolute/path/bin/opencode",
      "args": ["acp"]
    }
  }
}
```

Avante.nvim:

```lua
{
  acp_providers = {
    ["opencode"] = {
      command = "opencode",
      args = { "acp" }
    }
  }
}
```

With env:

```lua
{
  acp_providers = {
    ["opencode"] = {
      command = "opencode",
      args = { "acp" },
      env = {
        OPENCODE_API_KEY = os.getenv("OPENCODE_API_KEY")
      }
    }
  }
}
```

CodeCompanion.nvim:

```lua
require("codecompanion").setup({
  interactions = {
    chat = {
      adapter = {
        name = "opencode",
        model = "claude-sonnet-4",
      },
    },
  },
})
```

#### Feature parity

> OpenCode works the same via ACP as it does in the terminal. All features are supported:
>
> - Built-in tools (file operations, terminal commands, etc.)
> - Custom tools and slash commands
> - MCP servers configured in your OpenCode config
> - Project-specific rules from AGENTS.md
> - Custom formatters and linters
> - Agents and permissions system
>
> **Note**
> Some built-in slash commands like `/undo` and `/redo` are currently unsupported.

Note what that implies: **MCP servers, formatters, LSP, permissions and AGENTS.md all come from
OpenCode's own config, not from the editor.** An ACP client does not configure any of §1–§3. It gets
whatever the user has in `opencode.json`.

### The protocol's shape (from the ACP spec, since the OpenCode page omits it)

JSON-RPC 2.0, two message kinds: **Methods** (request/response) and **Notifications** (one-way).
Transport for local agents is stdio; remote agents over HTTP/WebSocket are explicitly
"a work in progress".

Lifecycle:

```
1. Initialization
   Client → Agent:  initialize            (protocolVersion + clientCapabilities)
   Client → Agent:  authenticate          (only if the agent advertises authMethods)

2. Session setup  (one of)
   Client → Agent:  session/new           (cwd + mcpServers[])  → { sessionId }
   Client → Agent:  session/load          (requires loadSession capability; replays history)
   Client → Agent:  session/resume        (requires sessionCapabilities.resume; no replay)

3. Prompt turn  (repeatable)
   Client → Agent:  session/prompt        (sessionId + ContentBlock[])
   Agent  → Client: session/update        (notifications, streamed)
   Agent  → Client: session/request_permission, fs/*, terminal/*  (as needed)
   Client → Agent:  session/cancel        (notification, optional)
   Agent  → Client: session/prompt response { stopReason }
```

`initialize` request:

```json
{
  "jsonrpc": "2.0",
  "id": 0,
  "method": "initialize",
  "params": {
    "protocolVersion": 1,
    "clientCapabilities": {
      "fs": {
        "readTextFile": true,
        "writeTextFile": true
      },
      "terminal": true
    },
    "clientInfo": {
      "name": "my-client",
      "title": "My Client",
      "version": "1.0.0"
    }
  }
}
```

`initialize` response:

```json
{
  "jsonrpc": "2.0",
  "id": 0,
  "result": {
    "protocolVersion": 1,
    "agentCapabilities": {
      "loadSession": true,
      "promptCapabilities": {
        "image": true,
        "audio": true,
        "embeddedContext": true
      },
      "mcpCapabilities": {
        "http": true,
        "sse": true
      }
    },
    "agentInfo": {
      "name": "my-agent",
      "title": "My Agent",
      "version": "1.0.0"
    },
    "authMethods": []
  }
}
```

`protocolVersion` is a single integer = MAJOR version, bumped only on breaking changes. Capabilities
omitted MUST be treated as unsupported; adding capabilities is not a breaking change.

`session/new`:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "session/new",
  "params": {
    "cwd": "/home/user/project",
    "mcpServers": [
      {
        "name": "filesystem",
        "command": "/path/to/mcp-server",
        "args": ["--stdio"],
        "env": []
      }
    ]
  }
}
```

→ `{ "result": { "sessionId": "sess_abc123def456" } }`

(Note: the *client* can pass MCP servers per session. OpenCode additionally loads its own.)

`session/prompt`:

```json
{
  "jsonrpc": "2.0",
  "id": 2,
  "method": "session/prompt",
  "params": {
    "sessionId": "sess_abc123def456",
    "prompt": [
      {
        "type": "text",
        "text": "Can you analyze this code for potential issues?"
      },
      {
        "type": "resource",
        "resource": {
          "uri": "file:///home/user/project/main.py",
          "mimeType": "text/x-python",
          "text": "def process_data(items):\n    for item in items:\n        print(item)"
        }
      }
    ]
  }
}
```

#### `session/update` notification variants (what the panel renders)

| `sessionUpdate` | Payload |
|---|---|
| `agent_message_chunk` | `{ messageId, content }` — streamed assistant text |
| `user_message_chunk` | same, used during `session/load` replay |
| `plan` | `entries[]` of `{ content, priority: high\|medium\|low, status: pending\|... }` |
| `tool_call` | `{ toolCallId, title, kind, status, content[], locations[], rawInput, rawOutput }` |
| `tool_call_update` | same fields, all optional except `toolCallId` — partial patch |
| `usage_update` | `{ used, size, cost: { amount, currency } }` — context + spend |

Example tool call announcement:

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "sess_abc123def456",
    "update": {
      "sessionUpdate": "tool_call",
      "toolCallId": "call_001",
      "title": "Reading configuration file",
      "kind": "read",
      "status": "pending"
    }
  }
}
```

`kind` ∈ `read`, `edit`, `delete`, `move`, `search`, `execute`, `think`, `fetch`, `other` (default).
"Tool kinds help Clients choose appropriate icons and optimize how they display tool execution
progress."

`status` ∈ `pending` (not started — input streaming or awaiting approval), `in_progress`,
`completed`, `failed`.

Tool call **content** is one of three shapes — the second and third are why ACP exists and MCP
doesn't suffice:

```json
{ "type": "content", "content": { "type": "text", "text": "Analysis complete. Found 3 issues." } }
```

```json
{
  "type": "diff",
  "path": "/home/user/project/src/config.json",
  "oldText": "{\n  \"debug\": false\n}",
  "newText": "{\n  \"debug\": true\n}"
}
```

```json
{ "type": "terminal", "terminalId": "term_xyz789" }
```

> When a terminal is embedded in a tool call, the Client displays live output as it's generated and
> continues to display it even after the terminal is released.

**Follow-along** — tool calls report `locations[]` so the editor can track the agent live:

```json
{ "path": "/home/user/project/src/main.py", "line": 42 }
```

`usage_update`:

```json
{
  "jsonrpc": "2.0",
  "method": "session/update",
  "params": {
    "sessionId": "sess_abc123def456",
    "update": {
      "sessionUpdate": "usage_update",
      "used": 53000,
      "size": 200000,
      "cost": { "amount": 0.045, "currency": "USD" }
    }
  }
}
```

#### Permission request (Agent → Client)

```json
{
  "jsonrpc": "2.0",
  "id": 5,
  "method": "session/request_permission",
  "params": {
    "sessionId": "sess_abc123def456",
    "toolCall": {
      "toolCallId": "call_001"
    },
    "options": [
      { "optionId": "allow-once",  "name": "Allow once", "kind": "allow_once" },
      { "optionId": "reject-once", "name": "Reject",     "kind": "reject_once" }
    ]
  }
}
```

Response:

```json
{ "jsonrpc": "2.0", "id": 5, "result": { "outcome": { "outcome": "selected", "optionId": "allow-once" } } }
```

`kind` ∈ `allow_once`, `allow_always`, `reject_once`, `reject_always` — a hint for icon/UI treatment.
"Clients MAY automatically allow or reject permission requests according to the user settings."
Note the **agent supplies the option set**; the client renders it. Our permission *policy* would
therefore live on the OpenCode side, not ours, if we drive OpenCode.

#### File system (Agent → Client)

```json
{
  "jsonrpc": "2.0",
  "id": 3,
  "method": "fs/read_text_file",
  "params": {
    "sessionId": "sess_abc123def456",
    "path": "/home/user/project/src/main.py",
    "line": 10,
    "limit": 50
  }
}
```

```json
{
  "jsonrpc": "2.0",
  "id": 4,
  "method": "fs/write_text_file",
  "params": {
    "sessionId": "sess_abc123def456",
    "path": "/home/user/project/config.json",
    "content": "{\n  \"debug\": true,\n  \"version\": \"1.0.0\"\n}"
  }
}
```

Crucially: "These methods enable Agents to access **unsaved editor state** and allow Clients to track
file modifications made during agent execution." That is the whole point — the agent reads the dirty
buffer, not the file on disk.

#### Cancellation and stop reasons

```json
{ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": "sess_abc123def456" } }
```

The client SHOULD preemptively mark unfinished tool calls cancelled and MUST answer pending
`session/request_permission` requests with the `cancelled` outcome. The agent MUST eventually answer
the original `session/prompt` with `stopReason: "cancelled"` — not a JSON-RPC error.

`StopReason` ∈ `end_turn`, `max_tokens`, `max_turn_requests`, `refusal`, `cancelled`.

#### Method inventory — what each side owes the other

**Agent implements (OpenCode already does):**
- baseline: `initialize`, `authenticate`, `session/new`, `session/prompt`
- optional: `session/load`, `logout`, `session/set_mode`, `session/resume`
- notifications received: `session/cancel`

**Client implements (this is our work):**
- **baseline, required: `session/request_permission`**
- optional: `fs/read_text_file`, `fs/write_text_file` (capability `fs.readTextFile` / `fs.writeTextFile`)
- optional: `terminal/create`, `terminal/output`, `terminal/release`, `terminal/wait_for_exit`,
  `terminal/kill` (capability `terminal`)
- optional: `elicitation/create` (structured user input)
- notifications received: `session/update`

Minimum viable client = `initialize` + `session/new` + `session/prompt` + handle `session/update` +
`session/request_permission`. Everything else is capability-gated and can be advertised as false on
day one. The agent adapts.

#### Ecosystem

Clients that speak ACP today: **Zed** (reference implementation, plus an ACP Registry),
**JetBrains IDEs**, **Avante.nvim**, **CodeCompanion.nvim**, and others tracked in the "ACP progress
report" linked from the OpenCode docs. Agents that speak it include OpenCode, Gemini CLI, Claude Code
(via adapter), and a growing set — the value proposition is explicitly LSP-shaped:

> Agents that implement ACP work with any compatible editor. Editors that support ACP gain access to
> the entire ecosystem of ACP-compatible agents.

Design notes worth knowing: ACP "re-uses the JSON representations used in MCP where possible, but
includes custom types for useful agentic coding UX elements, like displaying diffs," and "the default
format for user-readable text is Markdown."

### What Cowork would need

**We are the Client.** Here is the concrete build list.

1. **Vendor the protocol types.** `agent-client-protocol` is a published Rust crate (it's what
   upstream Zed uses). Adding it gives us every struct above, serde-derived, version-tracked. This is
   the single biggest lever — it turns "implement a protocol" into "implement five method handlers".
   - Check licence compatibility (Wu is GPL-3.0-or-later in `crates/cowork`; upstream Zed's agent
     crates were GPL too, so this is likely fine).
2. **A subprocess supervisor.** Spawn `opencode acp`, wire stdin/stdout to a JSON-RPC codec, keep a
   `Task` alive on the GPUI executor, surface stderr into a log view, handle crash/restart. Upstream
   Zed's `agent_servers` crate is exactly this; we deleted it, so we either re-vendor it from
   `01acd0ee` or write ~400 lines.
3. **A bidirectional JSON-RPC peer.** Not a client. We both send requests and serve them on the same
   pipe, with request-id correlation in both directions. Wu's existing `crates/lsp/` already has a
   well-tested bidirectional JSON-RPC-over-stdio implementation — that's the closest prior art in the
   tree.
4. **`session/update` → panel rendering.** This is the bulk of the UI work and most of it is work we
   would need *anyway*:
   - stream `agent_message_chunk` into the markdown renderer (`thread_view.rs` already renders
     markdown — coalesce by `messageId`)
   - render `tool_call` / `tool_call_update` as collapsible cards, iconed by `kind`, with a spinner
     driven by `status`
   - render `diff` content with `crates/buffer_diff` + the editor's diff hunk rendering — we already
     have world-class diff UI, this should look better than Zed's
   - render `terminal` content by embedding a terminal view bound to `terminalId`
   - render `plan` entries as a checklist
   - render `usage_update` as a context/cost meter in the panel header
   - use `locations[]` to implement follow-along: open/scroll the editor to the file and line the
     agent is touching. Flagship demo feature, ~30 lines.
5. **`session/request_permission` → modal or inline card.** Render the agent-supplied `options` with
   `kind`-appropriate styling, return `{ outcome: "selected", optionId }`. Must also handle the
   cancellation contract: on `session/cancel`, answer every in-flight permission request with
   `{ outcome: "cancelled" }`.
6. **`fs/read_text_file` / `fs/write_text_file` against live buffers.** Advertise
   `fs.readTextFile: true, fs.writeTextFile: true` and serve them from `Project` / `BufferStore` —
   the *open buffer* if there is one (unsaved state!), falling back to `Fs::load`. Writes should go
   through the buffer so undo works and the user sees the change live. This is where being an editor
   beats being a terminal, and it is maybe 150 lines.
7. **`terminal/*` against `crates/terminal`.** `create` / `output` / `release` / `wait_for_exit` /
   `kill` map almost 1:1 onto Wu's terminal model. Optional for v1 — advertise `terminal: false` and
   the agent falls back to running commands in its own process, which works but the user can't see
   them. Worth doing in v2 for the observability alone.
8. **Session persistence.** `thread.rs` already stores threads in kvp. Map `ThreadId` ↔ ACP
   `sessionId`, and use `session/load` (agent replays history as `session/update`s) or
   `session/resume` (no replay) to reopen a thread after a restart. Gate on the advertised
   `loadSession` / `sessionCapabilities.resume` capabilities.
9. **Agent server configuration.** Copy Zed's `agent_servers` settings key verbatim so existing Zed
   user configs migrate for free:

```json
{
  "agent_servers": {
    "OpenCode": {
      "type": "custom",
      "command": "opencode",
      "args": ["acp"]
    }
  }
}
```

10. **Cancellation plumbing.** A stop button that sends `session/cancel`, marks tool calls cancelled
    optimistically, and waits for `stopReason: "cancelled"` rather than tearing down the subprocess.

**What we would *not* get from ACP, and must accept:**
- We do not control the system prompt, model selection, context window management, compaction,
  sub-agent strategy, or tool set. Those live inside OpenCode. `model_selector.rs` and `catalog.rs`
  become inert for ACP threads — the agent picks the model (some agents expose `session/set_mode`,
  and CodeCompanion's config shows a `model` field, but it is agent-specific).
- MCP servers, formatters, LSP and permissions for an ACP thread come from `opencode.json`, not from
  Wu settings. Users configure the agent, not the editor. That is a real product seam and we should
  be honest in the UI about which settings apply to which thread type.
- `/undo` and `/redo` don't work over ACP with OpenCode today.
- The agent must be installed. `opencode` is a separate binary the user has to have on PATH.
- Remote/cloud agents over ACP are still "a work in progress" — today this is a local-subprocess story.

---

## 5. Verdict: does adopting ACP let us skip building an agent loop?

**Yes — and given where `crates/cowork` actually is, it is close to the only sane first move.**

The honest framing:

**What ACP gives us for free.** Everything inside the loop: the model conversation, tool schemas,
tool dispatch, the read/edit/bash/grep/glob tool suite, MCP client and OAuth, AGENTS.md loading,
sub-agents, skills, compaction, doom-loop detection, formatter and LSP integration, provider auth for
dozens of providers, and the ongoing maintenance of all of it. OpenCode's own feature list under
"Support" is the list of things we would otherwise be writing for the next year. `provider.rs` today
is 418 lines that can stream text from an OpenAI-shaped endpoint; a competitive agent loop is two
orders of magnitude more than that, and it is a moving target.

**What it costs us.** A Client implementation — realistically 2,000–4,000 lines, most of it panel
rendering we need regardless — plus a permanent dependency on an external binary and a loss of
control over prompt/model/tool strategy.

**Why it is *more* attractive for us than for a generic editor:**
- We are a Zed fork, so the ACP *shape* (external agent threads, `agent_servers` settings,
  `NewExternalAgentThread`) matches idioms the codebase and its users already expect — even though
  the fork deleted the implementation.
- The upstream Rust implementation exists at a known commit (`01acd0ee`) and is re-vendorable:
  `agent_servers`, `acp_thread`, plus the `agent-client-protocol` crate. This is closer to a port
  than a greenfield build.
- The Client-side capabilities ACP asks for — live buffer reads, buffer writes with undo, terminals,
  diff rendering, jump-to-location — are exactly the things Wu already does better than a terminal.
  We would be *strong* at the half of the protocol we own.

**The strategic caveat, stated plainly.** ACP makes us a great *host* for other people's agents. It
does not make Cowork a differentiated agent. If the product thesis is "the best AI panel in a Rust
editor," ACP delivers that quickly and credibly. If the thesis is "our agent is smarter than
OpenCode's," ACP delivers none of it — the intelligence lives in the subprocess.

**Recommended sequencing:**

1. **Now — build the ACP client.** Ship Cowork as an ACP host. Immediately supports OpenCode, Gemini
   CLI, Claude Code, and every future ACP agent. Every line of it (thread view, tool cards, diff
   review, permission UI, follow-along, terminal embedding) is reusable by a native loop later,
   because a native loop would produce the same `session/update`-shaped events.
2. **In parallel — build the permission engine and the post-edit diagnostics/format hooks natively.**
   These are ours regardless. The three-valued `allow`/`ask`/`deny` pattern matcher, `doom_loop`, and
   `external_directory` from §1 are small, high-value, and agent-agnostic.
3. **Later — add a native loop behind the same internal event type.** If we define Cowork's internal
   thread events as a superset of `SessionUpdate`, a native agent and an ACP agent become two
   implementations of one trait, and the entire panel works with both. That is the design decision to
   get right *now*, in step 1, even if the native loop never ships.

The thing not to do is build a bespoke agent loop first and bolt ACP on afterwards. That ordering
throws away the free ecosystem and produces a panel whose internal events don't match the protocol,
making ACP support a rewrite instead of an adapter.
