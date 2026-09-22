//! Runs, events, and active context: lifecycle consistency, event ordering, and context isolation.

mod common;

use common::{memory_store, payload};
use dagos_core::domain::{
    ContextSource, ErrorCode, EventData, ModelId, NodeId, NodeType, ProjectId, ProviderId, Run,
    RunConfig, RunId, RunStatus,
};
use dagos_core::store::{DagViolation, Store, StoreError};
use serde_json::json;

fn config(provider: &str, model: &str) -> RunConfig {
    RunConfig {
        provider_id: ProviderId::parse(provider).unwrap(),
        model_id: ModelId::parse(model).unwrap(),
        system_prompt: "You are a careful coding assistant.".into(),
    }
}

fn setup() -> (Store, ProjectId) {
    let store = memory_store();
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    (store, project)
}

fn start(store: &Store, project: &ProjectId) -> Run {
    store.transaction(|tx| tx.create_run(project, &config("fake", "fake-echo"))).unwrap()
}

fn node(store: &Store, project: &ProjectId, text: &str) -> NodeId {
    store
        .transaction(|tx| tx.insert_node(project, NodeType::Task, payload(json!({ "text": text }))))
        .unwrap()
        .id
}

fn note(text: &str) -> EventData {
    // Any non-lifecycle event would do; failure events are the only other kind defined so far.
    EventData::RunFailed { error_code: ErrorCode::Internal, message: text.into() }
}

#[test]
fn creating_a_run_records_identity_and_run_started_first() {
    let (store, project) = setup();
    let run = store
        .transaction(|tx| tx.create_run(&project, &config("openai-compatible", "gpt-4o-mini")))
        .unwrap();
    assert_eq!(run.status, RunStatus::Running);
    assert_eq!(run.provider_id.as_str(), "openai-compatible");
    assert_eq!(run.model_id.as_str(), "gpt-4o-mini");
    assert_eq!((run.completed_at, run.error_code), (None, None));

    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].sequence, 1);
    assert_eq!(
        events[0].data,
        EventData::RunStarted {
            provider_id: run.provider_id.clone(),
            model_id: run.model_id.clone(),
            system_prompt: run.system_prompt.clone(),
        }
    );
    assert_eq!(store.transaction(|tx| tx.run(&run.id)).unwrap(), Some(run));
}

#[test]
fn event_sequences_are_monotonic_per_run() {
    let store = memory_store();
    let [first_project, second_project] =
        ["a", "b"].map(|name| store.transaction(|tx| tx.create_project(name)).unwrap().id);
    let first = start(&store, &first_project);
    let second = start(&store, &second_project);

    // Interleave appends across two runs: each run still counts 1, 2, 3, ...
    for index in 0..3 {
        for run in [&first, &second] {
            store.transaction(|tx| tx.append_event(&run.id, note(&format!("{index}")))).unwrap();
        }
    }
    for run in [&first, &second] {
        let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
        let sequences: Vec<u32> = events.iter().map(|event| event.sequence).collect();
        assert_eq!(sequences, [1, 2, 3, 4]);
        assert!(events.windows(2).all(|pair| pair[0].created_at < pair[1].created_at));
        assert!(events.iter().all(|event| event.run_id == run.id));
    }

    let later = store.transaction(|tx| tx.events_after(&first.id, 2)).unwrap();
    assert_eq!(later.iter().map(|event| event.sequence).collect::<Vec<_>>(), [3, 4]);
}

#[test]
fn finishing_a_run_records_the_terminal_event_and_freezes_history() {
    let (store, project) = setup();
    let run = start(&store, &project);
    let completed = store.transaction(|tx| tx.complete_run(&run.id)).unwrap();
    assert_eq!(completed.status, RunStatus::Completed);
    assert!(completed.completed_at.unwrap() > run.started_at);
    assert_eq!(completed.error_code, None);
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    assert_eq!(events.last().unwrap().data, EventData::RunCompleted {});

    fn finished<T>(result: Result<T, StoreError>) -> bool {
        matches!(result, Err(StoreError::RunFinished { status: RunStatus::Completed, .. }))
    }
    assert!(finished(store.transaction(|tx| tx.append_event(&run.id, note("late")))));
    assert!(finished(store.transaction(|tx| tx.complete_run(&run.id))));
    assert!(finished(store.transaction(|tx| tx.fail_run(&run.id, ErrorCode::Internal, "x"))));
    assert_eq!(store.transaction(|tx| tx.events(&run.id)).unwrap(), events);
}

#[test]
fn failing_a_run_records_the_error_code_on_run_and_event() {
    let (store, project) = setup();
    let run = start(&store, &project);
    let failed = store
        .transaction(|tx| tx.fail_run(&run.id, ErrorCode::ProviderTimeout, "no response in 30s"))
        .unwrap();
    assert_eq!(failed.status, RunStatus::Failed);
    assert_eq!(failed.error_code, Some(ErrorCode::ProviderTimeout));
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    assert_eq!(
        events.last().unwrap().data,
        EventData::RunFailed {
            error_code: ErrorCode::ProviderTimeout,
            message: "no response in 30s".into()
        }
    );
}

#[test]
fn one_run_at_a_time_per_project() {
    let (store, project) = setup();
    let first = start(&store, &project);
    let error =
        store.transaction(|tx| tx.create_run(&project, &config("fake", "fake-echo"))).unwrap_err();
    assert!(matches!(error, StoreError::RunInProgress { ref running } if *running == first.id));
    assert_eq!(store.transaction(|tx| tx.running_run(&project)).unwrap(), Some(first.clone()));

    store.transaction(|tx| tx.complete_run(&first.id)).unwrap();
    let second = start(&store, &project);
    assert_eq!(store.transaction(|tx| tx.previous_run(&second.id)).unwrap().unwrap().id, first.id);
    assert_eq!(store.transaction(|tx| tx.previous_run(&first.id)).unwrap(), None);
    let runs: Vec<RunId> =
        store.transaction(|tx| tx.runs(&project)).unwrap().into_iter().map(|run| run.id).collect();
    assert_eq!(runs, [first.id, second.id]);
}

#[test]
fn context_is_ordered_by_node_creation_and_records_its_source() {
    let (store, project) = setup();
    let [a, b, c] = ["a", "b", "c"].map(|text| node(&store, &project, text));
    let run = start(&store, &project);

    let members = store
        .transaction(|tx| {
            tx.replace_context(
                &run.id,
                &[(c.clone(), ContextSource::Jev), (a.clone(), ContextSource::Carried)],
            )
        })
        .unwrap();
    let view: Vec<(&NodeId, u32, ContextSource)> =
        members.iter().map(|m| (&m.node_id, m.ordering, m.source)).collect();
    assert_eq!(view, [(&a, 0, ContextSource::Carried), (&c, 1, ContextSource::Jev)]);
    assert!(members.iter().all(|m| m.run_id == run.id));
    assert_eq!(store.transaction(|tx| tx.context(&run.id)).unwrap(), members);
    assert!(!members.iter().any(|m| m.node_id == b));
}

#[test]
fn removing_context_membership_never_deletes_the_node() {
    let (store, project) = setup();
    let [a, b] = ["a", "b"].map(|text| node(&store, &project, text));
    let run = start(&store, &project);
    let members = [(a.clone(), ContextSource::Jev), (b.clone(), ContextSource::Jev)];
    store.transaction(|tx| tx.replace_context(&run.id, &members)).unwrap();

    let remaining =
        store.transaction(|tx| tx.replace_context(&run.id, &[(a.clone(), ContextSource::Jev)]));
    assert_eq!(remaining.unwrap().len(), 1);
    let removed_node = store.transaction(|tx| tx.node(&b)).unwrap();
    assert!(removed_node.is_some(), "removal from context must not delete the durable node");
    assert_eq!(store.transaction(|tx| tx.nodes(&project)).unwrap().len(), 2);

    store.transaction(|tx| tx.replace_context(&run.id, &[])).unwrap();
    assert!(store.transaction(|tx| tx.context(&run.id)).unwrap().is_empty());
    assert_eq!(store.transaction(|tx| tx.nodes(&project)).unwrap().len(), 2);
}

#[test]
fn context_is_isolated_per_run_and_frozen_once_finished() {
    let (store, project) = setup();
    let [a, b] = ["a", "b"].map(|text| node(&store, &project, text));
    let first = start(&store, &project);
    store
        .transaction(|tx| tx.replace_context(&first.id, &[(a.clone(), ContextSource::Jev)]))
        .unwrap();
    store.transaction(|tx| tx.complete_run(&first.id)).unwrap();

    let second = start(&store, &project);
    store
        .transaction(|tx| tx.replace_context(&second.id, &[(b.clone(), ContextSource::Jev)]))
        .unwrap();

    let first_context = store.transaction(|tx| tx.context(&first.id)).unwrap();
    assert_eq!(first_context.iter().map(|m| &m.node_id).collect::<Vec<_>>(), [&a]);
    let frozen = store.transaction(|tx| tx.replace_context(&first.id, &[]));
    assert!(matches!(frozen, Err(StoreError::RunFinished { .. })));
    assert_eq!(store.transaction(|tx| tx.context(&first.id)).unwrap(), first_context);
}

#[test]
fn context_members_must_be_unique_nodes_of_the_run_project() {
    let (store, project) = setup();
    let other = store.transaction(|tx| tx.create_project("other")).unwrap().id;
    let a = node(&store, &project, "a");
    let foreign = node(&store, &other, "foreign");
    let run = start(&store, &project);

    let result = store.transaction(|tx| {
        tx.replace_context(
            &run.id,
            &[(a.clone(), ContextSource::Jev), (a.clone(), ContextSource::Carried)],
        )
    });
    assert!(matches!(result, Err(StoreError::DuplicateContextMember(ref id)) if *id == a));

    let result = store
        .transaction(|tx| tx.replace_context(&run.id, &[(foreign.clone(), ContextSource::Jev)]));
    assert!(
        matches!(result, Err(StoreError::Dag(DagViolation::UnknownNode(ref id))) if *id == foreign)
    );
    assert!(store.transaction(|tx| tx.context(&run.id)).unwrap().is_empty());
}

#[test]
fn runs_events_and_context_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dagos.sqlite3");
    let (run, events, context) = {
        let store = Store::open(&path).unwrap();
        let project = store.transaction(|tx| tx.create_project("durable")).unwrap().id;
        let a = node(&store, &project, "a");
        let run = store
            .transaction(|tx| {
                tx.create_run(&project, &config("openrouter", "anthropic/claude-sonnet-4.5"))
            })
            .unwrap();
        store.transaction(|tx| tx.replace_context(&run.id, &[(a, ContextSource::Jev)])).unwrap();
        let run = store.transaction(|tx| tx.complete_run(&run.id)).unwrap();
        let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
        let context = store.transaction(|tx| tx.context(&run.id)).unwrap();
        (run, events, context)
    };

    let reopened = Store::open(&path).unwrap();
    let restored = reopened.transaction(|tx| tx.run(&run.id)).unwrap().unwrap();
    assert_eq!(restored, run);
    assert_eq!(restored.model_id.as_str(), "anthropic/claude-sonnet-4.5");
    assert_eq!(reopened.transaction(|tx| tx.events(&run.id)).unwrap(), events);
    assert_eq!(reopened.transaction(|tx| tx.context(&run.id)).unwrap(), context);
}

#[test]
fn a_crashed_run_is_found_after_restart_and_can_be_failed_explicitly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dagos.sqlite3");
    let run = {
        let store = Store::open(&path).unwrap();
        let project = store.transaction(|tx| tx.create_project("crashy")).unwrap().id;
        start(&store, &project)
        // Dropped while still running, as if the process died.
    };

    let reopened = Store::open(&path).unwrap();
    let running = reopened.transaction(|tx| tx.running_runs()).unwrap();
    assert_eq!(running.iter().map(|r| &r.id).collect::<Vec<_>>(), [&run.id]);
    let failed = reopened
        .transaction(|tx| tx.fail_run(&run.id, ErrorCode::Interrupted, "DAGOS stopped mid-run"))
        .unwrap();
    assert_eq!(failed.error_code, Some(ErrorCode::Interrupted));
    assert!(reopened.transaction(|tx| tx.running_runs()).unwrap().is_empty());
}
