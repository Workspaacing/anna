# Cowork

Native AI agents for Wu, split the way the rest of the workspace is:

- **`CoworkPanel`** (dock panel, left by default) — session history, search, per-thread delete, and
  the model picker in the footer. It is *not* the chat.
- **`CoworkThreadView`** (workspace item, center pane) — where a conversation actually happens:
  streamed markdown responses, tool calls and their results, the composer, and the per-thread model
  picker.
- **Settings** live in the settings window on its own **Cowork** page
  (`crates/settings_ui/src/page_data.rs`, `cowork_page`), reached from the panel's gear button via
  `wu::OpenSettingsPage`. Providers and Models are sub-pages there; the panel shows runtime status
  only, because the settings framework renders declarative fields backed by JSON.

## Status

Compiles, lints and unit-tests clean. **None of this has been run against a real model** — no one
has opened the panel, sent a prompt, watched a tool call execute, or seen a response stream in.
Every claim below about runtime behaviour is derived from the code, not from observation.

Verified on 2026-09-11 (Rust 1.97.1, `x86_64-pc-windows-msvc`):

| Gate | Result |
| --- | --- |
| `cargo test -p cowork` | 69/69 pass |
| `cargo clippy -p cowork -p settings_ui -p settings_content --all-targets -- --deny warnings` | pass |
| `cargo test -p xtask` | pass — no forbidden crate edge |

What that does **not** cover: the panel rendering, dock persistence, SSE streaming against a real
provider, a tool call round-tripping through a model, Biome actually running, or an OSV query
returning an advisory. Those need a human at a running build.

## Models

Models come only from the [models.dev](https://models.dev) catalog (`catalog_url`, default
`https://models.dev/api.json`) — the same registry the Vercel AI SDK publishes. The catalog is
fetched on first use, cached in Wu's key-value store, and refreshed when it is older than 24 hours
or when the user asks for a refresh.

**There is no default-model setting, and no output-token setting.** A new thread starts on the
model you used last — remembered in the key-value store, not in `settings.json` — falling back to
the first model on offer when that one's provider has been disconnected or the model hidden. A
configured default would be something you had to keep in step by hand with the providers you have
actually connected, and it can name a model that no longer exists.

Each request asks for the model's **own** published output ceiling, from the catalog's
`limit.output`. Where models.dev declares none, the field is omitted so the provider's default
stands — except for Anthropic, whose Messages API rejects a request without `max_tokens`.

A provider's wire format is chosen from the AI SDK package the catalog names in its `npm` field:

| `npm` contains | Request/response shape | Endpoint |
| --- | --- | --- |
| `anthropic` | Anthropic Messages API, SSE | `{api}/v1/messages` |
| `google` | Gemini `streamGenerateContent`, SSE | `{api}/models/{model}:streamGenerateContent?alt=sse` |
| anything else | OpenAI Chat Completions, SSE | `{api}/v1/chat/completions` |

The catalog declares no `api` endpoint for `google`, `vertex`, `bedrock` or `azure`;
`Provider::api_base` supplies the missing one for Google and Azure. Vertex and Bedrock stay
unsupported and are filtered out of the selector, because they need service-account and SigV4
**request signing** rather than a different request shape — a key in a header is not enough.

Three shapes, three disagreements worth knowing about, each covered by a test:

- **Anthropic** puts tool results on a *user* message, and its `input_json_delta` events carry no
  id — they belong to whichever content block is open.
- **OpenAI** sends indexed tool-call fragments where the id and name arrive only on the first one.
- **Google** calls the assistant role `model`, carries tool calls and results as *parts* rather
  than fields, matches a result to its call by function **name**, and reports
  `finishReason: "STOP"` even when it is asking for a tool — so the stop reason has to be inferred
  from what the chunk contained.

## Tools

The agent gets `read`, `list`, `write` and `edit`. Everything routes through `Project` and the
buffers the editor owns rather than the filesystem, so the agent sees unsaved edits, its changes
appear in an open editor immediately, they join the undo history and the git gutter, and remote
projects work without a second code path.

`edit` refuses to guess: if the text to replace is absent it says so, and if it appears more than
once it says how many times and asks for more context. A tool that silently picks one occurrence is
a tool that silently corrupts files.

A tool failure returns an error *result* to the model rather than ending the turn, so the model can
correct itself. One user message is capped at `MAX_STEPS` model round trips.

**Not yet:** running commands. That needs a permission broker, and shipping shell access without
one would be indefensible.

## Verification

Everything the agent writes is checked by machinery compiled into Wu. No model is consulted, so
none of it costs tokens. Each check has its own toggle under **Cowork > Verification**, all on by
default.

| Check | Setting | When | What it does |
| --- | --- | --- | --- |
| Secrets | `cowork.verification.secret_scan` | **before** the write | Refuses to write an API key, token or private key, and tells the agent to read it from the environment instead |
| Biome | `cowork.verification.biome` | after the edit, before the save | Runs the project's own Biome over JS/TS/JSX/JSON/CSS and applies what it fixes |
| Diagnostics | `cowork.verification.diagnostics` | after the save | Hands the agent the errors and warnings the language servers report |
| Dependencies | `cowork.verification.dependency_audit` | after the save | Checks an edited manifest's packages against the OSV advisory database |

Two deliberate design decisions:

**The secret scan judges only the text the agent adds**, never the resulting file. Scanning the
whole file would flag a credential the user put there themselves and lock the agent out of that
file entirely. The agent is answerable for what it writes.

**Biome is the project's own, not a copy.** The `biome_*` crates are on crates.io and format, lint
and autofix all work from them — but they are frozen at 0.5.7, which is Biome 1.6.1 from March
2024, and the code that reads `biome.json` is `pub(crate)` in crates that are not published at all.
Embedding them would mean a linter that ignores your configuration and disagrees with the Biome
your CI runs, for roughly +10 MB of binary and 117 extra packages. So Cowork finds the Biome in
`node_modules/.bin` (or on `PATH`), passes the source on stdin with `--stdin-file-path`, and takes
the corrected source back on stdout. Your `biome.json` applies, the rules match CI, and it updates
when you update Biome. In a project without Biome the check is silent.

### On CodeQL and Dependabot

Neither can be embedded, and it is worth saying plainly why rather than quietly shipping something
else under their names:

- **CodeQL** is a proprietary GitHub binary of roughly 500 MB with no Rust crate, and its licence
  restricts use to open-source projects and GitHub Advanced Security. It also needs to *build* your
  project into a database before each analysis. Its job — static analysis of your source — is done
  here by the language servers and linters Wu already runs warm, reported to the agent by the
  Diagnostics check.
- **Dependabot** is a service GitHub hosts; `dependabot-core` is Ruby running one container per
  ecosystem. Its *data* is public, though: [OSV](https://osv.dev) aggregates RustSec, the GitHub
  Advisory Database and PyPA — the same sources. The Dependencies check queries it directly for
  Cargo, npm, PyPI and Go manifests and lockfiles, and reports the version that fixes each
  advisory. Secret scanning is covered by the Secrets check above, using our own patterns rather
  than GHAS's proprietary ones.

The Dependencies check is the only part of Cowork's verification that touches the network. It sends
package names and versions from the manifest that was just edited to `api.osv.dev`, and nothing
else. Turn it off if that is not acceptable.

## Credentials

API keys are stored in the operating system's credential store — Windows Credential Manager, the
macOS keychain, or the platform equivalent — under `cowork://<provider-id>`, never in
`settings.json`. Connect and disconnect a provider under **Cowork > Catalog > Providers**; a
provider with no key is not offered in the model selector, and neither are its models.

## Storage

Threads live in the existing `scoped_kv_store` table under the `cowork` namespace — no new SQLite
domain and therefore no new migration:

- `index` — `Vec<ThreadMetadata>`, what the panel lists and searches.
- `thread/<id>` — the full `Thread`, loaded only when opened.
- `catalog` — the cached models.dev response with its fetch timestamp.
- `providers_with_keys` — which providers are connected, so the panel does not hit the credential
  store on every frame.

## Actions and default bindings

| Action | Windows / Linux | macOS |
| --- | --- | --- |
| `cowork::ToggleFocus` | `ctrl-alt-a` | `cmd-ctrl-a` |
| `cowork::NewThread` | `ctrl-alt-n` | `cmd-ctrl-t` |
| `cowork::Submit` | `enter` (in the composer) | same |
| `editor::Newline` | `shift-enter` (in the composer) | same |

Also registered, unbound by default: `cowork::Cancel`, `cowork::SelectModel`,
`cowork::OpenSettings`, `cowork::RefreshCatalog`, and the panel's `SelectNextThread` /
`SelectPreviousThread` / `OpenSelectedThread` / `DeleteSelectedThread`.

## What still needs a human

1. **Run it.** Build, connect a provider, open the panel, send a prompt that makes the agent read
   and edit a file. Watch for: markdown re-parsing cost while a long response streams
   (`Markdown::append` reparses the whole source on every chunk), the scroll-to-bottom behaviour,
   whether the dock position survives a restart, and whether a tool call reads legibly in the
   transcript.
2. **Try Google.** The Gemini wire format is covered by tests but has never met the real endpoint.
3. **Try Biome.** In a project with `node_modules/.bin/biome`, confirm the fix lands in the buffer
   and that the diagnostics parsed out of its output are accurate.
4. **Build the permission broker**, then the `bash` tool behind it.

The 69 unit tests are pure logic and need no network: catalog parsing, SSE decoding for all three
wire formats, tool-call assembly, the secret scanner's precision and its false-positive guards,
manifest parsing for six ecosystems, advisory filtering against a version requirement, and Biome's
diagnostic output.
