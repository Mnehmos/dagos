//! The provider boundary: an adapter receives a model ID and compiled, versioned IR — nothing else.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::{memory_store, payload};
use dagos_core::context::carry_context;
use dagos_core::contracts::Contract;
use dagos_core::domain::{
    ContextSource, ConversationTurn, EdgeType, ModelId, NodeType, ProviderId, RunConfig,
};
use dagos_core::ir::compile;
use dagos_core::provider::{
    CollectDeltas, DeltaSink, InferenceProvider, InferenceRequest, ProviderError,
};
use dagos_core::store::StoreError;
use serde_json::{Value, json};

/// Records exactly what crosses the boundary, as the JSON a remote endpoint would receive.
struct Spy {
    id: ProviderId,
    received: Mutex<Vec<(String, Value)>>,
}

#[async_trait]
impl InferenceProvider for Spy {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn suggested_models(&self) -> Vec<ModelId> {
        vec![ModelId::parse("spy-1").unwrap()]
    }

    async fn infer(
        &self,
        request: InferenceRequest<'_>,
        deltas: &mut dyn DeltaSink,
    ) -> Result<String, ProviderError> {
        let document = serde_json::to_value(request.ir).unwrap();
        self.received.lock().unwrap().push((request.model_id.to_string(), document));
        deltas.delta("seen");
        Err(ProviderError::Failed("spies do not answer".into()))
    }
}

#[tokio::test]
async fn providers_receive_exactly_the_compiled_ir_and_the_model_id() {
    let store = memory_store();
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let config = RunConfig {
        provider_id: ProviderId::parse("spy").unwrap(),
        model_id: ModelId::parse("spy-1").unwrap(),
        system_prompt: "Be exact.".into(),
    };
    let (run, task, ir) = store
        .transaction(|tx| {
            let decision =
                tx.insert_node(&project, NodeType::Decision, payload(json!({"text": "SQLite"})))?;
            let todo =
                tx.insert_node(&project, NodeType::Task, payload(json!({"title": "Ship"})))?;
            tx.insert_edge(&project, &todo.id, &decision.id, EdgeType::DependsOn)?;
            let run = tx.create_run(&project, &config)?;
            let task = tx.insert_node(
                &project,
                NodeType::Conversation,
                ConversationTurn::user("Ship it").to_payload(),
            )?;
            carry_context(tx, &run.id)?;
            tx.replace_context(
                &run.id,
                &[(decision.id, ContextSource::Jev), (todo.id, ContextSource::Jev)],
            )?;
            let ir = compile(tx, &run.id, &task.id, &[]).map_err(|error| match error {
                dagos_core::ir::CompileError::Store(error) => error,
                other => panic!("{other}"),
            })?;
            Ok::<_, StoreError>((run, task, ir))
        })
        .unwrap();

    let spy = Arc::new(Spy { id: config.provider_id.clone(), received: Mutex::new(Vec::new()) });
    let provider: Arc<dyn InferenceProvider> = spy.clone();
    let mut deltas = CollectDeltas::default();
    let result =
        provider.infer(InferenceRequest { model_id: &run.model_id, ir: &ir }, &mut deltas).await;

    assert_eq!(result, Err(ProviderError::Failed("spies do not answer".into())));
    assert_eq!(deltas.0, ["seen"]);
    assert_eq!(provider.id().as_str(), "spy");
    let received = spy.received.lock().unwrap();
    let (model_id, document) = &received[0];
    assert_eq!(model_id, "spy-1");
    // What crossed the boundary is the compiled, versioned IR — validated and nothing more.
    assert_eq!(document, &serde_json::to_value(&ir).unwrap());
    Contract::InferenceIr.validate(document).unwrap();
    assert_eq!(document["schema"], "kiss.inference-ir.v1");
    assert_eq!(document["task"]["node_id"], task.id.as_str());
    assert_eq!(document["context"].as_array().unwrap().len(), 2);
}
