# Cowork: from chat to agent

Cowork today is a chat panel. This is the plan for making it an agent, natively in Rust, with no
external runtime and no dependency on another agent product.

It is written against research into OpenCode's design, kept in
`.claude/skills/wu/references/opencode/`. Where a decision differs from theirs, the reason is stated.

## The one structural fact

**Tool calls are the trunk.** Tool cards in the transcript, diff review, permission prompts,
interrupting a turn, undoing a turn, and sub-agents are all branches of having a tool-call event
model. Until the wire protocol and the thread model carry tool calls, none of them can be built.

`provider.rs` currently says tool calls are "deliberately absent: nothing in the UI can execute one
yet." That comment describes the starting point of this plan, and is the first thing to delete.

## Event vocabulary

Thread events are modelled as a superset of the Agent Client Protocol's `SessionUpdate`:
`agent_message_chunk`, `tool_call` / `tool_call_update` (with a `kind` of read/edit/execute and a
status of pending → in_progress → completed/failed), `plan`, `usage_update`, and tool content as
text, a diff (`path` / `old_text` / `new_text`), or a terminal handle.

This is a vocabulary choice, not a dependency. ACP is an open protocol that Zed, JetBrains and
several editors already speak. Naming our events after it costs nothing now and leaves the option of
*hosting* third-party agents later as a second implementation of one trait, rather than a rewrite.
The reverse order — a bespoke event model first, ACP bolted on after — is the one sequencing that
throws work away.

## Phases

### 1. The loop

- Tool calls in both wire formats: Anthropic `tool_use` / `tool_result`, OpenAI `tool_calls` /
  `role: "tool"`. These differ in shape and in how partial JSON arrives during streaming; the SSE
  decoder has to accumulate argument fragments before a call is complete.
- A `Tool` trait: name, description, JSON-Schema parameters, and an async `run` returning content.
- The turn loop: prompt → stream → on tool calls, execute → append results → continue until the
  model stops. With a step budget, because a loop that cannot end is the default failure mode.
- Thread persistence extended to hold tool calls and their results.

### 2. Tools, routed through the editor

Not reimplementations of shell utilities. Each one goes through machinery Wu already has, which is
the entire reason for building this natively:

| Tool | Goes through |
| --- | --- |
| `read`, `list`, `glob`, `grep` | `Project`, `Worktree`, `fs::Fs`, `fuzzy` |
| `edit`, `write` | `Buffer` — so edits land in the editor's own undo, dirty state and diff gutter |
| `bash` | `crates/terminal` |
| `diagnostics` | the language servers Wu already runs warm |

Editing through `Buffer` rather than the filesystem is the difference between an agent that writes
files behind the editor's back and one whose changes are reviewable before they touch disk.

**Nothing is built for LSP or formatters.** `Project::format()`, `DiagnosticSet` and the language
registry exist. After an edit the sequence is format → save → diagnostics → report back to the model.
OpenCode's docs advise against LSP because starting servers is expensive for a TUI; Wu's are already
running.

### 3. The permission broker

- Three values, `allow` / `ask` / `deny`, matched against the tool's *input*: a path for `read` and
  `edit`, the parsed command for `bash`.
- Approval is `once` / `always` / `reject`, and **`always` is session-scoped and never written to
  settings**. A user should not be able to permanently widen their own permissions by clicking a
  button during a turn.
- **Workspace settings may tighten permissions, never loosen them.** OpenCode has this inverted: a
  cloned repository can widen what the user set globally. Wu also has worktree trust, which gates
  whether repository-supplied agent configuration is honoured at all.
- Two cross-cutting interceptors, adopted from OpenCode because they are the best idea in its
  permission model: `external_directory` (any path outside the worktree) and `doom_loop` (the same
  call with the same input three times). They sit outside the tools, so a newly added tool cannot
  forget to ask.
- Path matching is Windows-first: canonicalize after resolving symlinks, compare case-insensitively,
  handle `C:\` and UNC. OpenCode's patterns are POSIX-shaped and are silently wrong here.

Command-string matching is a weak boundary — `sh -c`, `eval`, `$(…)`, `&&` all defeat it. It is a
speed bump for honest mistakes, not a sandbox, and should be described that way in the UI.

### 4. The panel

- Tool calls as first-class transcript items, collapsible, with per-kind rendering.
- Diffs routed into a **multibuffer with per-hunk accept/reject**. This is where a native editor
  decisively beats a terminal, and the multibuffer already exists.
- Interrupt on `escape`.
- The permission prompt suggests the safe pattern rather than making the user write a glob.
- `@` extended past files to symbols, diagnostics and the current selection.

### 5. Undo

Turn-granular undo that reverts messages and file changes together, backed by git snapshots.
This is what makes it acceptable to let an agent edit at all.

### 6. Extensibility

In this order, because each is cheap once the loop exists:

- **Rules** — `AGENTS.md`, already present in this repo and already pointing at `.rules`.
- **Skills** — Anthropic's Agent Skills format, read from `.claude/skills/` as well as our own
  directory, for ecosystem compatibility. Progressive disclosure: the tool description carries only
  names and one-line descriptions; the body is fetched when the model asks for it.
- **Agents** — markdown with YAML frontmatter, exposed to the model through a `task` tool whose
  description is generated from the permission-filtered agent list. A denied agent is absent from
  the description entirely, so the model never attempts it.
- **Commands** — user-triggered only.
- **MCP client.**

The four surfaces are separated by who triggers them: rules are always on, agents are triggered by
the user or the model, skills only by the model, commands only by the user. Keeping that clean is
what stops them collapsing into each other.

## Not adopted

- Remote configuration fetched from a URL that can name npm packages to load. That is remote code
  execution wearing a config schema.
- Project-local tool definitions that override built-ins. A hostile `bash` implementation arriving by
  `git clone` should not be possible.
- `` !`shell` `` inside repository-supplied command files, which executes on a keystroke.
- Provider SDKs downloaded at runtime. Wire formats are compiled in.
- Plaintext credential files. Keys go to the OS credential store.
