//! Applying Jev classifications to the active context: membership only, carried between runs,
//! recorded as events, and fail-closed on invalid output.

mod common;

use common::{memory_store, payload};
use dagos_core::context::{
    ClassificationError, FakeJev, JevClassifier, apply_classification, carry_context,
    classification_request, validate_classification,
};
use dagos_core::domain::{
    Classification, ContextClassification, ContextSource, EdgeType, EventData, JevContextSchema,
    ModelId, NodeClassification, NodeId, NodeType, ProjectId, ProviderId, Run, RunConfig,
};
use dagos_core::store::Store;
use serde_json::json;

fn config() -> RunConfig {
    RunConfig {
        provider_id: ProviderId::parse("fake").unwrap(),
        model_id: ModelId::parse("fake-echo").unwrap(),
        system_prompt: String::new(),
    }
}

fn setup() -> (Store, ProjectId) {
    let store = memory_store();
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    (store, project)
}

fn node(store: &Store, project: &ProjectId, node_type: NodeType, text: &str) -> NodeId {
    store
        .transaction(|tx| tx.insert_node(project, node_type, payload(json!({ "text": text }))))
        .unwrap()
        .id
}

/// Starts a run the way the runtime will: create it, record the message node, carry context.
fn start_run(store: &Store, project: &ProjectId, message: &str) -> (Run, NodeId) {
    store
        .transaction(|tx| {
            let run = tx.create_run(project, &config())?;
            let task = tx.insert_node(
                project,
                NodeType::Conversation,
                payload(json!({"role": "user", "text": message})),
            )?;
            carry_context(tx, &run.id)?;
            Ok::<_, dagos_core::store::StoreError>((run, task.id))
        })
        .unwrap()
}

fn classification(entries: &[(&NodeId, Classification)]) -> ContextClassification {
    ContextClassification {
        schema: JevContextSchema,
        classifications: entries
            .iter()
            .map(|(node_id, classification)| NodeClassification {
                node_id: (*node_id).clone(),
                classification: *classification,
            })
            .collect(),
        tools: vec![],
    }
}

fn membership(store: &Store, run: &Run) -> Vec<(NodeId, ContextSource)> {
    let members = store.transaction(|tx| tx.context(&run.id)).unwrap();
    members.into_iter().map(|member| (member.node_id, member.source)).collect()
}

fn events_after(store: &Store, run: &Run, after: u32) -> Vec<EventData> {
    let events = store.transaction(|tx| tx.events_after(&run.id, after)).unwrap();
    events.into_iter().map(|event| event.data).collect()
}

#[test]
fn first_run_carries_nothing_and_later_runs_carry_the_previous_context() {
    let (store, project) = setup();
    let a = node(&store, &project, NodeType::Decision, "a");
    let (first, _) = start_run(&store, &project, "hello");
    assert!(membership(&store, &first).is_empty());
    assert_eq!(
        events_after(&store, &first, 1),
        [EventData::ContextCarried { from_run_id: None, node_ids: vec![] }]
    );

    let accept = classification(&[(&a, Classification::Active)]);
    store.transaction(|tx| apply_classification(tx, &first.id, &accept)).unwrap();
    store.transaction(|tx| tx.complete_run(&first.id)).unwrap();

    let (second, _) = start_run(&store, &project, "again");
    assert_eq!(membership(&store, &second), [(a.clone(), ContextSource::Carried)]);
    assert_eq!(
        events_after(&store, &second, 1),
        [EventData::ContextCarried { from_run_id: Some(first.id), node_ids: vec![a] }]
    );
}

#[test]
fn requests_exclude_the_task_node_and_flag_current_membership() {
    let (store, project) = setup();
    let old = node(&store, &project, NodeType::Decision, "old");
    let new = node(&store, &project, NodeType::Decision, "new");
    store.transaction(|tx| tx.insert_edge(&project, &new, &old, EdgeType::Supersedes)).unwrap();
    let (first, _) = start_run(&store, &project, "one");
    let keep_old = classification(&[(&old, Classification::Active)]);
    store.transaction(|tx| apply_classification(tx, &first.id, &keep_old)).unwrap();
    store.transaction(|tx| tx.complete_run(&first.id)).unwrap();

    let (run, task) = start_run(&store, &project, "Which storage?");
    let request =
        store.transaction(|tx| classification_request(tx, &run, &task, "Which storage?")).unwrap();

    assert_eq!(request.message, "Which storage?");
    let candidates: Vec<(&NodeId, bool)> =
        request.candidates.iter().map(|c| (&c.node_id, c.in_context)).collect();
    // The first run's message node is a candidate now; this run's message node is the task.
    assert_eq!(candidates.len(), 3);
    assert_eq!(candidates[..2], [(&old, true), (&new, false)]);
    assert!(request.candidates.iter().all(|c| c.node_id != task));
    assert_eq!(request.edges.len(), 1);
    assert_eq!((&request.edges[0].from, &request.edges[0].to), (&new, &old));
}

#[test]
fn classifications_update_membership_only_and_record_why() {
    let (store, project) = setup();
    let [carried_kept, carried_dropped, reaffirmed, fresh, ignored] =
        ["kept", "dropped", "reaffirmed", "fresh", "ignored"]
            .map(|text| node(&store, &project, NodeType::Task, text));
    let (first, _) = start_run(&store, &project, "one");
    let seed = classification(&[
        (&carried_kept, Classification::Active),
        (&carried_dropped, Classification::Active),
        (&reaffirmed, Classification::Active),
    ]);
    store.transaction(|tx| apply_classification(tx, &first.id, &seed)).unwrap();
    store.transaction(|tx| tx.complete_run(&first.id)).unwrap();

    let (run, _) = start_run(&store, &project, "two");
    let nodes_before = store.transaction(|tx| tx.nodes(&project)).unwrap();
    let edges_before = store.transaction(|tx| tx.edges(&project)).unwrap();
    let last_sequence =
        store.transaction(|tx| tx.events(&run.id)).unwrap().last().unwrap().sequence;

    let output = classification(&[
        (&carried_dropped, Classification::Inactive),
        (&reaffirmed, Classification::Active),
        (&fresh, Classification::Active),
        (&ignored, Classification::Inactive),
    ]);
    let changes = store.transaction(|tx| apply_classification(tx, &run.id, &output)).unwrap();

    assert_eq!(changes.added, vec![fresh.clone()]);
    assert_eq!(changes.removed, vec![carried_dropped.clone()]);
    assert_eq!(
        membership(&store, &run),
        [
            (carried_kept, ContextSource::Carried),
            (reaffirmed, ContextSource::Jev),
            (fresh.clone(), ContextSource::Jev),
        ]
    );
    assert_eq!(
        events_after(&store, &run, last_sequence),
        [
            EventData::JevClassified { classification: output },
            EventData::ContextAdded { node_id: fresh },
            EventData::ContextRemoved { node_id: carried_dropped },
        ]
    );
    // Only membership changed: the durable DAG is byte-for-byte the same.
    assert_eq!(store.transaction(|tx| tx.nodes(&project)).unwrap(), nodes_before);
    assert_eq!(store.transaction(|tx| tx.edges(&project)).unwrap(), edges_before);
}

#[test]
fn invalid_classifications_fail_closed() {
    let (store, project) = setup();
    let a = node(&store, &project, NodeType::Task, "a");
    let (run, task) = start_run(&store, &project, "hi");
    let request = store.transaction(|tx| classification_request(tx, &run, &task, "hi")).unwrap();

    let unknown = NodeId::parse("node_999999").unwrap();
    for (raw, expected) in [
        (
            json!({"schema": "kiss.jev-context.v1", "classifications": [
                {"node_id": task.as_str(), "classification": "active"}]}),
            ClassificationError::UnknownCandidate(task.clone()),
        ),
        (
            json!({"schema": "kiss.jev-context.v1", "classifications": [
                {"node_id": unknown.as_str(), "classification": "active"}]}),
            ClassificationError::UnknownCandidate(unknown.clone()),
        ),
        (
            json!({"schema": "kiss.jev-context.v1", "classifications": [
                {"node_id": a.as_str(), "classification": "active"},
                {"node_id": a.as_str(), "classification": "inactive"}]}),
            ClassificationError::DuplicateClassification(a.clone()),
        ),
    ] {
        assert_eq!(validate_classification(&request, &raw.to_string()), Err(expected));
    }
    for raw in
        ["not json", r#"{"schema":"kiss.jev-context.v1","classifications":[],"plan":["ship it"]}"#]
    {
        assert!(matches!(
            validate_classification(&request, raw),
            Err(ClassificationError::Contract(_))
        ));
    }
    assert!(membership(&store, &run).is_empty(), "nothing may be applied from rejected output");
}

#[tokio::test]
async fn fake_jev_drives_membership_and_its_classification_replays() {
    let (store, project) = setup();
    let old = node(&store, &project, NodeType::Decision, "Use JSON files");
    let new = node(&store, &project, NodeType::Decision, "Use SQLite");
    store.transaction(|tx| tx.insert_edge(&project, &new, &old, EdgeType::Supersedes)).unwrap();
    let (run, task) = start_run(&store, &project, "Persist runs");

    let jev = FakeJev::new();
    let request =
        store.transaction(|tx| classification_request(tx, &run, &task, "Persist runs")).unwrap();
    store
        .transaction(|tx| {
            tx.append_event(
                &run.id,
                EventData::JevRequested { jev_id: jev.id().into(), request: request.clone() },
            )
        })
        .unwrap();
    let raw = jev.classify(&request).await.unwrap();
    let output = validate_classification(&request, &raw).unwrap();
    store.transaction(|tx| apply_classification(tx, &run.id, &output)).unwrap();

    assert_eq!(membership(&store, &run), [(new, ContextSource::Jev)]);
    assert!(store.transaction(|tx| tx.node(&old)).unwrap().is_some(), "superseded node is durable");

    // Replay: the recorded request, classified again, reproduces the recorded classification.
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    let recorded_request = events.iter().find_map(|event| match &event.data {
        EventData::JevRequested { request, .. } => Some(request.clone()),
        _ => None,
    });
    let recorded_output = events.iter().find_map(|event| match &event.data {
        EventData::JevClassified { classification } => Some(classification.clone()),
        _ => None,
    });
    let replayed_raw = jev.classify(&recorded_request.unwrap()).await.unwrap();
    assert_eq!(validate_classification(&request, &replayed_raw).unwrap(), recorded_output.unwrap());
}
