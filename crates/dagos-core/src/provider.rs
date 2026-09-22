//! Provider layer: the common inference adapter interface and the deterministic fake provider.
//!
//! Providers are interchangeable inference endpoints. The whole boundary is [`InferenceRequest`]:
//! a model ID and a compiled [`InferenceIr`]. A provider streams presentation prose through a
//! [`DeltaSink`] and returns its raw final output, which is untrusted until the
//! [`crate::response`] layer validates it. Providers cannot see the store, and authentication,
//! HTTP, and provider-specific request formatting stay inside each adapter.

use async_trait::async_trait;

use crate::domain::{InferenceIr, ModelId, ProviderId};

/// Everything a provider receives for one inference: which model to use and the compiled IR.
#[derive(Debug, Clone, Copy)]
pub struct InferenceRequest<'a> {
    /// Passed through unchanged; only the adapter interprets it.
    pub model_id: &'a ModelId,
    pub ir: &'a InferenceIr,
}

/// Receives presentation prose as a provider streams it.
///
/// Deltas are fragments of the response's `presentation.prose`, in order. They are presentation
/// only: nothing streamed through a sink ever becomes canonical DAG state.
pub trait DeltaSink: Send {
    fn delta(&mut self, text: &str);
}

/// Why a provider produced no final output. Output that *was* produced but is invalid is detected
/// later, by validation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// The adapter does not serve this model.
    #[error("model `{0}` is not available from this provider")]
    UnknownModel(ModelId),
    /// The endpoint could not be reached or refused the request.
    #[error("inference request failed: {0}")]
    Failed(String),
}

/// A common interface over inference endpoints (fake, OpenAI-compatible, ...).
#[async_trait]
pub trait InferenceProvider: Send + Sync {
    /// Stable identity, recorded on every run that uses this provider.
    fn id(&self) -> &ProviderId;

    /// Model IDs to offer by default, e.g. in a picker. Adapters may accept others.
    fn suggested_models(&self) -> Vec<ModelId>;

    /// Runs inference: streams presentation deltas into `deltas` and returns the raw final output,
    /// which should be a `kiss.inference-response.v1` document.
    async fn infer(
        &self,
        request: InferenceRequest<'_>,
        deltas: &mut dyn DeltaSink,
    ) -> Result<String, ProviderError>;
}

/// Collects deltas in memory; useful for callers that do not persist streaming output.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CollectDeltas(pub Vec<String>);

impl DeltaSink for CollectDeltas {
    fn delta(&mut self, text: &str) {
        self.0.push(text.to_owned());
    }
}
