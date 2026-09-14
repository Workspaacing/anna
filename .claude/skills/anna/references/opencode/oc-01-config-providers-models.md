# OpenCode: Config, Providers, Models, Zen, Network
### Research report for the Cowork AI panel (Rust editor)

Sources (fetched 2026-09-11):
- https://opencode.ai/docs/config/
- https://opencode.ai/docs/providers/
- https://opencode.ai/docs/models/
- https://opencode.ai/docs/zen/
- https://opencode.ai/docs/network/
- Supporting: https://opencode.ai/docs/cli/ , https://opencode.ai/docs/agents/

Everything below marked "How OpenCode does it" is what the docs actually state. Where the docs
are silent or self-contradictory I say so explicitly rather than guessing.

---

## 1. Config file

### How OpenCode does it

**Filenames**

| File | Purpose |
|---|---|
| `opencode.json` | main config |
| `opencode.jsonc` | same, JSONC (comments allowed) |
| `tui.json` / `tui.jsonc` | TUI-only settings (theme, keybinds, cursor, mouse, scroll) |

Schema URLs, declared via `$schema`:
- `https://opencode.ai/config.json`
- `https://opencode.ai/tui.json`

**Search / merge order** — documented lowest-to-highest precedence. Note that this is a
*merge chain*, not a "first one wins" lookup:

1. Remote config served at `.well-known/opencode`
2. Global config: `~/.config/opencode/opencode.json`
3. Custom config: path in `OPENCODE_CONFIG`
4. Project config: `opencode.json` at project root — **walks up the directory tree to the
   nearest Git directory**
5. `.opencode/` directories
6. Inline config: JSON literal in `OPENCODE_CONFIG_CONTENT`
7. Managed config files (system-level, admin-only):
   - macOS `/Library/Application Support/opencode/`
   - Linux `/etc/opencode/`
   - Windows `%ProgramData%\opencode`
8. macOS managed preferences (MDM) — highest priority, unoverridable

Extra path env vars: `OPENCODE_CONFIG_DIR` (a directory searched like `.opencode`),
`OPENCODE_TUI_CONFIG`.

**Merge semantics** — quoted from the docs: *"Configuration files are merged together, not
replaced."* and *"Later configs override earlier ones only for conflicting keys.
Non-conflicting settings from all configs are preserved."*

So: recursive/deep object merge, last-writer-wins at the leaf. The docs do **not** state what
happens to arrays (replace vs concat) — that is an undocumented hole and a real interop
hazard for keys like `instructions`, `plugin`, `enabled_providers`, `watcher.ignore`.

**Top-level keys** (full list I could extract):

*Execution*
- `model` (string) — e.g. `"anthropic/claude-sonnet-4-5"`
- `small_model` (string) — "lightweight tasks", defaults to a cheaper variant
- `provider` (object) — per-provider config; also carries transport knobs `timeout`,
  `headerTimeout`, `chunkTimeout`, `setCacheKey`
- `shell` (string) — shell for the interactive terminal; auto-discovered if unset
- `default_agent` (string) — e.g. `"build"`, `"plan"`

*Tools / permissions*
- `tools` (object) — hard-disable tools from the model: `{"write": false, "bash": false}`
- `permission` (object) — `{"edit": "ask", "bash": "ask"}`; values `allow` / `ask` / `deny`

*Server*
- `server` (object) — `port` (number), `hostname` (string), `mdns` (bool),
  `mdnsDomain` (string), `cors` (string[])

*Customization*
- `agent` (object) — agent definitions (see §3)
- `command` (object) — custom slash commands: `template`, `description`, `agent`, `model`
- `instructions` (string[]) — paths / glob patterns to instruction files
- `formatter` (bool | object)
- `lsp` (bool | object)
- `mcp` (object) — MCP servers
- `plugin` (string[]) — npm packages; also auto-loaded from `.opencode/plugins/`

*Behaviour / limits*
- `autoupdate` (bool | `"notify"`)
- `snapshot` (bool, default `true`) — file-change tracking for undo
- `subagent_depth` (number, default `1`)
- `share` (`"manual"` | `"auto"` | `"disabled"`)
- `compaction` (object) — `auto` (bool), `prune` (bool), `reserved` (number)
- `watcher` (object) — `{"ignore": ["node_modules/**", "dist/**", ".git/**"]}`
- `attachment.image` — `auto_resize`, `max_width`, `max_height`, `max_base64_bytes`

*Access control*
- `enabled_providers` (string[]) — allowlist of provider ids
- `disabled_providers` (string[]) — denylist; **takes priority over the allowlist**
- `experimental` (object) — policies, unstable features

*TUI-only (tui.json)*: `theme`, `themes`, `keybinds` (e.g. `"command_list": "ctrl+p"`),
`scroll_speed`, `scroll_acceleration`, `diff_style`, `cursor` (`style`, `blinking`),
`mouse`, `attention` (`enabled`, `notifications`, `sound`, `volume`).

**Variable substitution** — two forms, usable in any string value:

```json
{
  "model": "{env:OPENCODE_MODEL}",
  "provider": {
    "openai": { "options": { "apiKey": "{file:~/.secrets/openai-key}" } }
  }
}
```

- `{env:VARIABLE_NAME}` — environment variable
- `{file:path/to/file}` — file contents; relative to the config's own directory, or absolute
  when starting with `/` or `~`

**`.opencode/` directory layout** — convention-over-config, mirrored at
`~/.config/opencode/`:

```
.opencode/agents/     .opencode/commands/   .opencode/plugins/
.opencode/skills/     .opencode/tools/      .opencode/themes/
.opencode/modes/
```

Plural names are canonical; singular names still accepted for backwards compatibility.

### What Cowork would need

1. **Two-file split is worth copying, but draw the line differently.** OpenCode splits
   `opencode.json` (agent behaviour) from `tui.json` (presentation). Cowork lives inside a
   Rust editor that already owns theme, keybinds, cursor and mouse. So Cowork should ship
   **one** file — `cowork.json` (or a `cowork` section in the editor's existing settings) —
   and delegate all presentation to the host editor's settings system. Do not reinvent a
   keybind layer.

2. **Search order** — recommend a 4-level chain, not 8:
   - `$COWORK_CONFIG` (explicit override, for CI/tests)
   - global: `~/.config/cowork/cowork.json` (plus the Windows `%APPDATA%` equivalent)
   - project: nearest `cowork.json` walking up from cwd, stopping at the repo root
   - `.cowork/` directory contents (agents, prompts, commands)

   Drop the remote `.well-known/opencode` level entirely — see design-mistake note below.
   Keep an MDM/managed tier only if enterprise is on the roadmap; it costs little to reserve
   the precedence slot now.

3. **Merge must be specified in writing, including arrays.** Decide and document:
   deep-merge objects, and for arrays choose *replace* (predictable) rather than *concat*
   (surprising, impossible to un-inherit). If concat is wanted for `instructions`, make it
   opt-in via an explicit `"instructions+": [...]` style key. Serde in Rust makes this easy
   if the config type is layered as `Option<T>` fields and folded.

4. **Substitution**: implement `{env:VAR}` and `{file:path}`. These are cheap and they are
   the single feature that lets a config be committed to a repo without leaking secrets.
   Resolve `{file:}` relative to the *defining config's* directory, exactly as OpenCode does,
   otherwise project configs break when inherited.

5. **Publish a JSON Schema** and point `$schema` at it. In a Rust editor this is nearly free
   with `schemars` derived off the config structs, and it buys autocomplete inside Cowork's
   own editor for Cowork's own config — a strong demo.

### Design smells to avoid

- **Remote config from `.well-known/opencode` is a security liability.** A config level
  fetched over the network that can set `provider.*.options.baseURL`, `permission`, `mcp`
  servers and `plugin` npm packages is a remote-code-execution and exfiltration vector
  wearing a config hat. Even at lowest precedence it can inject keys nobody else sets. Cowork
  should not have this. If org-wide defaults are needed, ship them as a signed/pinned file
  the user installs, not a URL the tool fetches.
- **Eight precedence layers is too many.** `OPENCODE_CONFIG` *below* project config is
  counter-intuitive — an explicit `--config`-style override should win over ambient files,
  not lose to them. Cowork should make the explicit override the top non-managed tier.
- **`OPENCODE_CONFIG_CONTENT` (whole JSON in an env var)** is an ergonomic hack for CI.
  Prefer `--config <path>` plus a temp file; env vars leak into child processes and `ps`.
- **`enabled_providers` vs `disabled_providers` as two parallel lists** with a precedence
  rule between them is confusing. One list plus a mode field, or just a denylist, is clearer.
- **Plural/singular directory aliasing** (`agents/` and `agent/` both work) is technical debt
  the docs already apologise for. Pick one name on day 1.

---

## 2. Providers

### How OpenCode does it

**Config shape** — everything hangs off the top-level `provider` object, keyed by provider id:

```json
{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "provider-id": {
      "npm": "@ai-sdk/package-name",
      "name": "Display Name",
      "options": {
        "baseURL": "https://api.example.com/v1",
        "apiKey": "{env:ENV_VAR_NAME}",
        "headers": { "Custom-Header": "value" }
      },
      "models": {
        "model-id": {
          "name": "Model Display Name",
          "limit": { "context": 128000, "output": 65536 }
        }
      },
      "blacklist": ["model-to-hide"],
      "whitelist": ["model-to-keep"]
    }
  }
}
```

Subkeys:
- `npm` — the Vercel AI SDK package that implements the wire protocol
  (`@ai-sdk/openai`, `@ai-sdk/anthropic`, `@ai-sdk/openai-compatible`, ...)
- `name` — display string in the picker
- `options.baseURL` — endpoint override, available on **any** provider, including first-party
  ones (so `anthropic` can be pointed at a corporate proxy)
- `options.apiKey` — supports `{env:}` / `{file:}`
- `options.headers` — arbitrary extra HTTP headers
- `models` — map of model id to per-model config
- `blacklist` / `whitelist` — model filtering; whitelist narrows, blacklist then subtracts
- Bedrock aliases `baseURL` as `endpoint`

**Credentials**

- Stored in **`~/.local/share/opencode/auth.json`** — a plain JSON file. No OS keychain.
- `opencode auth login` — *"OpenCode is powered by the provider list at Models.dev, so you
  can use `opencode auth login` to configure API keys for any provider you'd like to use."*
  Interactive provider picker then method picker (paste API key / OAuth). Flags:
  `--provider` / `-p` (provider id or name), `--method` / `-m` (login method label, skips the
  method prompt).
- `opencode auth list` (alias `auth ls`) — *"Lists all the authenticated providers as stored
  in the credentials file."*
- `opencode auth logout` — *"Logs you out of a provider by clearing it from the credentials
  file."*
- `/connect` — the in-TUI equivalent of `auth login`.

**Env vars recognised without any auth step** (provider auto-detects if the var is set):

| Provider | Env vars |
|---|---|
| Anthropic | `ANTHROPIC_API_KEY` |
| OpenAI | `OPENAI_API_KEY` |
| Azure OpenAI | `AZURE_API_KEY`, `AZURE_RESOURCE_NAME` |
| Azure Cognitive Services | `AZURE_API_KEY`, `AZURE_COGNITIVE_SERVICES_RESOURCE_NAME` |
| Amazon Bedrock | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_PROFILE`, `AWS_REGION`, `AWS_BEARER_TOKEN_BEDROCK` |
| Google Vertex AI | `GOOGLE_APPLICATION_CREDENTIALS`, `GOOGLE_CLOUD_PROJECT`, `VERTEX_LOCATION` |
| Cloudflare Workers AI | `CLOUDFLARE_ACCOUNT_ID`, `CLOUDFLARE_API_KEY` |
| Cloudflare AI Gateway | `CLOUDFLARE_ACCOUNT_ID`, `CLOUDFLARE_GATEWAY_ID`, `CLOUDFLARE_API_TOKEN` |
| DigitalOcean | `DIGITALOCEAN_ACCESS_TOKEN` |
| GitLab Duo | `GITLAB_TOKEN`, `GITLAB_INSTANCE_URL`, `GITLAB_AI_GATEWAY_URL`, `GITLAB_OAUTH_CLIENT_ID` |
| SAP AI Core | `AICORE_SERVICE_KEY`, `AICORE_DEPLOYMENT_ID`, `AICORE_RESOURCE_GROUP` |
| Snowflake Cortex | `SNOWFLAKE_ACCOUNT`, `SNOWFLAKE_CORTEX_TOKEN` |
| NVIDIA | `NVIDIA_API_KEY` |

**Custom / OpenAI-compatible provider** — the escape hatch for anything not in the registry:

```json
{
  "provider": {
    "custom-id": {
      "npm": "@ai-sdk/openai-compatible",
      "name": "Custom Provider",
      "options": { "baseURL": "https://api.custom.com/v1" },
      "models": { "model-name": { "name": "Display Name" } }
    }
  }
}
```

Documented caveat: if the endpoint speaks `/v1/responses` (not `/v1/chat/completions`), use
`@ai-sdk/openai` instead of `@ai-sdk/openai-compatible`.

Local runtimes are all just this pattern with a localhost `baseURL`:
- Ollama — `http://localhost:11434/v1`
- LM Studio — `http://127.0.0.1:1234/v1`
- llama.cpp (`llama-server`) — `http://127.0.0.1:8080/v1`

**models.dev** — the external registry OpenCode uses to auto-populate provider lists, model
ids, context/output limits, capabilities and pricing. `opencode models [provider]` lists
`provider/model` pairs; `--refresh` refreshes the cache *from models.dev*; `--verbose` adds
metadata such as costs. Custom providers get **no** metadata from models.dev, so they must
declare `limit.context` and `limit.output` by hand.

Bedrock custom inference profiles show that a model entry can remap its wire id:

```json
{
  "provider": {
    "amazon-bedrock": {
      "models": {
        "anthropic-claude-sonnet-4.5": {
          "id": "arn:aws:bedrock:us-east-1:xxx:application-inference-profile/yyy"
        }
      }
    }
  }
}
```

### What Cowork would need

1. **Separate the "wire protocol" axis from the "endpoint" axis.** OpenCode's `npm` field is
   really "which adapter", and it works only because everything is JS. In Rust, the
   equivalent is an enum:

   ```json
   { "provider": { "acme": { "api": "openai-chat", "base_url": "...", "api_key": "{env:ACME_KEY}" } } }
   ```

   with `api` one of `anthropic-messages` | `openai-chat` | `openai-responses` |
   `google-genai` | `bedrock` | ... A closed enum is *better* than OpenCode's npm string: it
   is type-checked, it cannot pull arbitrary code at runtime, and it removes the documented
   `/v1/responses` footgun (the user picks the protocol explicitly instead of discovering it
   broke).

2. **`base_url` + `headers` + `api_key` on every provider, including first-party.** This is
   the single most important interop feature and it is what makes corporate gateways,
   LiteLLM, Cloudflare AI Gateway, OpenRouter and local Ollama all work with zero adapter
   code. Do not special-case Anthropic/OpenAI as unoverridable.

3. **Credential storage must be better than `auth.json`.** OpenCode writes API keys in
   plaintext to `~/.local/share/opencode/auth.json`. Cowork should:
   - default to the OS keychain (`keyring` crate: macOS Keychain, Windows Credential Manager,
     Secret Service / libsecret on Linux),
   - fall back to a `0600` file only when no keychain is available (headless Linux, CI),
   - still honour `{env:}` and `{file:}` so that config-as-code works,
   - never write a key that came from an env var back into storage.

   Also: keep credentials **out of the config file** as the default path. The config file gets
   committed; the credential store does not.

4. **Provider precedence / resolution order must be explicit.** OpenCode has three sources
   (env var auto-detect, `auth.json`, `provider.*.options.apiKey`) and never documents which
   wins. Cowork should state it: explicit config `api_key` > keychain entry > env var, and
   surface the resolved source in a diagnostics command (`cowork doctor`), because "wrong key
   silently picked up from a stale env var" is a top-tier support burden.

5. **Registry**: a models.dev-equivalent is genuinely valuable (context windows, pricing,
   capability flags change weekly and hardcoding them ages badly). But Cowork should
   **vendor a snapshot at build time** and treat the network fetch as an optional refresh,
   not a boot dependency. See §5.

6. **Allow per-model `limit.context` / `limit.output` overrides** regardless of registry data
   — users behind gateways often have different real limits than the upstream model.

### Design smells to avoid

- **Plaintext `auth.json`** — already covered. This is the clearest thing to do differently.
- **`npm` as a config value** means the config file can name an arbitrary npm package that
  then gets loaded into the agent process. Combined with remote `.well-known` config, that is
  a supply-chain hole. A Rust enum closes it.
- **`blacklist` / `whitelist` naming** — use `allow` / `deny` or `include` / `exclude`.
  Trivial but it is 2026.
- **Undocumented key-resolution precedence** (env vs auth.json vs config) is a bug factory.
- Transport knobs (`timeout`, `headerTimeout`, `chunkTimeout`, `setCacheKey`) living under
  `provider` next to per-provider definitions is a namespace collision waiting to happen —
  Cowork should nest them under something like `provider_defaults` or per-provider
  `transport: { ... }`.

---

## 3. Models

### How OpenCode does it

**Identifier format**: `provider_id/model_id`. The model id may itself contain slashes, so
the split is on the **first** `/` only:
- `anthropic/claude-sonnet-4-20250514`
- `opencode/gpt-5.1-codex`
- `lmstudio/google/gemma-3n-e4b`  (provider `lmstudio`, model `google/gemma-3n-e4b`)

**Selection & switching**, in documented precedence order:
1. CLI flag `--model` / `-m`
2. `model` key in `opencode.json`
3. Last used model (persisted state)
4. First model by an internal priority list

Interactive: `/models` in the TUI. Listing: `opencode models [provider]`.

**Small vs large model roles**: yes — `small_model` is a top-level config key, documented on
the **config** page as *"lightweight tasks, defaults to a cheaper variant"*. The providers
page shows it in real use:

```json
{ "small_model": "gitlab/duo-chat-haiku-4-5", "share": "disabled" }
```
```json
{ "model": "snowflake-cortex/claude-sonnet-4-6", "small_model": "snowflake-cortex/claude-haiku-4-5" }
```

Note the doc gap: the **models page never mentions `small_model` at all**. The docs do not
enumerate which internal tasks route to it (title generation and summarisation/compaction are
the obvious candidates given the `compaction` key, but that is inference, not documentation).

**Model-specific options** live at `provider.<id>.models.<model>.options`:

```json
{
  "provider": {
    "openai": {
      "models": {
        "gpt-5": {
          "options": {
            "reasoningEffort": "high",
            "textVerbosity": "low",
            "reasoningSummary": "auto"
          }
        }
      }
    }
  }
}
```

Documented options: `reasoningEffort` (OpenAI: `none`/`minimal`/`low`/`medium`/`high`/`xhigh`),
`textVerbosity`, `reasoningSummary`, Anthropic `thinking: { type, budgetTokens }`, and
`include` arrays for content selection. Token limits are `limit.context` / `limit.output` on
the model entry (documented on the *providers* page, not the models page).
`temperature` and `top_p` are **not** documented as model options — they only appear as
**agent** fields.

**Variants** — a nice idea: named bundles of model options that can be cycled at runtime.
Built-ins exist per provider (Anthropic high/max; OpenAI reasoning levels; Google low/high).
Custom:

```json
{
  "provider": {
    "openai": {
      "models": {
        "gpt-5": {
          "variants": {
            "thinking": { "reasoningEffort": "high" },
            "fast": { "disabled": true }
          }
        }
      }
    }
  }
}
```

Bound to a `variant_cycle` keybind so the user can flip reasoning depth mid-session without
editing config or changing model.

**Per-agent override** — models page: *"The agent config overrides any global options here."*
Agent shape (from the agents page):

```json
{
  "agent": {
    "agentName": {
      "description": "...",
      "mode": "primary|subagent|all",
      "model": "provider/model-id",
      "temperature": 0.1,
      "top_p": 0.9,
      "steps": 5,
      "prompt": "{file:./prompts/file.txt}",
      "permission": { "edit": "deny", "bash": "ask" },
      "disable": true,
      "hidden": true,
      "color": "#FF5733"
    }
  }
}
```

Agents can also be markdown files with YAML frontmatter, filename = agent id:
`~/.config/opencode/agents/<name>.md` or `.opencode/agents/<name>.md`.

```yaml
---
description: Purpose statement
mode: subagent
permission:
  edit: deny
  bash: deny
---
```

Built-in primary agents: **Build** (all tools), **Plan** (edits and bash default to `ask`).
Built-in subagents: **General**, **Explore**, **Scout**. `subagent_depth` (default 1) caps
nesting. `permission.task` gates which subagents may be invoked:
`"task": {"*": "deny", "code-reviewer": "ask"}`.

### What Cowork would need

1. **Adopt `provider/model` with first-slash splitting.** It is the de-facto standard
   (OpenRouter, LiteLLM, Continue, Aider all use it) and users will paste these strings
   between tools. Parse as `split_once('/')`, never `split('/')`.

2. **Ship the small/large split from day 1, and name the roles explicitly.** OpenCode's single
   `small_model` is under-specified. Cowork should define a role map so users can see and
   control what the cheap model is doing:

   ```json
   {
     "model": "anthropic/claude-sonnet-4-5",
     "models": {
       "small": "anthropic/claude-haiku-4-5",
       "title": "{small}",
       "summarize": "{small}",
       "autocomplete": "..."
     }
   }
   ```

   In an editor, there are more cheap-model jobs than in a TUI (inline completion, commit
   messages, symbol summaries, semantic search reranking) — a flat `small_model` will not
   stretch far enough.

3. **Copy variants.** Named option bundles plus a keybind to cycle is the best idea on these
   pages. In a GPUI panel this becomes a segmented control in the composer
   (`fast / balanced / thinking`), which is far better UX than a model dropdown, because it
   maps to what the user actually wants (how hard should it think) rather than to SKU names.

4. **Per-agent model + sampling override**, exactly as OpenCode does, plus the markdown-file
   form. The markdown-with-frontmatter agent file is a good pattern: it is diffable,
   reviewable in a PR, and the long system prompt lives in the body rather than escaped
   inside JSON. For a Rust editor, parse with `serde_yaml` over the frontmatter and treat the
   body as the prompt.

5. **Model metadata struct** should carry, per model: `context_limit`, `output_limit`,
   `supports_tools`, `supports_images`, `supports_reasoning`, `supports_cache`,
   `input_cost` / `output_cost` / `cache_cost`. OpenCode gets these from models.dev and the
   docs never spell out the shape — Cowork should define it as a first-class Rust type since
   the token-budget and cost UI both depend on it.

### Design smells to avoid

- **`small_model` is a single untyped string with no documented consumers.** Users cannot
  tell what it affects or verify it is being used. Make the roles explicit and observable.
- **Model options nested four levels deep**
  (`provider.openai.models.gpt-5.options.reasoningEffort`) is painful to hand-edit and
  impossible to express as "this setting, for whatever model I'm currently on". Cowork should
  support both the per-model path *and* a session-level override that the composer UI writes.
- **Option names are raw provider API field names** (`reasoningEffort`, `textVerbosity`,
  `reasoningSummary`, `thinking.budgetTokens`). That leaks vendor vocabulary into the config
  and breaks when a user switches provider. Cowork should define normalised names
  (`reasoning: "high"`, `verbosity: "low"`) mapped per-provider internally, with a
  `raw: { ... }` passthrough escape hatch for anything not modelled.
- **`temperature` exists on agents but not on models** — an inconsistency; the two sampling
  surfaces should be unified.
- **"Last used model" at precedence 3** is invisible state that makes sessions
  non-reproducible. Cowork should scope last-used per workspace and show it in the UI, not
  silently in a state file.

---

## 4. OpenCode Zen

### How OpenCode does it

Zen is **a first-party AI gateway**, described as *"an AI gateway that gives you access to
these models"* — a curated, pay-as-you-go proxy in front of OpenAI, Anthropic, Google,
DeepSeek and others, with models selected/tested for coding-agent use.

- Provisioning: sign up at `https://opencode.ai/auth`, add billing, get an API key, then
  `/connect` in the TUI and pick "OpenCode Zen".
- Provider id in config: **`opencode`** — so models are `opencode/gpt-5.5`,
  `opencode/gpt-5.1-codex`, etc.
- Model list endpoint: `https://opencode.ai/zen/v1/models`
- Catalogue at time of writing includes GPT 5.6 Sol, GPT 5.5, Claude Opus 5, Claude Sonnet 5,
  Gemini 3.8 Flash, DeepSeek V4 variants, plus free/beta models (Big Pickle, MiMo-V2.5 Free,
  Nemotron variants).
- Sample pricing: GPT 5.5 at `$5 / $30` per 1M input/output tokens; Claude Sonnet 5 at
  `$2 / $10`. Auto-reload $20 when balance drops below $5; per-workspace-member monthly
  spend limits.
- **Optionality, quoted**: *"Completely optional and you don't need to use it to use
  OpenCode."* Nothing in the product requires it; BYO keys is fully supported.

### What Cowork would need

1. **Nothing, initially — and that is the point.** Zen is a business model bolted onto the
   provider abstraction. Because it is just "provider id `opencode` with a baseURL and a
   key", it costs OpenCode almost no code. That is the lesson worth stealing: **if Cowork's
   provider layer is good enough, a future Cowork-hosted gateway is a config entry, not a
   rewrite.** Design the provider abstraction so a first-party gateway is indistinguishable
   from a third-party one.

2. **Do keep the "curated model list" idea, decoupled from billing.** The genuinely useful
   part of Zen is *"these are the models that actually work well as coding agents"*. Cowork
   can ship that as a recommended-models list / default ordering in the picker without
   running a gateway. This is what makes first-run not-terrible.

3. **Free-tier models as an onboarding path** is worth noting competitively: it lets a new
   user get a working agent with zero keys and zero credit card. If Cowork wants a
   comparable zero-config first run, options are a bundled local model via Ollama detection,
   or a partner free tier.

### Notes

- Zen being optional is correct and Cowork must preserve the equivalent property: **no
  first-party account should ever be required** to use the panel with your own key. Any
  coupling here (telemetry gate, login-to-configure, "sign in to see models") would be the
  main thing a competitor could attack.
- Reserve the provider id namespace early. OpenCode took the bare id `opencode` for its
  gateway, which now conflicts conceptually with "the tool itself" in config files.
  Cowork should use something like `cowork-gateway`, not `cowork`.

---

## 5. Network

### How OpenCode does it

**Proxies** — standard env vars only, no config keys:
- `HTTPS_PROXY` — recommended: `export HTTPS_PROXY=https://proxy.example.com:8080`
- `HTTP_PROXY` — fallback
- `NO_PROXY` — *required*: `export NO_PROXY=localhost,127.0.0.1`
- Credentials inline: `export HTTPS_PROXY=http://username:password@proxy.example.com:8080`
  (docs caution against hardcoding passwords)
- NTLM / Kerberos are **not** supported; docs redirect users to an LLM gateway instead.

**Certificates**
- `NODE_EXTRA_CA_CERTS=/path/to/ca-cert.pem` — applies to both proxied and direct API calls.
- No TLS-skip / `insecure` option is documented.

**Critical gotcha, quoted**: the TUI talks to a local HTTP server, and *"You must bypass the
proxy for this connection to prevent routing loops."* Hence `NO_PROXY` being mandatory rather
than optional. Server `port` / `hostname` are configurable by CLI flag and via `server` in
config. Related env vars from the CLI page: `OPENCODE_SERVER_PASSWORD`,
`OPENCODE_SERVER_USERNAME` (basic auth for `serve` / `web`, default username `opencode`).

**Air-gapped / offline**: the network page *does not address it at all* — no offline mode, no
documented models.dev cache policy, no way to disable remote connectivity. Adjacent evidence:
`opencode models --refresh` implies a models.dev cache exists, and `autoupdate` can be set to
`false` / `"notify"`, but the behaviour of a cold start with no network is undocumented.

### What Cowork would need

1. **Honour `HTTPS_PROXY` / `HTTP_PROXY` / `NO_PROXY` / `ALL_PROXY`** — in Rust, `reqwest`
   does this automatically with `Client::builder()` default proxy detection, but verify:
   `NO_PROXY` CIDR and wildcard handling differs between libraries and is a frequent
   enterprise complaint. Test `NO_PROXY=localhost,127.0.0.1,.corp.internal`.

2. **Also support explicit config keys**, not just env vars:
   ```json
   { "network": { "proxy": "http://proxy:8080", "no_proxy": ["localhost"], "ca_bundle": "/path/ca.pem" } }
   ```
   Env-var-only is hostile to a GUI editor, where the app is launched from a Dock/Start Menu
   icon and never inherits the user's shell environment. **This is a concrete OpenCode
   assumption that does not survive the move from TUI to GUI editor** and Cowork must fix it.

3. **Custom CA**: `SSL_CERT_FILE` / `NODE_EXTRA_CA_CERTS` equivalents. In Rust, build the
   `reqwest` client with `add_root_certificate()` from a configured bundle, and consider
   `rustls-native-certs` or `reqwest`'s `native-tls` feature so corporate MITM proxies with
   OS-trusted roots work out of the box — a meaningful advantage over Node's behaviour, where
   the OS trust store is *not* consulted by default and every enterprise user hits it.

4. **Proxy credentials**: support them, but read from the keychain, not from a URL embedded
   in an env var. Explicitly support the case where the proxy needs auth but the API key does
   not.

5. **Offline / air-gapped must be a designed state, not an accident.** Cowork should:
   - vendor the model registry snapshot in the binary so a cold start with no network still
     lists models,
   - make registry refresh explicit and cached on disk with a TTL,
   - support `"offline": true` / a `--offline` flag that disables *all* non-model network
     calls (registry, update check, telemetry, share),
   - ensure a purely local provider (Ollama at `127.0.0.1:11434`) works with zero outbound
     internet — this is a real requirement for defence/finance/health customers and OpenCode
     currently has no documented answer.

6. **Do not adopt the local-HTTP-server architecture without thinking.** OpenCode's TUI is a
   client to a local server, which is why proxy loops, `NO_PROXY`, CORS config and
   `OPENCODE_SERVER_PASSWORD` / `OPENCODE_SERVER_USERNAME` all exist. Cowork inside a Rust
   editor is in-process: none of that complexity is needed, and skipping it removes a whole
   class of bugs and an attack surface. (The one thing the server buys OpenCode is remote/web
   access and scriptability — if Cowork wants that later, add it as an opt-in feature, not as
   the default transport.)

### Design smells to avoid

- **Env-var-only network config** — unusable for a GUI app. Biggest portability defect here.
- **No documented offline story** for a tool that bootstraps its model list from a remote
  registry. Any customer with an air-gapped network cannot evaluate the product.
- **No `ALL_PROXY` / SOCKS mention** — worth supporting.
- **Mandatory `NO_PROXY` for the app's own loopback traffic** is a self-inflicted wound from
  the client/server split; the app should always bypass the proxy for its own loopback
  address rather than making the user configure it.

---

## Cross-cutting: what to copy vs. what to change

| Area | Copy | Change |
|---|---|---|
| Config | deep-merge across global/project, `{env:}` + `{file:}`, published JSON Schema, `.cowork/` convention dirs | drop remote config; collapse 8 layers to ~4; specify array merge; explicit override wins |
| Providers | `base_url` + `headers` + `api_key` on every provider; OpenAI-compatible escape hatch; per-model `limit` overrides | keychain instead of plaintext `auth.json`; closed protocol enum instead of `npm` package string; document key precedence |
| Models | `provider/model` ids, first-slash split; **variants + cycle keybind**; per-agent model/sampling override; markdown agent files | explicit model-role map instead of one `small_model`; normalised option names with `raw` passthrough; session-level override |
| Zen | provider abstraction good enough that a gateway is pure config; curated model list for first-run | never make a first-party account required; don't take the bare product name as the provider id |
| Network | standard proxy env vars; `NODE_EXTRA_CA_CERTS` equivalent | add config keys (GUI apps don't inherit shells); use OS trust store; designed offline mode; skip the local HTTP server |

**Highest-value single idea to steal:** *variants* — named, cyclable bundles of model options
surfaced as a `fast / balanced / thinking` control instead of a model dropdown.

**Highest-value single thing to do better:** *credential handling* — OS keychain, documented
resolution precedence, and a `cowork doctor` that shows where each resolved key came from.

---

## Documentation gaps found (things the docs do not answer)

These are unknowns the Cowork team should not assume away:

1. Array merge behaviour across config levels — replace or concat? Never stated.
2. `small_model` — which internal operations actually use it. Never stated; absent from the
   models page entirely despite being a core key.
3. API key resolution precedence between env var, `auth.json`, and `provider.*.options.apiKey`.
4. Whether `provider.*.options.baseURL` also redirects the models.dev metadata lookup.
5. models.dev cache location, TTL, and cold-start-without-network behaviour.
6. Whether `temperature` / `top_p` can be set outside an agent definition.
7. Exact `limit` semantics — is `limit.output` `max_tokens` on the request, or only a
   budgeting hint for the UI?
