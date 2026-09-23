//! Inference IR v1 (`kiss.inference-ir.v1`): the only provider-facing contract.
//!
//! Valid IR round-trips through the schema and the Rust mirror; documents carrying storage records
//! or fields outside the contract are rejected by both.

mod common;

use common::payload;
use dagos_core::contracts::Contract;
use dagos_core::domain::{
    DagNode, EdgeType, ErrorCode, InferenceIr, InferenceIrSchema, IrContextItem, IrEvent,
    IrEventType, IrRelation, IrTask, IrTool, NodeId, NodeType, RunId, Timestamp,
};
use serde_json::{Value, json};

fn node_id(n: u32) -> NodeId {
    NodeId::parse(format!("node_{n:06}")).unwrap()
}

fn ir(tools: Vec<IrTool>) -> InferenceIr {
    InferenceIr {
        schema: InferenceIrSchema,
        system_prompt: "You are a careful coding assistant.".into(),
        task: IrTask { node_id: node_id(4), message: "Add restart tests".into() },
        context: vec![
            IrContextItem {
                node_id: node_id(1),
                node_type: NodeType::Decision,
                payload: payload(json!({"text": "Use SQLite"})),
                relations: vec![],
            },
            IrContextItem {
                node_id: node_id(2),
                node_type: NodeType::Task,
                payload: payload(json!({"title": "Persist runs"})),
                relations: vec![IrRelation { edge_type: EdgeType::DependsOn, to: node_id(1) }],
            },
        ],
        recent_events: vec![
            IrEvent {
                run_id: RunId::parse("run_000001").unwrap(),
                event_type: IrEventType::RunCompleted,
                request: Some("Which storage should we use?".into()),
                error_code: None,
                message: None,
                prose: Some("Recorded the storage decision.".into()),
            },
            IrEvent {
                run_id: RunId::parse("run_000002").unwrap(),
                event_type: IrEventType::RunFailed,
                request: Some("Add restart tests".into()),
                error_code: Some(ErrorCode::ResponseInvalid),
                message: Some("at /emissions/0: \"kind\" is a required property".into()),
                prose: None,
            },
        ],
        tools,
        tool_results: vec![],
        recalled: Vec::new(),
    }
}

fn tool() -> IrTool {
    IrTool {
        name: "read_file".into(),
        description: "Read a file from the workspace.".into(),
        input_schema: payload(
            json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        ),
    }
}

fn assert_rejected(document: Value) {
    assert!(Contract::InferenceIr.validate(&document).is_err(), "contract accepted {document}");
    assert!(
        serde_json::from_value::<InferenceIr>(document.clone()).is_err(),
        "domain type accepted {document}"
    );
}

#[test]
fn compiled_ir_satisfies_the_contract_and_round_trips() {
    for ir in [ir(vec![]), ir(vec![tool()])] {
        let document = serde_json::to_value(&ir).unwrap();
        Contract::InferenceIr.validate(&document).unwrap();
        let parsed: InferenceIr = Contract::InferenceIr.parse(&document.to_string()).unwrap();
        assert_eq!(parsed, ir);
    }
}

#[test]
fn tools_are_optional_and_omitted_when_absent() {
    let document = serde_json::to_value(ir(vec![])).unwrap();
    assert!(document.get("tools").is_none());
    let with_tools = serde_json::to_value(ir(vec![tool()])).unwrap();
    assert_eq!(with_tools["tools"][0]["name"], "read_file");
}

#[test]
fn serialization_is_deterministic() {
    let minimal = InferenceIr {
        schema: InferenceIrSchema,
        system_prompt: "Be brief.".into(),
        task: IrTask { node_id: node_id(1), message: "hi".into() },
        context: vec![],
        recent_events: vec![],
        tools: vec![],
        tool_results: vec![],
        recalled: Vec::new(),
    };
    assert_eq!(
        serde_json::to_string(&minimal).unwrap(),
        r#"{"schema":"kiss.inference-ir.v1","system_prompt":"Be brief.","task":{"node_id":"node_000001","message":"hi"},"context":[],"recent_events":[]}"#
    );
}

#[test]
fn storage_fields_never_cross_the_provider_boundary() {
    let valid = serde_json::to_value(ir(vec![])).unwrap();
    for (field, value) in [
        ("created_at", json!("2026-01-01T00:00:00.000Z")),
        ("updated_at", json!("2026-01-01T00:00:00.000Z")),
        ("project_id", json!("proj_000001")),
        ("payload_json", json!("{}")),
        ("id", json!("node_000001")),
    ] {
        let mut document = valid.clone();
        document["context"][0][field] = value;
        assert_rejected(document);
    }
}

#[test]
fn a_raw_dag_record_is_not_a_context_item() {
    let raw_node = DagNode {
        id: node_id(1),
        node_type: NodeType::Decision,
        payload: payload(json!({"text": "Use SQLite"})),
        created_at: Timestamp::parse("2026-01-01T00:00:00.000Z").unwrap(),
        updated_at: Timestamp::parse("2026-01-01T00:00:00.000Z").unwrap(),
    };
    let mut document = serde_json::to_value(ir(vec![])).unwrap();
    document["context"] = json!([serde_json::to_value(raw_node).unwrap()]);
    assert_rejected(document);
}

#[test]
fn fields_outside_the_contract_are_rejected() {
    let valid = serde_json::to_value(ir(vec![tool()])).unwrap();
    let mut cases = Vec::new();
    for field in ["dag", "nodes", "provider", "model", "run_id", "project_id"] {
        let mut document = valid.clone();
        document[field] = json!("x");
        cases.push(document);
    }
    let mutations: [(&str, Value); 8] = [
        ("/schema", json!("kiss.inference-ir.v2")),
        ("/task/extra", json!(true)),
        ("/context/0/type", json!("plan")),
        ("/context/1/relations/0/type", json!("blocks")),
        ("/recent_events/0/type", json!("run.retried")),
        ("/recent_events/0/detail", json!("x")),
        ("/tools/0/input_schema", json!("not an object")),
        ("/tools/0/name", json!("")),
    ];
    for (pointer, value) in mutations {
        let mut document = valid.clone();
        let (parent, key) = pointer.rsplit_once('/').unwrap();
        let target =
            if parent.is_empty() { &mut document } else { document.pointer_mut(parent).unwrap() };
        target[key] = value;
        cases.push(document);
    }
    let mut missing_message = valid.clone();
    missing_message["task"].as_object_mut().unwrap().remove("message");
    cases.push(missing_message);
    let mut missing_recent_events = valid.clone();
    missing_recent_events.as_object_mut().unwrap().remove("recent_events");
    cases.push(missing_recent_events);

    for document in cases {
        assert!(Contract::InferenceIr.validate(&document).is_err(), "contract accepted {document}");
    }
}
