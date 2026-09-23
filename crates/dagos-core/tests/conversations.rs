//! Conversations: threads of runs inside a project. The DAG is shared by the project; context
//! carries and the IR's recent turns stay within one conversation.

mod common;

use std::sync::Arc;

use common::memory_store;
use dagos_core::context::FakeJev;
use dagos_core::domain::{EventData, IrEventType, ModelId, ProjectId, ProviderId, Run, RunConfig};
use dagos_core::provider::FakeProvider;
use dagos_core::runtime::{Runtime, Thread};
use dagos_core::store::{Store, StoreError, clean_title};

fn config() -> RunConfig {
    RunConfig {
        provider_id: ProviderId::parse("fake").unwrap(),
        model_id: ModelId::parse("fake-echo").unwrap(),
        system_prompt: String::new(),
    }
}

fn setup() -> (Arc<Store>, Runtime, ProjectId) {
    let store = Arc::new(memory_store());
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let runtime = Runtime::new(store.clone(), Arc::new(FakeJev::new()))
        .with_provider(Arc::new(FakeProvider::new()));
    (store, runtime, project)
}

async fn say(runtime: &Runtime, thread: Thread<'_>, message: &str) -> Run {
    let started = runtime.start_in(thread, message, &config()).unwrap();
    runtime.finish(started).await.unwrap()
}

fn ir_turns(store: &Store, run: &Run) -> Vec<(Option<String>, IrEventType)> {
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    let ir = events
        .into_iter()
        .find_map(|event| match event.data {
            EventData::IrCompiled { ir } => Some(ir),
            _ => None,
        })
        .unwrap();
    ir.recent_events.into_iter().map(|turn| (turn.request, turn.event_type)).collect()
}

#[tokio::test]
async fn new_conversations_are_titled_after_their_first_message_and_keep_their_own_thread() {
    let (store, runtime, project) = setup();
    let first = say(&runtime, Thread::New(&project), "Plan the storage layer").await;
    let second = say(&runtime, Thread::Conversation(&first.conversation_id), "Use SQLite?").await;
    let other = say(&runtime, Thread::New(&project), "Unrelated: write the README").await;
    let latest = say(&runtime, Thread::Latest(&project), "And add a license section").await;

    assert_eq!(second.conversation_id, first.conversation_id);
    assert_ne!(other.conversation_id, first.conversation_id);
    assert_eq!(latest.conversation_id, other.conversation_id, "latest = most recently active");

    let conversations = store.transaction(|tx| tx.conversations(&project)).unwrap();
    let titles: Vec<&str> = conversations.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(titles, ["Unrelated: write the README", "Plan the storage layer"]);

    // The IR's recent turns are this conversation's own, each with the user's request.
    assert_eq!(
        ir_turns(&store, &second),
        [(Some("Plan the storage layer".to_owned()), IrEventType::RunCompleted)]
    );
    assert_eq!(
        ir_turns(&store, &latest),
        [(Some("Unrelated: write the README".to_owned()), IrEventType::RunCompleted)]
    );
    assert!(ir_turns(&store, &other).is_empty(), "a new conversation starts without turns");

    let runs = store.transaction(|tx| tx.conversation_runs(&first.conversation_id)).unwrap();
    assert_eq!(runs.iter().map(|r| &r.id).collect::<Vec<_>>(), [&first.id, &second.id]);
}

#[tokio::test]
async fn context_carries_within_a_conversation_while_the_dag_is_shared_by_the_project() {
    let (store, runtime, project) = setup();
    let first = say(&runtime, Thread::New(&project), "Plan the storage layer").await;
    let second = say(&runtime, Thread::Conversation(&first.conversation_id), "Continue").await;
    let third = say(&runtime, Thread::Conversation(&first.conversation_id), "And then?").await;
    let fresh = say(&runtime, Thread::New(&project), "Something new").await;

    let carried = |run: &Run| {
        let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
        events.into_iter().find_map(|event| match event.data {
            EventData::ContextCarried { from_run_id, node_ids } => Some((from_run_id, node_ids)),
            _ => None,
        })
    };
    assert_eq!(carried(&second).unwrap().0, Some(first.id.clone()));
    let (from, nodes) = carried(&third).unwrap();
    assert_eq!(from, Some(second.id.clone()));
    assert!(!nodes.is_empty());
    assert_eq!(carried(&fresh).unwrap(), (None, vec![]), "a new conversation carries nothing");

    // ...but Jev still sees every durable node of the project as a candidate.
    let events = store.transaction(|tx| tx.events(&fresh.id)).unwrap();
    let candidates = events
        .iter()
        .find_map(|event| match &event.data {
            EventData::JevRequested { request, .. } => Some(request.candidates.len()),
            _ => None,
        })
        .unwrap();
    let created_by_fresh = events
        .iter()
        .filter(|event| matches!(event.data, EventData::DagNodeCreated { .. }))
        .count();
    let nodes = store.transaction(|tx| tx.nodes(&project)).unwrap().len();
    assert!(candidates > 0);
    assert_eq!(
        candidates,
        nodes - 1 - created_by_fresh,
        "every node that existed before the run, except the run's own message"
    );
}

#[test]
fn conversations_are_renamed_and_archived_but_never_deleted() {
    let store = memory_store();
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let conversation =
        store.transaction(|tx| tx.create_conversation(&project, "   first\n\n draft  ")).unwrap();
    assert_eq!(conversation.title, "first draft");

    let renamed =
        store.transaction(|tx| tx.rename_conversation(&conversation.id, "Storage")).unwrap();
    assert_eq!(renamed.title, "Storage");
    let error = store.transaction(|tx| tx.rename_conversation(&conversation.id, "  ")).unwrap_err();
    assert!(matches!(error, StoreError::Invalid(_)));

    let archived =
        store.transaction(|tx| tx.set_conversation_archived(&conversation.id, true)).unwrap();
    assert!(archived.archived_at.is_some());
    assert!(store.transaction(|tx| tx.latest_conversation(&project)).unwrap().is_none());
    let run = store.transaction(|tx| tx.create_run_in(&conversation.id, &config())).unwrap();
    let restored = store.transaction(|tx| tx.conversation(&conversation.id)).unwrap().unwrap();
    assert!(restored.archived_at.is_none(), "a new run restores an archived conversation");
    assert_ne!(restored.updated_at, conversation.updated_at, "and marks it active");
    assert_eq!(run.conversation_id, conversation.id);

    assert_eq!(clean_title(&"x".repeat(100)).unwrap().chars().count(), 81, "80 chars + …");
    assert!(clean_title(" \n ").is_none());
    let renamed = store.transaction(|tx| tx.rename_project(&project, "  Renamed  ")).unwrap();
    assert_eq!(renamed.name, "Renamed");
}
