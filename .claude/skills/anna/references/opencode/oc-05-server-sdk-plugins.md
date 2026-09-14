# OpenCode as a Headless Engine — Server, SDK, Plugins, Ecosystem

Research pass 05. Sources fetched 2026-09-11:
- https://opencode.ai/docs/server/
- https://opencode.ai/docs/sdk/
- https://opencode.ai/docs/plugins/
- https://opencode.ai/docs/ecosystem/
- Supplementary: https://opencode.ai/docs/cli/, https://opencode.ai/docs/permissions/, https://github.com/sst/opencode

Audience: the Cowork team building an AI panel inside a Rust (GPUI) code editor.
Central question: **can we run `opencode serve` as a child process and drive the whole
agent loop over its API instead of reimplementing agents, tools and permissions?**

---

## Verdict up front

**Yes, technically — and it is the single cheapest path to a working agent panel.**
The server is a real, complete, documented HTTP + SSE control plane over the entire
agent loop: sessions, prompts, streaming parts, tool calls, permission round-trips,
file/search access, LSP status, MCP status, agent list, provider auth. There is an
OpenAPI 3.1 spec served live at `/doc`, so a Rust client can be code-generated rather
than hand-written.

**But it is not a library you embed — it is a daemon you adopt.** You would be shipping
a second runtime (Bun/TypeScript, ~100MB class binary), inheriting OpenCode's config
file format, its provider auth store, its agent definitions, its tool set, and its
permission model, none of which are pluggable from the outside in Rust. Deep product
divergence (our own tools, our own permission UX semantics, our own context assembly)
is only reachable through the **plugin system, which is JavaScript-only and runs
inside their process**. That is the real ceiling.

Recommendation stated plainly in "What Cowork would need" at the end.

---

# How OpenCode does it

## 1. The server

### 1.1 Process model

OpenCode's own TUI is already a client of this server. From the docs: *"When you run
opencode it starts a TUI and a server. Where the TUI is the client that talks to the
server."* The headless mode is not a bolt-on — it is the primary architecture, and the
TUI is a reference consumer. That is a strong signal for us: the API is on the critical
path of their own product and will not rot.

```bash
opencode serve [--port <number>] [--hostname <string>] [--cors <origin>]
```

| Flag | Default | Notes |
|---|---|---|
| `--port` | `4096` | listening port |
| `--hostname` | `127.0.0.1` | bind address |
| `--cors` | — | repeatable; add browser origins |
| `--mdns` | `false` | mDNS discovery |
| `--mdns-domain` | `opencode.local` | custom mDNS domain |

```bash
opencode serve --cors http://localhost:5173 --cors https://app.example.com
```

Running `opencode serve` launches a standalone headless server. If a TUI is already
running it starts a *new* server instance — i.e. instances are not shared implicitly.
Conversely a TUI can be pointed at an existing server via `--hostname`/`--port`, and
`opencode attach [url]` connects a TUI to a running backend (flags: `--dir`,
`--continue`, `--session`, `--fork`, `--password`, `--username`).

There is also `POST /instance/dispose`, which matters for clean child-process shutdown.

### 1.2 Authentication

HTTP **basic auth**, off by default, enabled by environment variable:

```bash
OPENCODE_SERVER_PASSWORD=your-password opencode serve
```

`OPENCODE_SERVER_USERNAME` overrides the default username `opencode`.

That is the whole auth story. There is no token issuance, no per-session scoping, no
capability model. Bound to `127.0.0.1` by default, which is the only thing protecting
it out of the box. **For Cowork this is adequate but must be treated deliberately:**
generate a random password per editor launch, pass it via env to the child, bind
loopback, never pass it on the command line (visible in process listings).

Note the *provider* credentials (Anthropic/OpenAI keys, OAuth tokens) are a separate
concern managed by the server itself — see `/provider/auth` and `PUT /auth/:id` below.
Those live in OpenCode's own auth store on disk, not ours.

### 1.3 OpenAPI spec

**Yes.** OpenAPI **3.1**, served live by the running server:

```
http://<hostname>:<port>/doc      e.g. http://localhost:4096/doc
```

This is the most important single fact in this report for implementation cost. It
means a Rust client is a build-step artifact (`progenitor`, `openapi-generator`) rather
than hand-maintained glue, and it means the SDK's TypeScript types are themselves
generated from it (confirmed in the SDK docs), so the TS SDK is a faithful mirror of the
wire protocol and can be read as documentation.

### 1.4 Endpoint surface

**Global**

| Method | Path | Response |
|---|---|---|
| GET | `/global/health` | `{ healthy: true, version: string }` |
| GET | `/global/event` | SSE stream |

**Events (SSE)**

| Method | Path | Description |
|---|---|---|
| GET | `/event` | SSE stream; first event is `server.connected`, then bus events |

**Projects / path / VCS**

| Method | Path | Response |
|---|---|---|
| GET | `/project` | `Project[]` |
| GET | `/project/current` | `Project` |
| GET | `/path` | `Path` |
| GET | `/vcs` | `VcsInfo` |

**Config & providers**

| Method | Path | Body | Response |
|---|---|---|---|
| GET | `/config` | — | `Config` |
| PATCH | `/config` | partial | `Config` |
| GET | `/config/providers` | — | `{ providers: Provider[], default: { [key: string]: string } }` |
| GET | `/provider` | — | `{ all: Provider[], default: {...}, connected: string[] }` |
| GET | `/provider/auth` | — | `{ [providerID: string]: ProviderAuthMethod[] }` |
| POST | `/provider/{id}/oauth/authorize` | — | `ProviderAuthAuthorization` |
| POST | `/provider/{id}/oauth/callback` | — | `boolean` |
| PUT | `/auth/:id` | credentials matching provider schema | `boolean` |

`PATCH /config` is notable: config is mutable at runtime over the wire, which is how a
host app would flip permission modes or switch models without restarting the child.

**Sessions**

| Method | Path | Body | Response |
|---|---|---|---|
| GET | `/session` | — | `Session[]` |
| POST | `/session` | `{ parentID?, title? }` | `Session` |
| GET | `/session/status` | — | `{ [sessionID: string]: SessionStatus }` |
| GET | `/session/:id` | — | `Session` |
| DELETE | `/session/:id` | — | `boolean` |
| PATCH | `/session/:id` | `{ title? }` | `Session` |
| GET | `/session/:id/children` | — | `Session[]` |
| GET | `/session/:id/todo` | — | `Todo[]` |
| POST | `/session/:id/init` | `{ messageID, providerID, modelID }` | `boolean` |
| POST | `/session/:id/fork` | `{ messageID? }` | `Session` |
| POST | `/session/:id/abort` | — | `boolean` |
| POST | `/session/:id/share` | — | `Session` |
| DELETE | `/session/:id/share` | — | `Session` |
| GET | `/session/:id/diff` | query `messageID?` | `FileDiff[]` |
| POST | `/session/:id/summarize` | `{ providerID, modelID }` | `boolean` |
| POST | `/session/:id/revert` | `{ messageID, partID? }` | `boolean` |
| POST | `/session/:id/unrevert` | — | `boolean` |
| POST | `/session/:id/permissions/:permissionID` | `{ response, remember? }` | `boolean` |

Worth calling out the ones that are *product features we would otherwise build*:
`fork` (branch a conversation at a message), `revert`/`unrevert` (undo an agent's
edits back to a message/part — a real checkpointing system), `diff` (the change set a
message produced), `summarize` (compaction), `children` (subagent sessions are child
sessions, addressable from outside), `share` (public link).

**Messages / prompting**

| Method | Path | Body | Response |
|---|---|---|---|
| GET | `/session/:id/message` | query `limit?` | `{ info: Message, parts: Part[] }[]` |
| POST | `/session/:id/message` | `{ messageID?, model?, agent?, noReply?, system?, tools?, parts }` | `{ info: Message, parts: Part[] }` |
| GET | `/session/:id/message/:messageID` | — | `{ info: Message, parts: Part[] }` |
| POST | `/session/:id/prompt_async` | same body as `/message` | `204 No Content` |
| POST | `/session/:id/command` | `{ messageID?, agent?, model?, command, arguments }` | `{ info: Message, parts: Part[] }` |
| POST | `/session/:id/shell` | `{ agent, model?, command }` | `{ info: Message, parts: Part[] }` |

The prompt body is the richest object in the API. Note `system` (per-call system prompt
override), `tools` (per-call tool allow/deny selection), `agent` (which configured agent
persona to run), `model`, and `noReply` (insert a user message without triggering a
completion — useful for injecting editor context). `parts` is the multimodal content
array, e.g. `[{ type: "text", text: "..." }]`.

**Commands, files, search**

| Method | Path | Query | Response |
|---|---|---|---|
| GET | `/command` | — | `Command[]` |
| GET | `/find` | `pattern=<pat>` | match objects (`path`, `lines`, `line_number`, `absolute_offset`, `submatches`) |
| GET | `/find/file` | `query`, `type?`, `directory?`, `limit?`, `dirs?` | `string[]` |
| GET | `/find/symbol` | `query` | `Symbol[]` |
| GET | `/file` | `path` | `FileNode[]` |
| GET | `/file/content` | `path` | `FileContent` |
| GET | `/file/status` | — | `File[]` (tracked files) |

The `/find` shape is literally ripgrep's JSON output. `/find/symbol` is LSP-backed.

**Tools (experimental), LSP, formatters, MCP, agents, logging**

| Method | Path | Query/Body | Response |
|---|---|---|---|
| GET | `/experimental/tool/ids` | — | `ToolIDs` |
| GET | `/experimental/tool` | `provider=<p>&model=<m>` | `ToolList` |
| GET | `/lsp` | — | `LSPStatus[]` |
| GET | `/formatter` | — | `FormatterStatus[]` |
| GET | `/mcp` | — | `{ [name: string]: MCPStatus }` |
| POST | `/mcp` | `{ name, config }` | MCP status object |
| GET | `/agent` | — | `Agent[]` |
| POST | `/log` | `{ service, level, message, extra? }` | `boolean` |

`POST /mcp` is significant: **MCP servers can be registered at runtime over HTTP.**
That is the one supported way to add tools from outside the process without writing a
JS plugin. See the assessment section — this is the main escape hatch for Cowork.

**TUI control** (`/tui/append-prompt`, `/open-help`, `/open-sessions`, `/open-themes`,
`/open-models`, `/submit-prompt`, `/clear-prompt`, `/execute-command`, `/show-toast`,
plus `GET /tui/control/next` and `POST /tui/control/response`). This suite exists so
editor plugins can drive an already-running TUI — irrelevant to us except as proof that
their own IDE integrations take the "drive the process" approach rather than embedding.

**Instance**: `POST /instance/dispose` → `boolean`.

### 1.5 The prompt → stream → tool-call → permission round-trip

This is the part that matters most, so here it is as a sequence.

1. **Open the event stream first.** `GET /event` (SSE). First event delivered is
   `server.connected`; after that it is a firehose of the internal event bus, *not*
   scoped per session — you filter client-side by session id.

2. **Create a session.** `POST /session` with `{ parentID?, title? }` → `Session`
   (`{ id, ... }`).

3. **Send the prompt asynchronously.**
   `POST /session/:id/prompt_async` with
   `{ messageID?, model?, agent?, noReply?, system?, tools?, parts }` → `204 No Content`.
   The synchronous sibling `POST /session/:id/message` blocks until the full assistant
   turn completes and returns `{ info: Message, parts: Part[] }`. **For an editor panel
   you want `prompt_async` plus the event stream**; the synchronous form is for scripts.

4. **Consume streamed output from the event bus.** Streaming is expressed as *mutation
   events on message parts*, not as a token delta protocol:
   - `message.updated` — the assistant message envelope changed (status, usage, model)
   - `message.part.updated` — a part was created or amended (this is where text deltas,
     tool call state, and reasoning arrive)
   - `message.part.removed`, `message.removed`
   - `session.status`, `session.idle` (turn finished), `session.error`,
     `session.diff`, `session.compacted`, `todo.updated`, `file.edited`

   Rendering is therefore **"reconcile a part list keyed by part id"**, not "append
   tokens". This is actually a good fit for a GPUI panel — it maps to a retained model
   you patch, and it makes reconnection trivial (re-`GET /session/:id/message` to
   resync, then resume the stream).

5. **Tool calls** appear as entries in the same `parts` array (documented as `tool_call`
   entries), progressing through states via `message.part.updated`. The editor renders
   them; it does not execute them. Execution happens inside the OpenCode process.

6. **Permission round-trip.** When a tool needs approval the server emits
   `permission.asked` on the event bus. The client answers out-of-band with:

   ```
   POST /session/:id/permissions/:permissionID
   Body: { response, remember? }
   → boolean
   ```

   and a `permission.replied` event follows. `response` corresponds to the TUI's three
   choices — documented in the permissions page as **`once`**, **`always`**
   (session-wide whitelist), **`reject`**; `remember` is the persistence flag.
   The agent loop is genuinely blocked on this HTTP call — meaning **a host editor gets
   a real, synchronous-feeling approval gate it can render as native UI.** This is the
   single most valuable thing the API gives us for free.

   Policy is configured declaratively in `opencode.json` and is `PATCH /config`-able:

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

   Values: `"allow"`, `"ask"`, `"deny"`. Gated tool ids: `read`, `edit`, `glob`, `grep`,
   `bash`, `task`, `skill`, `lsp`, `question`, `webfetch`, `websearch`,
   `external_directory` (out-of-project paths), `doom_loop` (repeated identical calls).
   Bash supports glob patterns with **last matching rule wins**:

   ```json
   "bash": {
     "*": "ask",
     "git *": "allow",
     "rm *": "deny"
   }
   ```

   Per-agent overrides merge over the global config, agent taking precedence:

   ```json
   "agent": {
     "build": {
       "permission": {
         "bash": { "git commit *": "deny" }
       }
     }
   }
   ```

7. **Interrupt** with `POST /session/:id/abort`.
8. **Undo** with `POST /session/:id/revert` `{ messageID, partID? }`.

### 1.6 Gaps in the documented wire protocol

Honest accounting of what the docs do **not** pin down and we would have to learn from
`/doc` or the TS types:

- The exact SSE envelope. Docs say "bus events" and name the types, but the concrete
  `{ type, properties }` payload shape per event is only in the OpenAPI spec /
  `types.gen.ts`. Not a blocker — it is machine-readable — but it is not in prose.
- The `Part` union discriminants (text / reasoning / tool / file / patch / step markers)
  and the tool-call state machine. Same situation.
- Whether `/event` supports `Last-Event-ID` resumption. Unstated. Assume no; design for
  resync-by-refetch.
- Backpressure / stream fan-out limits with many concurrent subscribers. Unstated.

---

## 2. The SDK

**One language: JavaScript/TypeScript.** Package `@opencode-ai/sdk`,
`npm install @opencode-ai/sdk`. There is **no official Rust, Go, or Python SDK.** (The
Go TUI talks to the server directly.) Types are auto-generated from the server's OpenAPI
spec — `packages/sdk/js/src/gen/types.gen.ts` in the repo — which is exactly the
pipeline we would replicate in Rust.

Two entry points:

```javascript
// spawn a server AND get a client
import { createOpencode } from "@opencode-ai/sdk"
const { client } = await createOpencode()
```
Options: `hostname` (default `127.0.0.1`), `port` (default `4096`), `signal`
(`AbortSignal`), `timeout` (default `5000` ms), `config` (a `Config` object).

```javascript
// client only, against an already-running server
import { createOpencodeClient } from "@opencode-ai/sdk"
const client = createOpencodeClient({ baseUrl: "http://localhost:4096" })
```
Options: `baseUrl` (default `http://localhost:4096`), `fetch` (custom), `parseAs`,
`responseStyle` (`data` | `fields`), `throwOnError`.

`createOpencode()` is precisely the "spawn a child process and talk to it" pattern —
in TypeScript. **That is the pattern we want, and it is the pattern we would have to
reimplement in Rust**, because the process-spawning half of the SDK is JS.

### Method map (1:1 with the HTTP surface)

| Namespace | Methods |
|---|---|
| `client.global` | `health()` |
| `client.app` | `log({ body: { service, level, message } })`, `agents()` |
| `client.project` | `list()`, `current()` |
| `client.path` | `get()` |
| `client.config` | `get()`, `providers()` |
| `client.session` | `list()`, `get({path:{id}})`, `children()`, `create({body:{title}})`, `delete()`, `update()`, `init()`, `abort()`, `share()`, `unshare()`, `summarize()`, `messages()`, `message()`, `prompt()`, `command()`, `shell()`, `revert()`, `unrevert()` |
| `client.find` | `text({ query: { pattern } })`, `files({ query: { query, type?, directory?, limit? } })`, `symbols({ query })` |
| `client.file` | `read({ query: { path } })` → `{ type: "raw" \| "patch", content: string }`, `status()` |
| `client.tui` | `appendPrompt`, `openHelp`, `openSessions`, `openThemes`, `openModels`, `submitPrompt`, `clearPrompt`, `executeCommand`, `showToast` |
| `client.auth` | `set({ path: { id }, body: { type, key } })` |
| `client.event` | `subscribe()` → SSE stream |

`client.session.prompt()` returns `AssistantMessage` (or `UserMessage` when
`noReply: true`). Note the SDK's `prompt` maps to the **synchronous**
`POST /session/:id/message`. The async `prompt_async` is on the HTTP surface; drive it
directly.

### Structured output

The prompt body accepts a `format` field — validated JSON-schema responses with
automatic retries. This is a genuinely useful primitive for an editor panel (think:
"give me a rename plan as JSON"):

```javascript
const result = await client.session.prompt({
  path: { id: sessionId },
  body: {
    parts: [{ type: "text", text: "..." }],
    format: {
      type: "json_schema",
      schema: {
        type: "object",
        properties: { /* ... */ },
        required: ["field1"]
      },
      retryCount: 2
    }
  }
})
// result.data.info.structured_output
```
`format.type` is `text` (default) or `json_schema`. Failure surfaces as
`result.data.info.error?.name === "StructuredOutputError"`.

Error handling is plain try/catch (or `throwOnError: false` + `responseStyle: "fields"`).

---

## 3. Plugins

### Format and location

JS/TS ES modules exporting async factory functions. Loaded automatically from:
- project: `.opencode/plugins/`
- global: `~/.config/opencode/plugins/`

Load order: global config (`~/.config/opencode/opencode.json`) → project config
(`opencode.json`) → global plugin dir → project plugin dir. Duplicate npm packages load
once; local and npm plugins of similar names load separately.

```javascript
export const MyPlugin = async ({ project, client, $, directory, worktree }) => {
  return {
    // hook implementations
  }
}
```

```typescript
import type { Plugin } from "@opencode-ai/plugin"
```

### The API surface handed to a plugin

| Arg | What it is |
|---|---|
| `project` | current project info |
| `directory` | current working directory |
| `worktree` | git worktree path |
| `client` | **a full OpenCode SDK client** — the plugin can call the whole HTTP API |
| `$` | **Bun's shell API** — arbitrary command execution |

Read that twice: a plugin gets `$` (shell) and a fully-privileged SDK client. **Plugins
are unsandboxed code running in the agent's process with the agent's authority.** Any
plugin-based extension story for Cowork is also a supply-chain surface, and npm-sourced
plugins are auto-installed (see below).

### Hooks

Mutation hooks, `async (input, output) => void`, where you mutate `output` in place or
`throw` to veto:

- `tool.execute.before` — intercept/rewrite/deny a tool call before it runs
- `tool.execute.after` — post-process a tool result
- `shell.env` — inject environment variables into shell tool invocations
- `experimental.session.compacting` — append to or replace the compaction prompt
- `tool` — an object of custom tool definitions (not a callback; a registry)
- `event` — `async ({ event }) => void`, matched on `event.type`

The `event` hook receives the same bus that `/event` exposes over SSE. Documented event
types, grouped:

| Group | Types |
|---|---|
| Command | `command.executed` |
| File | `file.edited`, `file.watcher.updated` |
| Installation | `installation.updated` |
| LSP | `lsp.client.diagnostics`, `lsp.updated` |
| Message | `message.updated`, `message.removed`, `message.part.updated`, `message.part.removed` |
| Permission | `permission.asked`, `permission.replied` |
| Server | `server.connected` |
| Session | `session.created`, `session.updated`, `session.deleted`, `session.idle`, `session.error`, `session.status`, `session.diff`, `session.compacted` |
| Todo | `todo.updated` |
| Shell | `shell.env` |
| Tool | `tool.execute.before`, `tool.execute.after` |
| TUI | `tui.prompt.append`, `tui.command.execute`, `tui.toast.show` |

**Important asymmetry:** the *events* are observable over HTTP/SSE, but the
*interception* hooks are not. Over the wire you can watch `tool.execute.before` happen;
you cannot **veto** it. Vetoing requires in-process JS. The only wire-level veto is the
permission system (`permission.asked` → `POST .../permissions/:permissionID`), which
only fires for actions the config marks `"ask"`.

### Examples (verbatim)

```javascript
export const InjectEnvPlugin = async () => {
  return {
    "shell.env": async (input, output) => {
      output.env.MY_API_KEY = "secret"
      output.env.PROJECT_ROOT = input.cwd
    }
  }
}
```

```javascript
export const EnvProtection = async () => {
  return {
    "tool.execute.before": async (input, output) => {
      if (input.tool === "read" && output.args.filePath.includes(".env")) {
        throw new Error("Do not read .env files")
      }
    }
  }
}
```

```typescript
import { type Plugin, tool } from "@opencode-ai/plugin"

export const CustomToolsPlugin: Plugin = async (ctx) => {
  return {
    tool: {
      mytool: tool({
        description: "This is a custom tool",
        args: { foo: tool.schema.string() },
        async execute(args, context) {
          return `Hello ${args.foo} from ${context.directory}`
        }
      })
    }
  }
}
```

```typescript
export const CompactionPlugin: Plugin = async (ctx) => {
  return {
    "experimental.session.compacting": async (input, output) => {
      output.context.push("## Custom Context\n...")
      // OR replace entirely:
      output.prompt = "Custom compaction prompt..."
    }
  }
}
```

```typescript
export const MyPlugin = async ({ client }) => {
  await client.app.log({
    body: { service: "my-plugin", level: "info", message: "Plugin initialized" }
  })
}
```

### Distribution

npm packages, declared in `opencode.json`:

```json
{
  "plugin": ["opencode-helicone-session", "@my-org/custom-plugin"]
}
```

Installed automatically **using Bun**, cached at `~/.cache/opencode/node_modules/`.
Scoped packages supported. Local plugins may declare their own deps via a
`.opencode/package.json`:

```json
{
  "dependencies": {
    "shescape": "^2.1.0"
  }
}
```

There is no registry, no signing, no manifest of permissions, no version pinning story
beyond npm semver. **If Cowork ships OpenCode, it ships an auto-installing npm plugin
loader inside the editor.** That is a security review item, not a footnote.

---

## 4. Ecosystem

Documented at /docs/ecosystem/. Shape of it: a large, active, **almost entirely
JS/TS-and-npm** community, organised around plugins rather than alternate SDKs.

**Auth / billing workarounds** (evidence people run OpenCode against subscription
plans, not just API keys): `opencode-openai-codex-auth` (ChatGPT Plus/Pro subscription),
`opencode-gemini-auth`, `opencode-antigravity-auth`, `opencode-google-antigravity-auth`.

**Dev/workflow**: `opencode-daytona` (isolated sandboxes + git sync),
`opencode-devcontainers` (multi-branch isolation, shallow clones), `opencode-worktree`,
`opencode-type-inject` (auto-inject TS/Svelte types into file reads),
`opencode-dynamic-context-pruning` (prune obsolete tool outputs),
`opencode-vibeguard` (redact secrets/PII before LLM calls), `opencode-pty` (background
processes), `opencode-shell-strategy` (prevent TTY hangs), `opencode-websearch-cited`.

**Productivity/monitoring**: `opencode-helicone-session`, `opencode-sentry-monitor`,
`opencode-wakatime`, `opencode-morph-fast-apply` / `opencode-morph-plugin` (Morph Fast
Apply, WarpGrep), `oh-my-opencode` (background agents, LSP/AST/MCP tools),
`opencode-notificator` / `opencode-notifier` / `opencode-notify` (desktop notifications
for permission, completion, error events), `opencode-supermemory` (persistent memory),
`opencode-skillful` (lazy-load prompts / skill discovery), `@plannotator/opencode`
(interactive plan review with annotation), `@openspoon/subtask2`, `opencode-scheduler`,
`opencode-conductor` (Context → Spec → Plan → Implement), `opencode-background-agents`,
`opencode-workspace` (multi-agent orchestration), `opencode-goal-plugin`,
`opencode-firecrawl`, `opencode-tavily`, `opencode-jfrog-plugin`,
`opencode-md-table-formatter`, `opencode-zellij-namer`.

**Clients & editor integrations** — the directly relevant category:
- `opencode.nvim` (NickvanDyke) — Neovim plugin
- `opencode.nvim` (sudo-tee) — terminal-based Neovim frontend
- `OpenChamber` — web/desktop app **and a VS Code extension**
- `CodeNomad` — desktop, web, mobile and remote client
- `portal` — mobile-first web UI over Tailscale/VPN
- `OpenCode-Obsidian` — embeds OpenCode in Obsidian
- `octto` — browser UI for brainstorming
- `kimaki` — Discord bot for session control
- `sdks/vscode` in-repo — first-party VS Code integration

**Tooling**: `ai-sdk-provider-opencode-sdk` (exposes OpenCode as a Vercel AI SDK
provider), `ocx` (extension manager with isolated profiles), `opencode plugin template`,
`micode`, `OpenWork` (self-described open-source Claude Cowork alternative — worth a
competitive look given our product name), `Agentic` and `opencode-agents` (agent configs).

Aggregators: `awesome-opencode` (github.com/awesome-opencode/awesome-opencode) and
opencode.cafe.

**Read for us:** every one of these editor clients is a *thin UI over the server*. None
of them embed the loop. Multiple mature, non-trivial editor front ends have converged
on exactly the architecture we are considering. That is the strongest empirical
evidence that the approach works.

**Also read for us:** there is **no Rust client in the ecosystem.** We would be first.

## 5. Licensing and packaging

- **License: MIT.** Permissive — we can bundle, redistribute, and ship it commercially
  inside a closed-source editor, with attribution and the license text. No copyleft
  contamination of our Rust code, and no obligation triggered by driving it over HTTP
  in any case.
- **Runtime: TypeScript on Bun** (`package.json`, `bunfig.toml`, `tsconfig.json`;
  Turbo monorepo under `packages/`). The TUI is Go. So the "single binary" is a
  Bun-compiled bundle, not a small static executable.
- **Distribution**: `curl -fsSL https://opencode.ai/install | bash`; npm, Homebrew,
  Scoop, Pacman, Nix; plus desktop binaries for macOS/Windows/Linux from releases.
- **Repo**: github.com/sst/opencode. (Note: the SDK docs link `types.gen.ts` under
  `anomalyco/opencode` — an org/ownership move appears to be in flight. Worth
  confirming before we take a hard dependency; governance changes are a supply risk.)

Packaging consequences for Cowork:
- We must ship or fetch a per-platform OpenCode binary (3 platforms x arm64/x64 ≈ 6
  artifacts) and version-lock it against the OpenAPI spec we generated from.
- Bun-bundled binaries are large (tens to >100 MB). Against a Rust editor's own
  footprint that is a visible, but survivable, increase.
- Code signing / notarization on macOS and Windows applies to the bundled child binary
  too.
- Auto-update becomes two-headed: the editor updates, and the engine updates. The
  `installation.updated` event and `/global/health` (`{ healthy, version }`) give us
  the hooks to detect drift.

---

# What Cowork would need

## The honest assessment

### What works — and works well

1. **The whole agent loop is genuinely drivable over the wire.** Sessions, prompting,
   streaming, tool execution, subagents (child sessions), compaction, abort, and — the
   big one — **permission approval as a blocking HTTP round-trip we can render as native
   GPUI UI.** We would not reimplement any of it.
2. **OpenAPI 3.1 at `/doc`** makes a typed Rust client a build-step, not a project. Use
   `progenitor` or `openapi-generator` against a spec snapshot pinned per OpenCode
   version, checked into our repo so builds are hermetic.
3. **Multi-provider model support, provider OAuth, MCP, LSP integration, ripgrep search,
   agent personas, custom commands, checkpoint/revert, session sharing** — all free.
   Conservatively this is 6–12 engineer-months of work we skip.
4. **Prior art**: Neovim, VS Code, Obsidian, web, desktop and mobile clients all do
   exactly this. The API is load-bearing for OpenCode's own TUI, so it will stay good.
5. **MIT license** — no obstacle to bundling in a commercial closed-source editor.
6. **Runtime extensibility without JS, via `POST /mcp`.** We can register an MCP server
   at runtime and expose *Cowork's own* capabilities (open buffer contents, selection,
   diagnostics, "apply this diff to the editor", project index) as tools the agent can
   call. This is the key architectural move: **Cowork becomes an MCP server as well as
   an OpenCode client.** Editor-native context stops being something we have to fake
   through prompt stuffing.
7. **`prompt_async` + event-stream reconciliation** is a better fit for a retained-mode
   GPUI panel than a token-delta stream would be. Parts are keyed and patched; resync
   after a disconnect is a refetch.

### What does not work

1. **It is a second runtime, not a library.** Bun + TypeScript inside a Rust editor.
   Process lifecycle, crash recovery, zombie cleanup on hard-kill, port allocation
   collisions, antivirus/EDR flagging an unfamiliar bundled binary spawning shells,
   corporate proxy behaviour — all now our problems. `POST /instance/dispose` plus a
   supervisor with health-checking via `/global/health` is table stakes.
2. **No Rust SDK, and no Rust *server-spawning* helper.** `createOpencode()` — the
   spawn-and-connect convenience — is JS. We reimplement it: spawn, wait for port,
   handshake, backoff, restart. Not hard, but it is ours to own forever.
3. **Deep customization requires writing JavaScript plugins that run in their process.**
   We cannot intercept `tool.execute.before` from Rust. We cannot replace their `edit`
   tool with an editor-aware one from Rust. We cannot rewrite the system prompt per turn
   from Rust *except* via the per-call `system` and `tools` fields on the prompt body
   (which do cover a lot — do not underrate them). Anything finer means shipping a
   bundled JS plugin, i.e. maintaining a TypeScript component inside a Rust product.
4. **Permission semantics are theirs.** The gate points are OpenCode's tool taxonomy
   (`read`/`edit`/`bash`/`webfetch`/`task`/`external_directory`/`doom_loop`/…) with
   `allow`/`ask`/`deny` and last-match-wins bash globs. If Cowork's product wants a
   different risk model — per-workspace trust, diff-preview-before-write, staged
   auto-approval by file path — we are bending their model, not defining ours. Some of
   that is reachable via `PATCH /config` at runtime; some is not.
5. **Weak auth.** Basic auth only, no per-session scoping. Fine on loopback,
   disqualifying for any remote/hosted Cowork story without us fronting it with our own
   proxy.
6. **Plugin supply chain.** If we expose OpenCode's plugin config to users, the editor
   auto-installs arbitrary npm code with shell access into the agent process. We would
   need to either disable plugin loading, pin an allowlist, or accept and disclose it.
7. **Product identity risk.** Session model, agent concept, config file
   (`opencode.json`), slash commands, and auth store are all OpenCode-shaped and
   user-visible. Cowork would be a skin unless we invest in hiding the seams — and if
   we hide them well, users with existing OpenCode configs get a confusing half-match.
8. **Undocumented protocol corners** (SSE envelope shapes, Part union, resumption
   semantics, backpressure) mean real integration work reading `types.gen.ts` and the
   live `/doc`, not just reading prose.
9. **Governance**: the `sst` → `anomalyco` repo signal. Confirm ownership and any
   commercial plans before committing.

### The strategic shape of the decision

The two options are not actually symmetric:

- **Drive OpenCode**: fastest to a credible panel (weeks, not quarters), ceiling set by
  someone else's roadmap and someone else's extension language.
- **Embed our own loop in Rust**: slowest to parity, but every differentiator we want —
  editor-native tools, our permission UX, our context assembly, our checkpointing over
  the editor's own buffer history, single-binary shipping — is directly in reach.

They are compatible in sequence, and that is the recommendation.

## Recommended path

**Phase 1 — adopt, behind an abstraction.** Ship OpenCode as a supervised child process
and prove the product. Concretely:

1. Define a Rust `AgentBackend` trait in Cowork that speaks in *our* domain vocabulary
   (turn, part, tool invocation, approval request, checkpoint) — **not** OpenCode's.
   Everything in the UI layer talks to the trait. This single decision is what keeps
   Phase 2 open.
2. Generate the OpenCode HTTP client from a **pinned snapshot** of `/doc` (OpenAPI 3.1)
   checked into the repo; `progenitor` or `openapi-generator`. Regenerate deliberately
   on version bumps, with a CI diff gate on the spec.
3. Build the supervisor: spawn `opencode serve --port <ephemeral> --hostname 127.0.0.1`
   with `OPENCODE_SERVER_PASSWORD` = per-launch random secret **passed via env, never
   argv**; poll `/global/health`; reconnect `/event` with backoff; `POST /instance/dispose`
   then SIGTERM then SIGKILL on shutdown; reap orphans on next launch.
4. Panel data flow: `GET /event` (SSE) into a GPUI-owned session store; `POST /session`
   to open; `POST /session/:id/prompt_async` to send; reconcile
   `message.part.updated` / `message.updated` into the retained part list; `session.idle`
   ends the turn; `POST /session/:id/abort` for stop.
5. Permissions: render `permission.asked` as native Cowork UI; reply with
   `POST /session/:id/permissions/:permissionID` `{ response, remember }`. Drive the
   baseline policy via `PATCH /config` so the user's mode switch in our UI is real.
6. **Stand up Cowork as an MCP server and register it with `POST /mcp`** — exposing
   open buffers, selection, diagnostics, symbol index, and an `apply_edit` that routes
   through the editor's own undo/multibuffer rather than the filesystem. This is where
   Cowork stops being a skin.
7. Use the prompt body's `system` and `tools` fields for per-turn control (context
   injection, tool narrowing per mode) before reaching for a JS plugin.
8. Use `noReply: true` prompts to inject editor context as user turns without burning a
   completion.
9. Map `session.diff` / `GET /session/:id/diff` and `revert`/`unrevert` onto the editor's
   own change review and undo affordances.
10. Packaging: bundle per-platform binaries, sign/notarize the child, version-lock to the
    spec snapshot, and decide explicitly whether third-party plugin loading is on. Ship
    the MIT license text.

**Phase 2 — decide with data.** Once the panel is live, the question becomes empirical:
how often do we hit the JS-plugin ceiling? If the answer is "rarely, MCP covers it",
stay. If we find ourselves maintaining a growing TypeScript plugin bundled inside a Rust
editor to get the behaviour we want, that is the signal to build the loop natively —
and by then the `AgentBackend` trait means the UI does not change.

**What to avoid:** wiring the GPUI panel directly to OpenCode's types. That is the one
mistake that converts a reversible decision into a permanent one.
