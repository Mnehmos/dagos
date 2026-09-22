//! Durable DAG repository: persistence, validation, transactions, and restart behavior.

mod common;

use common::{memory_store, payload};
use dagos_core::domain::{DagNode, EdgeType, NodeId, NodeType, ProjectId};
use dagos_core::store::{DagViolation, Store, StoreError};
use serde_json::json;

fn create_project(store: &Store, name: &str) -> ProjectId {
    store.transaction(|tx| tx.create_project(name)).unwrap().id
}

fn insert(store: &Store, project: &ProjectId, node_type: NodeType, text: &str) -> DagNode {
    store
        .transaction(|tx| tx.insert_node(project, node_type, payload(json!({ "text": text }))))
        .unwrap()
}

fn violation(result: Result<impl std::fmt::Debug, StoreError>) -> DagViolation {
    match result {
        Err(StoreError::Dag(violation)) => violation,
        other => panic!("expected a DAG violation, got {other:?}"),
    }
}

#[test]
fn nodes_round_trip_and_list_in_creation_order() {
    let store = memory_store();
    let project = create_project(&store, "demo");
    let task = insert(&store, &project, NodeType::Task, "Write the store");
    let decision = insert(&store, &project, NodeType::Decision, "Use SQLite");

    assert_eq!(task.id.as_str(), "node_000001");
    assert_eq!(task.created_at, task.updated_at);
    assert_eq!(store.transaction(|tx| tx.nodes(&project)).unwrap(), vec![task.clone(), decision]);
    assert_eq!(store.transaction(|tx| tx.node(&task.id)).unwrap(), Some(task));
    let missing = NodeId::parse("node_999999").unwrap();
    assert_eq!(store.transaction(|tx| tx.node(&missing)).unwrap(), None);
}

#[test]
fn nodes_are_scoped_to_their_project() {
    let store = memory_store();
    let first = create_project(&store, "first");
    let second = create_project(&store, "second");
    let node = insert(&store, &first, NodeType::Task, "only in first");
    assert_eq!(store.transaction(|tx| tx.nodes(&first)).unwrap(), vec![node]);
    assert!(store.transaction(|tx| tx.nodes(&second)).unwrap().is_empty());
}

#[test]
fn updating_a_payload_advances_updated_at_only() {
    let store = memory_store();
    let project = create_project(&store, "demo");
    let node = insert(&store, &project, NodeType::Artifact, "v1");
    let updated = store
        .transaction(|tx| tx.update_node_payload(&node.id, payload(json!({"text": "v2"}))))
        .unwrap();
    assert_eq!(updated.id, node.id);
    assert_eq!(updated.node_type, node.node_type);
    assert_eq!(updated.created_at, node.created_at);
    assert!(updated.updated_at > node.updated_at);
    assert_eq!(updated.payload, payload(json!({"text": "v2"})));

    let missing = NodeId::parse("node_999999").unwrap();
    let error =
        store.transaction(|tx| tx.update_node_payload(&missing, payload(json!({})))).unwrap_err();
    assert!(matches!(error, StoreError::NotFound { kind: "node", .. }));
}

#[test]
fn nodes_require_an_existing_project() {
    let store = memory_store();
    let missing = ProjectId::parse("proj_999999").unwrap();
    let error = store
        .transaction(|tx| tx.insert_node(&missing, NodeType::Task, payload(json!({}))))
        .unwrap_err();
    assert!(matches!(error, StoreError::NotFound { kind: "project", .. }));
}

#[test]
fn edge_endpoints_must_exist_in_the_edge_project() {
    let store = memory_store();
    let project = create_project(&store, "demo");
    let other = create_project(&store, "other");
    let node = insert(&store, &project, NodeType::Task, "a");
    let foreign = insert(&store, &other, NodeType::Task, "b");
    let missing = NodeId::parse("node_999999").unwrap();

    let result =
        store.transaction(|tx| tx.insert_edge(&project, &node.id, &missing, EdgeType::DependsOn));
    assert_eq!(violation(result), DagViolation::UnknownNode(missing));

    let result = store
        .transaction(|tx| tx.insert_edge(&project, &node.id, &foreign.id, EdgeType::RelatedTo));
    assert_eq!(violation(result), DagViolation::UnknownNode(foreign.id));
}

#[test]
fn edges_reject_self_loops_duplicates_and_cycles() {
    let store = memory_store();
    let project = create_project(&store, "demo");
    let [a, b, c] = ["a", "b", "c"].map(|text| insert(&store, &project, NodeType::Task, text).id);
    let edge = |from: &NodeId, to: &NodeId, edge_type| {
        store.transaction(|tx| tx.insert_edge(&project, from, to, edge_type))
    };

    let ab = edge(&a, &b, EdgeType::DependsOn).unwrap();
    assert_eq!((&ab.from_node_id, &ab.to_node_id, ab.edge_type), (&a, &b, EdgeType::DependsOn));
    edge(&b, &c, EdgeType::DependsOn).unwrap();
    // A second path to an already-reachable node is fine: the graph is a DAG, not a tree.
    edge(&a, &c, EdgeType::RelatedTo).unwrap();

    assert_eq!(violation(edge(&a, &a, EdgeType::RelatedTo)), DagViolation::SelfLoop(a.clone()));
    assert_eq!(
        violation(edge(&a, &b, EdgeType::DependsOn)),
        DagViolation::DuplicateEdge {
            from: a.clone(),
            to: b.clone(),
            edge_type: EdgeType::DependsOn
        }
    );
    assert_eq!(
        violation(edge(&b, &a, EdgeType::Supersedes)),
        DagViolation::Cycle { from: b.clone(), to: a.clone(), edge_type: EdgeType::Supersedes }
    );
    // Transitive cycle: a → b → c, so c → a closes a loop.
    assert!(matches!(violation(edge(&c, &a, EdgeType::ObservedFrom)), DagViolation::Cycle { .. }));
    assert_eq!(store.transaction(|tx| tx.edges(&project)).unwrap().len(), 3);
}

#[test]
fn a_failed_transaction_leaves_no_partial_mutation() {
    let store = memory_store();
    let project = create_project(&store, "demo");
    let existing = insert(&store, &project, NodeType::Task, "existing");

    let result: Result<(), StoreError> = store.transaction(|tx| {
        let node = tx.insert_node(&project, NodeType::Decision, payload(json!({})))?;
        tx.insert_edge(&project, &node.id, &existing.id, EdgeType::DependsOn)?;
        // A later step fails: everything above must roll back.
        tx.insert_edge(&project, &node.id, &node.id, EdgeType::RelatedTo)?;
        Ok(())
    });

    assert!(matches!(result, Err(StoreError::Dag(DagViolation::SelfLoop(_)))));
    assert_eq!(store.transaction(|tx| tx.nodes(&project)).unwrap(), vec![existing]);
    assert!(store.transaction(|tx| tx.edges(&project)).unwrap().is_empty());
}

#[test]
fn nodes_and_edges_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("dagos.sqlite3");
    let (project, nodes, edges) = {
        let store = Store::open(&path).unwrap();
        let project = create_project(&store, "durable");
        let a = insert(&store, &project, NodeType::Observation, "tests pass");
        let b = insert(&store, &project, NodeType::Result, "shipped");
        store
            .transaction(|tx| tx.insert_edge(&project, &b.id, &a.id, EdgeType::ObservedFrom))
            .unwrap();
        let nodes = store.transaction(|tx| tx.nodes(&project)).unwrap();
        let edges = store.transaction(|tx| tx.edges(&project)).unwrap();
        (project, nodes, edges)
    };

    let reopened = Store::open(&path).unwrap();
    assert_eq!(reopened.transaction(|tx| tx.nodes(&project)).unwrap(), nodes);
    assert_eq!(reopened.transaction(|tx| tx.edges(&project)).unwrap(), edges);
    // Invariants still see persisted edges after restart.
    let (a, b) = (&nodes[0].id, &nodes[1].id);
    let result = reopened.transaction(|tx| tx.insert_edge(&project, a, b, EdgeType::DependsOn));
    assert!(matches!(violation(result), DagViolation::Cycle { .. }));
}
