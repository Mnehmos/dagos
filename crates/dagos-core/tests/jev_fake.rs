//! The deterministic fake Jev: offline, contract-conforming, and replayable.

mod common;

use common::payload;
use dagos_core::context::{FakeJev, JevClassifier, JevError};
use dagos_core::contracts::Contract;
use dagos_core::domain::{
    Classification, ContextClassification, EdgeType, JevCandidate, JevEdge, JevRequest,
    JevRequestSchema, NodeId, NodeType,
};
use serde_json::json;

fn id(n: u32) -> NodeId {
    NodeId::parse(format!("node_{n:06}")).unwrap()
}

fn candidate(n: u32, node_type: NodeType) -> JevCandidate {
    JevCandidate {
        node_id: id(n),
        node_type,
        payload: payload(json!({ "text": format!("node {n}") })),
        in_context: n.is_multiple_of(2),
    }
}

fn request(candidates: Vec<JevCandidate>, edges: Vec<JevEdge>) -> JevRequest {
    JevRequest { schema: JevRequestSchema, message: "What next?".into(), candidates, edges }
}

async fn classify(jev: &FakeJev, request: &JevRequest) -> Vec<(NodeId, Classification)> {
    let raw = jev.classify(request).await.unwrap();
    let output: ContextClassification = Contract::JevContext.parse(&raw).unwrap();
    output.classifications.into_iter().map(|entry| (entry.node_id, entry.classification)).collect()
}

#[tokio::test]
async fn policy_classifies_every_candidate_with_contract_output() {
    let request = request(
        vec![
            candidate(1, NodeType::Task),
            candidate(2, NodeType::Decision),
            candidate(3, NodeType::Artifact),
        ],
        vec![],
    );
    let labels = classify(&FakeJev::new(), &request).await;
    assert_eq!(labels.len(), 3);
    assert!(labels.iter().all(|(_, label)| *label == Classification::Active));
    let ids: Vec<NodeId> = labels.into_iter().map(|(id, _)| id).collect();
    assert_eq!(ids, [id(1), id(2), id(3)]);
}

#[tokio::test]
async fn superseded_nodes_are_inactive() {
    let request = request(
        vec![candidate(1, NodeType::Decision), candidate(2, NodeType::Decision)],
        // node 2 supersedes node 1
        vec![JevEdge { from: id(2), to: id(1), edge_type: EdgeType::Supersedes }],
    );
    let labels = classify(&FakeJev::new(), &request).await;
    assert_eq!(labels, [(id(1), Classification::Inactive), (id(2), Classification::Active)]);
}

#[tokio::test]
async fn only_recent_conversation_turns_stay_active() {
    let request = request(
        vec![
            candidate(1, NodeType::Conversation),
            candidate(2, NodeType::Task),
            candidate(3, NodeType::Conversation),
            candidate(4, NodeType::Conversation),
        ],
        vec![],
    );
    let labels = classify(&FakeJev::with_conversation_window(2), &request).await;
    assert_eq!(
        labels,
        [
            (id(1), Classification::Inactive),
            (id(2), Classification::Active),
            (id(3), Classification::Active),
            (id(4), Classification::Active),
        ]
    );
}

#[tokio::test]
async fn classification_replays_identically_from_a_recorded_request() {
    let original = request(
        vec![
            candidate(1, NodeType::Conversation),
            candidate(2, NodeType::Decision),
            candidate(3, NodeType::Decision),
        ],
        vec![JevEdge { from: id(3), to: id(2), edge_type: EdgeType::Supersedes }],
    );
    let jev = FakeJev::new();
    let first = jev.classify(&original).await.unwrap();

    // A request as it will be recorded in the event log, then read back for replay.
    let recorded = serde_json::to_string(&original).unwrap();
    let replayed: JevRequest = Contract::JevRequest.parse(&recorded).unwrap();
    for _ in 0..3 {
        assert_eq!(jev.classify(&replayed).await.unwrap(), first);
    }
    assert_eq!(FakeJev::new().classify(&original).await.unwrap(), first);
}

#[tokio::test]
async fn scripted_and_unavailable_modes_support_failure_testing() {
    let request = request(vec![candidate(1, NodeType::Task)], vec![]);
    let raw = r#"{"schema":"kiss.jev-context.v1","plan":["take over"]}"#;
    assert_eq!(FakeJev::scripted(raw).classify(&request).await.unwrap(), raw);
    assert_eq!(
        FakeJev::unavailable("connection refused").classify(&request).await,
        Err(JevError("connection refused".into()))
    );
    assert_eq!(FakeJev::new().id(), "fake-jev");
}
