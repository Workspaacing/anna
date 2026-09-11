# Cowork

Native AI threads for Wu, split the way the rest of the workspace is:

- **`CoworkPanel`** (dock panel, left by default) — session history, search, per-thread delete, and
  a provider roster in the footer. It is *not* the chat.
- **`CoworkThreadView`** (workspace item, center pane) — where a conversation actually happens:
  streamed markdown responses, the composer, and the per-thread model picker.
- **Models** can be hidden from the selector individually; the hidden set is persisted as
  `cowork.disabled_models`, so a model added to a provider later is available by default.
- **Settings** live in the settings window on its own **Cowork** page
  (`crates/settings_ui/src/page_data.rs`, `cowork_page`), reached from the panel's gear button via
  `wu::OpenSettingsPage`. The panel's provider roster is deliberately *not* there: it is runtime
  status, and the settings framework only renders declarative fields backed by JSON.

## Status

Compiles, lints and unit-tests clean. **It has never been run** — no one has opened the panel, sent a
prompt, or watched a response stream in. Every claim below about runtime behaviour is derived from
the code, not from observation.

Verified on 2026-09-11 (Rust 1.97.1, `x86_64-pc-windows-msvc`):

| Gate | Result |
| --- | --- |
| `cargo check -p cowork` | pass |
| `cargo test -p cowork` | 15/15 pass |
| `cargo clippy -p cowork --all-targets --all-features -- --deny warnings` | pass |
| `cargo test -p xtask` | pass — no forbidden crate edge |
| `cargo build -p wu` | pass — the wiring links |

What that does **not** cover: the panel rendering, dock persistence, the model picker, SSE streaming
against a real provider, and error paths reaching the UI. Those need a human at a running build.

## Models

Models come only from the [models.dev](https://models.dev) catalog (`catalog_url`, default
`https://models.dev/api.json`) — the same registry the Vercel AI SDK publishes. The catalog is
fetched on first use, cached in Wu's key-value store, and refreshed when it is older than 24 hours
or when the user asks for a refresh.

A provider's wire format is chosen from the AI SDK package the catalog names in its `npm` field:

| `npm` contains | Request/response shape | Endpoint |
| --- | --- | --- |
| `anthropic` | Anthropic Messages API, SSE | `{api}/v1/messages` |
| anything else | OpenAI Chat Completions, SSE | `{api}/v1/chat/completions` |

**Known limitation:** providers that are neither Anthropic nor OpenAI-compatible — Google's
`generativelanguage` endpoint is the main one — are attempted as OpenAI-compatible and will fail
with the provider's own error surfaced in the thread. Adding a third wire format is the natural
next step.

## Credentials

Cowork **never stores an API key**. It reads the environment variable that models.dev declares for
the provider (`env[0]`, e.g. `ANTHROPIC_API_KEY`), which is the same contract the AI SDK uses. The
panel's provider section shows which keys are visible to Wu; it is a status view, not a form.
Because the variable is read from Wu's own process environment, a key exported after Wu started
requires a restart.

## Storage

Threads live in the existing `scoped_kv_store` table under the `cowork` namespace — no new SQLite
domain and therefore no new migration:

- `index` — `Vec<ThreadMetadata>`, what the panel lists and searches.
- `thread/<id>` — the full `Thread`, loaded only when opened.
- `catalog` — the cached models.dev response with its fetch timestamp.

## Actions and default bindings

| Action | Windows / Linux | macOS |
| --- | --- | --- |
| `cowork::ToggleFocus` | `ctrl-alt-a` | `cmd-ctrl-a` |
| `cowork::NewThread` | `ctrl-alt-n` | `cmd-ctrl-t` |
| `cowork::Submit` | `enter` (in the composer) | same |
| `editor::Newline` | `shift-enter` (in the composer) | same |

Also registered, unbound by default: `cowork::Cancel`, `cowork::SelectModel`,
`cowork::OpenSettings`, `cowork::ToggleProviders`, `cowork::RefreshCatalog`, and the panel's
`SelectNextThread` / `SelectPreviousThread` / `OpenSelectedThread` / `DeleteSelectedThread`.

## What still needs a human

1. **Run it.** Build, export a provider key, open the panel, send a prompt. Watch for: markdown
   re-parsing cost while a long response streams (`Markdown::append` reparses the whole source on
   every chunk), the scroll-to-bottom behaviour, and whether the dock position survives a restart.
2. **Try a provider that is neither Anthropic nor OpenAI-compatible** and confirm the failure is
   legible rather than confusing. Google is the case that matters.
3. **Decide on scope.** Tool calls, file/context attachment, and reading the open buffer into the
   prompt are all absent by design. Adding any of them is a product decision.

The 15 unit tests are pure logic and need no network: catalog parsing (including unknown fields and
deprecated models), SSE decoding for both wire formats, the `[DONE]` sentinel, errors embedded mid
stream, endpoint construction, and thread metadata/summarization.
