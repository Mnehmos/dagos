//! End-to-end runs with the fake Jev and the fake provider: no network, every state transition
//! recorded, and every failure explicit.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::memory_store;
use dagos_core::context::FakeJev;
use dagos_core::domain::{
    ContextSource, ConversationTurn, EdgeType, ErrorCode, Event, EventData, ModelId, NodeType,
    ProjectId, ProviderId, Role, Run, RunConfig, RunStatus,
};
use dagos_core::ir::compile;
use dagos_core::provider::FakeProvider;
use dagos_core::runtime::{Runtime, RuntimeError};
use dagos_core::store::{Store, StoreError};

fn config(model: &str) -> RunConfig {
    RunConfig {
        provider_id: ProviderId::parse("fake").unwrap(),
        model_id: ModelId::parse(model).unwrap(),
        system_prompt: "You are a careful coding assistant.".into(),
    }
}

fn setup(jev: FakeJev) -> (Arc<Store>, Runtime, ProjectId) {
    let store = Arc::new(memory_store());
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let runtime = Runtime::new(store.clone(), Arc::new(jev))
        .with_provider(Arc::new(FakeProvider::new()))
        .with_inference_timeout(Duration::from_secs(5));
    (store, runtime, project)
}

fn events(store: &Store, run: &Run) -> Vec<Event> {
    store.transaction(|tx| tx.events(&run.id)).unwrap()
}

/// Event types in order, with each run of consecutive deltas collapsed to `inference.delta*`.
fn timeline(events: &[Event]) -> Vec<String> {
    let mut types: Vec<String> = Vec::new();
    for event in events {
        let event_type = event.data.event_type();
        if event_type == "inference.delta" {
            if types.last().map(String::as_str) != Some("inference.delta*") {
                types.push("inference.delta*".into());
            }
        } else {
            types.push(event_type);
        }
    }
    types
}

#[tokio::test]
async fn a_complete_fake_run_works_offline_and_records_every_transition() {
    let (store, runtime, project) = setup(FakeJev::new());
    let run = runtime.run(&project, "Add restart tests", &config("fake-echo")).await.unwrap();

    assert_eq!(run.status, RunStatus::Completed);
    assert_eq!((run.provider_id.as_str(), run.model_id.as_str()), ("fake", "fake-echo"));
    let events = events(&store, &run);
    assert_eq!(
        timeline(&events),
        [
            "run.started",
            "message.recorded",
            "context.carried",
            "jev.requested",
            "jev.classified",
            "ir.compiled",
            "inference.started",
            "inference.delta*",
            "inference.completed",
            "response.validated",
            "dag.node_created",
            "dag.edge_created",
            "run.completed",
        ]
    );
    let sequences: Vec<u32> = events.iter().map(|event| event.sequence).collect();
    assert_eq!(sequences, (1..=events.len() as u32).collect::<Vec<_>>());

    // The DAG now holds the user's message and the emitted observation, linked by the edge.
    let nodes = store.transaction(|tx| tx.nodes(&project)).unwrap();
    assert_eq!(nodes.len(), 2);
    let (message, observation) = (&nodes[0], &nodes[1]);
    assert_eq!(
        ConversationTurn::from_payload(&message.payload),
        Some(ConversationTurn::user("Add restart tests"))
    );
    assert_eq!(observation.node_type, NodeType::Observation);
    let edges = store.transaction(|tx| tx.edges(&project)).unwrap();
    assert_eq!(edges.len(), 1);
    assert_eq!(
        (&edges[0].from_node_id, &edges[0].to_node_id, edges[0].edge_type),
        (&observation.id, &message.id, EdgeType::ObservedFrom)
    );

    // The recorded IR is exactly what compiles from the run's state; prose streamed separately.
    let recorded_ir = events.iter().find_map(|event| match &event.data {
        EventData::IrCompiled { ir } => Some(ir.clone()),
        _ => None,
    });
    let recompiled = store.transaction(|tx| compile(tx, &run.id, &message.id, &[])).unwrap();
    assert_eq!(recorded_ir.unwrap(), recompiled);
    let streamed: String = events
        .iter()
        .filter_map(|event| match &event.data {
            EventData::InferenceDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let validated = events.iter().find_map(|event| match &event.data {
        EventData::ResponseValidated { response } => Some(response.clone()),
        _ => None,
    });
    assert_eq!(streamed, validated.unwrap().presentation.prose);
}

#[tokio::test]
async fn context_and_conversation_carry_into_the_next_run() {
    let (store, runtime, project) = setup(FakeJev::new());
    let first =
        runtime.run(&project, "Where should state live?", &config("fake-echo")).await.unwrap();
    let second = runtime.run(&project, "Add restart tests", &config("fake-echo")).await.unwrap();
    assert_eq!(second.status, RunStatus::Completed);

    let ir = events(&store, &second)
        .into_iter()
        .find_map(|event| match event.data {
            EventData::IrCompiled { ir } => Some(ir),
            _ => None,
        })
        .unwrap();
    let first_nodes = store.transaction(|tx| tx.nodes(&project)).unwrap();
    let context: Vec<_> = ir.context.iter().map(|item| &item.node_id).collect();
    // The first run's message and its observation are both active in the second run.
    assert_eq!(context, [&first_nodes[0].id, &first_nodes[1].id]);
    assert_eq!(ir.task.message, "Add restart tests");
    // The first run's reply reaches the model through recent events, not through DAG state.
    assert_eq!(ir.recent_events.len(), 1);
    assert_eq!(ir.recent_events[0].run_id, first.id);
    assert!(ir.recent_events[0].prose.as_deref().unwrap().starts_with("fake-echo received"));
    let members = store.transaction(|tx| tx.context(&second.id)).unwrap();
    assert!(members.iter().all(|member| member.source == ContextSource::Jev));
}

#[tokio::test(start_paused = true)]
async fn provider_and_response_failures_are_explicit_and_leave_the_dag_clean() {
    for (model, code, evidence) in [
        ("fake-malformed", ErrorCode::ResponseInvalid, Some("response.rejected")),
        ("fake-invalid-schema", ErrorCode::ResponseInvalid, Some("response.rejected")),
        ("fake-dangling-edge", ErrorCode::ResponseInvalid, Some("response.rejected")),
        ("fake-cycle", ErrorCode::EmissionRejected, Some("response.rejected")),
        ("fake-error", ErrorCode::ProviderFailed, None),
        ("fake-timeout", ErrorCode::ProviderTimeout, None),
    ] {
        let (store, runtime, project) = setup(FakeJev::new());
        let run = runtime.run(&project, "Try this", &config(model)).await.unwrap();

        assert_eq!(run.status, RunStatus::Failed, "{model}");
        assert_eq!(run.error_code, Some(code), "{model}");
        let events = events(&store, &run);
        let timeline = timeline(&events);
        assert_eq!(timeline.last().unwrap(), "run.failed", "{model}");
        let before_failure = &timeline[timeline.len() - 2];
        match evidence {
            Some(evidence) => assert_eq!(before_failure, evidence, "{model}"),
            None => assert_ne!(before_failure, "response.rejected", "{model}"),
        }
        assert!(!timeline.contains(&"response.validated".to_string()), "{model}");
        assert!(!timeline.contains(&"dag.node_created".to_string()), "{model}");

        // Only the user's message is durable: no emission and no prose reached the DAG.
        let nodes = store.transaction(|tx| tx.nodes(&project)).unwrap();
        assert_eq!(nodes.len(), 1, "{model}");
        assert_eq!(ConversationTurn::from_payload(&nodes[0].payload).unwrap().role, Role::User);
        assert!(store.transaction(|tx| tx.edges(&project)).unwrap().is_empty(), "{model}");
        if model != "fake-error" {
            assert!(timeline.contains(&"inference.delta*".to_string()), "{model} streamed");
        }
    }
}

#[tokio::test]
async fn jev_failures_stop_the_run_before_inference() {
    for (jev, code, rejected) in [
        (FakeJev::unavailable("classifier offline"), ErrorCode::JevFailed, false),
        (
            FakeJev::scripted(r#"{"schema":"kiss.jev-context.v1","plan":["route to gpt"]}"#),
            ErrorCode::JevInvalidOutput,
            true,
        ),
    ] {
        let (store, runtime, project) = setup(jev);
        let run = runtime.run(&project, "hi", &config("fake-echo")).await.unwrap();
        assert_eq!(run.error_code, Some(code));
        let events = events(&store, &run);
        let timeline = timeline(&events);
        assert!(!timeline.contains(&"ir.compiled".to_string()));
        assert!(!timeline.contains(&"inference.started".to_string()));
        if rejected {
            let output = events.iter().find_map(|event| match &event.data {
                EventData::JevRejected { output, .. } => Some(output.clone()),
                _ => None,
            });
            assert!(output.unwrap().contains("route to gpt"), "the rejected output is kept");
        }
    }
}

#[tokio::test]
async fn runs_that_cannot_start_create_nothing() {
    let (store, runtime, project) = setup(FakeJev::new());
    let mut unknown = config("gpt-4o");
    unknown.provider_id = ProviderId::parse("openrouter").unwrap();
    let error = runtime.run(&project, "hi", &unknown).await.unwrap_err();
    assert!(matches!(error, RuntimeError::UnknownProvider(ref id) if id.as_str() == "openrouter"));
    assert!(store.transaction(|tx| tx.runs(&project)).unwrap().is_empty());
    assert!(store.transaction(|tx| tx.nodes(&project)).unwrap().is_empty());

    let running = store.transaction(|tx| tx.create_run(&project, &config("fake-echo"))).unwrap();
    let error = runtime.run(&project, "hi", &config("fake-echo")).await.unwrap_err();
    assert!(matches!(error, RuntimeError::Store(StoreError::RunInProgress { .. })));
    assert_eq!(store.transaction(|tx| tx.runs(&project)).unwrap(), vec![running]);
}

#[tokio::test]
async fn interrupted_runs_are_failed_explicitly_on_recovery() {
    let (store, runtime, project) = setup(FakeJev::new());
    let orphan = store.transaction(|tx| tx.create_run(&project, &config("fake-echo"))).unwrap();
    let recovered = runtime.recover_interrupted_runs().unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].id, orphan.id);
    assert_eq!(recovered[0].error_code, Some(ErrorCode::Interrupted));
    let next = runtime.run(&project, "carry on", &config("fake-echo")).await.unwrap();
    assert_eq!(next.status, RunStatus::Completed);
}

#[test]
fn runs_can_be_spawned_onto_a_multithreaded_executor() {
    fn assert_send<T: Send>(_: &T) {}
    let (_, runtime, project) = setup(FakeJev::new());
    let config = config("fake-echo");
    let future = runtime.run(&project, "hi", &config);
    assert_send(&future);
}
