# OpenCode: Intro, Go, CI integrations, Enterprise, Troubleshooting, Windows, References

Source pages (fetched 2026-09-11, each fetched separately):

- https://opencode.ai/docs/
- https://opencode.ai/docs/go/
- https://opencode.ai/docs/github/
- https://opencode.ai/docs/gitlab/
- https://opencode.ai/docs/enterprise/
- https://opencode.ai/docs/troubleshooting/
- https://opencode.ai/docs/windows-wsl
- https://opencode.ai/docs/references/

Scope note: the GitHub org behind the docs is now `anomalyco/opencode` (was `sst/opencode`). Where a
fact is *not* stated on the page, this report says so explicitly rather than filling it in from
memory — the distinction matters when we copy behaviour.

---

## 1. How OpenCode does it

### 1.1 Positioning and the first mental model (`/docs/`)

**Claim:** "open source AI coding agent", "available as a terminal-based interface, desktop app, or
IDE extension." Three surfaces, one agent core.

Page headings, in order — this *is* the onboarding funnel:

`Intro -> Prerequisites -> Install -> Configure -> Initialize -> Usage -> Ask questions -> Add
features -> Make changes -> Undo changes -> Share -> Customize`

The mental model taught, in the order it is taught:

1. **Install a binary, open a TUI.** No project wiring, no extension host, no account first.
2. **Connect a provider** (`/connect`), not "log into OpenCode". Provider-agnostic from minute one.
   Newcomers are steered to **OpenCode Zen** — a curated, pre-tested model list — so the first run
   cannot fail on a bad model choice.
3. **`/init` writes `AGENTS.md`.** The agent reads the repo and writes a project-structure /
   coding-patterns file into the project root. Docs say: *"You should commit your project's
   `AGENTS.md` file to Git."* Persistent project memory is a **committed artifact**, not hidden
   state. This is the single highest-leverage idea on the page.
4. **Plan mode vs Build mode**, toggled with `Tab`, with a mode indicator in the lower-right corner.
   Plan mode "disables changes and suggests implementation". Two modes, one key, always visible.
5. **`@` fuzzy-file reference** to pull files into the prompt.
6. **`/undo` and `/redo`** revert/restore agent changes iteratively — a first-class, repeatable
   escape hatch, taught *before* "Share".
7. **`/share`** produces a public link to the conversation.
8. Only then: themes, keybinds, formatters, custom commands.

Install surface (breadth is the point — meet the user in whatever package manager they already have):

```bash
curl -fsSL https://opencode.ai/install | bash
npm install -g opencode-ai          # also bun / pnpm / yarn global
brew install anomalyco/tap/opencode
sudo pacman -S opencode             # paru -S opencode-bin
choco install opencode              # scoop install opencode
mise use -g github:anomalyco/opencode
docker run -it --rm ghcr.io/anomalyco/opencode
```

**Runtime / language:** **NOT STATED on the intro page.** No mention of the implementation language,
of a client/server split, or of the TUI's implementation. What the distribution *implies*: an npm
package (`opencode-ai`) plus standalone cross-platform binaries on GitHub Releases plus a Docker
image. So: a JS/TS-ecosystem core bundled into a self-contained binary, plus a separate desktop app
and IDE extension that talk to it. Do not quote a language to the team as documented — it isn't.

There *is* a server, documented elsewhere (see the Windows page): `opencode serve --hostname --port`,
`opencode web --hostname`, an `OPENCODE_PORT` env var, a desktop app that connects to
`http://localhost:4096`, and `OPENCODE_SERVER_PASSWORD`. So the real architecture is **headless agent
server + multiple thin clients (TUI, desktop, web, IDE)** — the docs just never say so out loud on
the intro page.

### 1.2 `/docs/go/` — not a Go SDK

Trap for anyone skimming the sitemap: **"OpenCode Go" is a $10/month model subscription**, not the Go
language, not an SDK, not the TUI.

- Curated access to open-weight models (DeepSeek, Qwen, GLM, Kimi, MiniMax; ~30 models).
- Flow: subscribe at `https://opencode.ai/auth` -> copy API key -> `/connect` in the TUI -> select
  "OpenCode Go" -> paste -> `/models`.
- Model ids namespaced as `opencode-go/<model-id>`, e.g. `opencode-go/kimi-k3`.
- Endpoints — note they expose **both** wire formats:
  - `https://opencode.ai/zen/go/v1/chat/completions` (OpenAI chat completions)
  - `https://opencode.ai/zen/go/v1/responses` (OpenAI responses)
  - `https://opencode.ai/zen/go/v1/messages` (Anthropic messages)
  - `https://opencode.ai/zen/go/v1/models` (discovery)
- Rate shaping is a **rolling triple**: 5-hour limit = 20% of monthly cap, weekly = 50%, monthly =
  100%. Overage can fall through to a prepaid "Zen balance".
- "One member per workspace."
- Per-model data retention is published: most models 0-day retention; Grok 30 days; one model
  (Muse Spark) trains on user data. Retention as a **per-model attribute in the catalogue** is a
  genuinely good idea.
- Third-party clients (Claude Code, Codex, Kilo Code, Hermes, Pi, jcode, ZCode) are listed as
  validated consumers — the gateway is deliberately not OpenCode-exclusive.

### 1.3 GitHub integration (`/docs/github/`)

**Invocation:** a human writes `/opencode` or `/oc` in
(a) an issue comment, (b) a PR comment, or (c) a **PR review comment on a specific line** in the
Files tab — in which case the action automatically picks up file path, line number and surrounding
diff context. The agent also reads the whole comment thread for context.

**Install:** `opencode github install` — an interactive CLI that installs the GitHub App, writes the
workflow, and configures the secret.

**Workflow file:** `.github/workflows/opencode.yml`

```yaml
name: opencode
on:
  issue_comment:
    types: [created]
  pull_request_review_comment:
    types: [created]
jobs:
  opencode:
    if: |
      contains(github.event.comment.body, '/oc') ||
      contains(github.event.comment.body, '/opencode')
    runs-on: ubuntu-latest
    permissions:
      id-token: write
    steps:
      - name: Checkout repository
        uses: actions/checkout@v6
        with:
          fetch-depth: 1
          persist-credentials: false
      - name: Run OpenCode
        uses: anomalyco/opencode/github@latest
        env:
          ANTHROPIC_API_KEY: ${{ secrets.ANTHROPIC_API_KEY }}
        with:
          model: anthropic/claude-sonnet-4-20250514
```

Details worth stealing:

- The trigger check is a cheap `if:` on the comment body, so 99% of comment events cost ~0 runner
  seconds.
- `persist-credentials: false` and `fetch-depth: 1` — the checkout token is deliberately *not* left
  in `.git/config` for the agent to find.
- Default permission is only `id-token: write`: the action exchanges an **OIDC token** with the
  GitHub App for a scoped installation token. Broad repo write is not granted to the job itself.
- Escape hatch: `use_github_token: true` uses the ambient `GITHUB_TOKEN` and removes the App
  requirement; then you must add `contents: write`, `pull-requests: write`, `issues: write` yourself.
- Inputs: `model` (required, `provider/model`), `agent` (defaults to `default_agent` or `"build"`),
  `share` (defaults **true on public repos**), `prompt` (override the default behaviour),
  `mentions` (defaults `/opencode,/oc`), `variant` (reasoning effort: `high` / `max` / `minimal`).
- Capabilities: branch + PR creation, pushing commits to an existing PR, inline code edits during
  review, explanatory comments.
- Security posture is "it all runs in *your* runner with *your* secrets" — no OpenCode-hosted
  execution.

### 1.4 GitLab integration (`/docs/gitlab/`)

Less first-party; two paths:

1. **CI/CD component** (community-maintained, `nagyv/gitlab-opencode`):

```yaml
include:
  - component: $CI_SERVER_FQDN/nagyv/gitlab-opencode/opencode@2
    inputs:
      config_dir: ${CI_PROJECT_DIR}/opencode-config
      auth_json: $OPENCODE_AUTH_JSON
      command: optional-custom-command
      message: "Your prompt here"
```

2. **GitLab Duo** flow: mention `@opencode` in an issue or MR comment
   (`@opencode explain this issue`, `@opencode fix this`, `@opencode review this merge request`).
   Trigger phrase is configurable. Requires a longer bootstrap job that installs OpenCode, installs
   the `glab` CLI, and wires auth before invoking the agent.

CI/CD variables: `OPENCODE_AUTH_JSON` (masked + hidden), `ANTHROPIC_API_KEY`,
`GITLAB_TOKEN_OPENCODE`, `GITLAB_HOST`.

**Key detail for us:** `OPENCODE_AUTH_JSON` is literally the contents of the on-disk `auth.json`,
shape `{"anthropic": {"type": "api", "key": "..."}}`. The same credential file format is used
locally and in CI — a single auth schema with a `type` discriminator (`api`, and presumably `oauth`
for subscription logins). That is why CI works with one env var.

Differences vs GitHub: no OIDC/App short-lived-token story (a long-lived PAT in
`GITLAB_TOKEN_OPENCODE` instead), no line-level review-comment context documented, no
`opencode gitlab install` one-shot installer, and the component is third-party.

### 1.5 Enterprise (`/docs/enterprise/`)

Thin page. What it does commit to:

- **Per-seat licensing**, custom quotes, contact at `https://opencode.ai/enterprise`.
- **"A single central config for your entire organization."** The *mechanism* of distribution to
  developer machines is **not documented** — no env var, no URL, no path given.
- **SSO:** "Through the central config, OpenCode can integrate with your organization's SSO provider
  for authentication." Credentials for an internal AI gateway come from existing identity
  management.
- **Internal AI gateway routing**, plus: "You can also disable all other AI providers, ensuring all
  requests go through your organization's approved infrastructure." Allowlist mechanism unspecified.
- **Pricing:** "If you have your own LLM gateway, we do not charge for tokens used." Seats only.
- **Data handling:** "OpenCode does not store your code or context data." Local processing or direct
  provider API calls. The one exception is `/share` — shared conversations are hosted on the
  opencode.ai CDN, and the sample central config shows `share` being **disabled** org-wide.
- Self-hosting the share pages is "currently on our roadmap." No on-prem deployment.
- **Audit logging: entirely absent from the page.** So are spend monitoring and per-user usage
  reporting. For a 2026 enterprise pitch this is a visible hole.

### 1.6 Troubleshooting + on-disk layout (`/docs/troubleshooting/`) — the most useful page

Headings: Troubleshooting, Logs, Storage, Uninstall, Desktop app (Quick checks, Disable plugins,
Check the global config, Check plugin directories, Clear the cache, Fix server connection issues,
Clear the desktop default server URL, Remove `server.port`/`server.hostname` from your config, Check
environment variables, Linux: Wayland / X11 issues, Windows: WebView2 runtime, Windows: General
performance issues, Notifications not showing, Reset desktop app storage), Getting help, Common
issues.

**The headline finding: OpenCode does NOT use platform-native app-data directories for its core
state.** It hardcodes a Unix-style `~/.local/share/opencode`, `~/.config/opencode`,
`~/.cache/opencode` layout and then uses **the same literal paths on Windows** under `%USERPROFILE%`
— i.e. `C:\Users\you\.local\share\opencode`. It does *not* use `%APPDATA%` / `%LOCALAPPDATA%` for the
agent, and no `XDG_DATA_HOME` / `XDG_CONFIG_HOME` / `XDG_CACHE_HOME` override is documented on this
page. The only thing that *does* follow platform convention is the **desktop app's** UI state.

#### On-disk paths, per OS

| What | macOS | Linux | Windows |
|---|---|---|---|
| Data root | `~/.local/share/opencode/` | `~/.local/share/opencode/` | `%USERPROFILE%\.local\share\opencode` |
| Credentials | `~/.local/share/opencode/auth.json` | same | `%USERPROFILE%\.local\share\opencode\auth.json` |
| Logs | `~/.local/share/opencode/log/` | same | `%USERPROFILE%\.local\share\opencode\log` |
| Sessions / messages | `~/.local/share/opencode/project/` | same | `%USERPROFILE%\.local\share\opencode\project` |
| — git repo | `project/<project-slug>/storage/` | same | `project\<project-slug>\storage\` |
| — non-git dir | `project/global/storage/` | same | `project\global\storage\` |
| Global config | `~/.config/opencode/opencode.jsonc` | same | `%USERPROFILE%\.config\opencode\opencode.jsonc` |
| Global plugins | `~/.config/opencode/plugins/` | same | `%USERPROFILE%\.config\opencode\plugins` |
| Project plugins | `<project>/.opencode/plugins/` | same | `<project>\.opencode\plugins\` |
| Provider cache | `~/.cache/opencode` | `~/.cache/opencode` | `%USERPROFILE%\.cache\opencode` |
| Desktop UI state | `~/Library/Application Support` | `~/.local/share` | `%APPDATA%` |

Desktop UI state files (note the flat, per-scope split):
`opencode.settings.dat`, `opencode.global.dat`, `opencode.workspace.*.dat`.

**Log files:** timestamped, e.g. `2025-01-09T123456.log`; **the 10 most recent are retained**, older
ones pruned. Flags: `opencode --log-level DEBUG`, `opencode --print-logs`.

**Session storage:** keyed by *project*, with a `<project-slug>` directory per git repo and a single
`global` bucket for anything not in a repo. Sessions therefore survive across TUI/desktop/web clients
and are naturally scoped to the workspace — a session list is a directory listing, not a DB query.

**Bug reporting:** `github.com/anomalyco/opencode/issues` (search existing issues first), plus
`opencode.ai/discord` for real-time help.

**Documented failure modes and their fixes:**

| Symptom | Documented fix |
|---|---|
| Auth failures | Re-run `/connect`; verify key validity; check network |
| Model not available | Check provider auth; model id must be `<providerId>/<modelId>` (`openai/gpt-4.1`, `openrouter/google/gemini-2.5-flash`, `opencode/kimi-k2`); check subscription |
| `ProviderInitError` | Validate provider config; `rm -rf ~/.local/share/opencode`; re-auth |
| `AI_APICallError` / provider package issues | `rm -rf ~/.cache/opencode`, restart to reinstall provider packages |
| Copy/paste broken on Linux | `apt install -y xclip` / `xsel` (X11) or `wl-clipboard` (Wayland); headless CI needs `xvfb`, `Xvfb :99 -screen 0 1024x768x24 &`, `export DISPLAY=:99.0` |
| Wayland issues | `OC_ALLOW_WAYLAND=1` |
| Desktop can't reach server | Clear the stored default server URL; remove `server.port` / `server.hostname` from config; check `OPENCODE_PORT` |
| Windows desktop app | Missing **WebView2 runtime**; plus a separate "general performance issues" section |
| Plugin misbehaviour | Set `"plugin": []` in the global config to disable all plugins |
| Nuclear options | `opencode uninstall`, `opencode upgrade`, `opencode models`, "reset desktop app storage" |

Note that `rm -rf ~/.cache/opencode` "to reinstall provider packages" reveals that **provider SDKs are
downloaded at runtime into the cache**, not bundled. That is a supply-chain and offline-use
consideration, and it explains why enterprises with no npm egress would break.

### 1.7 Windows / WSL (`/docs/windows-wsl`)

Position, verbatim: *"While OpenCode can run directly on Windows, we recommend using Windows
Subsystem for Linux (WSL) for the best experience."* Rationale: *"WSL offers better file system
performance, full terminal support, and compatibility with development tools that OpenCode relies
on."*

Sections: Overview -> Setup (install WSL; `curl -fsSL https://opencode.ai/install | bash`;
`cd /mnt/c/Users/YourName/project` then `opencode`) -> Desktop App + WSL Server -> Web Client + WSL ->
Accessing Windows Files -> Tips.

- Desktop app across the boundary: `opencode serve --hostname 0.0.0.0 --port 4096`, then point the
  desktop app at `http://localhost:4096`. If localhost forwarding fails, get the VM IP with
  `hostname -I` and use `http://<wsl-ip>:4096`.
- Binding to `0.0.0.0` is gated on a password:
  `OPENCODE_SERVER_PASSWORD=your-password opencode serve --hostname 0.0.0.0`.
- Web client: `opencode web --hostname 0.0.0.0`, reachable from the Windows browser at
  `http://localhost:<port>`. Docs explicitly say to run `opencode web` **in the WSL terminal rather
  than PowerShell** for "proper file system access and terminal integration".
- Drive mapping `C:` -> `/mnt/c/`, `D:` -> `/mnt/d/`. Strong recommendation to clone repos **into**
  the WSL filesystem (`~/code/`) rather than working over `/mnt/c` — the 9p filesystem is the
  performance cliff.
- Config inside WSL lives at `~/.local/share/opencode/` — i.e. **a WSL install and a native Windows
  install have completely separate auth and session stores.**
- Notably **absent**: any Windows-native package-manager instructions on this page (they appear only
  on the intro page), and any discussion of CRLF, symlinks, long paths, ripgrep, or shell selection
  for tool execution. Native-Windows correctness is effectively undocumented; the answer is "use
  WSL".

### 1.8 References (`/docs/references/`)

**This is not a config schema reference.** It documents a feature called *references*: a
`references` map in `opencode.json` / `opencode.jsonc` that mounts **external directories and git
repos** into the agent's context as named aliases.

```jsonc
{
  "$schema": "https://opencode.ai/config.json",
  "references": {
    "design-system": { "path": "../design-system", "description": "..." },
    "api":           { "repository": "owner/repo", "branch": "main", "hidden": true }
  }
}
```

| Field | Type | Applies to | Meaning |
|---|---|---|---|
| `path` | string | local dirs | relative, absolute, or home-relative directory |
| `repository` | string | git | git URL, `host/path`, or GitHub `owner/repo` |
| `branch` | string | git | optional branch or ref |
| `description` | string | both | tells the agent *when* to consult this reference |
| `hidden` | boolean | both | hide from TUI `@` autocomplete |

Alias constraints: non-empty, no `/`, no whitespace, no backticks, no commas.

Mechanics that matter: aliases appear in TUI `@` autocomplete, **and the resolved paths plus
descriptions are injected into the agent's system context automatically**, so the model can decide to
go look at a sibling repo without the user attaching anything. The `description` field is effectively
a routing hint written by a human. Git references are cloned/checked out by OpenCode.

The real full JSON schema lives at `https://opencode.ai/config.json` (a public JSON Schema URL used
as `$schema` in every config example) and is documented on `/docs/config/`, which was not in scope
here. **Recommendation: fetch `https://opencode.ai/config.json` directly — it is the
machine-readable, complete source of truth for every config key, and worth diffing against Cowork's
settings schema.**

---

## 2. What Cowork would need

Ordered roughly by "blocks a demo" -> "blocks an enterprise sale".

### 2.1 On-disk layout — decide this first, and do NOT copy OpenCode

OpenCode's `%USERPROFILE%\.local\share\opencode` on Windows is a Unix-ism leaking onto a platform
that has had a correct answer since 2001. Since Cowork is a Rust editor developed on Windows, use the
`directories` crate (`ProjectDirs::from("dev", "Anna", "Cowork")`) and get:

| Purpose | Windows | macOS | Linux |
|---|---|---|---|
| Config | `%APPDATA%\Anna\Cowork\config` | `~/Library/Application Support/dev.Anna.Cowork` | `$XDG_CONFIG_HOME/cowork` (default `~/.config/cowork`) |
| Sessions / data | `%APPDATA%\Anna\Cowork\data` | `~/Library/Application Support/dev.Anna.Cowork` | `$XDG_DATA_HOME/cowork` (default `~/.local/share/cowork`) |
| Cache | `%LOCALAPPDATA%\Anna\Cowork\cache` | `~/Library/Caches/dev.Anna.Cowork` | `$XDG_CACHE_HOME/cowork` (default `~/.cache/cowork`) |
| Logs | `logs/` under the data root | same | same |
| Credentials | OS keychain (see 2.2) | Keychain | Secret Service / kernel keyring |

Honour `XDG_*` on Linux (OpenCode apparently does not). Keep the **project-scoped session layout** —
it is the best idea on the troubleshooting page:

```
<data>/projects/<project-slug>/sessions/<session-id>/...
<data>/projects/global/sessions/...        # for files opened outside a workspace
```

One directory per workspace plus a `global` bucket means "list my sessions for this repo" is a
readdir, and deleting a project's history is an `rm -rf`. Derive `<project-slug>` from the git remote
plus worktree path, and **write a `meta.json` inside each project dir recording the absolute path** —
Windows path casing and drive-letter variance will otherwise produce duplicate slugs for the same
repo.

Also cap log retention the way OpenCode does (10 most recent timestamped files). Unbounded agent logs
in `%APPDATA%` is a support ticket waiting to happen.

### 2.2 Credentials — do better than `auth.json`

OpenCode keeps provider keys in **plaintext `auth.json`** in the data dir. It buys them one real
thing: `OPENCODE_AUTH_JSON` in GitLab CI is literally that file's contents, so local and CI auth share
one schema. Cowork should keep the *schema* and change the *storage*:

- Secrets in the OS keychain via the `keyring` crate, with the same tagged-union shape
  (`{"anthropic": {"type": "api", "key": ...}}`, `type` also allowing `oauth` with refresh token and
  expiry, and `gateway` for enterprise).
- A `COWORK_AUTH_JSON` env var accepting that JSON verbatim for CI/headless/container use, bypassing
  the keychain. This is exactly what makes OpenCode's CI story a one-liner.
- Never write secrets into the session store, and redact them from logs before the
  `--log-level DEBUG` path ever touches them.

### 2.3 The onboarding funnel

Copy the shape, it is well sequenced: **install -> connect a provider -> `/init` -> ask -> build ->
undo**. Specifics worth lifting:

- A **curated default model list** (their "Zen") so a first run cannot fail on model-id typos. Ship a
  known-good default and let power users override.
- **`/init` writing a committed `AGENTS.md`.** Cowork should read `AGENTS.md` *and* `CLAUDE.md` (both
  are now de-facto standards) and should write the file into the repo, not into app data. Project
  memory that is reviewable in a PR is the whole point.
- **Plan vs Build as a one-key toggle with a persistent on-screen indicator.** In a GPUI panel this
  is a segmented control in the panel header, not a buried setting. The "agent cannot write in Plan
  mode" guarantee must be enforced in the tool layer, not in the prompt.
- **`@` fuzzy file reference** — Cowork already has a fuzzy file finder in the editor; reuse the same
  matcher and ranking in the panel input so muscle memory transfers.
- **`/undo` / `/redo` over agent turns.** OpenCode does this with per-session snapshots. In a Rust
  editor we have a better substrate: a hidden git worktree / index snapshot per agent turn, so undo
  is a checkout, it survives a crash, and it composes with the user's own edits. This is a
  differentiator — make it turn-granular and visible in the transcript.

### 2.4 Architecture: headless core + thin clients

OpenCode never says "client/server" in the intro, but the whole product is one: `opencode serve
--hostname --port`, `opencode web`, a desktop app pointed at `http://localhost:4096`,
`OPENCODE_PORT`, `OPENCODE_SERVER_PASSWORD`. That is what lets one agent core serve a TUI, a desktop
app, an IDE extension and CI.

For Cowork: put the agent core in its own crate with an in-process API *and* an optional local
HTTP/WS server, rather than wiring it directly into the panel's view code. It is much cheaper to do
this on day one than to retrofit. It buys headless CI runs, a future remote/devcontainer story, and
testability without GPUI.

If we expose a port at all, adopt their default-deny posture: bind `127.0.0.1` by default, and
**require a password/token before `0.0.0.0` binding is permitted at all** (not merely recommended).

### 2.5 Windows, since that is where we develop

The OpenCode docs' answer to Windows is "use WSL", which is an admission, not a design. Cowork is a
native Windows app and cannot punt. Concretely:

- **Shell selection for tool execution.** OpenCode's docs are silent. Decide explicitly whether the
  agent's shell tool runs `pwsh`/`powershell.exe`, `cmd`, or a bundled `sh`; make it configurable;
  and make the agent's prompt *know* which one it got — half of all agent shell failures on Windows
  are POSIX syntax sent to PowerShell.
- **Paths:** canonicalise to a single internal representation; watch for `\\?\` extended-length
  prefixes from `fs::canonicalize` leaking into prompts and tool args, drive-letter case, and
  `/mnt/c` vs `C:\` when the repo is opened from WSL.
- **CRLF:** never let the agent's diff/apply path normalise line endings silently. Preserve per-file
  EOL and honour `.gitattributes`.
- **WSL projects:** if the user opens `\\wsl$\Ubuntu\home\...`, decide whether tools run inside the
  distro (`wsl.exe -d <distro> -- ...`) or on the Windows side. OpenCode sidesteps this by making the
  user pick a world; we can be better by detecting it and telling the user which side commands run
  on. Note their footgun: a WSL install and a native install have **completely separate auth and
  session stores** — Cowork should at minimum warn, ideally share.
- **File watching and 9p:** their perf warning about `/mnt/c` is real; if we watch a WSL path from
  Windows, expect notification latency and fall back to polling.
- **WebView2:** their Windows desktop failure mode is a missing WebView2 runtime. Any Cowork panel
  that embeds a webview needs a runtime check and a clear remediation message, not a blank pane.

### 2.6 CI / bot integration (if Cowork wants a "@cowork" bot)

The GitHub design is the one to copy, near-verbatim:

- A cheap `if: contains(github.event.comment.body, '/cowork')` gate so idle comment traffic is free.
- Subscribe to both `issue_comment` and `pull_request_review_comment`; the second gives you **file +
  line + diff hunk for free**, which is the highest-signal invocation there is.
- `actions/checkout` with `persist-credentials: false` and `fetch-depth: 1` — do not leave a token in
  `.git/config` where the agent can read it.
- Default `permissions:` to `id-token: write` only, and exchange OIDC for a scoped App installation
  token. Offer `use_github_token: true` as the low-friction fallback, documenting that it needs
  `contents: write`, `pull-requests: write`, `issues: write`.
- Inputs to mirror: `model` (`provider/model`), `agent`, `prompt`, `mentions`, `share`, `variant`
  (reasoning effort).
- **Default `share` to false**, including on public repos. OpenCode defaults it *true* for public
  repos; that is a surprising default that uploads transcripts to a vendor CDN.
- GitLab: a first-party CI component beats OpenCode's third-party one, and accepting
  `COWORK_AUTH_JSON` as a single masked variable is what makes it a two-line `.gitlab-ci.yml`.

### 2.7 Enterprise — where we can straightforwardly beat them

OpenCode's enterprise page promises a central config, SSO and gateway-only routing, but:

- **It never says how the central config reaches the machine.** Cowork should specify this precisely:
  a machine-wide policy file at `%ProgramData%\Cowork\policy.json` /
  `/Library/Application Support/Cowork/policy.json` / `/etc/cowork/policy.json`, plus a
  `COWORK_POLICY_URL` for managed fetch, plus documented precedence
  (**policy > env > user config > project config**, with policy able to mark keys non-overridable).
- **Audit logging is entirely absent from their docs.** This is the clearest gap. Cowork should emit
  a structured, append-only audit log (session start, model and provider used, every tool invocation
  with its arguments, every file written, every command executed, token counts) with an optional
  syslog/OTLP sink. Enterprises will ask for exactly this and OpenCode has no answer on the page.
- **Model/provider allowlists** need to be a real enforcement point in provider resolution, not a
  docs claim.
- **Per-model data-retention metadata** (OpenCode Go publishes 0-day vs 30-day vs trains-on-your-data
  per model) is genuinely good; surface it in Cowork's model picker so a user sees the retention
  policy at selection time.
- **Offline / air-gapped:** OpenCode downloads provider packages at runtime into `~/.cache/opencode`.
  Cowork is Rust — compile providers in. That is a real enterprise selling point (no runtime npm
  egress, reproducible binary, no run-time supply-chain surface).
- Say clearly, as they do, that code and context are not stored by us, and make any share/telemetry
  feature opt-in and centrally disableable.

### 2.8 Config schema

- Publish a JSON Schema at a stable URL and put `$schema` in every generated config — OpenCode does
  this (`https://opencode.ai/config.json`) and it is why editor completion works in their config
  files. Cowork is an editor; our own config should be the best-completing config in our own product.
- Support `.jsonc` (comments) for the global config, as they do.
- Steal the **`references`** feature outright. A map of aliases to local dirs / git repos, each with
  a `description` injected into the agent's system context and a `hidden` flag for `@` autocomplete,
  is a small feature with a large effect: it is how a monorepo-adjacent developer gets the agent to
  consult a sibling repo without manually attaching files. Match their alias constraints (no `/`,
  whitespace, backticks or commas) and additionally sandbox reference paths against the agent's write
  permissions — references are **read-only context**, and that must be enforced, not assumed.

### 2.9 Support surface

Small, cheap, high-return items OpenCode already has:

- `cowork --log-level DEBUG` and `cowork --print-logs`.
- A "doctor" command that prints resolved config path, data dir, cache dir, active provider, model
  id, and the last log file path — the single most useful thing to have in a bug report.
- Documented one-liners for the nuclear options: clear cache, reset session store, re-auth.
- A documented `plugin: []`-style kill switch to disable all extensions when diagnosing.
- An issue template that asks for the log file and the doctor output.

---

## 3. Open questions / follow-ups

1. Fetch `https://opencode.ai/config.json` — the full machine-readable config schema, and the
   highest-value artifact for a competitive diff. `/docs/references/` was *not* the config reference.
2. `/docs/config/`, `/docs/agents/`, `/docs/permissions/`, `/docs/plugins/`, `/docs/server/` were out
   of scope here and cover the parts the intro page elides (agent definitions, the permission model,
   the HTTP server API).
3. The implementation language is not stated anywhere in the pages fetched; if it matters for a
   competitive claim, confirm from the repo rather than from docs.
4. No audit-logging, spend-control or on-prem story is documented — worth re-checking closer to any
   enterprise positioning work, since that is the axis where Cowork can differentiate.
