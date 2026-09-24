//! "Load all context": Jev classifies the person's intent, and DAGOS loads everything.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::memory_store;
use dagos_core::context::recall::RecallChunk;
use dagos_core::context::{FakeJev, JevClassifier, JevError, NoulQuestion};
use dagos_core::domain::{EventData, InferenceIr, JevRequest, ModelId, ProviderId, Run, RunConfig};
use dagos_core::provider::FakeProvider;
use dagos_core::runtime::{Runtime, Thread};
use dagos_core::store::Store;
use serde_json::Value;

/// Recognises requests for everything; judges every earlier turn irrelevant, so only the
/// expansion can bring them in.
struct IntentJev;

#[async_trait]
impl JevClassifier for IntentJev {
    fn id(&self) -> &str {
        "intent-jev"
    }

    async fn classify(&self, request: &JevRequest) -> Result<String, JevError> {
        FakeJev::new().classify(request).await
    }

    async fn relevance(
        &self,
        _: &str,
        chunks: &[RecallChunk],
    ) -> Result<Option<Vec<f64>>, JevError> {
        Ok(Some(vec![0.01; chunks.len()]))
    }

    async fn decide(
        &self,
        state: &Value,
        questions: &[NoulQuestion],
    ) -> Result<Option<Vec<f64>>, JevError> {
        let everything = state["message"].as_str().unwrap_or_default().contains("all context");
        Ok(Some(questions.iter().map(|_| if everything { 0.96 } else { 0.03 }).collect()))
    }
}

fn config() -> RunConfig {
    RunConfig {
        provider_id: ProviderId::parse("fake").unwrap(),
        model_id: ModelId::parse("fake-echo").unwrap(),
        system_prompt: String::new(),
    }
}

fn first_ir(store: &Store, run: &Run) -> InferenceIr {
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    events
        .into_iter()
        .find_map(|event| match event.data {
            EventData::IrCompiled { ir } => Some(ir),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn asking_for_all_context_loads_every_node_and_turn() {
    let store = Arc::new(memory_store());
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let runtime = Runtime::new(store.clone(), Arc::new(IntentJev))
        .with_provider(Arc::new(FakeProvider::new()))
        .with_conversation_window(1);
    for message in ["Lisbon in October", "My sister is allergic to shellfish", "Hotel in Alfama"] {
        runtime.run(&project, message, &config()).await.unwrap();
    }

    let specific =
        runtime.start_in(Thread::New(&project), "Where is the hotel?", &config()).unwrap();
    let specific = runtime.finish(specific).await.unwrap();
    let ir = first_ir(&store, &specific);
    assert!(ir.recalled.is_empty(), "a specific question: only what Jev judges relevant");

    let everything = runtime
        .start_in(Thread::New(&project), "Please load all context first", &config())
        .unwrap();
    let everything = runtime.finish(everything).await.unwrap();
    let ir = first_ir(&store, &everything);
    assert_eq!(ir.recalled.len(), 4, "every earlier turn of every chat");
    let nodes = store.transaction(|tx| tx.nodes(&project)).unwrap();
    let events = store.transaction(|tx| tx.events(&everything.id)).unwrap();
    let emitted_after = events
        .iter()
        .filter(|event| matches!(event.data, EventData::DagNodeCreated { .. }))
        .count();
    assert_eq!(
        ir.context.len(),
        nodes.len() - 1 - emitted_after,
        "every node that existed, except the task itself"
    );
    assert!(events.iter().any(|event| matches!(
        event.data,
        EventData::ContextExpanded { probability } if probability > 0.9
    )));
}
