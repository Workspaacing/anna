# OpenCode: Tools, Custom Tools, Permissions, Policies
### Research report for the Cowork AI panel (Rust editor)

Sources fetched 2026-09-11:
- https://opencode.ai/docs/tools/
- https://opencode.ai/docs/custom-tools/
- https://opencode.ai/docs/permissions/
- https://opencode.ai/docs/policies/
- Supplementary (needed for per-agent tool gating): https://opencode.ai/docs/agents/ , https://opencode.ai/docs/config/

> **Fidelity note.** The four assigned pages were fetched and summarized through a text-extraction step, so quoted
> descriptions and JSON blocks below are reproduced as faithfully as the extraction allowed. Where a detail was
> *not* stated in the docs it is marked **[not documented]** rather than guessed. Anything marked **[inferred]**
> is my reading, not OpenCode's text. Before Cowork copies a schema verbatim, re-check against
> `https://opencode.ai/config.json` (the published JSON Schema) — that is the authoritative shape.

---

## 1. Tools

### 1.1 How OpenCode does it

OpenCode exposes a fixed set of built-in tools to the model. There is no "tool registry" UI — the set is
compiled in, and everything user-facing is about *restricting* it (permissions) or *adding* to it (custom
tools, MCP servers).

Two things stand out architecturally:

1. **Tools and permission keys are not 1:1.** Several tools collapse onto a single permission key. Most
   importantly `write`, `edit`, and `apply_patch` are **all governed by the single `edit` permission**. So a
   user who denies `edit` has also denied `write` and `apply_patch` — the docs state this explicitly. This is a
   deliberate simplification: users think in terms of "can it change my files", not in terms of which of three
   mutation tools was chosen.
2. **Most tools default to `allow`.** OpenCode ships permissive. The doc says all tools are enabled by default
   without requiring permission; only a handful of keys default to `ask`/`deny` (see §3.4). **This is the single
   biggest divergence Cowork should consider** — see §5.

#### Complete built-in tool table

| Tool | Documented description | Permission key that gates it | Granular match target | Notes |
|---|---|---|---|---|
| `bash` | "Execute shell commands in your project environment." | `bash` | the **parsed command**, e.g. `git status --porcelain` | The highest-risk tool. Pattern matching is per-command, not per-raw-string (see §3.3). |
| `edit` | "Modify existing files using exact string replacements." | `edit` | file path | Exact string replacement, i.e. old_string/new_string style. Parameters **[not documented]** on this page. |
| `write` | "Create new files or overwrite existing ones." | `edit` | file path | Overwrites. Shares the `edit` permission. |
| `read` | "Read file contents from your codebase." | `read` | file path | Default rules deny `*.env` / `*.env.*` (see §3.4). |
| `grep` | "Search file contents using regular expressions." | `grep` | the regex pattern | ripgrep-backed; respects `.gitignore`. |
| `glob` | "Find files by pattern matching." | `glob` | the glob pattern | ripgrep-backed; respects `.gitignore`. |
| `list` | **[not listed on the tools page]** — appears only as a permission key on the agents page. | `list` | **[not documented]** | Directory listing. Discrepancy between pages; verify against the schema. |
| `apply_patch` | "Apply patches to files." | `edit` | file path | Takes `output.args.patchText`; the patch body embeds paths using sentinel lines such as `*** Add File: src/new-file.ts`. So **the file path is inside the payload, not a separate argument** — a parser is required to enforce path-scoped permissions. |
| `lsp` *(experimental)* | Interact with LSP servers for code intelligence. | `lsp` | **non-granular** (whole tool only) | Requires env var `OPENCODE_EXPERIMENTAL_LSP_TOOL=true`. |
| `skill` | "Load a skill (a SKILL.md file) and return its content in the conversation." | `skill` | skill name | Injects file content into context — a prompt-injection surface. |
| `todowrite` | "Manage todo lists during coding sessions." | `todowrite` | **[not documented]** | **Disabled for subagents by default**; can be enabled manually. |
| `webfetch` | "Fetch web content." | `webfetch` | the URL | Network egress. |
| `websearch` | Web search. | `websearch` | the query | Only available with the OpenCode provider, or with `OPENCODE_ENABLE_EXA=1` / `OPENCODE_ENABLE_PARALLEL=1`. |
| `question` | "Ask the user questions during execution." | `question` | **[not documented]** | A first-class tool for model→user interrogation, not just a UI affordance. Notable design choice. |
| `task` | **[not described on the tools page]** — present as a permission key. | `task` | the **subagent type** | Launches a subagent. Gating by subagent *type* is the interesting bit. |

Two pseudo-tools exist that are **permission keys with no corresponding tool** — they are cross-cutting
interceptors (see §3.2):

| Pseudo-key | Trigger |
|---|---|
| `external_directory` | any tool touching a path outside the working directory |
| `doom_loop` | the same tool call repeating **3 times** |

Exact per-tool JSON parameter schemas are **[not documented]** on these pages — the docs describe tools
prose-style and defer the schema to the model-facing tool definitions.

#### File-search behaviour

> "By default, ripgrep respects `.gitignore` patterns."

To let the agent see ignored paths, the user adds a `.ignore` file:

```
!node_modules/
!dist/
!build/
```

This is a nice property: the agent's default visibility is the same as the user's version-control visibility,
so secrets in gitignored files are incidentally protected.

#### Enabling/disabling tools per agent

There are **two generations** of this, and the docs say the old one is deprecated.

**Legacy (`tools` map, deprecated in favour of `permission`):**

```json
{
  "$schema": "https://opencode.ai/config.json",
  "tools": {
    "write": true,
    "bash": true
  },
  "agent": {
    "plan": {
      "tools": {
        "write": false,
        "bash": false
      }
    }
  }
}
```

Semantics: `true` is equivalent to `{"*": "allow"}` and `false` is equivalent to `{"*": "deny"}`. Wildcards work
here too — `"mymcp_*": false` disables every tool from an MCP server named `mymcp`.

**Current (`permission` block per agent)** — see §3.5.

**Built-in agents and their tool posture:**

| Agent | Mode | Tool access |
|---|---|---|
| `build` | primary | all tools enabled |
| `plan` | primary | `edit` and `bash` default to `"deny"` |
| `general` | subagent | full access **except** `todo` |
| `explore` | subagent | read-only (file modification disabled) |
| `scout` | subagent | read-only, scoped to external dependency research |

The `plan` agent is the key pattern: a read-only mode is not a UI toggle, it is an *agent* whose permission
block denies mutation. Mode and capability are the same object.

### 1.2 What Cowork would need

- **A tool catalogue with stable names, versioned.** Cowork's tool names will end up in users' config files and
  in model prompts. Renaming `edit` later breaks every saved config. Pick names once; OpenCode's set is a
  reasonable starting vocabulary and is close enough to Claude Code's that model priors help.
- **Decide the tool↔permission collapse deliberately.** OpenCode folds write/edit/patch into `edit`. For a Rust
  editor this is right: the user's mental model is "can it modify my buffer/disk", and three separate keys
  invite a config where one is accidentally left open. Recommend Cowork mirror this — one `edit` key covering
  every mutation path, including any editor-native "apply diff to buffer" tool.
- **Editor-native tools OpenCode does not have.** Cowork lives inside the editor, so it has strictly more
  surface than a CLI agent: open buffer contents (including *unsaved* changes), multi-buffer edits, LSP
  rename/code-actions, the project panel, terminal panes, debugger, git panel, extension host. Each of these is
  a tool that needs a permission key *before* it is implemented, not after.
- **Unsaved-buffer semantics are a genuinely new hazard.** A CLI agent reads and writes the filesystem. Cowork's
  `read` may return the dirty buffer while `bash` sees the on-disk version — the agent can be shown one thing
  and act on another. Define this explicitly: either the agent always sees disk (and Cowork flushes or warns),
  or the agent always sees buffers (and shell tools are told so). Silent divergence will produce corrupt edits.
- **A `question` equivalent.** Making "ask the user" a *tool* rather than a prose convention is worth copying —
  it gives the panel a structured place to render a real UI prompt (buttons, a file picker) instead of hoping
  the user reads a paragraph and replies in chat.
- **A doom-loop breaker.** OpenCode's "same call 3 times → ask" is cheap to implement and catches the most
  common runaway failure mode. In an editor, a runaway loop burns tokens *and* thrashes the user's files.
- **Respect `.gitignore` by default for search/glob**, with an escape hatch. Free secret-hygiene.

---

## 2. Custom tools

### 2.1 How OpenCode does it

> "Custom tools are functions you create that the LLM can call during conversations. They work alongside
> opencode's built-in tools like `read`, `write`, and `bash`."

**Locations** (file-based discovery, no registration step):
- Project-local: `.opencode/tools/`
- Global: `~/.config/opencode/tools/`

**Language:** TypeScript or JavaScript. Runtime is Bun.

**The filename becomes the tool name.** `.opencode/tools/database.ts` with a default export creates a tool
called `database`. No name field, no manifest.

**Canonical definition:**

```typescript
import { tool } from "@opencode-ai/plugin"

export default tool({
  description: "Query the project database",
  args: {
    query: tool.schema.string().describe("SQL query to execute"),
  },
  async execute(args) {
    // Your database logic here
    return `Executed query: ${args.query}`
  },
})
```

**The three fields:**
- `description` — string. This *is* the model-facing documentation; there is no separate prompt or docs field.
- `args` — an object of **Zod schemas**, accessed via `tool.schema` (a re-exported Zod). Per-argument
  descriptions come from `.describe(...)`, which is what the model sees for each parameter.
- `execute` — async, receives `(args, context)`, returns a **string** result.

Zod can also be imported directly:

```typescript
import { z } from "zod"

args: {
  param: z.string().describe("Parameter description"),
}
```

So argument validation is **schema-first and automatic** — OpenCode derives the JSON Schema handed to the model
from the Zod definition, and validates the model's arguments against it before `execute` runs. The author never
writes JSON Schema by hand and never validates manually.

**Context object** — second parameter to `execute`:

```typescript
async execute(args, context) {
  const { agent, sessionID, messageID, directory, worktree } = context
}
```

| Field | Meaning |
|---|---|
| `agent` | which agent invoked the tool |
| `sessionID` | current session |
| `messageID` | current message |
| `directory` | session working directory |
| `worktree` | git worktree root |

`agent` being in context is significant — a custom tool can enforce its *own* policy based on caller identity.

**Multiple tools per file** via named exports, named `<filename>_<exportname>`:

```typescript
export const add = tool({
  description: "Add two numbers",
  args: {
    a: tool.schema.number().describe("First number"),
    b: tool.schema.number().describe("Second number"),
  },
  async execute(args) {
    return (args.a + args.b).toString()
  },
})
```

In `math.ts` this produces a tool named `math_add`.

**Any language, via subprocess.** The docs note the TS/JS is "only used for the tool definition itself":

```typescript
// .opencode/tools/python-add.ts
import { tool } from "@opencode-ai/plugin"
import path from "path"

export default tool({
  description: "Add two numbers using Python",
  args: {
    a: tool.schema.number().describe("First number"),
    b: tool.schema.number().describe("Second number"),
  },
  async execute(args, context) {
    const script = path.join(context.worktree, ".opencode/tools/add.py")
    const result = await Bun.$`python3 ${script} ${args.a} ${args.b}`.text()
    return result.trim()
  },
})
```

**Naming collisions: custom tools silently override built-ins with the same name.** The docs' guidance is that
if you want to *disable* a built-in, use the permission system rather than shadowing it.

Error handling semantics, timeouts, and streaming/partial output are **[not documented]** on this page.

### 2.2 What Cowork would need

- **This is the highest-risk feature in the whole surface, and OpenCode's design makes it worse.** A custom tool
  is arbitrary code, dropped into `.opencode/tools/` **inside the repo**, executed by the agent runtime with no
  sandbox and no install prompt. Cloning a hostile repository and opening it is enough. Cowork must not ship
  project-local executable tool definitions without, at minimum, a trust prompt per workspace — the same gate a
  Rust editor already needs for tasks, LSP binaries, and format-on-save commands. Reuse that existing trust
  mechanism rather than inventing a second one.
- **Override-a-builtin is a takeover primitive.** A project-local `bash.ts` that shadows the built-in `bash`
  intercepts every shell call. If Cowork supports custom tools at all, **built-in names must be reserved** —
  refuse to load a custom tool that shadows one, rather than letting it win.
- **Schema-first argument declaration is the right call.** Cowork is Rust: the natural analogue is a `serde`
  struct with `schemars` deriving the JSON Schema, so the model-facing schema and the deserialization target are
  the same type and cannot drift. Validate before dispatch, return a structured error the model can act on.
- **Pass a context struct.** `agent`, `session_id`, `worktree`, plus editor-specific additions: active buffer,
  selection, workspace roots, and the current permission decision. Tools that know the caller can self-restrict.
- **Return type: do not limit to `String`.** OpenCode returns a string, which forces every tool to serialize for
  the model and loses structure the UI could render. Cowork should return a structured result — model-facing
  text *plus* an optional UI payload (a diff to render, a file list to make clickable, a chart). This is a real
  advantage of being in a GUI editor and the place Cowork can beat a TUI.
- **Prefer out-of-process for third-party tools.** MCP over stdio, or WASM, gives an isolation boundary that
  OpenCode's in-process Bun model does not. In-process only for first-party tools.
- **Cap it:** per-tool timeout, output size limit, and cancellation — none are documented in OpenCode, and an
  editor cannot afford a tool that hangs the panel.

---

## 3. Permissions

### 3.1 How OpenCode does it — the core model

Three states:

| Value | Behaviour |
|---|---|
| `"allow"` | runs without approval |
| `"ask"` | prompts the user |
| `"deny"` | blocks the action |

The whole `permission` value can be a single string, applying to everything:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "permission": "allow"
}
```

Or an object keyed by tool, with `*` as a catch-all default:

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

### 3.2 The permission keys

Keyed by tool name; each key documents *what the pattern matches against*, which is the crucial detail:

| Key | Gates | Pattern matches against |
|---|---|---|
| `read` | file reading | file path |
| `edit` | file modification — covers `edit`, `write`, `patch` | file path |
| `glob` | file globbing | glob pattern |
| `grep` | content search | regex pattern |
| `list` | directory listing | **[not documented]** |
| `bash` | shell commands | the **parsed command**, e.g. `git status --porcelain` |
| `task` | launching subagents | subagent type |
| `skill` | skill loading | skill name |
| `lsp` | LSP queries | **non-granular** |
| `question` | asking the user questions | **[not documented]** |
| `todowrite` | todo list writes | **[not documented]** |
| `webfetch` | URL fetching | the URL |
| `websearch` | web search | the query |
| `external_directory` | *any* tool touching a path outside the working directory | the path |
| `doom_loop` | identical tool call repeated 3 times | n/a |

`external_directory` and `doom_loop` are the interesting ones: they are **cross-cutting interceptors**, not
tools. They fire regardless of which tool triggered them. That is a better primitive than per-tool path checks
because it cannot be bypassed by adding a new tool that forgot to call the checker.

### 3.3 Granular rules — object syntax, last-match-wins

Any key can take an object of pattern→value instead of a bare value:

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

**Resolution: the LAST matching pattern wins** — not the most specific. Order in the object is semantically
meaningful. (This is a footgun: JSON object key order is preserved by most parsers but is not guaranteed by the
JSON spec, and many config tools will happily reorder keys. Cowork should prefer an ordered array.)

**Wildcards:**
- `*` matches zero or more characters
- `?` matches exactly one character
- all other characters match literally

**Home expansion:** patterns may start with `~` or `$HOME`, expanded to the user's home directory — so
`~/projects/*` becomes `/Users/username/projects/*`.

**Documented pattern-matching footgun**, worth quoting to the team verbatim in spirit: commands with arguments
require an explicit wildcard. `"grep *"` permits `grep pattern file.txt`, but `"grep"` alone **blocks** it. Even
`git status` needs `"git status *"` if arguments will be passed. Users will get this wrong constantly and will
believe they have allowed something they have not — or, in the dangerous direction, believe they have denied
something they have not.

Note also that `bash` matches against the *parsed* command. So a rule set is evaluated against the decomposed
command, which is what makes `"rm *": "deny"` meaningful at all. How compound commands
(`git status && rm -rf /`), pipelines, subshells, `$(...)`, and shell aliases are decomposed is
**[not documented]** — and it is exactly where a bypass would live.

### 3.4 Defaults

Most permissions default to `"allow"`. The documented exceptions:

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

`doom_loop` and `external_directory` default to `"ask"`.

The `.env` rule is a good, concrete pattern: deny the secret files, re-allow the harmless example. Note it
demonstrates last-match-wins in the default config itself.

### 3.5 Scoping: external directories, and agent overrides

**External directory allow-listing:**

```json
{
  "$schema": "https://opencode.ai/config.json",
  "permission": {
    "external_directory": {
      "~/projects/personal/**": "allow"
    }
  }
}
```

Composable with a tool restriction — read outside the workspace, but never write there:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "permission": {
    "external_directory": {
      "~/projects/personal/**": "allow"
    },
    "edit": {
      "~/projects/personal/**": "deny"
    }
  }
}
```

**Agent-level overrides.** Agent permissions *merge* with global config; agent rules take precedence:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "permission": {
    "bash": {
      "*": "ask",
      "git *": "allow",
      "git commit *": "deny",
      "git push *": "deny",
      "grep *": "allow"
    }
  },
  "agent": {
    "build": {
      "permission": {
        "bash": {
          "*": "ask",
          "git *": "allow",
          "git commit *": "ask",
          "git push *": "deny",
          "grep *": "allow"
        }
      }
    }
  }
}
```

Also settable in agent Markdown frontmatter:

```markdown
---
description: Code review without edits
mode: subagent
permission:
  edit: deny
  bash: ask
  webfetch: deny
---
Only analyze code and suggest changes.
```

And the agents-page form:

```json
{
  "agent": {
    "build": {
      "permission": {
        "bash": {
          "git push": "ask",
          "grep *": "allow"
        }
      }
    }
  }
}
```

**MCP tools** are gated by the same mechanism via prefix wildcards: `"mymcp_*": "ask"` covers every tool from
that server. MCP tool names are namespaced by server, which is what makes this work.

### 3.6 Approval UX

When a permission resolves to `"ask"`, the UI offers three choices:

| Choice | Effect |
|---|---|
| `once` | approve this request only |
| `always` | approve matching future requests **for the current session** |
| `reject` | deny |

Note `always` is **session-scoped**, not persisted to config. Nothing the user clicks in a hurry silently
becomes permanent policy — the durable grants live only in files the user edited deliberately. That separation
is worth copying exactly.

**Auto mode:**

```
opencode --auto
opencode run --auto "Refactor this module"
```

`--auto` auto-approves every non-denied request. **Explicit `"deny"` rules are still enforced** — `--auto` is
"ask → allow", not "ignore config". The TUI shows a muted `auto` indicator while active, so the state is always
visible.

### 3.7 Config resolution

Permissions are merged, not replaced, across sources (later overrides earlier):

1. Remote config (`.well-known/opencode`)
2. Global config (`~/.config/opencode/opencode.json`)
3. Custom path (`OPENCODE_CONFIG` env var)
4. Project config (`opencode.json` in project root)
5. `.opencode` directories
6. Inline config (`OPENCODE_CONFIG_CONTENT` env var)
7. Managed system files
8. macOS managed preferences (**highest priority**)

> "Configuration files are merged together, not replaced."

Variable substitution is supported: `{env:VARIABLE_NAME}` and `{file:path/to/file}` (relative paths and `~`).

**Security observation:** in this order, **project config outranks global config** for permissions. A repository
can therefore loosen a permission the user set globally — the opposite of the policies rule in §4, where global
deliberately wins. Only the managed/system tiers (7, 8) sit above the project. Note also that tier 1 is a
*remote* config fetched from a well-known URL, and `{file:...}`/`{env:...}` substitution runs inside configs —
a project config can pull values from arbitrary files.

### 3.8 What Cowork would need

- **Adopt allow/ask/deny and the three-button once/always/reject prompt.** It is the right vocabulary and users
  coming from Claude Code or OpenCode already know it.
- **Keep `always` session-scoped.** Persisted grants should require deliberate config editing or an explicit
  "remember this" affordance that says where it will be written.
- **Invert the defaults.** OpenCode defaults to `allow` for almost everything because it is a CLI the user ran
  on purpose in a directory they chose. **Cowork is a panel inside an editor that may be pointed at any
  workspace the user opened, including one they just cloned.** Recommended Cowork defaults: `read`/`grep`/`glob`
  allow within the workspace; `edit` ask (or allow-with-undo, see below); `bash` **ask, always, first time per
  command shape**; network tools ask; everything outside the workspace root ask.
- **Use an ordered array, not an object, for pattern rules.** Last-match-wins over JSON object keys is fragile.
  `[{ pattern, effect }]` in Rust is unambiguous, serde-friendly, and diffable.
- **`external_directory` as a cross-cutting interceptor is the best idea on these pages — implement it first.**
  In Rust this is one canonicalization choke point every tool must pass through. Critically it must:
  - canonicalize **after** resolving symlinks (a symlink inside the workspace pointing at `~/.ssh` must be
    caught — path-prefix checks on the un-resolved path will not catch it);
  - reject `..` traversal and UNC/device paths;
  - be **case-insensitive on Windows**, and handle 8.3 short names, alternate data streams, and drive-relative
    paths. OpenCode's patterns are POSIX-flavoured (`~/projects/*`, `/Users/username/...`); Cowork ships on
    Windows and the pattern layer needs first-class `C:\` handling or every path rule is silently wrong.
- **Treat `bash` as a category of its own.** Pattern-matching shell strings is a weak defence; it is defeated by
  `sh -c`, `eval`, `$(...)`, pipelines, `&&` chains, aliases, env-var indirection, and any interpreter
  (`python -c`, `node -e`). Cowork should: parse the command, apply rules to **every** segment (not just the
  first), and treat anything unparseable as `ask` — **fail closed**. Never let a compound command inherit the
  permission of its first segment. Also decide explicitly whether allowing `bash` is considered equivalent to
  allowing everything, and say so in the UI, because in practice it is.
- **Show the user what they are approving, precisely.** The prompt must render the exact resolved command, the
  exact absolute path, or the exact URL — not a paraphrase, and not the pattern that matched. For `edit`, show
  the diff. An editor can do this far better than a TUI; it is the main UX advantage Cowork has here.
- **`apply_patch`-style tools need payload parsing before approval.** OpenCode's patch text embeds target paths
  (`*** Add File: src/new-file.ts`) inside the argument blob. Any Cowork equivalent must extract and check
  *every* path in the payload against `edit` and `external_directory` rules, and display them in the prompt —
  otherwise a single approval covers files the user never saw.
- **Add an `edit` safety net that a CLI cannot offer.** Cowork sits on the editor's undo/history and (usually) a
  git repo. Snapshot before an agent edit batch and offer one-click revert. This lets Cowork default `edit` to
  something friendlier than `ask` without the usual risk, which is a genuine product advantage.
- **Add keys OpenCode lacks but an editor needs:** terminal-pane creation, debugger attach/launch, task running,
  extension install, settings/keymap modification, git write operations (commit/push/reset/checkout), and
  credential/keychain access. Note OpenCode's own example config denies `git push *` and asks on `git commit *`
  — evidence that even the CLI treats VCS writes as a distinct tier. Cowork should make them real keys rather
  than leaving them to `bash` pattern luck.
- **Gate MCP/extension tools by namespaced prefix**, as OpenCode does — third-party tools should never land in
  the same flat namespace as built-ins.
- **A doom-loop counter** with a threshold and a user-visible "the agent is repeating itself" prompt.
- **Prompt fatigue is the real failure mode.** Every `ask` that fires too often trains the user to click
  through. Budget the prompt count: coarse, meaningful gates that fire rarely beat fine-grained ones that fire
  constantly. Consider batching ("the agent wants to edit these 7 files") over per-file prompts.

---

## 4. Policies

### 4.1 How OpenCode does it

> "Policies control whether OpenCode may use a resource such as an LLM provider."

And the distinction, stated directly:

> "Permissions control what tools can do during a session, while policies control whether OpenCode may use a
> resource such as an LLM provider."

So: **permissions are about actions; policies are about resources.** Today the only resource is the LLM
provider — this is about data egress and vendor choice, not about file safety.

**Location and status:** `opencode.json`, under `experimental.policies` (an array). Marked experimental.

**Statement shape** — three required fields:

| Field | Values |
|---|---|
| `effect` | `"allow"` or `"deny"` |
| `action` | the operation being controlled |
| `resource` | resource ID or wildcard pattern |

**Available actions** — currently exactly one:

| Action | Resource | Description |
|---|---|---|
| `provider.use` | Provider ID (e.g. `openai`) | Allow or deny use of an LLM provider |

**Example:**

```json
{
  "$schema": "https://opencode.ai/config.json",
  "experimental": {
    "policies": [
      {
        "effect": "deny",
        "action": "provider.use",
        "resource": "openai"
      }
    ]
  }
}
```

**Wildcards** work on `resource` — `*` zero-or-more, `?` exactly one:

```json
{
  "experimental": {
    "policies": [
      {
        "effect": "deny",
        "action": "provider.use",
        "resource": "company-*"
      }
    ]
  }
}
```

**Deny-all-then-allow** (the enterprise pattern):

```json
{
  "experimental": {
    "policies": [
      {
        "effect": "deny",
        "action": "provider.use",
        "resource": "*"
      },
      {
        "effect": "allow",
        "action": "provider.use",
        "resource": "anthropic"
      }
    ]
  }
}
```

**Precedence:**
1. **Last matching rule wins** when several statements match.
2. **Default when nothing matches: allowed.**
3. **Global policy beats project policy** — quoting the docs: *"Your global policy takes priority over the
   project policy. This prevents a repository from re-enabling a provider that you deny globally."*

Point 3 is the whole point of policies existing as a separate mechanism. It is the **inverse** of the normal
config precedence in §3.7, where the project layer overrides the global layer. Policies are the one layer a
repository cannot loosen — which is what makes them an enforcement primitive rather than a preference.

Policies supersede the older `disabled_providers` / `enabled_providers` options.

The format is recognisably AWS IAM shaped (effect/action/resource), which suggests the intended trajectory is
more actions over time, not just providers.

### 4.2 What Cowork would need

- **Adopt the separation: permissions = actions, policies = resources.** They have genuinely different
  lifecycles. Permissions are tuned per project by the developer; policies are set once by the person or org
  who decides what is allowed to exist, and must not be locally overridable.
- **The direction of precedence is the entire feature.** Any Cowork policy layer must be *immune* to workspace
  config. If a cloned repo's `cowork.json` can re-enable a denied provider or re-enable custom tools, the layer
  is decorative. Concretely: load policies only from the user-global config and any OS-managed location, and
  **never** from the workspace — do not merely rank the workspace lower.
- **Extend `action` beyond providers.** For an editor the obvious set: `provider.use`, `tool.use`,
  `mcp.connect`, `extension.install`, `telemetry.send`, `network.egress` (by host), `credential.read`. Keeping
  OpenCode's `effect`/`action`/`resource` triple means all of these fit one evaluator and one config shape.
- **Provide OS-managed enforcement paths** if enterprise is a target — OpenCode already reads macOS managed
  preferences and "managed system files" at the top of its config precedence. The Windows equivalent is Group
  Policy / registry under `HKLM`, which Cowork will need given its platform mix.
- **Default-allow-when-no-match is the wrong default for a shipped editor.** OpenCode's choice suits a
  developer tool with no admin. If Cowork ever targets managed fleets, the deny-all-then-allow example should be
  the documented starting template, and admins should be able to set the no-match default itself.
- **Ship the evaluator before the surface.** One `evaluate(action, resource) -> Effect` function, ordered rules,
  last-match-wins, with the ordering coming from an array. Cheap now, very expensive to retrofit.

---

## 5. Safety-critical summary

Cowork executes against the user's real files and real shell. Flagging the parts that matter most:

1. **OpenCode defaults to `allow` for nearly every tool, including `bash` and `edit`.** Do not copy this.
   A CLI is invoked deliberately in a chosen directory; an editor panel is ambient and points at whatever
   workspace is open, including a repo cloned minutes ago.
2. **Project config outranks global config for permissions (§3.7).** A repository's `opencode.json` can loosen
   what the user set globally. Cowork must invert this for anything security-relevant: workspace config may
   *tighten*, never *loosen*. Policies (§4) get this right; permissions do not.
3. **Project-local custom tools are unsandboxed arbitrary code (§2).** `.opencode/tools/*.ts` executes on a repo
   that was merely opened. This is remote code execution via `git clone`. Gate behind workspace trust, and
   forbid custom tools from shadowing built-in names — otherwise a malicious `bash.ts` intercepts every command.
4. **`bash` pattern matching is a weak boundary.** `sh -c`, `eval`, `$(...)`, pipes, `&&`, and any interpreter
   defeat prefix rules. Parse every segment, apply rules to all of them, and fail closed on anything
   unparseable. Decide and communicate that granting `bash` is effectively granting everything.
5. **`"grep"` and `"grep *"` mean different things** and users will not internalise this. A rule that looks like
   a deny but silently matches nothing is worse than no rule. Cowork's config UI should show the user, live,
   which recent commands a rule would have matched.
6. **Path checks must be a single canonicalizing choke point, after symlink resolution, and Windows-correct.**
   OpenCode's `external_directory` interceptor is the right shape; its POSIX-flavoured pattern syntax is not
   sufficient for Cowork's platform mix. Symlink-through-workspace to `~/.ssh` is the concrete attack.
7. **Approval prompts must show exact resolved values, and payload-embedded paths must be extracted first.**
   Approving a patch blob whose target paths were never displayed is a blind grant.
8. **`skill`, `webfetch`, and `websearch` pull untrusted text into context.** Anything fetched is data, not
   instructions. Cowork needs an explicit boundary here, and ideally should not let content fetched by one tool
   silently widen what another tool may do.
9. **Keep `always` session-scoped (§3.6).** A rushed click should never become permanent policy.
10. **Prompt fatigue defeats the whole system.** Coarse, rare, meaningful prompts with excellent diffs beat
    fine-grained prompts that users learn to dismiss.

---

## 6. Open questions to resolve against source

The docs left these unstated; all of them matter for a faithful or safer implementation:

- Per-tool JSON parameter schemas for the built-ins (not on the tools page).
- Whether `list` is a real tool — it appears as a permission key but not in the tools list. Pages disagree.
- How `bash` commands are decomposed for pattern matching: compound commands, pipelines, subshells, aliases.
- Custom tool error handling, timeouts, output limits, cancellation.
- Whether `read` denial on `*.env` is enforced for `bash` too (`cat .env` is the obvious bypass, and `bash`
  matching is command-shaped, not path-shaped — so it likely is **not** covered).
- Whether `external_directory` applies to `bash` working directories and to paths inside command arguments.
- What `doom_loop` considers an "identical" call (exact arg equality? normalized?).
- Whether the remote `.well-known/opencode` config tier can introduce permission changes.
