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
use dagos_core::context::{JevClassifier, JevError};
use dagos_core::domain::JevRequest;
use dagos_core::domain::{ModelId, ProviderId};
use dagos_core::provider::{DeltaSink, InferenceProvider, InferenceRequest, ProviderError};
use serde_json::Value;

pub use prose::ProseExtractor;
pub use protocol::{
    jev_request_body, jev_system_message, request_body, system_message, user_message,
};
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
        self.url("/chat/completions")
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.config.base_url.trim_end_matches('/'))
    }

    /// Checks the connection: with `key_check` (a path such as OpenRouter's `/key`, for endpoints
    /// whose model list is public) the key must be accepted, then `/models` must answer. Returns
    /// the model IDs the endpoint lists, sorted.
    pub async fn check(&self, key_check: Option<&str>) -> Result<Vec<String>, String> {
        if let Some(path) = key_check {
            self.get_json(path).await?;
        }
        let models = self.get_json("/models").await?;
        let mut ids: Vec<String> = models
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|model| model.get("id")?.as_str().map(str::to_owned))
            .collect();
        ids.sort();
        ids.dedup();
        Ok(ids)
    }

    async fn get_json(&self, path: &str) -> Result<Value, String> {
        let url = self.url(path);
        let mut http =
            self.client.get(&url).header("accept", "application/json").timeout(CHECK_TIMEOUT);
        if let Some(key) = &self.config.api_key {
            http = http.bearer_auth(key);
        }
        let response =
            http.send().await.map_err(|error| format!("request to {url} failed: {error}"))?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(format!("HTTP {status} from {url}{}", detail(&body)));
        }
        serde_json::from_str(&body)
            .map_err(|error| format!("unreadable answer from {url}: {error}"))
    }
}

/// How long a connection check may take.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

fn failed(message: String) -> ProviderError {
    ProviderError::Failed(message)
}

/// `: <excerpt>` of an error body, or nothing if it is empty.
fn detail(body: &str) -> String {
    let body = excerpt(body.trim());
    if body.is_empty() { String::new() } else { format!(": {body}") }
}

/// At most the first 500 characters of an error body.
fn excerpt(body: &str) -> &str {
    body.char_indices().nth(500).map_or(body, |(end, _)| &body[..end])
}

impl OpenAiCompatible {
    /// Streams one Chat Completions request and returns the concatenated content, calling
    /// `on_content` with each content fragment as it arrives.
    async fn stream_completion(
        &self,
        body: Value,
        mut on_content: impl FnMut(&str) + Send,
    ) -> Result<String, String> {
        let endpoint = self.endpoint();
        let mut http = self
            .client
            .post(&endpoint)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .body(body.to_string());
        if let Some(key) = &self.config.api_key {
            http = http.bearer_auth(key);
        }
        let mut response =
            http.send().await.map_err(|error| format!("request to {endpoint} failed: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(format!("HTTP {status} from {endpoint}{}", detail(&body)));
        }

        let mut events = SseDecoder::default();
        let mut output = String::new();
        'stream: while let Some(chunk) =
            response.chunk().await.map_err(|error| format!("stream interrupted: {error}"))?
        {
            for data in events.push(&chunk) {
                if data == "[DONE]" {
                    break 'stream;
                }
                let event: Value = serde_json::from_str(&data)
                    .map_err(|error| format!("unreadable stream event: {error}"))?;
                if let Some(error) = event.get("error") {
                    return Err(format!("endpoint reported an error: {error}"));
                }
                if let Some(content) =
                    event.pointer("/choices/0/delta/content").and_then(Value::as_str)
                {
                    output.push_str(content);
                    on_content(content);
                }
            }
        }
        Ok(output)
    }
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
        let body = request_body(request.model_id, request.ir, self.config.json_mode);
        let mut prose = ProseExtractor::default();
        self.stream_completion(body, |content| {
            let text = prose.push(content);
            if !text.is_empty() {
                deltas.delta(&text);
            }
        })
        .await
        .map_err(failed)
    }
}

/// A Jev classifier backed by a model on an OpenAI-compatible endpoint (e.g. OpenRouter).
///
/// It sends the `kiss.jev-request.v1` document with classifier-only instructions and returns the
/// model's raw text. It adds no authority of its own: DAGOS validates the output against
/// `kiss.jev-context.v1` and the request, so anything beyond node labels is rejected.
#[derive(Debug, Clone)]
pub struct OpenAiCompatibleJev {
    chat: OpenAiCompatible,
    model: ModelId,
    id: String,
}

impl OpenAiCompatibleJev {
    pub fn new(chat: OpenAiCompatible, model: ModelId) -> Self {
        let id = format!("{}-jev:{}", chat.config.id, model);
        Self { chat, model, id }
    }
}

#[async_trait]
impl JevClassifier for OpenAiCompatibleJev {
    fn id(&self) -> &str {
        &self.id
    }

    async fn classify(&self, request: &JevRequest) -> Result<String, JevError> {
        let body = jev_request_body(&self.model, request, self.chat.config.json_mode);
        self.chat.stream_completion(body, |_| {}).await.map_err(JevError)
    }
}
