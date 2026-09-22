//! DAGOS inference adapter for OpenAI-compatible Chat Completions endpoints.
//!
//! One adapter serves any endpoint that speaks the streaming Chat Completions protocol:
//! OpenRouter, OpenAI, Z.ai, Ollama, LM Studio, vLLM. Authentication, HTTP, streaming, and
//! request formatting stay inside this crate; DAGOS only sees the [`InferenceProvider`] interface,
//! and the adapter only sees compiled IR.

mod prose;
mod protocol;
mod sse;

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use dagos_core::domain::{ModelId, ProviderId};
use dagos_core::provider::{DeltaSink, InferenceProvider, InferenceRequest, ProviderError};
use serde_json::Value;

pub use prose::ProseExtractor;
pub use protocol::{request_body, system_message, user_message};
pub use sse::SseDecoder;

/// Connection settings for one OpenAI-compatible endpoint.
#[derive(Clone)]
pub struct OpenAiCompatibleConfig {
    /// The provider ID runs record, e.g. `openrouter`.
    pub id: ProviderId,
    /// Base URL without `/chat/completions`, e.g. `https://openrouter.ai/api/v1`.
    pub base_url: String,
    /// Bearer token, if the endpoint needs one. DAGOS never persists it.
    pub api_key: Option<String>,
    /// Models to offer by default; the endpoint may accept others.
    pub models: Vec<ModelId>,
    /// Request a JSON object response (`response_format: {"type": "json_object"}`).
    pub json_mode: bool,
}

impl fmt::Debug for OpenAiCompatibleConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiCompatibleConfig")
            .field("id", &self.id)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("models", &self.models)
            .field("json_mode", &self.json_mode)
            .finish()
    }
}

/// An [`InferenceProvider`] backed by an OpenAI-compatible Chat Completions endpoint.
#[derive(Debug, Clone)]
pub struct OpenAiCompatible {
    config: OpenAiCompatibleConfig,
    client: reqwest::Client,
}

impl OpenAiCompatible {
    pub fn new(config: OpenAiCompatibleConfig) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .expect("HTTP client configuration is valid");
        Self { config, client }
    }

    pub fn config(&self) -> &OpenAiCompatibleConfig {
        &self.config
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.config.base_url.trim_end_matches('/'))
    }
}

fn failed(message: String) -> ProviderError {
    ProviderError::Failed(message)
}

/// At most the first 500 characters of an error body.
fn excerpt(body: &str) -> &str {
    body.char_indices().nth(500).map_or(body, |(end, _)| &body[..end])
}

#[async_trait]
impl InferenceProvider for OpenAiCompatible {
    fn id(&self) -> &ProviderId {
        &self.config.id
    }

    fn suggested_models(&self) -> Vec<ModelId> {
        self.config.models.clone()
    }

    async fn infer(
        &self,
        request: InferenceRequest<'_>,
        deltas: &mut dyn DeltaSink,
    ) -> Result<String, ProviderError> {
        let endpoint = self.endpoint();
        let body = request_body(request.model_id, request.ir, self.config.json_mode);
        let mut http = self
            .client
            .post(&endpoint)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .body(body.to_string());
        if let Some(key) = &self.config.api_key {
            http = http.bearer_auth(key);
        }
        let mut response = http
            .send()
            .await
            .map_err(|error| failed(format!("request to {endpoint} failed: {error}")))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(failed(format!("HTTP {status} from {endpoint}: {}", excerpt(&body))));
        }

        let mut events = SseDecoder::default();
        let mut prose = ProseExtractor::default();
        let mut output = String::new();
        'stream: while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|error| failed(format!("stream interrupted: {error}")))?
        {
            for data in events.push(&chunk) {
                if data == "[DONE]" {
                    break 'stream;
                }
                let event: Value = serde_json::from_str(&data)
                    .map_err(|error| failed(format!("unreadable stream event: {error}")))?;
                if let Some(error) = event.get("error") {
                    return Err(failed(format!("endpoint reported an error: {error}")));
                }
                if let Some(content) =
                    event.pointer("/choices/0/delta/content").and_then(Value::as_str)
                {
                    output.push_str(content);
                    let text = prose.push(content);
                    if !text.is_empty() {
                        deltas.delta(&text);
                    }
                }
            }
        }
        Ok(output)
    }
}
