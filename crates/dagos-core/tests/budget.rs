//! Fitting IR to the model's window: nothing is left out while everything fits; when it does not,
//! the least relevant recalled turns go first.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::memory_store;
use dagos_core::context::recall::RecallChunk;
use dagos_core::context::{FakeJev, JevClassifier, JevError};
use dagos_core::domain::{EventData, InferenceIr, JevRequest, ModelId, ProviderId, Run, RunConfig};
use dagos_core::provider::{
    DeltaSink, FakeProvider, InferenceProvider, InferenceRequest, ProviderError,
};
use dagos_core::runtime::Runtime;
use dagos_core::store::Store;

/// Relevance from the turn's text: "rank 7" scores 0.7.
struct RankJev;

#[async_trait]
impl JevClassifier for RankJev {
    fn id(&self) -> &str {
        "rank-jev"
    }

    async fn classify(&self, request: &JevRequest) -> Result<String, JevError> {
        FakeJev::new().classify(request).await
    }

    async fn relevance(
        &self,
        _query: &str,
        chunks: &[RecallChunk],
    ) -> Result<Option<Vec<f64>>, JevError> {
        let rank = |chunk: &RecallChunk| {
            (5..=9)
                .rev()
                .find(|n| chunk.text.contains(&format!("rank {n}")))
                .map_or(0.0, |n| n as f64 / 10.0)
        };
        Ok(Some(chunks.iter().map(rank).collect()))
    }
}

/// The fake provider with a context window of `tokens`.
struct Windowed {
    inner: FakeProvider,
    tokens: Option<usize>,
}

#[async_trait]
impl InferenceProvider for Windowed {
    fn id(&self) -> &ProviderId {
        self.inner.id()
    }

    fn suggested_models(&self) -> Vec<ModelId> {
        self.inner.suggested_models()
    }

    async fn infer(
        &self,
        request: InferenceRequest<'_>,
        deltas: &mut dyn DeltaSink,
    ) -> Result<String, ProviderError> {
        self.inner.infer(request, deltas).await
    }

    async fn context_window(&self, _model: &ModelId) -> Option<usize> {
        self.tokens
    }
}

async fn last_ir(tokens: Option<usize>) -> (Arc<Store>, Run, InferenceIr) {
    let store = Arc::new(memory_store());
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let provider = Windowed { inner: FakeProvider::new(), tokens };
    let runtime = Runtime::new(store.clone(), Arc::new(RankJev))
        .with_provider(Arc::new(provider))
        .with_conversation_window(0);
    let config = RunConfig {
        provider_id: ProviderId::parse("fake").unwrap(),
        model_id: ModelId::parse("fake-echo").unwrap(),
        system_prompt: String::new(),
    };
    for rank in 5..=9 {
        let message = format!("rank {rank}: {}", "context ".repeat(400));
        runtime.run(&project, &message, &config).await.unwrap();
    }
    let run = runtime.run(&project, "Use what matters", &config).await.unwrap();
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    let ir = events
        .into_iter()
        .find_map(|event| match event.data {
            EventData::IrCompiled { ir } => Some(ir),
            _ => None,
        })
        .unwrap();
    (store, run, ir)
}

fn ranks(ir: &InferenceIr) -> Vec<usize> {
    ir.recalled
        .iter()
        .filter_map(|turn| (5..=9).find(|n| turn.text.contains(&format!("rank {n}:"))))
        .collect()
}

#[tokio::test]
async fn everything_relevant_goes_in_while_it_fits() {
    let (_, _, unknown) = last_ir(None).await;
    assert_eq!(ranks(&unknown), [5, 6, 7, 8, 9], "no known window: nothing is left out");
    let (_, _, roomy) = last_ir(Some(1_000_000)).await;
    assert_eq!(ranks(&roomy), [5, 6, 7, 8, 9], "a large window: nothing is left out");
}

#[tokio::test]
async fn the_least_relevant_turns_go_first_when_the_window_is_small() {
    // Measure the IR without a limit, then allow its non-recalled part plus about two turns.
    let (_, _, full) = last_ir(None).await;
    let recalled: usize = full.recalled.iter().map(|turn| turn.text.len()).sum();
    let base = serde_json::to_string(&full).unwrap().len() - recalled;
    let budget = base + 7_000;
    let (_, _, ir) = last_ir(Some(16_000 + budget.div_ceil(3))).await;
    let kept = ranks(&ir);
    assert!(!kept.is_empty() && kept.len() < 5, "{kept:?}");
    assert_eq!(kept.last(), Some(&9), "the most relevant stays");
    assert!(!kept.contains(&5), "the least relevant goes first: {kept:?}");
    let size = serde_json::to_string(&ir).unwrap().len();
    assert!(size <= budget.div_ceil(3) * 3, "the IR fits the budget: {size} > {budget}");
}

#[tokio::test]
async fn what_cannot_fit_is_still_sent_trimmed_as_far_as_possible() {
    // A window smaller than the non-recalled part: every recalled turn goes, the rest is sent.
    let (_, run, ir) = last_ir(Some(16_000 + 1_000)).await;
    assert!(ir.recalled.is_empty());
    assert_eq!(run.status, dagos_core::domain::RunStatus::Completed, "the provider decides");
}
