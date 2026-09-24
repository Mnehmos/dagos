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
use dagos_core::context::recall::RecallChunk;
use dagos_core::context::{JevClassifier, JevError, NoulQuestion};
use dagos_core::domain::JevRequest;
use dagos_core::domain::{ModelId, ProviderId};
use dagos_core::provider::{DeltaSink, InferenceProvider, InferenceRequest, ProviderError};
use serde_json::Value;

pub use prose::ProseExtractor;
pub use protocol::{
    ACTIVE_THRESHOLD, RECALL_BATCH_CHARS, classification_from_decisions, decisions_body,
    jev_request_body, jev_system_message, noul_answers, noul_body, recall_batches, recall_body,
    relevance_from_decisions, request_body, system_message, unwrap_document, user_message,
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
        let http = self.client.get(&url).timeout(CHECK_TIMEOUT);
        self.send_json(&url, http).await
    }

    /// Sends a JSON request with the API key and returns the JSON answer of a 2xx response.
    async fn send_json(&self, url: &str, http: reqwest::RequestBuilder) -> Result<Value, String> {
        let mut http = http.header("accept", "application/json");
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
        .map(|output| unwrap_document(&output))
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

/// Whether `model` is a decisions model (TypeSafe's Jev, e.g. `~typesafe/jev-latest`), served by
/// the Decisions API rather than Chat Completions.
pub fn is_decisions_model(model: &ModelId) -> bool {
    model.as_str().trim_start_matches('~').starts_with("typesafe/")
}

/// The Decisions API URL next to an OpenAI-compatible base URL:
/// `https://openrouter.ai/api/v1` becomes `https://openrouter.ai/api/alpha/decisions`.
pub fn decisions_url(base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    format!("{}/alpha/decisions", base.strip_suffix("/v1").unwrap_or(base))
}

/// A Jev classifier backed by a decisions model through OpenRouter's Decisions API, such as
/// TypeSafe's Jev (`~typesafe/jev-latest`).
///
/// Each candidate becomes one `noul` question ("does this node belong in the active context?");
/// the calibrated answers become a `kiss.jev-context.v1` document, which DAGOS validates exactly
/// like any other Jev output.
#[derive(Debug, Clone)]
pub struct DecisionsJev {
    chat: OpenAiCompatible,
    model: ModelId,
    id: String,
    url: String,
}

impl DecisionsJev {
    pub fn new(chat: OpenAiCompatible, model: ModelId) -> Self {
        let id = format!("{}-jev:{}", chat.config.id, model);
        let url = decisions_url(&chat.config.base_url);
        Self { chat, model, id, url }
    }
}

#[async_trait]
impl JevClassifier for DecisionsJev {
    fn id(&self) -> &str {
        &self.id
    }

    async fn classify(&self, request: &JevRequest) -> Result<String, JevError> {
        if request.candidates.is_empty() && request.tools.is_empty() {
            return classification_from_decisions(request, &serde_json::json!({"answers": {}}))
                .map_err(JevError);
        }
        let response = self.decide_raw(decisions_body(&self.model, request)).await?;
        classification_from_decisions(request, &response).map_err(JevError)
    }

    async fn relevance(
        &self,
        query: &str,
        chunks: &[RecallChunk],
    ) -> Result<Option<Vec<f64>>, JevError> {
        let mut scores = Vec::with_capacity(chunks.len());
        for batch in recall_batches(chunks) {
            let response = self.decide_raw(recall_body(&self.model, query, batch)).await?;
            scores.extend(relevance_from_decisions(batch, &response).map_err(JevError)?);
        }
        Ok(Some(scores))
    }

    async fn decide(
        &self,
        state: &Value,
        questions: &[NoulQuestion],
    ) -> Result<Option<Vec<f64>>, JevError> {
        if questions.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let response = self.decide_raw(noul_body(&self.model, state, questions)).await?;
        noul_answers(questions, &response).map(Some).map_err(JevError)
    }
}

impl DecisionsJev {
    /// Sends one Decisions API request and returns its JSON answer.
    async fn decide_raw(&self, body: Value) -> Result<Value, JevError> {
        let http = self
            .chat
            .client
            .post(&self.url)
            .header("content-type", "application/json")
            .body(body.to_string());
        self.chat.send_json(&self.url, http).await.map_err(JevError)
    }
}
