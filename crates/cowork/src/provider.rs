use crate::catalog::{Model, Provider, WireApi};
use anyhow::{Context as _, Result, anyhow, bail};
use collections::HashMap;
use futures::{
    AsyncBufReadExt as _, AsyncReadExt as _, Stream, StreamExt as _, io::BufReader,
    stream::BoxStream,
};
use http_client::{AsyncBody, HttpClient, Request};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

/// What to ask Anthropic for when the catalog declares no output limit for the model.
///
/// Anthropic's Messages API rejects a request without `max_tokens`, so unlike the other two formats
/// this one cannot simply leave the ceiling to the provider. Every Anthropic model in the catalog
/// does publish a limit, so this is a floor for an entry that is missing data, not a policy.
const ANTHROPIC_FALLBACK_MAX_TOKENS: u64 = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    /// Carries the results of the tool calls in the preceding assistant message. Both wire formats
    /// have this concept; they disagree only on how to spell it.
    Tool,
}

/// A tool call the model asked for.
///
/// `arguments` is the raw JSON text rather than a parsed `Value` because it arrives in fragments
/// during streaming and is only valid JSON once the call is complete. Parsing is the caller's job,
/// so a malformed call can be reported back to the model instead of breaking the turn.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
    /// The file the tool changed, so the transcript can highlight its diff as that language.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub path: String,
    /// A unified diff of what the tool changed, for the transcript.
    ///
    /// Never sent to a provider: each wire format builds its own request body from `content`, so
    /// this field simply is not read there. It is persisted with the thread, so reopening a
    /// conversation still shows what was changed.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub diff: String,
}

/// Fields added after the first release default, so threads stored by an earlier version still load.
/// An image sent along with a message.
///
/// Held in the format the user supplied rather than re-encoded: a PNG screenshot stays a PNG, a
/// photograph stays a JPEG. Re-encoding would cost quality for nothing, and every provider accepts
/// the common types directly.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attachment {
    /// The IANA media type, e.g. `image/png`. Taken from the file rather than guessed, because
    /// every provider trusts this field over the bytes.
    pub media_type: String,
    /// Standard base64, padded, no line breaks.
    pub data: String,
    /// The file's own name, for the transcript.
    pub name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<ToolResult>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Attachment>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            text: text.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            attachments: Vec::new(),
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            text: text.into(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
            attachments: Vec::new(),
        }
    }

    pub fn tool_results(results: Vec<ToolResult>) -> Self {
        Self {
            role: Role::Tool,
            text: String::new(),
            tool_calls: Vec::new(),
            tool_results: results,
            attachments: Vec::new(),
        }
    }
}

/// What the model may call. `parameters` is a JSON Schema object.
#[derive(Clone, Debug)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

pub struct CompletionRequest {
    pub provider_id: String,
    pub provider: Provider,
    /// The catalog entry for this model, which may override the provider's protocol or endpoint.
    pub model: Model,
    pub model_id: String,
    pub api_key: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    /// The model's published output ceiling, or `None` when models.dev declares none — in which
    /// case the provider's own default is left to stand rather than a number invented here.
    pub max_output_tokens: Option<u64>,
}

/// Why the model stopped. `ToolUse` is the one the turn loop acts on: it means the assistant message
/// is complete and is waiting on tool results before it can continue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionEvent {
    Text(String),
    /// The model's own reasoning, which is not part of its answer.
    ///
    /// Reasoning models stream this while `content` stays empty, which is why a turn can look
    /// frozen for several seconds before the first word appears. Shown as what the agent is
    /// working through, separately from what it decided.
    Reasoning(String),
    /// How many tokens the exchange cost, as the provider counts them.
    ///
    /// Reported at different moments by each format and, for some OpenAI-compatible providers, not
    /// at all unless asked for. Counting locally is not an alternative: every provider tokenizes
    /// differently, so an estimate would be wrong in a way the user could not see.
    Usage { input: u64, output: u64 },
    /// A tool call has begun. Its arguments arrive in later `ToolCallDelta` events.
    ToolCallStart { id: String, name: String },
    ToolCallDelta { id: String, arguments: String },
    Stop(StopReason),
}

/// Builds the chat endpoint for a provider. models.dev records some `api` bases with a version
/// segment already applied and some without, so the version is only added when it is missing.
fn endpoint(base: &str, suffix: &str) -> String {
    let base = base.trim_end_matches('/');
    if base.ends_with("/v1") || base.ends_with("/v1beta") {
        format!("{base}/{suffix}")
    } else {
        format!("{base}/v1/{suffix}")
    }
}

pub async fn stream_completion(
    http: Arc<dyn HttpClient>,
    request: CompletionRequest,
) -> Result<BoxStream<'static, Result<CompletionEvent>>> {
    if request.messages.is_empty() {
        bail!("cannot start a completion with no messages");
    }

    let api_base = request
        .provider
        .api_base_for(&request.model)
        .ok_or_else(|| anyhow!("no API endpoint is known for this model"))?;
    let wire_api = request.provider.wire_api_for(&request.model);

    let (url, body, http_request) = match wire_api {
        WireApi::Anthropic => {
            let url = endpoint(&api_base, "messages");
            let body = anthropic_body(&request);
            let http_request = Request::post(&url)
                .header("content-type", "application/json")
                .header("accept", "text/event-stream")
                .header("anthropic-version", "2023-06-01")
                .header("x-api-key", request.api_key.as_str());
            (url, body, http_request)
        }
        WireApi::Google => {
            // Gemini names the model in the path and takes the key in a header. `alt=sse` is what
            // turns `streamGenerateContent` into an event stream rather than a JSON array.
            let url = format!(
                "{}/models/{}:streamGenerateContent?alt=sse",
                api_base.trim_end_matches('/'),
                request.model_id
            );
            let body = google_body(&request);
            let http_request = Request::post(&url)
                .header("content-type", "application/json")
                .header("accept", "text/event-stream")
                .header("x-goog-api-key", request.api_key.as_str());
            (url, body, http_request)
        }
        WireApi::OpenAiCompatible => {
            let url = endpoint(&api_base, "chat/completions");
            let body = openai_body(&request);
            let http_request = Request::post(&url)
                .header("content-type", "application/json")
                .header("accept", "text/event-stream")
                .header("authorization", format!("Bearer {}", request.api_key));
            (url, body, http_request)
        }
    };

    let body = serde_json::to_vec(&body).context("serializing the completion request")?;
    let http_request = http_request
        .body(AsyncBody::from(body))
        .with_context(|| format!("building the completion request for {url}"))?;

    let mut response = http
        .send(http_request)
        .await
        .with_context(|| format!("sending the completion request to {url}"))?;

    let status = response.status();
    if !status.is_success() {
        let mut body = Vec::new();
        response
            .body_mut()
            .read_to_end(&mut body)
            .await
            .context("reading the provider's error response")?;
        bail!(
            "{} returned {status}: {}",
            request.provider.display_name(&request.provider_id),
            describe_error(&body)
        );
    }

    Ok(decode_sse(response.into_body(), wire_api).boxed())
}

/// Asks a provider which models it actually serves right now.
///
/// The models.dev catalog is a community-maintained snapshot and drifts from reality: OpenCode Zen
/// lists 102 models where the endpoint serves 70, and of its 31 zero-cost entries only 8 exist. A
/// model offered in the picker that answers 401 is worse than one that was never offered.
///
/// `GET {base}/models` is the OpenAI convention and is what every OpenAI-compatible provider
/// implements. Anthropic and Google serve the same path with the same envelope, so one request
/// covers all three wire formats. The key is sent when there is one; several providers answer
/// without it.
pub async fn list_models(
    http: Arc<dyn HttpClient>,
    provider_id: &str,
    provider: &Provider,
    api_key: Option<&str>,
) -> Result<Vec<String>> {
    let api_base = provider
        .api_base()
        .ok_or_else(|| anyhow!("no API endpoint is known for this provider"))?;
    let url = endpoint(&api_base, "models");

    let mut request = Request::get(&url).header("accept", "application/json");
    if let Some(api_key) = api_key {
        request = match provider.wire_api() {
            WireApi::Anthropic => request
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01"),
            WireApi::Google => request.header("x-goog-api-key", api_key),
            WireApi::OpenAiCompatible => request.header("authorization", format!("Bearer {api_key}")),
        };
    }

    let request = request
        .body(AsyncBody::empty())
        .with_context(|| format!("building the model list request for {url}"))?;

    let mut response = http
        .send(request)
        .await
        .with_context(|| format!("asking {provider_id} for its model list"))?;

    let mut body = Vec::new();
    response
        .body_mut()
        .read_to_end(&mut body)
        .await
        .context("reading the model list")?;

    let status = response.status();
    anyhow::ensure!(
        status.is_success(),
        "{provider_id} returned {status}: {}",
        describe_error(&body)
    );

    let payload: Value = serde_json::from_slice(&body).context("parsing the model list")?;
    Ok(model_ids(&payload))
}

/// Reads ids out of whichever envelope the provider used.
///
/// OpenAI and Anthropic both answer `{"data": [{"id": ...}]}`; Google answers
/// `{"models": [{"name": "models/gemini-..."}]}`. Anything unrecognised yields nothing, which the
/// caller treats as "no opinion" rather than "no models".
fn model_ids(payload: &Value) -> Vec<String> {
    if let Some(entries) = payload.get("data").and_then(Value::as_array) {
        return entries
            .iter()
            .filter_map(|entry| entry.get("id")?.as_str())
            .map(str::to_owned)
            .collect();
    }

    if let Some(entries) = payload.get("models").and_then(Value::as_array) {
        return entries
            .iter()
            .filter_map(|entry| entry.get("name").or_else(|| entry.get("id"))?.as_str())
            .map(|name| name.trim_start_matches("models/").to_owned())
            .collect();
    }

    Vec::new()
}

/// Providers report failures in several shapes; pull out the human-readable message when one of the
/// common ones matches, and fall back to the raw body so nothing is swallowed.
fn describe_error(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return text.into_owned();
    };

    let message = value
        .pointer("/error/message")
        .or_else(|| value.pointer("/error"))
        .or_else(|| value.pointer("/message"));

    match message {
        Some(Value::String(message)) => message.clone(),
        Some(other) => other.to_string(),
        None => text.into_owned(),
    }
}

fn anthropic_tools(request: &CompletionRequest) -> Vec<Value> {
    request
        .tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.parameters,
            })
        })
        .collect()
}

/// A user turn's blocks: the pictures first, then the words.
///
/// Anthropic documents that an image placed *before* the text referring to it gives better results
/// than the other order. Neither of the other two formats cares, so all three put images first —
/// one rule about how a turn is shaped is easier to keep right than three.
fn anthropic_user_content(message: &Message) -> Vec<Value> {
    // With more than one picture each is introduced by name, which is what Anthropic's own
    // guidance asks for: it gives the model something to refer to, so "the second one" in a
    // follow-up means something. A single image needs no label and reads better without one.
    let label_them = message.attachments.len() > 1;

    let mut content = Vec::new();
    for (index, attachment) in message.attachments.iter().enumerate() {
        if label_them {
            content.push(json!({ "type": "text", "text": format!("Image {}:", index + 1) }));
        }
        content.push(json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": attachment.media_type,
                "data": attachment.data,
            },
        }));
    }

    // An empty text block is rejected outright, so it is only included when there is something in
    // it — unless it is all there is, which leaves a message with no attachments shaped exactly as
    // it was before images existed.
    if !message.text.is_empty() || content.is_empty() {
        content.push(json!({ "type": "text", "text": message.text }));
    }
    content
}

fn anthropic_body(request: &CompletionRequest) -> Value {
    let mut messages = Vec::new();
    for message in &request.messages {
        match message.role {
            Role::User => messages.push(json!({
                "role": "user",
                "content": anthropic_user_content(message),
            })),
            Role::Assistant => {
                let mut content = Vec::new();
                if !message.text.is_empty() {
                    content.push(json!({ "type": "text", "text": message.text }));
                }
                for call in &message.tool_calls {
                    content.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": parse_arguments(&call.arguments),
                    }));
                }
                messages.push(json!({ "role": "assistant", "content": content }));
            }
            // Anthropic carries tool results on a *user* message, not a role of their own.
            Role::Tool => {
                let content = message
                    .tool_results
                    .iter()
                    .map(|result| {
                        json!({
                            "type": "tool_result",
                            "tool_use_id": result.call_id,
                            "content": result.content,
                            "is_error": result.is_error,
                        })
                    })
                    .collect::<Vec<_>>();
                messages.push(json!({ "role": "user", "content": content }));
            }
        }
    }

    let mut body = json!({
        "model": request.model_id,
        // Anthropic requires this field, so it is the one format that cannot simply omit it.
        "max_tokens": request.max_output_tokens.unwrap_or(ANTHROPIC_FALLBACK_MAX_TOKENS),
        "stream": true,
        "messages": messages,
    });
    if let Some(system) = &request.system {
        body["system"] = json!(system);
    }
    if !request.tools.is_empty() {
        body["tools"] = json!(anthropic_tools(request));
    }
    body
}

/// A user turn's parts, with any images inlined ahead of the text.
///
/// The field names are camelCase to match the rest of this encoder. Gemini is a protobuf service
/// and its JSON mapping accepts either spelling, which the existing `functionCall` already relies
/// on — `inline_data` would work equally well, and consistency is the only thing deciding it.
fn google_user_parts(message: &Message) -> Vec<Value> {
    let mut parts = message
        .attachments
        .iter()
        .map(|attachment| {
            json!({
                "inlineData": {
                    "mimeType": attachment.media_type,
                    "data": attachment.data,
                },
            })
        })
        .collect::<Vec<_>>();

    if !message.text.is_empty() || parts.is_empty() {
        parts.push(json!({ "text": message.text }));
    }
    parts
}

/// Gemini differs from both other formats in three ways that matter: the assistant role is
/// called `model`, tool calls and their results are *parts* of a message rather than a field on
/// it, and a result is matched to its call by function name rather than by an id.
fn google_body(request: &CompletionRequest) -> Value {
    let mut contents = Vec::new();
    for message in &request.messages {
        match message.role {
            Role::User => contents.push(json!({
                "role": "user",
                "parts": google_user_parts(message),
            })),
            Role::Assistant => {
                let mut parts = Vec::new();
                if !message.text.is_empty() {
                    parts.push(json!({ "text": message.text }));
                }
                for call in &message.tool_calls {
                    parts.push(json!({
                        "functionCall": {
                            "name": call.name,
                            "args": parse_arguments(&call.arguments),
                        },
                    }));
                }
                if parts.is_empty() {
                    continue;
                }
                contents.push(json!({ "role": "model", "parts": parts }));
            }
            Role::Tool => {
                let parts = message
                    .tool_results
                    .iter()
                    .map(|result| {
                        json!({
                            "functionResponse": {
                                // The call id is the function name for this format; see
                                // `decode_google_chunk`.
                                "name": result.call_id,
                                "response": { "output": result.content },
                            },
                        })
                    })
                    .collect::<Vec<_>>();
                contents.push(json!({ "role": "user", "parts": parts }));
            }
        }
    }

    let mut body = json!({ "contents": contents });
    // Gemini treats the field as optional, so leaving it out means "whatever this model can do".
    if let Some(limit) = request.max_output_tokens {
        body["generationConfig"] = json!({ "maxOutputTokens": limit });
    }
    if let Some(system) = &request.system {
        body["systemInstruction"] = json!({ "parts": [{ "text": system }] });
    }
    if !request.tools.is_empty() {
        body["tools"] = json!([{
            "functionDeclarations": request
                .tools
                .iter()
                .map(|tool| json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.parameters,
                }))
                .collect::<Vec<_>>(),
        }]);
    }
    body
}

/// A user turn's content, which stays a plain string until there is a picture in it.
///
/// The array form is the one that carries images, and it is also the form the long tail of
/// OpenAI-compatible servers in the catalog is least likely to have implemented. Sending a message
/// with no attachments as a bare string, exactly as before, means adding image support cannot
/// break a provider that never sees an image.
fn openai_user_content(message: &Message) -> Value {
    if message.attachments.is_empty() {
        return json!(message.text);
    }

    let mut parts = message
        .attachments
        .iter()
        .map(|attachment| {
            json!({
                "type": "image_url",
                // A data URL, not a link: the bytes travel with the request. `image_url` is an
                // object even though it holds a single field, which is the shape servers check.
                "image_url": {
                    "url": format!("data:{};base64,{}", attachment.media_type, attachment.data),
                },
            })
        })
        .collect::<Vec<_>>();

    if !message.text.is_empty() {
        parts.push(json!({ "type": "text", "text": message.text }));
    }
    json!(parts)
}

fn openai_body(request: &CompletionRequest) -> Value {
    let mut messages = Vec::new();
    if let Some(system) = &request.system {
        messages.push(json!({ "role": "system", "content": system }));
    }

    for message in &request.messages {
        match message.role {
            Role::User => messages.push(json!({
                "role": "user",
                "content": openai_user_content(message),
            })),
            Role::Assistant => {
                let mut entry = json!({ "role": "assistant", "content": message.text });
                if !message.tool_calls.is_empty() {
                    entry["tool_calls"] = json!(
                        message
                            .tool_calls
                            .iter()
                            .map(|call| json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    "arguments": call.arguments,
                                },
                            }))
                            .collect::<Vec<_>>()
                    );
                }
                messages.push(entry);
            }
            // OpenAI wants one message per result, each naming the call it answers.
            Role::Tool => {
                for result in &message.tool_results {
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": result.call_id,
                        "content": result.content,
                    }));
                }
            }
        }
    }

    let mut body = json!({
        "model": request.model_id,
        "stream": true,
        // Without this most OpenAI-compatible providers stream no usage at all, and the context
        // meter would sit empty for the whole conversation.
        "stream_options": { "include_usage": true },
        "messages": messages,
    });
    // Optional here too, and an omitted ceiling is the model's own.
    if let Some(limit) = request.max_output_tokens {
        body["max_completion_tokens"] = json!(limit);
    }
    if !request.tools.is_empty() {
        body["tools"] = json!(
            request
                .tools
                .iter()
                .map(|tool| json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    },
                }))
                .collect::<Vec<_>>()
        );
    }
    body
}

/// Tool arguments are echoed back to the provider as a JSON value. A call whose arguments never
/// parsed is sent as an empty object rather than dropped, so the conversation stays well-formed and
/// the model sees its own malformed call in the transcript.
fn parse_arguments(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| json!({}))
}

/// Streaming state that spans chunks.
///
/// OpenAI identifies a tool call by its position in an array and sends the id and name only on the
/// first fragment, so the index has to be remembered to attribute later argument deltas.
#[derive(Default)]
struct SseState {
    openai_tool_ids: HashMap<u64, String>,
    /// The stream is over: `[DONE]` arrived, or it failed.
    stopped: bool,
    /// A stop reason has already been reported, so `[DONE]` must not report a second one.
    reported_stop: bool,
}

fn decode_sse(
    body: AsyncBody,
    wire_api: WireApi,
) -> impl Stream<Item = Result<CompletionEvent>> + Send {
    let lines = BufReader::new(body).lines();

    futures::stream::unfold(
        (lines, wire_api, SseState::default(), Vec::new()),
        |(mut lines, wire_api, mut state, mut queued): (_, _, SseState, Vec<CompletionEvent>)| async move {
            loop {
                if let Some(event) = (!queued.is_empty()).then(|| queued.remove(0)) {
                    return Some((Ok(event), (lines, wire_api, state, queued)));
                }
                if state.stopped {
                    return None;
                }

                let line = match lines.next().await {
                    Some(Ok(line)) => line,
                    Some(Err(error)) => {
                        state.stopped = true;
                        return Some((
                            Err(anyhow!(error).context("reading the provider's event stream")),
                            (lines, wire_api, state, queued),
                        ));
                    }
                    None => return None,
                };

                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() {
                    continue;
                }
                if data == "[DONE]" {
                    state.stopped = true;
                    // Only when the provider has not already said why it stopped. Most send a
                    // `finish_reason` chunk first, and reporting the end twice would leave the
                    // turn loop weighing two answers to one question.
                    if state.reported_stop {
                        return None;
                    }
                    return Some((
                        Ok(CompletionEvent::Stop(StopReason::EndTurn)),
                        (lines, wire_api, state, queued),
                    ));
                }

                let chunk: Value = match serde_json::from_str(data) {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        state.stopped = true;
                        return Some((
                            Err(anyhow!(error)
                                .context("parsing a chunk of the provider's event stream")),
                            (lines, wire_api, state, queued),
                        ));
                    }
                };

                match decode_chunk(&chunk, wire_api, &mut state) {
                    Ok(events) if events.is_empty() => continue,
                    Ok(events) => {
                        queued = events;
                        let event = queued.remove(0);
                        // A stop reason is not the end of the stream, and treating it as one cost
                        // us the token counts: OpenAI-compatible providers send `finish_reason`
                        // in one chunk and `usage` in the next. The stream ends at `[DONE]`, or
                        // when the connection closes — which is what every one of the three
                        // formats actually promises.
                        if matches!(event, CompletionEvent::Stop(_)) {
                            state.reported_stop = true;
                        }
                        return Some((Ok(event), (lines, wire_api, state, queued)));
                    }
                    Err(error) => {
                        state.stopped = true;
                        return Some((Err(error), (lines, wire_api, state, queued)));
                    }
                }
            }
        },
    )
}

fn stop_reason(raw: Option<&str>) -> StopReason {
    match raw {
        Some("end_turn") | Some("stop") => StopReason::EndTurn,
        Some("tool_use") | Some("tool_calls") => StopReason::ToolUse,
        Some("max_tokens") | Some("length") => StopReason::MaxTokens,
        _ => StopReason::Other,
    }
}

fn decode_chunk(
    chunk: &Value,
    wire_api: WireApi,
    state: &mut SseState,
) -> Result<Vec<CompletionEvent>> {
    match wire_api {
        WireApi::Anthropic => decode_anthropic_chunk(chunk),
        WireApi::Google => decode_google_chunk(chunk),
        WireApi::OpenAiCompatible => decode_openai_chunk(chunk, state),
    }
}

/// Gemini sends whole parts rather than fragments, so a function call arrives complete: one
/// `ToolCallStart` immediately followed by its full arguments.
///
/// There is no call id in the response, and a result is matched back by function name, so the name
/// serves as the id. Two calls to the same tool in one turn therefore share an id; they stay
/// distinguishable because Gemini pairs responses by position within the parts array.
fn decode_google_chunk(chunk: &Value) -> Result<Vec<CompletionEvent>> {
    if let Some(message) = chunk.pointer("/error/message").and_then(Value::as_str) {
        bail!("{message}");
    }

    let mut events = Vec::new();
    if let Some(usage) = chunk.get("usageMetadata") {
        events.push(CompletionEvent::Usage {
            input: usage
                .get("promptTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output: usage
                .get("candidatesTokenCount")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        });
    }

    let Some(candidate) = chunk.pointer("/candidates/0") else {
        return Ok(events);
    };
    if let Some(parts) = candidate.pointer("/content/parts").and_then(Value::as_array) {
        for part in parts {
            if let Some(text) = part.get("text").and_then(Value::as_str)
                && !text.is_empty()
            {
                events.push(CompletionEvent::Text(text.to_owned()));
            }
            if let Some(call) = part.get("functionCall")
                && let Some(name) = call.get("name").and_then(Value::as_str)
            {
                events.push(CompletionEvent::ToolCallStart {
                    id: name.to_owned(),
                    name: name.to_owned(),
                });
                let arguments = call.get("args").cloned().unwrap_or_else(|| json!({}));
                events.push(CompletionEvent::ToolCallDelta {
                    id: name.to_owned(),
                    arguments: arguments.to_string(),
                });
            }
        }
    }

    if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
        events.push(CompletionEvent::Stop(google_stop_reason(reason, &events)));
    }

    Ok(events)
}

/// Gemini reports `STOP` even when the turn ends in a function call, so the reason has to be
/// inferred from what the chunk actually contained.
fn google_stop_reason(reason: &str, events: &[CompletionEvent]) -> StopReason {
    let asked_for_tools = events
        .iter()
        .any(|event| matches!(event, CompletionEvent::ToolCallStart { .. }));
    match reason {
        _ if asked_for_tools => StopReason::ToolUse,
        "STOP" => StopReason::EndTurn,
        "MAX_TOKENS" => StopReason::MaxTokens,
        _ => StopReason::Other,
    }
}

fn decode_anthropic_chunk(chunk: &Value) -> Result<Vec<CompletionEvent>> {
    match chunk.get("type").and_then(Value::as_str) {
        Some("content_block_start") => {
            let block = chunk.get("content_block");
            if block.and_then(|block| block.get("type")).and_then(Value::as_str) != Some("tool_use")
            {
                return Ok(Vec::new());
            }
            let (Some(id), Some(name)) = (
                block.and_then(|block| block.get("id")).and_then(Value::as_str),
                block
                    .and_then(|block| block.get("name"))
                    .and_then(Value::as_str),
            ) else {
                return Ok(Vec::new());
            };
            Ok(vec![CompletionEvent::ToolCallStart {
                id: id.to_owned(),
                name: name.to_owned(),
            }])
        }
        Some("content_block_delta") => {
            let delta = chunk.get("delta");
            match delta.and_then(|delta| delta.get("type")).and_then(Value::as_str) {
                Some("thinking_delta") => {
                    let thinking = delta
                        .and_then(|delta| delta.get("thinking"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if thinking.is_empty() {
                        Ok(Vec::new())
                    } else {
                        Ok(vec![CompletionEvent::Reasoning(thinking.to_owned())])
                    }
                }
                Some("text_delta") => {
                    let text = delta
                        .and_then(|delta| delta.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if text.is_empty() {
                        Ok(Vec::new())
                    } else {
                        Ok(vec![CompletionEvent::Text(text.to_owned())])
                    }
                }
                Some("input_json_delta") => {
                    let arguments = delta
                        .and_then(|delta| delta.get("partial_json"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if arguments.is_empty() {
                        Ok(Vec::new())
                    } else {
                        // Anthropic scopes deltas to the open block rather than naming the call, so
                        // the loop attributes them to the most recent `ToolCallStart`.
                        Ok(vec![CompletionEvent::ToolCallDelta {
                            id: String::new(),
                            arguments: arguments.to_owned(),
                        }])
                    }
                }
                _ => Ok(Vec::new()),
            }
        }
        Some("message_delta") => {
            let mut events = Vec::new();
            if let Some(usage) = chunk.get("usage") {
                events.push(anthropic_usage(usage));
            }
            if let Some(reason) = chunk.pointer("/delta/stop_reason").and_then(Value::as_str) {
                events.push(CompletionEvent::Stop(stop_reason(Some(reason))));
            }
            Ok(events)
        }
        Some("message_start") => match chunk.pointer("/message/usage") {
            Some(usage) => Ok(vec![anthropic_usage(usage)]),
            None => Ok(Vec::new()),
        },
        Some("message_stop") => Ok(vec![CompletionEvent::Stop(StopReason::EndTurn)]),
        Some("error") => {
            let message = chunk
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("the provider reported an error");
            bail!("{message}")
        }
        _ => Ok(Vec::new()),
    }
}

/// Anthropic reports the two counts separately and repeats the input on every delta.
fn anthropic_usage(usage: &Value) -> CompletionEvent {
    CompletionEvent::Usage {
        input: usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

fn decode_openai_chunk(chunk: &Value, state: &mut SseState) -> Result<Vec<CompletionEvent>> {
    if let Some(message) = chunk.pointer("/error/message").and_then(Value::as_str) {
        bail!("{message}");
    }

    // The usage chunk carries an empty `choices`, so it has to be read before bailing on one.
    let mut events = Vec::new();
    if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
        events.push(CompletionEvent::Usage {
            input: usage
                .get("prompt_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            output: usage
                .get("completion_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        });
    }

    let Some(choice) = chunk.pointer("/choices/0") else {
        return Ok(events);
    };

    if let Some(text) = choice.pointer("/delta/reasoning").and_then(Value::as_str)
        && !text.is_empty()
    {
        events.push(CompletionEvent::Reasoning(text.to_owned()));
    }

    if let Some(text) = choice.pointer("/delta/content").and_then(Value::as_str)
        && !text.is_empty()
    {
        events.push(CompletionEvent::Text(text.to_owned()));
    }

    if let Some(calls) = choice
        .pointer("/delta/tool_calls")
        .and_then(Value::as_array)
    {
        for call in calls {
            let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
            if let Some(id) = call.get("id").and_then(Value::as_str) {
                state.openai_tool_ids.insert(index, id.to_owned());
            }
            let id = state
                .openai_tool_ids
                .get(&index)
                .cloned()
                .unwrap_or_default();

            if let Some(name) = call.pointer("/function/name").and_then(Value::as_str)
                && !name.is_empty()
            {
                events.push(CompletionEvent::ToolCallStart {
                    id: id.clone(),
                    name: name.to_owned(),
                });
            }
            if let Some(arguments) = call.pointer("/function/arguments").and_then(Value::as_str)
                && !arguments.is_empty()
            {
                events.push(CompletionEvent::ToolCallDelta {
                    id,
                    arguments: arguments.to_owned(),
                });
            }
        }
    }

    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        events.push(CompletionEvent::Stop(stop_reason(Some(reason))));
    }

    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(npm: &str, max_output_tokens: Option<u64>) -> CompletionRequest {
        CompletionRequest {
            provider_id: "p".into(),
            provider: serde_json::from_str(&format!(r#"{{"npm":"{npm}"}}"#)).unwrap(),
            model: Model::default(),
            model_id: "m".into(),
            api_key: "k".into(),
            system: None,
            messages: vec![Message::user("hi")],
            tools: Vec::new(),
            max_output_tokens,
        }
    }

    #[test]
    fn a_published_output_limit_is_asked_for_in_full() {
        // Whatever the model says it can produce is what we ask for; there is no reason to cap a
        // response below the ceiling the provider itself publishes.
        assert_eq!(anthropic_body(&request("@ai-sdk/anthropic", Some(64000)))["max_tokens"], 64000);
        assert_eq!(
            openai_body(&request("@ai-sdk/openai", Some(32768)))["max_completion_tokens"],
            32768
        );
        assert_eq!(
            google_body(&request("@ai-sdk/google", Some(8192)))["generationConfig"]
                ["maxOutputTokens"],
            8192
        );
    }

    #[test]
    fn with_no_published_limit_the_provider_decides() {
        // Omitting the field asks for the provider's own default, which is a better guess than any
        // number invented here.
        let openai = openai_body(&request("@ai-sdk/openai", None));
        assert!(openai.get("max_completion_tokens").is_none(), "got: {openai}");

        let google = google_body(&request("@ai-sdk/google", None));
        assert!(google.get("generationConfig").is_none(), "got: {google}");
    }

    #[test]
    fn anthropic_always_sends_a_ceiling_because_it_rejects_a_request_without_one() {
        let body = anthropic_body(&request("@ai-sdk/anthropic", None));

        assert_eq!(body["max_tokens"], ANTHROPIC_FALLBACK_MAX_TOKENS);
    }

    fn collect(body: &'static str, wire_api: WireApi) -> Vec<CompletionEvent> {
        futures::executor::block_on(async {
            decode_sse(AsyncBody::from(body), wire_api)
                .boxed()
                .map(|event| event.expect("the sample stream should decode"))
                .collect::<Vec<_>>()
                .await
        })
    }

    #[test]
    fn versioned_api_bases_are_not_versioned_twice() {
        assert_eq!(
            endpoint("https://api.anthropic.com/v1", "messages"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            endpoint("https://api.anthropic.com/v1/", "messages"),
            "https://api.anthropic.com/v1/messages"
        );
        assert_eq!(
            endpoint("https://example.test", "chat/completions"),
            "https://example.test/v1/chat/completions"
        );
    }

    #[test]
    fn decodes_an_anthropic_stream() {
        let events = collect(
            concat!(
                "event: content_block_delta\n",
                "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n",
                "\n",
                "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n",
                "data: {\"type\":\"ping\"}\n",
                "data: {\"type\":\"message_stop\"}\n",
            ),
            WireApi::Anthropic,
        );

        assert_eq!(
            events,
            vec![
                CompletionEvent::Text("Hel".into()),
                CompletionEvent::Text("lo".into()),
                CompletionEvent::Stop(StopReason::EndTurn),
            ]
        );
    }

    #[test]
    fn decodes_an_anthropic_tool_call() {
        let events = collect(
            concat!(
                "data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"read\"}}\n",
                "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n",
                "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"a.rs\\\"}\"}}\n",
                "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n",
            ),
            WireApi::Anthropic,
        );

        assert_eq!(
            events,
            vec![
                CompletionEvent::ToolCallStart {
                    id: "toolu_1".into(),
                    name: "read".into()
                },
                CompletionEvent::ToolCallDelta {
                    id: String::new(),
                    arguments: "{\"path\":".into()
                },
                CompletionEvent::ToolCallDelta {
                    id: String::new(),
                    arguments: "\"a.rs\"}".into()
                },
                CompletionEvent::Stop(StopReason::ToolUse),
            ]
        );
    }

    #[test]
    fn decodes_an_openai_stream() {
        let events = collect(
            concat!(
                "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n",
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n",
                "data: [DONE]\n",
            ),
            WireApi::OpenAiCompatible,
        );

        assert_eq!(
            events,
            vec![
                CompletionEvent::Text("Hi".into()),
                CompletionEvent::Stop(StopReason::EndTurn)
            ]
        );
    }

    #[test]
    fn openai_tool_calls_keep_their_id_across_fragments() {
        // The id and name arrive once, on the first fragment; later fragments carry only the index.
        let events = collect(
            concat!(
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"grep\",\"arguments\":\"\"}}]}}]}\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"q\\\":\"}}]}}]}\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"fn\\\"}\"}}]}}]}\n",
                "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n",
            ),
            WireApi::OpenAiCompatible,
        );

        assert_eq!(
            events,
            vec![
                CompletionEvent::ToolCallStart {
                    id: "call_1".into(),
                    name: "grep".into()
                },
                CompletionEvent::ToolCallDelta {
                    id: "call_1".into(),
                    arguments: "{\"q\":".into()
                },
                CompletionEvent::ToolCallDelta {
                    id: "call_1".into(),
                    arguments: "\"fn\"}".into()
                },
                CompletionEvent::Stop(StopReason::ToolUse),
            ]
        );
    }

    #[test]
    fn decodes_a_google_stream() {
        let events = collect(
            concat!(
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hel\"}],\"role\":\"model\"}}]}\n",
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"lo\"}]},\"finishReason\":\"STOP\"}]}\n",
            ),
            WireApi::Google,
        );

        assert_eq!(
            events,
            vec![
                CompletionEvent::Text("Hel".into()),
                CompletionEvent::Text("lo".into()),
                CompletionEvent::Stop(StopReason::EndTurn),
            ]
        );
    }

    #[test]
    fn a_google_function_call_arrives_whole_and_reports_tool_use() {
        // Gemini sends complete parts, and reports `STOP` even when it wants a tool — the reason
        // has to come from the content.
        let events = collect(
            concat!(
                "data: {\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"read\",",
                "\"args\":{\"path\":\"a.rs\"}}}]},\"finishReason\":\"STOP\"}]}\n",
            ),
            WireApi::Google,
        );

        assert_eq!(
            events,
            vec![
                CompletionEvent::ToolCallStart {
                    id: "read".into(),
                    name: "read".into()
                },
                CompletionEvent::ToolCallDelta {
                    id: "read".into(),
                    arguments: "{\"path\":\"a.rs\"}".into()
                },
                CompletionEvent::Stop(StopReason::ToolUse),
            ]
        );
    }

    #[test]
    fn google_renames_the_assistant_role_and_nests_tool_traffic_in_parts() {
        let request = CompletionRequest {
            provider_id: "google".into(),
            provider: serde_json::from_str(r#"{"npm":"@ai-sdk/google"}"#).unwrap(),
            model: Model::default(),
            model_id: "gemini".into(),
            api_key: "k".into(),
            system: Some("be brief".into()),
            messages: vec![
                Message::user("hi"),
                Message {
                    role: Role::Assistant,
                    text: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "read".into(),
                        name: "read".into(),
                        arguments: r#"{"path":"a.rs"}"#.into(),
                    }],
                    tool_results: Vec::new(),
                    attachments: Vec::new(),
                },
                Message::tool_results(vec![ToolResult {
                    call_id: "read".into(),
                    content: "fn main() {}".into(),
                    is_error: false,
                    path: String::new(),
                diff: String::new(),
                }]),
            ],
            tools: Vec::new(),
            max_output_tokens: Some(64),
        };

        let body = google_body(&request);
        let contents = body["contents"].as_array().unwrap();

        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be brief");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[1]["parts"][0]["functionCall"]["name"], "read");
        // A result comes back as a user turn, matched by function name.
        assert_eq!(contents[2]["role"], "user");
        assert_eq!(contents[2]["parts"][0]["functionResponse"]["name"], "read");
    }

    #[test]
    fn the_catalog_endpoint_is_supplied_when_the_provider_omits_it() {
        let google: Provider = serde_json::from_str(r#"{"npm":"@ai-sdk/google"}"#).unwrap();
        assert_eq!(google.wire_api(), WireApi::Google);
        assert!(
            google
                .api_base()
                .is_some_and(|base| base.contains("generativelanguage")),
            "google declares no `api` in the catalog, so one must be supplied"
        );

        let bedrock: Provider =
            serde_json::from_str(r#"{"npm":"@ai-sdk/amazon-bedrock"}"#).unwrap();
        assert_eq!(bedrock.api_base(), None);
    }

    #[test]
    fn usage_arrives_after_the_finish_reason() {
        // The real shape, captured from OpenRouter: the chunk carrying `finish_reason` comes
        // first, and the token counts follow in a chunk of their own. A reader that stops at the
        // finish reason never sees them.
        let events = collect(
            concat!(
                r#"data: {"choices":[{"delta":{"content":"OK"},"finish_reason":null}]}"#,
                "
",
                r#"data: {"choices":[{"delta":{"content":""},"finish_reason":"stop"}]}"#,
                "
",
                r#"data: {"choices":[{"delta":{},"finish_reason":"stop"}],"#,
                r#""usage":{"prompt_tokens":14,"completion_tokens":5,"total_tokens":19}}"#,
                "
",
                "data: [DONE]
",
            ),
            WireApi::OpenAiCompatible,
        );

        assert!(
            events.contains(&CompletionEvent::Usage {
                input: 14,
                output: 5
            }),
            "the counts must survive the stop that precedes them: {events:?}"
        );
    }

    #[test]
    fn a_usage_chunk_with_no_choices_is_still_read() {
        // Some providers send the counts in a chunk with an empty `choices`, which an early
        // `choices/0` lookup would discard.
        let events = collect(
            concat!(
                r#"data: {"choices":[],"usage":{"prompt_tokens":7,"completion_tokens":3}}"#,
                "
",
                "data: [DONE]
",
            ),
            WireApi::OpenAiCompatible,
        );

        assert!(
            events.contains(&CompletionEvent::Usage {
                input: 7,
                output: 3
            }),
            "got: {events:?}"
        );
    }

    #[test]
    fn stops_at_the_first_done_sentinel() {
        let events = collect(
            concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"a\"}}]}\n",
                "data: [DONE]\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"never read\"}}]}\n",
            ),
            WireApi::OpenAiCompatible,
        );

        assert_eq!(
            events,
            vec![
                CompletionEvent::Text("a".into()),
                CompletionEvent::Stop(StopReason::EndTurn)
            ]
        );
    }

    #[test]
    fn surfaces_errors_embedded_in_the_stream() {
        let error = futures::executor::block_on(async {
            // `StreamExt::next` needs `Unpin`, which the `unfold` stream is not.
            decode_sse(
                AsyncBody::from(
                    "data: {\"type\":\"error\",\"error\":{\"message\":\"overloaded\"}}\n",
                ),
                WireApi::Anthropic,
            )
            .boxed()
            .next()
            .await
            .expect("the stream should yield the error")
            .expect_err("an error chunk should not decode as an event")
        });

        assert!(error.to_string().contains("overloaded"), "got: {error}");
    }

    #[test]
    fn pulls_a_readable_message_out_of_an_error_body() {
        assert_eq!(
            describe_error(br#"{"error":{"message":"invalid api key"}}"#),
            "invalid api key"
        );
        assert_eq!(describe_error(b"plain text failure"), "plain text failure");
    }

    #[test]
    fn tool_results_take_the_shape_each_wire_format_expects() {
        let request = CompletionRequest {
            provider_id: "test".into(),
            provider: serde_json::from_str(r#"{"api":"https://x.test"}"#).unwrap(),
            model: Model::default(),
            model_id: "m".into(),
            api_key: "k".into(),
            system: None,
            messages: vec![
                Message::user("hi"),
                Message {
                    role: Role::Assistant,
                    text: String::new(),
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "read".into(),
                        arguments: r#"{"path":"a.rs"}"#.into(),
                    }],
                    tool_results: Vec::new(),
                    attachments: Vec::new(),
                },
                Message::tool_results(vec![ToolResult {
                    call_id: "c1".into(),
                    content: "fn main() {}".into(),
                    is_error: false,
                    path: String::new(),
                diff: String::new(),
                }]),
            ],
            tools: Vec::new(),
            max_output_tokens: Some(64),
        };

        // Anthropic folds results into a user message.
        let anthropic = anthropic_body(&request);
        let messages = anthropic["messages"].as_array().unwrap();
        assert_eq!(messages[2]["role"], "user");
        assert_eq!(messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(messages[2]["content"][0]["tool_use_id"], "c1");
        assert_eq!(messages[1]["content"][0]["type"], "tool_use");

        // OpenAI gives each result its own message with a role of its own.
        let openai = openai_body(&request);
        let messages = openai["messages"].as_array().unwrap();
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "c1");
        assert_eq!(messages[1]["tool_calls"][0]["function"]["name"], "read");
    }
}
