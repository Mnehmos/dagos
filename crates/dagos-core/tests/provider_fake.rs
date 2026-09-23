//! The fake streaming provider: offline, deterministic, and able to simulate every failure the
//! runtime must handle.

mod common;

use std::time::Duration;

use common::{memory_store, payload};
use dagos_core::context::carry_context;
use dagos_core::contracts::{Contract, ContractError};
use dagos_core::domain::{
    ContextSource, ConversationTurn, InferenceIr, InferenceIrSchema, InferenceResponse,
    IrContextItem, IrTask, ModelId, NodeId, NodeType, ProviderId, RunConfig,
};
use dagos_core::ir::compile;
use dagos_core::provider::{
    CollectDeltas, FakeModel, FakeProvider, InferenceProvider, InferenceRequest, ProviderError,
};
use dagos_core::store::StoreError;
use serde_json::json;

fn ir() -> InferenceIr {
    InferenceIr {
        schema: InferenceIrSchema,
        system_prompt: "Be brief.".into(),
        task: IrTask {
            node_id: NodeId::parse("node_000003").unwrap(),
            message: "Add restart tests".into(),
        },
        context: vec![IrContextItem {
            node_id: NodeId::parse("node_000001").unwrap(),
            node_type: NodeType::Decision,
            payload: payload(json!({"text": "Use SQLite"})),
            relations: vec![],
        }],
        recent_events: vec![],
        tools: vec![],
        tool_results: vec![],
    }
}

async fn infer(
    provider: &FakeProvider,
    model: &str,
    ir: &InferenceIr,
) -> (Result<String, ProviderError>, Vec<String>) {
    let model_id = ModelId::parse(model).unwrap();
    let mut deltas = CollectDeltas::default();
    let result = provider.infer(InferenceRequest { model_id: &model_id, ir }, &mut deltas).await;
    (result, deltas.0)
}

#[tokio::test]
async fn echo_streams_its_prose_and_returns_a_valid_response() {
    let ir = ir();
    let (result, deltas) = infer(&FakeProvider::new(), "fake-echo", &ir).await;
    let response: InferenceResponse = Contract::InferenceResponse.parse(&result.unwrap()).unwrap();

    assert!(deltas.len() > 1, "prose streams in several deltas");
    assert_eq!(deltas.concat(), response.presentation.prose);
    assert_eq!(response, FakeProvider::echo_response(&ir));
    assert_eq!(response.emissions.len(), 2);
}

#[tokio::test]
async fn streaming_order_and_output_are_deterministic() {
    let ir = ir();
    let provider = FakeProvider::new();
    let first = infer(&provider, "fake-echo", &ir).await;
    for _ in 0..3 {
        assert_eq!(infer(&provider, "fake-echo", &ir).await, first);
    }
}

#[tokio::test(start_paused = true)]
async fn delayed_streaming_keeps_its_order() {
    let ir = ir();
    let paced = FakeProvider::new().with_delta_delay(Duration::from_millis(40));
    let started = tokio::time::Instant::now();
    let (result, deltas) = infer(&paced, "fake-echo", &ir).await;
    let (unpaced_result, unpaced_deltas) = infer(&FakeProvider::new(), "fake-echo", &ir).await;
    assert_eq!((result, deltas.clone()), (unpaced_result, unpaced_deltas));
    assert_eq!(started.elapsed(), Duration::from_millis(40) * deltas.len() as u32);
}

#[tokio::test]
async fn malformed_and_schema_violating_output_can_be_simulated() {
    let ir = ir();
    let provider = FakeProvider::new();

    let (malformed, deltas) = infer(&provider, "fake-malformed", &ir).await;
    assert!(!deltas.is_empty(), "prose still streams before the malformed document");
    let error = Contract::InferenceResponse.parse::<InferenceResponse>(&malformed.unwrap());
    assert!(matches!(error, Err(ContractError::NotJson { .. })));

    let (invalid, _) = infer(&provider, "fake-invalid-schema", &ir).await;
    let error = Contract::InferenceResponse.parse::<InferenceResponse>(&invalid.unwrap());
    assert!(matches!(error, Err(ContractError::Violation(_))));

    // These two are contract-valid; their emissions are rejected later, against the IR or DAG.
    for model in ["fake-dangling-edge", "fake-cycle"] {
        let (raw, _) = infer(&provider, model, &ir).await;
        Contract::InferenceResponse.parse::<InferenceResponse>(&raw.unwrap()).unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn timeouts_and_endpoint_failures_can_be_simulated() {
    let ir = ir();
    let provider = FakeProvider::new();
    let model_id = ModelId::parse("fake-timeout").unwrap();
    let mut deltas = CollectDeltas::default();
    let timed_out = tokio::time::timeout(
        Duration::from_secs(30),
        provider.infer(InferenceRequest { model_id: &model_id, ir: &ir }, &mut deltas),
    )
    .await;
    assert!(timed_out.is_err(), "fake-timeout never finishes");
    assert_eq!(deltas.0, ["Thinking... "]);

    let (failed, _) = infer(&provider, "fake-error", &ir).await;
    assert!(matches!(failed, Err(ProviderError::Failed(_))));
    let (unknown, deltas) = infer(&provider, "gpt-4o", &ir).await;
    assert_eq!(unknown, Err(ProviderError::UnknownModel(ModelId::parse("gpt-4o").unwrap())));
    assert!(deltas.is_empty());
}

#[test]
fn identity_and_models_are_explicit() {
    let provider = FakeProvider::new();
    assert_eq!(provider.id(), &ProviderId::parse("fake").unwrap());
    let models: Vec<String> =
        provider.suggested_models().iter().map(|model| model.to_string()).collect();
    assert_eq!(models.len(), FakeModel::ALL.len());
    assert_eq!(models[0], "fake-echo");
}

#[tokio::test]
async fn the_provider_sees_only_ir_so_dag_state_outside_it_cannot_matter() {
    let store = memory_store();
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let config = RunConfig {
        provider_id: ProviderId::parse("fake").unwrap(),
        model_id: ModelId::parse("fake-echo").unwrap(),
        system_prompt: "Be brief.".into(),
    };
    let (run, task) = store
        .transaction(|tx| {
            let decision =
                tx.insert_node(&project, NodeType::Decision, payload(json!({"text": "SQLite"})))?;
            let run = tx.create_run(&project, &config)?;
            let turn = ConversationTurn::user("Add restart tests").to_payload();
            let task = tx.insert_node(&project, NodeType::Conversation, turn)?;
            carry_context(tx, &run.id)?;
            tx.replace_context(&run.id, &[(decision.id, ContextSource::Jev)])?;
            Ok::<_, StoreError>((run, task.id))
        })
        .unwrap();
    let compile_ir = || store.transaction(|tx| compile(tx, &run.id, &task, &[])).unwrap();
    let before = compile_ir();
    let (output_before, _) = infer(&FakeProvider::new(), "fake-echo", &before).await;

    // Durable state that is not projected into IR cannot reach, and so cannot influence, a provider.
    store
        .transaction(|tx| {
            tx.insert_node(
                &project,
                NodeType::Artifact,
                payload(json!({"secret": "not in context"})),
            )
        })
        .unwrap();
    let after = compile_ir();
    assert_eq!(after, before);
    let (output_after, _) = infer(&FakeProvider::new(), "fake-echo", &after).await;
    assert_eq!(output_after, output_before);
    assert!(!output_after.unwrap().contains("not in context"));
}
