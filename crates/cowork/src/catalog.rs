use anyhow::{Context as _, Result, bail};
use collections::HashMap;
use futures::AsyncReadExt as _;
use http_client::{AsyncBody, HttpClient, HttpRequestExt as _, Request};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};

const CATALOG_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_CATALOG_BYTES: usize = 16 * 1024 * 1024;

/// The wire format a provider speaks. models.dev describes providers by the AI SDK package that
/// drives them, which is the only signal available for picking a request/response shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireApi {
    Anthropic,
    /// Google's Gemini API: `generateContent`, roles of `user`/`model`, and `functionCall` /
    /// `functionResponse` parts instead of tool calls.
    Google,
    OpenAiCompatible,
}

/// The providers shown in the "Popular" section of the settings UI. Everything else in the
/// catalog is still listed, under "All providers".
///
/// models.dev carries 213 providers, most of them resellers and aggregators, and the catalog has
/// no popularity signal (model count is a poor proxy: the largest entry is an aggregator). So this
/// ordering is curated: first-party labs, the major clouds, and the gateways people reach for.
/// Every id here was checked against the live catalog.
/// How many of [`POPULAR_PROVIDERS`] the Providers page offers under "Popular".
///
/// The whole list orders the table; this shorter prefix is what a filter shows, because a filter
/// that still returns thirty rows has not filtered anything.
pub const POPULAR_FILTER_LENGTH: usize = 15;

pub const POPULAR_PROVIDERS: [&str; 30] = [
    "anthropic",
    "openai",
    "google",
    "github-copilot",
    "openrouter",
    "xai",
    "deepseek",
    "mistral",
    "groq",
    "amazon-bedrock",
    "azure",
    "google-vertex",
    "vercel",
    "togetherai",
    "fireworks-ai",
    "cerebras",
    "perplexity",
    "cohere",
    "deepinfra",
    "huggingface",
    "moonshotai",
    "zhipuai",
    "minimax",
    "alibaba",
    "nvidia",
    "llama",
    "baseten",
    "siliconflow",
    "lmstudio",
    "opencode",
];

/// Whether Cowork can actually speak to a provider. Wire formats beyond Anthropic's Messages API
/// and OpenAI-style chat completions are not implemented, so the UI says so instead of offering a
/// model that will fail on the first request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Supported,
    /// The provider needs a request signing scheme or request shape Cowork does not implement.
    Unsupported,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Catalog {
    #[serde(flatten)]
    pub providers: HashMap<String, Provider>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Provider {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub npm: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    #[serde(default)]
    pub doc: Option<String>,
    #[serde(default)]
    pub models: HashMap<String, Model>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Model {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub tool_call: bool,
    #[serde(default)]
    pub attachment: bool,
    /// What the model actually takes in and produces: `text`, `image`, `audio`, `video`, `pdf`.
    #[serde(default)]
    pub modalities: Option<Modalities>,
    #[serde(default)]
    pub temperature: bool,
    #[serde(default)]
    pub open_weights: bool,
    #[serde(default)]
    pub knowledge: Option<String>,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub last_updated: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub cost: Option<Cost>,
    #[serde(default)]
    pub limit: Option<Limit>,
    /// Overrides for a model that does not speak its provider's protocol.
    #[serde(default)]
    pub provider: Option<ModelProvider>,
}

/// What a single model overrides about how to reach it.
///
/// 305 models in the catalog carry one, and 114 of those contradict their provider's row on the
/// protocol — `agentrouter` is an OpenAI-compatible row serving Claude over Anthropic Messages,
/// `freemodel` is the reverse. Reading only the provider row sends every one of them in the wrong
/// format.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct ModelProvider {
    #[serde(default)]
    pub npm: Option<String>,
    #[serde(default)]
    pub api: Option<String>,
    /// Which OpenAI request shape: `completions` or `responses`.
    #[serde(default)]
    pub shape: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Modalities {
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub output: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct Cost {
    #[serde(default)]
    pub input: Option<f64>,
    #[serde(default)]
    pub output: Option<f64>,
    #[serde(default)]
    pub cache_read: Option<f64>,
    #[serde(default)]
    pub cache_write: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct Limit {
    #[serde(default)]
    pub context: Option<u64>,
    #[serde(default)]
    pub output: Option<u64>,
}

/// A fully qualified `provider_id/model_id` pair, the identity Cowork stores on a thread.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct ModelRef {
    pub provider_id: String,
    pub model_id: String,
}

impl ModelRef {
    pub fn parse(value: &str) -> Option<Self> {
        let (provider_id, model_id) = value.split_once('/')?;
        if provider_id.is_empty() || model_id.is_empty() {
            return None;
        }
        Some(Self {
            provider_id: provider_id.to_owned(),
            model_id: model_id.to_owned(),
        })
    }

    pub fn qualified(&self) -> String {
        format!("{}/{}", self.provider_id, self.model_id)
    }
}

impl Provider {
    pub fn display_name(&self, key: &str) -> String {
        self.name
            .clone()
            .or_else(|| self.id.clone())
            .unwrap_or_else(|| key.to_owned())
    }

    /// models.dev names the AI SDK package that drives each provider. Anthropic's package speaks
    /// the Messages API; every other package in the catalog is either OpenAI's own or an
    /// OpenAI-compatible shim, so that is the fallback.
    /// The protocol to speak to one of this provider's models.
    ///
    /// The model's own `npm` wins where it has one, because a provider row describes the common
    /// case and the override describes the exception.
    pub fn wire_api_for(&self, model: &Model) -> WireApi {
        match model.provider.as_ref().and_then(|over| over.npm.as_deref()) {
            Some(npm) => Self::wire_api_of(Some(npm)),
            None => self.wire_api(),
        }
    }

    /// Where to send a request for one of this provider's models.
    pub fn api_base_for(&self, model: &Model) -> Option<String> {
        model
            .provider
            .as_ref()
            .and_then(|over| over.api.clone())
            .or_else(|| self.api_base())
    }

    pub fn wire_api(&self) -> WireApi {
        Self::wire_api_of(self.npm.as_deref())
    }

    fn wire_api_of(npm: Option<&str>) -> WireApi {
        match npm {
            // The substring rather than the exact name, because the protocol can live in a
            // subpath: `@ai-sdk/google-vertex/anthropic` is Claude on Vertex and speaks Messages,
            // while `@ai-sdk/google-vertex` is Gemini. Normalising to the package root would route
            // the first one through the wrong format entirely.
            Some(npm) if npm.contains("anthropic") => WireApi::Anthropic,
            // Named after neither, and speaks Messages at a dedicated `/anthropic/v1` base. The
            // npm string is a good heuristic, not a contract, and this is where it breaks.
            Some("@ai-sdk/minimax") => WireApi::Anthropic,
            Some("@ai-sdk/google") => WireApi::Google,
            _ => WireApi::OpenAiCompatible,
        }
    }

    /// Where to send requests.
    ///
    /// The catalog leaves `api` empty for the providers whose AI SDK package hard-codes the
    /// endpoint, which is most of the big ones, so the well-known bases are supplied here.
    /// Where requests for this provider go.
    ///
    /// The catalog leaves `api` out for twenty-six providers, and it is not an omission: models.dev
    /// describes a provider by the AI SDK package that drives it, and a provider with a package of
    /// its own — `@ai-sdk/anthropic`, `@ai-sdk/openai`, `@ai-sdk/groq` — has no reason to state a
    /// base URL, because the package already knows it. Only the ones sharing
    /// `@ai-sdk/openai-compatible` have to say where they live.
    ///
    /// Reading the field alone therefore left Anthropic and OpenAI — the two wire formats this app
    /// implements natively — marked as providers it could not talk to. Every endpoint below was
    /// confirmed to answer: each returns 401 or 403 to an unauthenticated request, which is an
    /// endpoint asking for a key rather than an endpoint that is not there.
    pub fn api_base(&self) -> Option<String> {
        if let Some(api) = &self.api {
            return Some(api.clone());
        }

        const KNOWN: [(&str, &str); 12] = [
            ("@ai-sdk/anthropic", "https://api.anthropic.com/v1"),
            ("@ai-sdk/openai", "https://api.openai.com/v1"),
            (
                "@ai-sdk/google",
                "https://generativelanguage.googleapis.com/v1beta",
            ),
            ("@ai-sdk/xai", "https://api.x.ai/v1"),
            ("@ai-sdk/mistral", "https://api.mistral.ai/v1"),
            ("@ai-sdk/groq", "https://api.groq.com/openai/v1"),
            ("@ai-sdk/togetherai", "https://api.together.xyz/v1"),
            ("@ai-sdk/cerebras", "https://api.cerebras.ai/v1"),
            ("@ai-sdk/deepinfra", "https://api.deepinfra.com/v1/openai"),
            ("@ai-sdk/perplexity", "https://api.perplexity.ai"),
            ("@ai-sdk/cohere", "https://api.cohere.ai/compatibility/v1"),
            ("@ai-sdk/gateway", "https://ai-gateway.vercel.sh/v1"),
        ];

        let npm = self.npm.as_deref()?;
        KNOWN
            .iter()
            .find(|(package, _)| *package == npm)
            .map(|(_, base)| (*base).to_owned())
    }

    /// The environment variables that can hold this provider's credential, as declared by the
    /// catalog. Several entries usually mean alternatives rather than a set that must all be
    /// present (`google` accepts either `GOOGLE_API_KEY` or `GOOGLE_GENERATIVE_AI_API_KEY`), so a
    /// provider counts as connected when any one of them is set.
    pub fn env_vars(&self) -> &[String] {
        &self.env
    }

    pub fn primary_env_var(&self) -> Option<&str> {
        self.env.first().map(String::as_str)
    }

    /// Whether Cowork can reach this provider: it needs both a wire format Cowork implements and
    /// an endpoint to send to.
    pub fn support(&self) -> Support {
        // Vertex needs Google's service-account signing and Bedrock needs SigV4; neither is a
        // request shape, so neither is reachable by adding a wire format.
        const UNSUPPORTED_PACKAGES: [&str; 2] =
            ["@ai-sdk/google-vertex", "@ai-sdk/amazon-bedrock"];

        match self.npm.as_deref() {
            Some(npm) if UNSUPPORTED_PACKAGES.contains(&npm) => Support::Unsupported,
            // A provider with no endpoint cannot be called however well-formed the request is.
            _ if self.api_base().is_none() => Support::Unsupported,
            _ => Support::Supported,
        }
    }
}

impl Model {
    pub fn display_name(&self, key: &str) -> String {
        self.name
            .clone()
            .or_else(|| self.id.clone())
            .unwrap_or_else(|| key.to_owned())
    }

    /// Whether this model needs a request shape Cowork does not implement.
    ///
    /// `responses` is OpenAI's Responses API, a different request and response format from Chat
    /// Completions rather than a variation on it. 32 models ask for it. Hiding them is honest;
    /// offering them and sending Chat Completions would only fail at the first message.
    pub fn uses_unsupported_shape(&self) -> bool {
        self.provider
            .as_ref()
            .and_then(|over| over.shape.as_deref())
            .is_some_and(|shape| !shape.eq_ignore_ascii_case("completions"))
    }

    /// Whether an image may be sent to this model.
    ///
    /// Read from `modalities.input` rather than the `attachment` flag. The two disagree on 359
    /// models in the catalog, and `modalities` is the one that says what the model takes: some
    /// models flagged `attachment: false` accept images, and some flagged `true` are text-only.
    /// Offering an attachment the model will reject is worse than not offering it.
    pub fn accepts_images(&self) -> bool {
        self.modalities
            .as_ref()
            .is_some_and(|modalities| modalities.input.iter().any(|kind| kind == "image"))
    }

    pub fn is_deprecated(&self) -> bool {
        self.status
            .as_deref()
            .is_some_and(|status| status.eq_ignore_ascii_case("deprecated"))
    }
}

impl Catalog {
    pub fn provider(&self, provider_id: &str) -> Option<&Provider> {
        self.providers.get(provider_id)
    }

    pub fn model(&self, model: &ModelRef) -> Option<(&Provider, &Model)> {
        let provider = self.providers.get(&model.provider_id)?;
        let entry = provider.models.get(&model.model_id)?;
        Some((provider, entry))
    }

    /// Every non-deprecated model belonging to a connected provider, sorted by `provider/model` so
    /// the picker and the settings UI agree on an order. A model whose provider has no credential
    /// is never offered: picking it could only produce a failed request.
    pub fn entries(&self, is_connected: impl Fn(&str) -> bool) -> Vec<CatalogEntry> {
        let mut entries = Vec::new();
        for (provider_key, provider) in &self.providers {
            if !is_connected(provider_key) {
                continue;
            }
            // A provider with no endpoint of its own can still serve models that carry one.
            let provider_reachable = provider.support() == Support::Supported;
            for (model_key, model) in &provider.models {
                if model.is_deprecated() || model.uses_unsupported_shape() {
                    continue;
                }
                if !provider_reachable && provider.api_base_for(model).is_none() {
                    continue;
                }
                entries.push(CatalogEntry {
                    model_ref: ModelRef {
                        provider_id: provider_key.clone(),
                        model_id: model_key.clone(),
                    },
                    provider_name: provider.display_name(provider_key),
                    model_name: model.display_name(model_key),
                    env_var: provider.primary_env_var().map(str::to_owned),
                    reasoning: model.reasoning,
                    tool_call: model.tool_call,
                    context_limit: model.limit.and_then(|limit| limit.context),
                    output_limit: model.limit.and_then(|limit| limit.output),
                });
            }
        }
        entries.sort_by(|left, right| {
            left.provider_name
                .to_lowercase()
                .cmp(&right.provider_name.to_lowercase())
                .then_with(|| left.model_name.to_lowercase().cmp(&right.model_name.to_lowercase()))
        });
        entries
    }
}

#[derive(Clone, Debug)]
pub struct CatalogEntry {
    pub model_ref: ModelRef,
    pub provider_name: String,
    pub model_name: String,
    pub env_var: Option<String>,
    pub reasoning: bool,
    pub tool_call: bool,
    pub context_limit: Option<u64>,
    /// The most tokens this model will produce in one response, as models.dev declares it. Cowork
    /// asks for exactly this much: the point of a limit published per model is that there is no
    /// reason to guess a smaller one.
    pub output_limit: Option<u64>,
}

impl CatalogEntry {
    pub fn label(&self) -> String {
        format!("{} · {}", self.provider_name, self.model_name)
    }
}

pub async fn fetch(http: Arc<dyn HttpClient>, url: &str) -> Result<Catalog> {
    let request = Request::get(url)
        .header("Accept", "application/json")
        .timeout(CATALOG_REQUEST_TIMEOUT)
        .body(AsyncBody::empty())
        .context("building models.dev catalog request")?;

    let mut response = http
        .send(request)
        .await
        .context("fetching the models.dev catalog")?;

    let status = response.status();
    let mut body = Vec::new();
    response
        .body_mut()
        .take(MAX_CATALOG_BYTES as u64)
        .read_to_end(&mut body)
        .await
        .context("reading the models.dev catalog")?;

    if !status.is_success() {
        let text = String::from_utf8_lossy(&body);
        bail!("models.dev responded with {status}: {text}");
    }

    parse(&body)
}

pub fn parse(body: &[u8]) -> Result<Catalog> {
    let providers: HashMap<String, Provider> =
        serde_json::from_slice(body).context("parsing the models.dev catalog")?;
    Ok(Catalog { providers })
}

/// How long a cached catalog is served before Cowork refetches it in the background.
pub const CATALOG_STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_with_its_own_sdk_package_is_still_reachable() {
        // The bug this locks down: models.dev omits `api` for providers that have a dedicated AI
        // SDK package, because the package knows the endpoint. Reading only that field marked
        // Anthropic and OpenAI — the two formats this app speaks natively — as unreachable, and
        // the settings table showed a warning against both.
        for npm in [
            "@ai-sdk/anthropic",
            "@ai-sdk/openai",
            "@ai-sdk/google",
            "@ai-sdk/xai",
            "@ai-sdk/mistral",
            "@ai-sdk/groq",
            "@ai-sdk/togetherai",
            "@ai-sdk/cerebras",
            "@ai-sdk/deepinfra",
            "@ai-sdk/perplexity",
            "@ai-sdk/cohere",
            "@ai-sdk/gateway",
        ] {
            let provider: Provider =
                serde_json::from_str(&format!(r#"{{"npm":"{npm}"}}"#)).unwrap();

            let base = provider
                .api_base()
                .unwrap_or_else(|| panic!("{npm} has no endpoint"));
            assert!(base.starts_with("https://"), "{npm}: {base}");
            assert!(!base.ends_with('/'), "{npm}: a trailing slash doubles up: {base}");
            assert_eq!(provider.support(), Support::Supported, "{npm}");
        }
    }

    #[test]
    fn the_catalog_still_wins_over_the_built_in_endpoint() {
        // A provider that does state its own base — a self-hosted gateway, say — must be believed.
        let provider: Provider = serde_json::from_str(
            r#"{"npm":"@ai-sdk/openai","api":"https://gateway.internal/v1"}"#,
        )
        .unwrap();

        assert_eq!(
            provider.api_base().as_deref(),
            Some("https://gateway.internal/v1")
        );
    }

    #[test]
    fn the_two_that_need_signing_are_still_out_of_reach() {
        // Bedrock needs SigV4 and Vertex a service-account assertion. Neither is a request shape,
        // so neither becomes reachable by knowing a URL — guessing one would produce a provider
        // that looks connectable and fails on every call.
        for npm in ["@ai-sdk/amazon-bedrock", "@ai-sdk/google-vertex"] {
            let provider: Provider =
                serde_json::from_str(&format!(r#"{{"npm":"{npm}"}}"#)).unwrap();
            assert_eq!(provider.support(), Support::Unsupported, "{npm}");
        }
    }

    fn model_with_override(json: &str) -> Model {
        serde_json::from_str(json).expect("the fixture should parse")
    }

    fn provider_row(npm: &str, api: Option<&str>) -> Provider {
        let api = api
            .map(|api| format!(r#","api":"{api}""#))
            .unwrap_or_default();
        serde_json::from_str(&format!(r#"{{"npm":"{npm}"{api}}}"#)).expect("fixture")
    }

    #[test]
    fn image_support_is_read_from_what_the_model_takes_in() {
        let vision: Model =
            serde_json::from_str(r#"{"modalities":{"input":["text","image"],"output":["text"]}}"#)
                .unwrap();
        let text_only: Model =
            serde_json::from_str(r#"{"modalities":{"input":["text"],"output":["text"]}}"#).unwrap();

        assert!(vision.accepts_images());
        assert!(!text_only.accepts_images());
    }

    #[test]
    fn the_attachment_flag_is_not_what_decides_it() {
        // The two fields disagree on 359 models in the catalog. `modalities` says what the model
        // takes; `attachment` does not, and offering an attachment the model will reject is worse
        // than not offering it.
        let flagged_but_text_only: Model =
            serde_json::from_str(r#"{"attachment":true,"modalities":{"input":["text"]}}"#).unwrap();
        let unflagged_but_sees: Model =
            serde_json::from_str(r#"{"attachment":false,"modalities":{"input":["image"]}}"#)
                .unwrap();

        assert!(!flagged_but_text_only.accepts_images());
        assert!(unflagged_but_sees.accepts_images());
    }

    #[test]
    fn a_model_declaring_no_modalities_is_not_offered_attachments() {
        assert!(!Model::default().accepts_images());
    }

    #[test]
    fn a_model_may_speak_a_different_protocol_than_its_provider() {
        // `agentrouter` is an OpenAI-compatible row that serves Claude over Anthropic Messages.
        // Reading only the row sends 114 models in the catalog in the wrong format.
        let row = provider_row("@ai-sdk/openai-compatible", Some("https://example.com/v1"));
        let claude = model_with_override(r#"{"provider":{"npm":"@ai-sdk/anthropic"}}"#);

        assert_eq!(row.wire_api(), WireApi::OpenAiCompatible);
        assert_eq!(row.wire_api_for(&claude), WireApi::Anthropic);
    }

    #[test]
    fn a_model_without_an_override_follows_its_provider() {
        let row = provider_row("@ai-sdk/anthropic", Some("https://example.com/v1"));

        assert_eq!(row.wire_api_for(&Model::default()), WireApi::Anthropic);
    }

    #[test]
    fn a_model_may_live_at_its_own_endpoint() {
        let row = provider_row("@ai-sdk/openai-compatible", Some("https://row.example/v1"));
        let elsewhere = model_with_override(r#"{"provider":{"api":"https://model.example/v1"}}"#);

        assert_eq!(
            row.api_base_for(&elsewhere).as_deref(),
            Some("https://model.example/v1")
        );
        assert_eq!(
            row.api_base_for(&Model::default()).as_deref(),
            Some("https://row.example/v1")
        );
    }

    #[test]
    fn a_model_needing_the_responses_api_is_not_offered() {
        // Chat Completions is not a close-enough approximation; sending it would fail on the first
        // message, which is worse than the model not appearing.
        let responses = model_with_override(r#"{"provider":{"shape":"responses"}}"#);
        let completions = model_with_override(r#"{"provider":{"shape":"completions"}}"#);

        assert!(responses.uses_unsupported_shape());
        assert!(!completions.uses_unsupported_shape());
        assert!(!Model::default().uses_unsupported_shape());
    }

    #[test]
    fn minimax_speaks_anthropic_despite_its_name() {
        assert_eq!(
            provider_row("@ai-sdk/minimax", Some("https://api.minimax.io/anthropic/v1")).wire_api(),
            WireApi::Anthropic
        );
    }

    #[test]
    fn the_protocol_can_live_in_a_package_subpath() {
        // `@ai-sdk/google-vertex/anthropic` is Claude on Vertex. Normalising to the package root
        // would route it through Gemini's format.
        assert_eq!(
            provider_row("@ai-sdk/google-vertex/anthropic", Some("https://v/v1")).wire_api(),
            WireApi::Anthropic
        );
    }

    const SAMPLE: &str = r#"{
        "anthropic": {
            "id": "anthropic",
            "name": "Anthropic",
            "env": ["ANTHROPIC_API_KEY"],
            "npm": "@ai-sdk/anthropic",
            "api": "https://api.anthropic.com/v1",
            "models": {
                "claude-sonnet-4-5": {
                    "id": "claude-sonnet-4-5",
                    "name": "Claude Sonnet 4.5",
                    "reasoning": true,
                    "tool_call": true,
                    "limit": { "context": 200000, "output": 64000 }
                },
                "claude-2": {
                    "id": "claude-2",
                    "name": "Claude 2",
                    "status": "deprecated"
                }
            }
        },
        "openai": {
            "id": "openai",
            "name": "OpenAI",
            "env": ["OPENAI_API_KEY"],
            "npm": "@ai-sdk/openai",
            "api": "https://api.openai.com/v1",
            "models": {
                "gpt-5": { "id": "gpt-5", "name": "GPT-5", "tool_call": true }
            }
        }
    }"#;

    #[test]
    fn parses_catalog_and_skips_deprecated_models() {
        let catalog = parse(SAMPLE.as_bytes()).expect("sample catalog should parse");
        let entries = catalog.entries(|_| true);

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].label(), "Anthropic · Claude Sonnet 4.5");
        assert_eq!(entries[0].context_limit, Some(200000));
        assert_eq!(entries[0].env_var.as_deref(), Some("ANTHROPIC_API_KEY"));
        assert_eq!(entries[1].label(), "OpenAI · GPT-5");
    }

    #[test]
    fn selects_wire_api_from_the_ai_sdk_package() {
        let catalog = parse(SAMPLE.as_bytes()).expect("sample catalog should parse");

        assert_eq!(
            catalog.provider("anthropic").map(Provider::wire_api),
            Some(WireApi::Anthropic)
        );
        assert_eq!(
            catalog.provider("openai").map(Provider::wire_api),
            Some(WireApi::OpenAiCompatible)
        );
    }

    #[test]
    fn tolerates_unknown_fields_and_missing_optionals() {
        let catalog = parse(
            br#"{ "acme": { "api": "https://acme.test/v1", "models": { "m1": { "brand_new_field": 3 } } } }"#,
        )
        .expect("unknown fields should not fail the parse");

        let entries = catalog.entries(|_| true);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].model_ref.qualified(), "acme/m1");
        assert_eq!(entries[0].provider_name, "acme");
        assert_eq!(entries[0].model_name, "m1");
    }

    #[test]
    fn a_provider_with_no_endpoint_is_not_offered() {
        // The catalog leaves `api` empty for providers whose SDK hard-codes it. Without either an
        // `api` or a known default, no request can be formed, so the models must not be listed.
        let catalog = parse(br#"{ "mystery": { "models": { "m1": {} } } }"#)
            .expect("the catalog should still parse");

        assert!(catalog.entries(|_| true).is_empty());
        assert_eq!(
            catalog.provider("mystery").map(Provider::support),
            Some(Support::Unsupported)
        );
    }

    #[test]
    fn model_refs_round_trip_through_their_qualified_form() {
        let model_ref = ModelRef::parse("anthropic/claude-sonnet-4-5")
            .expect("a well formed reference should parse");

        assert_eq!(model_ref.provider_id, "anthropic");
        assert_eq!(model_ref.model_id, "claude-sonnet-4-5");
        assert_eq!(model_ref.qualified(), "anthropic/claude-sonnet-4-5");
        assert!(ModelRef::parse("no-slash").is_none());
        assert!(ModelRef::parse("/leading").is_none());
    }
}
