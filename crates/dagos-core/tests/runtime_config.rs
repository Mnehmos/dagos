//! Run configuration: persisted per-project defaults for provider, model, and the editable system
//! prompt; committed-event observation; and provider/model independence of DAG semantics.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::{deterministic, memory_store};
use dagos_core::context::FakeJev;
use dagos_core::domain::{
    DagNode, Event, EventData, InferenceIr, ModelId, ProjectId, ProviderId, Run, RunConfig,
    RunStatus,
};
use dagos_core::provider::{
    DeltaSink, FakeProvider, InferenceProvider, InferenceRequest, ProviderError,
};
use dagos_core::runtime::{Runtime, RuntimeError};
use dagos_core::store::{Store, StoreError};

fn config(provider: &str, model: &str, prompt: &str) -> RunConfig {
    RunConfig {
        provider_id: ProviderId::parse(provider).unwrap(),
        model_id: ModelId::parse(model).unwrap(),
        system_prompt: prompt.into(),
    }
}

/// A different provider identity whose answers are the fake's `fake-echo` answers.
struct Renamed {
    id: ProviderId,
}

#[async_trait]
impl InferenceProvider for Renamed {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn suggested_models(&self) -> Vec<ModelId> {
        vec![ModelId::parse("other-model").unwrap()]
    }

    async fn infer(
        &self,
        request: InferenceRequest<'_>,
        deltas: &mut dyn DeltaSink,
    ) -> Result<String, ProviderError> {
        let echo = ModelId::parse("fake-echo").unwrap();
        FakeProvider::new()
            .infer(InferenceRequest { model_id: &echo, ir: request.ir }, deltas)
            .await
    }
}

fn runtime(store: Arc<Store>) -> Runtime {
    Runtime::new(store, Arc::new(FakeJev::new()))
        .with_provider(Arc::new(FakeProvider::new()))
        .with_provider(Arc::new(Renamed { id: ProviderId::parse("other").unwrap() }))
}

fn project(store: &Store) -> ProjectId {
    store.transaction(|tx| tx.create_project("demo")).unwrap().id
}

fn recorded_ir(store: &Store, run: &Run) -> InferenceIr {
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    events
        .into_iter()
        .find_map(|event| match event.data {
            EventData::IrCompiled { ir } => Some(ir),
            _ => None,
        })
        .unwrap()
}

#[test]
fn defaults_are_per_project_updatable_and_validated() {
    let store = Arc::new(memory_store());
    let runtime = runtime(store.clone());
    let project = project(&store);
    assert_eq!(runtime.defaults(&project).unwrap(), None);

    let first = config("fake", "fake-echo", "Be brief.");
    runtime.set_defaults(&project, &first).unwrap();
    assert_eq!(runtime.defaults(&project).unwrap(), Some(first));

    let second = config("other", "other-model", "Explain your reasoning.");
    runtime.set_defaults(&project, &second).unwrap();
    assert_eq!(runtime.defaults(&project).unwrap(), Some(second.clone()));

    let unknown = config("openrouter", "some/model", "");
    assert!(matches!(
        runtime.set_defaults(&project, &unknown),
        Err(RuntimeError::UnknownProvider(ref id)) if id.as_str() == "openrouter"
    ));
    assert_eq!(runtime.defaults(&project).unwrap(), Some(second));

    let missing = ProjectId::parse("proj_999999").unwrap();
    let error = store.transaction(|tx| tx.set_run_defaults(&missing, &unknown)).unwrap_err();
    assert!(matches!(error, StoreError::NotFound { kind: "project", .. }));
}

#[test]
fn defaults_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dagos.sqlite3");
    let chosen = config("fake", "fake-malformed", "Answer in JSON only.");
    let project = {
        let store = Store::open(&path).unwrap();
        let project = project(&store);
        store.transaction(|tx| tx.set_run_defaults(&project, &chosen)).unwrap();
        project
    };
    let reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.transaction(|tx| tx.run_defaults(&project)).unwrap(), Some(chosen));
}

#[tokio::test]
async fn an_edited_system_prompt_changes_subsequent_runs_only() {
    let store = Arc::new(memory_store());
    let runtime = runtime(store.clone());
    let project = project(&store);

    runtime.set_defaults(&project, &config("fake", "fake-echo", "Version one.")).unwrap();
    let first_config = runtime.defaults(&project).unwrap().unwrap();
    let first = runtime.run(&project, "hello", &first_config).await.unwrap();

    runtime.set_defaults(&project, &config("fake", "fake-echo", "Version two.")).unwrap();
    let second_config = runtime.defaults(&project).unwrap().unwrap();
    let second = runtime.run(&project, "hello again", &second_config).await.unwrap();

    assert_eq!(recorded_ir(&store, &first).system_prompt, "Version one.");
    assert_eq!(recorded_ir(&store, &second).system_prompt, "Version two.");
    let first = store.transaction(|tx| tx.run(&first.id)).unwrap().unwrap();
    assert_eq!(first.system_prompt, "Version one.", "a run keeps the configuration it ran with");
}

/// Runs the same two messages in a fresh deterministic store with `config`.
async fn history_with(config: &RunConfig) -> (Vec<Run>, Vec<InferenceIr>, Vec<DagNode>, usize) {
    let store = Arc::new(deterministic(Store::open_in_memory().unwrap()));
    let runtime = runtime(store.clone());
    let project = project(&store);
    let mut runs = Vec::new();
    for message in ["Where should state live?", "Add restart tests"] {
        runs.push(runtime.run(&project, message, config).await.unwrap());
    }
    let irs = runs.iter().map(|run| recorded_ir(&store, run)).collect();
    let nodes = store.transaction(|tx| tx.nodes(&project)).unwrap();
    let edges = store.transaction(|tx| tx.edges(&project)).unwrap().len();
    (runs, irs, nodes, edges)
}

#[tokio::test]
async fn changing_provider_or_model_does_not_change_dag_semantics() {
    let (fake_runs, fake_irs, fake_nodes, fake_edges) =
        history_with(&config("fake", "fake-echo", "Same prompt.")).await;
    let (other_runs, other_irs, other_nodes, other_edges) =
        history_with(&config("other", "other-model", "Same prompt.")).await;

    assert!(fake_runs.iter().chain(&other_runs).all(|run| run.status == RunStatus::Completed));
    assert_eq!(
        (other_runs[0].provider_id.as_str(), other_runs[0].model_id.as_str()),
        ("other", "other-model"),
        "identity is recorded on each run"
    );
    // Identical IR and identical durable DAG: provider and model never enter DAG semantics.
    assert_eq!(fake_irs, other_irs);
    assert_eq!(fake_nodes, other_nodes);
    assert_eq!(fake_edges, other_edges);
}

#[tokio::test]
async fn listeners_observe_committed_events_in_order_and_never_rolled_back_ones() {
    let seen: Arc<Mutex<Vec<Event>>> = Arc::default();
    let sink = seen.clone();
    let store = Arc::new(
        memory_store()
            .with_event_listener(move |events| sink.lock().unwrap().extend_from_slice(events)),
    );
    let runtime = runtime(store.clone());
    let project = project(&store);

    // fake-cycle: its emissions are applied, hit a cycle, and roll back.
    let run =
        runtime.run(&project, "cycle please", &config("fake", "fake-cycle", "")).await.unwrap();
    assert_eq!(run.status, RunStatus::Failed);

    let observed = seen.lock().unwrap().clone();
    let stored = store.transaction(|tx| tx.events(&run.id)).unwrap();
    assert_eq!(observed, stored, "listeners see exactly the committed history, in order");
    assert!(!observed.iter().any(|event| matches!(event.data, EventData::DagNodeCreated { .. })));
    assert!(observed.iter().any(|event| matches!(event.data, EventData::InferenceDelta { .. })));
}
