use crate::catalog::{Provider, WireApi};
use anyhow::{Context as _, Result, anyhow, bail};
use futures::{
    AsyncBufReadExt as _, AsyncReadExt as _, Stream, StreamExt as _, io::BufReader,
    stream::BoxStream,
};
use http_client::{AsyncBody, HttpClient, Request};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub text: String,
}

pub struct CompletionRequest {
    pub provider_id: String,
    pub provider: Provider,
    pub model_id: String,
    pub api_key: String,
    pub messages: Vec<Message>,
    pub max_output_tokens: u64,
}

/// The subset of a streamed response Cowork renders today. Tool calls are deliberately absent:
/// nothing in the UI can execute one yet, so surfacing them would promise behavior that does not
/// exist.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CompletionEvent {
    Text(String),
    Stop,
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
        .api
        .clone()
        .ok_or_else(|| anyhow!("the models.dev catalog has no API endpoint for this provider"))?;
    let wire_api = request.provider.wire_api();

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

fn anthropic_body(request: &CompletionRequest) -> Value {
    let messages = request
        .messages
        .iter()
        .map(|message| {
            json!({
                "role": match message.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                },
                "content": [{ "type": "text", "text": message.text }],
            })
        })
        .collect::<Vec<_>>();

    json!({
        "model": request.model_id,
        "max_tokens": request.max_output_tokens,
        "stream": true,
        "messages": messages,
    })
}

fn openai_body(request: &CompletionRequest) -> Value {
    let messages = request
        .messages
        .iter()
        .map(|message| {
            json!({
                "role": match message.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                },
                "content": message.text,
            })
        })
        .collect::<Vec<_>>();

    json!({
        "model": request.model_id,
        "max_completion_tokens": request.max_output_tokens,
        "stream": true,
        "messages": messages,
    })
}

fn decode_sse(
    body: AsyncBody,
    wire_api: WireApi,
) -> impl Stream<Item = Result<CompletionEvent>> + Send {
    let lines = BufReader::new(body).lines();

    futures::stream::unfold(
        (lines, wire_api, false),
        |(mut lines, wire_api, mut stopped)| async move {
            loop {
                if stopped {
                    return None;
                }

                let line = match lines.next().await {
                    Some(Ok(line)) => line,
                    Some(Err(error)) => {
                        return Some((
                            Err(anyhow!(error).context("reading the provider's event stream")),
                            (lines, wire_api, true),
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
                    return Some((Ok(CompletionEvent::Stop), (lines, wire_api, true)));
                }

                let chunk: Value = match serde_json::from_str(data) {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        return Some((
                            Err(anyhow!(error)
                                .context("parsing a chunk of the provider's event stream")),
                            (lines, wire_api, true),
                        ));
                    }
                };

                match decode_chunk(&chunk, wire_api) {
                    Ok(Some(event)) => {
                        stopped = event == CompletionEvent::Stop;
                        return Some((Ok(event), (lines, wire_api, stopped)));
                    }
                    Ok(None) => continue,
                    Err(error) => return Some((Err(error), (lines, wire_api, true))),
                }
            }
        },
    )
}

fn decode_chunk(chunk: &Value, wire_api: WireApi) -> Result<Option<CompletionEvent>> {
    match wire_api {
        WireApi::Anthropic => {
            match chunk.get("type").and_then(Value::as_str) {
                Some("content_block_delta") => {
                    // Anthropic emits several delta shapes; only `text_delta` carries prose, and
                    // the rest (thinking, tool input) have no rendering path in Cowork yet.
                    let text = chunk
                        .pointer("/delta/text")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    if text.is_empty() {
                        Ok(None)
                    } else {
                        Ok(Some(CompletionEvent::Text(text.to_owned())))
                    }
                }
                Some("message_stop") => Ok(Some(CompletionEvent::Stop)),
                Some("error") => {
                    let message = chunk
                        .pointer("/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("the provider reported an error");
                    bail!("{message}")
                }
                _ => Ok(None),
            }
        }
        WireApi::OpenAiCompatible => {
            if let Some(message) = chunk.pointer("/error/message").and_then(Value::as_str) {
                bail!("{message}");
            }

            let Some(choice) = chunk.pointer("/choices/0") else {
                return Ok(None);
            };

            let text = choice
                .pointer("/delta/content")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !text.is_empty() {
                return Ok(Some(CompletionEvent::Text(text.to_owned())));
            }

            if choice
                .get("finish_reason")
                .is_some_and(|reason| !reason.is_null())
            {
                return Ok(Some(CompletionEvent::Stop));
            }

            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                CompletionEvent::Stop,
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
            vec![CompletionEvent::Text("Hi".into()), CompletionEvent::Stop]
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
            vec![CompletionEvent::Text("a".into()), CompletionEvent::Stop]
        );
    }

    #[test]
    fn surfaces_errors_embedded_in_the_stream() {
        let error = futures::executor::block_on(async {
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
}
