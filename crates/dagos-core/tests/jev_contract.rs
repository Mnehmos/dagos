//! The Jev contract is classification-only: node IDs plus a closed label, nothing else.
//!
//! These tests throw agent-shaped output at the boundary — plans, actions, tool calls, provider
//! routing, prose — and require it to fail closed, both at the schema and in the Rust mirror.

mod common;

use common::payload;
use dagos_core::contracts::{Contract, ContractError};
use dagos_core::domain::{
    Classification, ContextClassification, EdgeType, JevCandidate, JevEdge, JevRequest,
    JevRequestSchema, JevToolCandidate, NodeId, NodeType,
};
use serde_json::{Value, json};

fn valid_output() -> Value {
    json!({
        "schema": "kiss.jev-context.v1",
        "classifications": [
            {"node_id": "node_000001", "classification": "active"},
            {"node_id": "node_000002", "classification": "inactive"}
        ]
    })
}

fn parse(document: &Value) -> Result<ContextClassification, ContractError> {
    Contract::JevContext.parse(&document.to_string())
}

/// Asserts both the schema and the typed mirror reject `document`.
fn assert_rejected(document: Value) {
    assert!(
        matches!(parse(&document), Err(ContractError::Violation(_))),
        "contract accepted {document}"
    );
    assert!(
        serde_json::from_value::<ContextClassification>(document.clone()).is_err(),
        "domain type accepted {document}"
    );
}

#[test]
fn a_classification_document_parses_into_labels_only() {
    let output = parse(&valid_output()).unwrap();
    let labels: Vec<(&str, Classification)> = output
        .classifications
        .iter()
        .map(|entry| (entry.node_id.as_str(), entry.classification))
        .collect();
    assert_eq!(
        labels,
        [("node_000001", Classification::Active), ("node_000002", Classification::Inactive)]
    );
    assert!(parse(&json!({"schema": "kiss.jev-context.v1", "classifications": []})).is_ok());
}

#[test]
fn agent_shaped_top_level_fields_are_rejected() {
    for (field, value) in [
        ("plan", json!(["refactor the store", "then add tests"])),
        ("actions", json!([{"type": "edit_file", "path": "src/lib.rs"}])),
        ("tool_calls", json!([{"name": "shell", "arguments": {"cmd": "rm -rf target"}}])),
        ("provider", json!("openrouter")),
        ("model", json!("gpt-4o")),
        ("route", json!({"provider": "z.ai"})),
        ("next_step", json!("ask the user")),
        ("reasoning", json!("I think we should...")),
        ("prose", json!("Here is my plan.")),
        ("recovery", json!({"retry": true})),
    ] {
        let mut document = valid_output();
        document[field] = value;
        assert_rejected(document);
    }
}

#[test]
fn classification_entries_carry_no_extra_data() {
    for (field, value) in [
        ("reason", json!("relevant to the message")),
        ("action", json!("load_file")),
        ("tool", json!("grep")),
        ("provider", json!("fake")),
        ("priority", json!(1)),
    ] {
        let mut document = valid_output();
        document["classifications"][0][field] = value;
        assert_rejected(document);
    }
}

#[test]
fn only_the_two_classification_labels_exist() {
    for label in ["include", "remove", "pinned", "execute", "plan", "ACTIVE", ""] {
        let mut document = valid_output();
        document["classifications"][0]["classification"] = json!(label);
        assert_rejected(document);
    }
}

#[test]
fn malformed_documents_are_rejected() {
    let mut wrong_version = valid_output();
    wrong_version["schema"] = json!("kiss.jev-context.v2");
    let mut not_a_node = valid_output();
    not_a_node["classifications"][0]["node_id"] = json!("run_000001");
    let mut missing_label = valid_output();
    missing_label["classifications"][0].as_object_mut().unwrap().remove("classification");
    for document in [
        wrong_version,
        not_a_node,
        missing_label,
        json!({"schema": "kiss.jev-context.v1"}),
        json!({"classifications": []}),
        json!([]),
        json!("active"),
    ] {
        assert_rejected(document);
    }

    for raw in ["", "not json", "{\"schema\": ", "```json\n{}\n```"] {
        assert!(
            matches!(
                Contract::JevContext.parse::<ContextClassification>(raw),
                Err(ContractError::NotJson { .. })
            ),
            "accepted {raw:?}"
        );
    }
}

fn request() -> JevRequest {
    JevRequest {
        schema: JevRequestSchema,
        message: "Add restart tests".into(),
        candidates: vec![
            JevCandidate {
                node_id: NodeId::parse("node_000001").unwrap(),
                node_type: NodeType::Decision,
                payload: payload(json!({"text": "Use SQLite"})),
                in_context: true,
            },
            JevCandidate {
                node_id: NodeId::parse("node_000002").unwrap(),
                node_type: NodeType::Decision,
                payload: payload(json!({"text": "Use SQLite with WAL"})),
                in_context: false,
            },
        ],
        edges: vec![JevEdge {
            from: NodeId::parse("node_000002").unwrap(),
            to: NodeId::parse("node_000001").unwrap(),
            edge_type: EdgeType::Supersedes,
        }],
        tools: vec![JevToolCandidate {
            name: "ooda.read_file".into(),
            description: "Read file contents.".into(),
        }],
    }
}

#[test]
fn requests_satisfy_the_request_contract_and_round_trip() {
    let document = serde_json::to_value(request()).unwrap();
    Contract::JevRequest.validate(&document).unwrap();
    assert_eq!(document["schema"], "kiss.jev-request.v1");
    let parsed: JevRequest = Contract::JevRequest.parse(&document.to_string()).unwrap();
    assert_eq!(parsed, request());
}

#[test]
fn requests_reject_unknown_fields() {
    let mut document = serde_json::to_value(request()).unwrap();
    document["providers"] = json!(["fake"]);
    assert!(Contract::JevRequest.validate(&document).is_err());
    assert!(serde_json::from_value::<JevRequest>(document).is_err());

    let mut document = serde_json::to_value(request()).unwrap();
    document["candidates"][0]["created_at"] = json!("2026-01-01T00:00:00.000Z");
    assert!(Contract::JevRequest.validate(&document).is_err());
    assert!(serde_json::from_value::<JevRequest>(document).is_err());
}
