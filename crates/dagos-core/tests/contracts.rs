//! Agreement between the Rust domain types and the published JSON Schema contracts.
//!
//! The schemas are the language-neutral source of truth. Serialized domain values must satisfy
//! them, and documents the schemas reject must also be rejected by the domain types.

use dagos_core::contracts::Contract;
use dagos_core::domain::{DagNode, NodeId, NodeType, Payload, Timestamp};
use serde_json::json;

fn node() -> DagNode {
    let mut payload = Payload::new();
    payload.insert("text".into(), json!("Use SQLite for durable state."));
    DagNode {
        id: NodeId::parse("node_000001").unwrap(),
        node_type: NodeType::Decision,
        payload,
        created_at: Timestamp::parse("2026-09-22T19:19:34.123Z").unwrap(),
        updated_at: Timestamp::parse("2026-09-22T19:19:34.123Z").unwrap(),
    }
}

#[test]
fn serialized_dag_nodes_satisfy_the_node_contract() {
    for node_type in NodeType::ALL {
        let mut node = node();
        node.node_type = *node_type;
        Contract::DagNode
            .validate(&serde_json::to_value(&node).unwrap())
            .unwrap();
    }
}

#[test]
fn node_contract_and_domain_type_reject_the_same_documents() {
    let valid = serde_json::to_value(node()).unwrap();
    let mut cases = Vec::new();
    for (field, value) in [
        ("id", json!("run_000001")),
        ("type", json!("plan")),
        ("payload", json!("text")),
        ("created_at", json!("2026-09-22")),
        ("project_id", json!("proj_000001")),
    ] {
        let mut document = valid.clone();
        document[field] = value;
        cases.push(document);
    }
    let mut missing_timestamp = valid.clone();
    missing_timestamp
        .as_object_mut()
        .unwrap()
        .remove("updated_at");
    cases.push(missing_timestamp);

    for document in cases {
        assert!(
            Contract::DagNode.validate(&document).is_err(),
            "contract accepted {document}"
        );
        assert!(
            serde_json::from_value::<DagNode>(document.clone()).is_err(),
            "domain type accepted {document}"
        );
    }
}
