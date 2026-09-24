//! The DAG-to-IR compiler: deterministic, stable, explicit about the system prompt, and free of
//! storage records. Snapshots live in `tests/snapshots`.

mod common;

use common::{assert_snapshot, memory_store, payload};
use dagos_core::context::{
    FakeJev, JevClassifier, apply_classification, carry_context, classification_request,
    validate_classification,
};
use dagos_core::domain::{
    ConversationTurn, EdgeType, ErrorCode, InferenceIr, IrTool, ModelId, NodeId, NodeType,
    ProjectId, ProviderId, Run, RunConfig,
};
use dagos_core::ir::{CompileError, compile};
use dagos_core::store::{Store, StoreError};
use serde_json::{Value, json};

const SYSTEM_PROMPT: &str = "You are a careful coding assistant. Prefer small, tested changes.";

fn config() -> RunConfig {
    RunConfig {
        provider_id: ProviderId::parse("fake").unwrap(),
        model_id: ModelId::parse("fake-echo").unwrap(),
        system_prompt: SYSTEM_PROMPT.into(),
    }
}

fn node(store: &Store, project: &ProjectId, node_type: NodeType, value: Value) -> NodeId {
    store.transaction(|tx| tx.insert_node(project, node_type, payload(value))).unwrap().id
}

/// Starts a run the way the runtime does: create it, record the user turn, carry context.
fn start_run(store: &Store, project: &ProjectId, message: &str) -> (Run, NodeId) {
    store
        .transaction(|tx| {
            let run = tx.create_run(project, &config())?;
            let turn = ConversationTurn::user(message).to_payload();
            let task = tx.insert_node(project, NodeType::Conversation, turn)?;
            carry_context(tx, &run.id)?;
            Ok::<_, StoreError>((run, task.id))
        })
        .unwrap()
}

async fn classify_with_fake_jev(store: &Store, run: &Run, task: &NodeId, message: &str) {
    let request = store.transaction(|tx| classification_request(tx, run, task, message)).unwrap();
    let raw = FakeJev::new().classify(&request).await.unwrap();
    let output = validate_classification(&request, &raw).unwrap();
    store.transaction(|tx| apply_classification(tx, &run.id, &output)).unwrap();
}

fn compile_json(store: &Store, run: &Run, task: &NodeId, tools: &[IrTool]) -> String {
    let ir = store.transaction(|tx| compile(tx, &run.id, task, tools)).unwrap();
    serde_json::to_string_pretty(&ir).unwrap() + "\n"
}

fn read_file_tool() -> IrTool {
    IrTool {
        name: "read_file".into(),
        description: "Read a file from the workspace.".into(),
        input_schema: payload(json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        })),
    }
}

/// A project with superseded and dependent decisions, one completed run, one failed run, and a
/// third run whose context the fake Jev has classified.
async fn scenario() -> (Store, Run, NodeId) {
    let store = memory_store();
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let json_files =
        node(&store, &project, NodeType::Decision, json!({"text": "Store state in JSON files"}));
    let sqlite =
        node(&store, &project, NodeType::Decision, json!({"text": "Store state in SQLite"}));
    let persist =
        node(&store, &project, NodeType::Task, json!({"title": "Persist runs and events"}));
    store
        .transaction(|tx| {
            tx.insert_edge(&project, &sqlite, &json_files, EdgeType::Supersedes)?;
            tx.insert_edge(&project, &persist, &sqlite, EdgeType::DependsOn)
        })
        .unwrap();

    let (first, task) = start_run(&store, &project, "Where should state live?");
    classify_with_fake_jev(&store, &first, &task, "Where should state live?").await;
    store.transaction(|tx| tx.complete_run(&first.id)).unwrap();

    let (second, task) = start_run(&store, &project, "Add restart tests");
    classify_with_fake_jev(&store, &second, &task, "Add restart tests").await;
    let reason = "response violates kiss://schemas/inference-response/v1: at /emissions/0: \"kind\" is a required property";
    store.transaction(|tx| tx.fail_run(&second.id, ErrorCode::ResponseInvalid, reason)).unwrap();

    let (third, task) = start_run(&store, &project, "Try the restart tests again");
    classify_with_fake_jev(&store, &third, &task, "Try the restart tests again").await;
    (store, third, task)
}

#[tokio::test]
async fn a_first_run_compiles_to_its_task_alone() {
    let store = memory_store();
    let project = store.transaction(|tx| tx.create_project("empty")).unwrap().id;
    let (run, task) = start_run(&store, &project, "Hello, DAGOS");
    classify_with_fake_jev(&store, &run, &task, "Hello, DAGOS").await;
    assert_snapshot("ir_first_run.json", &compile_json(&store, &run, &task, &[]));
}

#[tokio::test]
async fn project_state_compiles_to_the_expected_ir() {
    let (store, run, task) = scenario().await;
    assert_snapshot(
        "ir_with_context.json",
        &compile_json(&store, &run, &task, &[read_file_tool()]),
    );
}

#[tokio::test]
async fn identical_state_compiles_to_identical_ir() {
    let (store, run, task) = scenario().await;
    let first = compile_json(&store, &run, &task, &[]);
    assert_eq!(compile_json(&store, &run, &task, &[]), first);

    // An independently built store with the same history compiles byte-for-byte the same.
    let (other_store, other_run, other_task) = scenario().await;
    assert_eq!(compile_json(&other_store, &other_run, &other_task, &[]), first);
}

#[tokio::test]
async fn system_prompt_is_explicit_and_context_order_is_stable() {
    let (store, run, task) = scenario().await;
    let ir: InferenceIr = store.transaction(|tx| compile(tx, &run.id, &task, &[])).unwrap();
    assert_eq!(ir.system_prompt, SYSTEM_PROMPT);

    // Context follows node creation order, whatever order Jev classified or members were added.
    let members = store.transaction(|tx| tx.context(&run.id)).unwrap();
    let ir_order: Vec<&NodeId> = ir.context.iter().map(|item| &item.node_id).collect();
    let stored_order: Vec<&NodeId> = members.iter().map(|member| &member.node_id).collect();
    assert_eq!(ir_order, stored_order);
    let project = project_of(&store, &run);
    let nodes = store.transaction(|tx| tx.nodes(&project)).unwrap();
    let creation_rank = |id: &NodeId| nodes.iter().position(|node| &node.id == id).unwrap();
    assert!(ir_order.windows(2).all(|pair| creation_rank(pair[0]) < creation_rank(pair[1])));
}

fn project_of(store: &Store, run: &Run) -> ProjectId {
    store.transaction(|tx| tx.run(&run.id)).unwrap().unwrap().project_id
}

#[tokio::test]
async fn ir_contains_no_storage_fields() {
    let (store, run, task) = scenario().await;
    let document: Value = serde_json::from_str(&compile_json(&store, &run, &task, &[])).unwrap();

    fn storage_keys(value: &Value, found: &mut Vec<String>) {
        match value {
            Value::Object(object) => {
                for (key, child) in object {
                    if ["created_at", "updated_at", "project_id", "payload_json"]
                        .contains(&key.as_str())
                    {
                        found.push(key.clone());
                    }
                    // Payloads are user content, not storage records.
                    if key != "payload" {
                        storage_keys(child, found);
                    }
                }
            }
            Value::Array(items) => items.iter().for_each(|item| storage_keys(item, found)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    storage_keys(&document, &mut found);
    assert!(found.is_empty(), "storage fields leaked into IR: {found:?}");
}

#[test]
fn the_task_must_be_a_user_conversation_turn() {
    let store = memory_store();
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let (run, _) = start_run(&store, &project, "hi");
    let not_a_turn = node(&store, &project, NodeType::Task, json!({"title": "not a message"}));
    let result = store.transaction(|tx| compile(tx, &run.id, &not_a_turn, &[]));
    assert!(matches!(result, Err(CompileError::InvalidTask { node_id }) if node_id == not_a_turn));
}
