//! v0.1 hardening: failure deadlines, restart continuity, end-to-end context removal, concurrent
//! event ordering, DAG acyclicity under random mutation, and fail-closed reads of corrupted rows.

mod common;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use common::{memory_store, payload};
use dagos_core::context::FakeJev;
use dagos_core::domain::{
    ErrorCode, EventData, InferenceIr, IrEventType, ModelId, NodeId, NodeType, ProjectId,
    ProviderId, Run, RunConfig, RunStatus,
};
use dagos_core::provider::{
    DeltaSink, FakeProvider, InferenceProvider, InferenceRequest, ProviderError,
};
use dagos_core::runtime::Runtime;
use dagos_core::store::{DagViolation, Store, StoreError};
use serde_json::json;

fn fake_config() -> RunConfig {
    RunConfig {
        provider_id: ProviderId::parse("fake").unwrap(),
        model_id: ModelId::parse("fake-echo").unwrap(),
        system_prompt: "Be brief.".into(),
    }
}

fn runtime(store: Arc<Store>) -> Runtime {
    Runtime::new(store, Arc::new(FakeJev::new())).with_provider(Arc::new(FakeProvider::new()))
}

fn recorded_ir(store: &Store, run: &Run) -> InferenceIr {
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    events
        .into_iter()
        .find_map(|event| match event.data {
            EventData::IrCompiled { ir } => Some(ir),
            _ => None,
        })
        .expect("the run compiled IR")
}

#[tokio::test(start_paused = true)]
async fn a_hung_jev_fails_the_run_at_its_deadline() {
    let store = Arc::new(memory_store());
    let runtime = Runtime::new(store.clone(), Arc::new(FakeJev::hanging()))
        .with_provider(Arc::new(FakeProvider::new()))
        .with_jev_timeout(Duration::from_secs(5));
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let run = runtime.run(&project, "hello", &fake_config()).await.unwrap();
    assert_eq!(run.error_code, Some(ErrorCode::JevFailed));
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    let EventData::RunFailed { message, .. } = &events.last().unwrap().data else { panic!() };
    assert!(message.contains("no classification within 5s"), "{message}");
}

#[tokio::test]
async fn context_and_conversation_continue_across_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dagos.sqlite3");
    let (project, first) = {
        let store = Arc::new(Store::open(&path).unwrap());
        let project = store.transaction(|tx| tx.create_project("durable")).unwrap().id;
        let first = runtime(store).run(&project, "Remember SQLite", &fake_config()).await.unwrap();
        assert_eq!(first.status, RunStatus::Completed);
        (project, first)
    };

    // A new process: fresh store handle and runtime over the same file.
    let store = Arc::new(Store::open(&path).unwrap());
    let second =
        runtime(store.clone()).run(&project, "What did we pick?", &fake_config()).await.unwrap();
    assert_eq!(second.status, RunStatus::Completed);
    let ir = recorded_ir(&store, &second);
    assert_eq!(ir.context.len(), 2, "the first run's message and observation are active again");
    assert_eq!(ir.recent_events.len(), 1);
    assert_eq!(ir.recent_events[0].run_id, first.id);
    assert!(ir.recent_events[0].prose.as_deref().unwrap().contains("Remember SQLite"));
}

#[tokio::test]
async fn a_crash_mid_run_is_recovered_and_reported_to_the_next_run() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dagos.sqlite3");
    let (project, crashed) = {
        let store = Store::open(&path).unwrap();
        let project = store.transaction(|tx| tx.create_project("crashy")).unwrap().id;
        // The process dies after the run started: it stays `running` in the database.
        let crashed = store.transaction(|tx| tx.create_run(&project, &fake_config())).unwrap();
        (project, crashed)
    };

    let store = Arc::new(Store::open(&path).unwrap());
    let runtime = runtime(store.clone());
    let recovered = runtime.recover_interrupted_runs().unwrap();
    assert_eq!(recovered.iter().map(|run| &run.id).collect::<Vec<_>>(), [&crashed.id]);
    let next = runtime.run(&project, "carry on", &fake_config()).await.unwrap();
    assert_eq!(next.status, RunStatus::Completed);
    let outcome = &recorded_ir(&store, &next).recent_events[0];
    assert_eq!(
        (outcome.event_type, outcome.error_code),
        (IrEventType::RunFailed, Some(ErrorCode::Interrupted))
    );
}

/// Answers by task message: `decide` records a decision, `revise` supersedes it, anything else
/// records nothing.
struct Decider {
    id: ProviderId,
}

#[async_trait]
impl InferenceProvider for Decider {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn suggested_models(&self) -> Vec<ModelId> {
        Vec::new()
    }

    async fn infer(
        &self,
        request: InferenceRequest<'_>,
        _deltas: &mut dyn DeltaSink,
    ) -> Result<String, ProviderError> {
        let ir = request.ir;
        let emissions = match ir.task.message.as_str() {
            "decide" => json!([
                {"kind": "node", "ref": "d1", "type": "decision", "payload": {"text": "JSON files"}}
            ]),
            "revise" => {
                let old = ir
                    .context
                    .iter()
                    .find(|item| item.payload.get("text") == Some(&json!("JSON files")))
                    .expect("the old decision is in context")
                    .node_id
                    .clone();
                json!([
                    {"kind": "node", "ref": "d2", "type": "decision", "payload": {"text": "SQLite"}},
                    {"kind": "edge", "from": "d2", "to": old, "type": "supersedes"}
                ])
            }
            _ => json!([]),
        };
        Ok(json!({
            "schema": "kiss.inference-response.v1",
            "presentation": {"prose": "ok"},
            "emissions": emissions
        })
        .to_string())
    }
}

#[tokio::test]
async fn a_superseded_decision_leaves_the_context_but_stays_durable() {
    let store = Arc::new(memory_store());
    let decider = Decider { id: ProviderId::parse("decider").unwrap() };
    let runtime =
        Runtime::new(store.clone(), Arc::new(FakeJev::new())).with_provider(Arc::new(decider));
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let config = RunConfig { provider_id: ProviderId::parse("decider").unwrap(), ..fake_config() };

    for message in ["decide", "revise"] {
        let run = runtime.run(&project, message, &config).await.unwrap();
        assert_eq!(run.status, RunStatus::Completed, "{message}");
    }
    let old = store
        .transaction(|tx| tx.nodes(&project))
        .unwrap()
        .into_iter()
        .find(|node| node.payload.get("text") == Some(&json!("JSON files")))
        .unwrap()
        .id;

    let check = runtime.run(&project, "check", &config).await.unwrap();
    let events = store.transaction(|tx| tx.events(&check.id)).unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.data,
        EventData::ContextCarried { node_ids, .. } if node_ids.contains(&old)
    )));
    assert!(events.iter().any(|event| matches!(
        &event.data,
        EventData::ContextRemoved { node_id } if *node_id == old
    )));
    let members = store.transaction(|tx| tx.context(&check.id)).unwrap();
    assert!(members.iter().all(|member| member.node_id != old));
    assert!(recorded_ir(&store, &check).context.iter().all(|item| item.node_id != old));
    assert!(store.transaction(|tx| tx.node(&old)).unwrap().is_some(), "the node is durable");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_runs_in_different_projects_keep_their_own_ordered_histories() {
    let store = Arc::new(memory_store());
    let runtime =
        Arc::new(Runtime::new(store.clone(), Arc::new(FakeJev::new())).with_provider(Arc::new(
            FakeProvider::new().with_delta_delay(Duration::from_millis(1)),
        )));
    let projects: Vec<ProjectId> = (0..4)
        .map(|index| store.transaction(|tx| tx.create_project(&format!("p{index}"))).unwrap().id)
        .collect();
    let tasks: Vec<_> = projects
        .into_iter()
        .map(|project| {
            let runtime = runtime.clone();
            tokio::spawn(async move {
                let mut runs = Vec::new();
                for turn in 0..3 {
                    let message = format!("turn {turn}");
                    runs.push(runtime.run(&project, &message, &fake_config()).await.unwrap());
                }
                runs
            })
        })
        .collect();
    for task in tasks {
        for run in task.await.unwrap() {
            assert_eq!(run.status, RunStatus::Completed);
            let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
            let sequences: Vec<u32> = events.iter().map(|event| event.sequence).collect();
            assert_eq!(sequences, (1..=events.len() as u32).collect::<Vec<_>>());
            assert!(events.iter().all(|event| event.run_id == run.id));
            assert!(events.windows(2).all(|pair| pair[0].created_at <= pair[1].created_at));
        }
    }
}

/// A small deterministic PRNG (xorshift64*), so the fuzz case is reproducible.
struct Prng(u64);

impl Prng {
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % bound as u64) as usize
    }
}

fn reaches(edges: &[(usize, usize)], from: usize, to: usize) -> bool {
    let mut stack = vec![from];
    let mut seen = [false; 64];
    while let Some(node) = stack.pop() {
        if node == to {
            return true;
        }
        if !std::mem::replace(&mut seen[node], true) {
            stack.extend(edges.iter().filter(|(a, _)| *a == node).map(|(_, b)| *b));
        }
    }
    false
}

#[test]
fn random_edge_insertions_never_create_a_cycle() {
    let store = memory_store();
    let project = store.transaction(|tx| tx.create_project("fuzz")).unwrap().id;
    let nodes: Vec<NodeId> = (0..12)
        .map(|index| {
            let node = store.transaction(|tx| {
                tx.insert_node(&project, NodeType::Task, payload(json!({ "index": index })))
            });
            node.unwrap().id
        })
        .collect();
    let types = dagos_core::domain::EdgeType::ALL;
    let mut accepted: Vec<(usize, usize, usize)> = Vec::new();
    let mut prng = Prng(0x5eed_da60_5eed_da60);
    for _ in 0..400 {
        let (from, to, kind) =
            (prng.below(nodes.len()), prng.below(nodes.len()), prng.below(types.len()));
        let result =
            store.transaction(|tx| tx.insert_edge(&project, &nodes[from], &nodes[to], types[kind]));
        let graph: Vec<(usize, usize)> = accepted.iter().map(|(a, b, _)| (*a, *b)).collect();
        match result {
            Ok(_) => {
                assert!(!reaches(&graph, to, from), "accepted an edge that closes a cycle");
                accepted.push((from, to, kind));
            }
            Err(StoreError::Dag(DagViolation::SelfLoop(_))) => assert_eq!(from, to),
            Err(StoreError::Dag(DagViolation::DuplicateEdge { .. })) => {
                assert!(accepted.contains(&(from, to, kind)));
            }
            Err(StoreError::Dag(DagViolation::Cycle { .. })) => {
                assert!(reaches(&graph, to, from), "rejected an edge that closes no cycle");
            }
            Err(other) => panic!("unexpected error: {other}"),
        }
    }
    assert!(accepted.len() > 20, "the fuzz case exercised the DAG ({} edges)", accepted.len());
    assert_eq!(store.transaction(|tx| tx.edges(&project)).unwrap().len(), accepted.len());
}

#[test]
fn corrupted_rows_fail_closed_on_read() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dagos.sqlite3");
    let (project, run) = {
        let store = Store::open(&path).unwrap();
        let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
        let run = store.transaction(|tx| tx.create_run(&project, &fake_config())).unwrap();
        (project, run)
    };
    // Tamper below the repository API, where only the schema's own constraints apply.
    let raw = rusqlite::Connection::open(&path).unwrap();
    raw.execute_batch(&format!(
        "INSERT INTO dag_nodes VALUES ('not an id', '{project}', 'task', '{{}}', 't', 't');
         INSERT INTO events VALUES ('evt_forged', '{run}', 2, 'run.exploded', '{{}}', 't');",
        run = run.id
    ))
    .unwrap();
    drop(raw);

    let store = Store::open(&path).unwrap();
    assert!(
        store.transaction(|tx| tx.nodes(&project)).is_err(),
        "an invalid node id is not trusted"
    );
    assert!(
        store.transaction(|tx| tx.events(&run.id)).is_err(),
        "an unknown event type is not trusted"
    );
    // Unaffected records remain readable.
    assert_eq!(store.transaction(|tx| tx.run(&run.id)).unwrap().unwrap().id, run.id);
    assert_eq!(store.transaction(|tx| tx.runs(&project)).unwrap().len(), 1);
    assert!(store.transaction(|tx| tx.edges(&project)).unwrap().is_empty());
}

#[test]
fn file_stores_commit_through_a_write_ahead_log() {
    // Every streamed delta is a committed event; WAL keeps those commits cheap.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dagos.sqlite3");
    let store = Store::open(&path).unwrap();
    store.transaction(|tx| tx.create_project("demo")).unwrap();
    assert!(dir.path().join("dagos.sqlite3-wal").exists(), "commits go to the write-ahead log");
    drop(store);
    let reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.transaction(|tx| tx.projects()).unwrap().len(), 1, "and survive a restart");
}
