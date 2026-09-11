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
    OpenAiCompatible,
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
    pub fn wire_api(&self) -> WireApi {
        match self.npm.as_deref() {
            Some(npm) if npm.contains("anthropic") => WireApi::Anthropic,
            _ => WireApi::OpenAiCompatible,
        }
    }

    /// The environment variable holding this provider's credential, as declared by the catalog.
    pub fn primary_env_var(&self) -> Option<&str> {
        self.env.first().map(String::as_str)
    }
}

impl Model {
    pub fn display_name(&self, key: &str) -> String {
        self.name
            .clone()
            .or_else(|| self.id.clone())
            .unwrap_or_else(|| key.to_owned())
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

    /// Every non-deprecated model in the catalog, sorted by `provider/model` so the picker and the
    /// settings UI agree on an order.
    pub fn entries(&self) -> Vec<CatalogEntry> {
        let mut entries = Vec::new();
        for (provider_key, provider) in &self.providers {
            for (model_key, model) in &provider.models {
                if model.is_deprecated() {
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
        let entries = catalog.entries();

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
            br#"{ "acme": { "models": { "m1": { "brand_new_field": 3 } } } }"#,
        )
        .expect("unknown fields should not fail the parse");

        let entries = catalog.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].model_ref.qualified(), "acme/m1");
        assert_eq!(entries[0].provider_name, "acme");
        assert_eq!(entries[0].model_name, "m1");
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
