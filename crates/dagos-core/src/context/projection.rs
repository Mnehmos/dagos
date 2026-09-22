//! The active-context projection: carrying context between runs, asking Jev, and applying its
//! classifications.
//!
//! Every function here changes active-context membership only (and records why as events). None
//! of them creates, modifies, or deletes a durable DAG node or edge.

use std::collections::{BTreeMap, BTreeSet};

use crate::contracts::{Contract, ContractError};
use crate::domain::{
    Classification, ContextClassification, ContextMember, ContextSource, EventData, JevCandidate,
    JevEdge, JevRequest, JevRequestSchema, NodeId, Run, RunId,
};
use crate::store::{StoreError, Tx};

/// Jev output that must not be applied. Rejection is fail-closed: nothing is applied.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClassificationError {
    #[error(transparent)]
    Contract(#[from] ContractError),
    #[error("node `{0}` was not a candidate in the classification request")]
    UnknownCandidate(NodeId),
    #[error("node `{0}` was classified more than once")]
    DuplicateClassification(NodeId),
}

/// How applying a classification changed a run's active context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextChanges {
    /// Nodes that joined the context, in IR order.
    pub added: Vec<NodeId>,
    /// Nodes that left the context, in their previous IR order.
    pub removed: Vec<NodeId>,
    /// The resulting active context, in IR order.
    pub members: Vec<ContextMember>,
}

/// Seeds a new run's active context with the previous run's members (source `carried`) and
/// records `context.carried`.
pub fn carry_context(tx: &Tx<'_>, run_id: &RunId) -> Result<Vec<ContextMember>, StoreError> {
    let previous = tx.previous_run(run_id)?;
    let carried: Vec<(NodeId, ContextSource)> = match &previous {
        Some(previous) => tx
            .context(&previous.id)?
            .into_iter()
            .map(|member| (member.node_id, ContextSource::Carried))
            .collect(),
        None => Vec::new(),
    };
    let members = tx.replace_context(run_id, &carried)?;
    tx.append_event(
        run_id,
        EventData::ContextCarried {
            from_run_id: previous.map(|run| run.id),
            node_ids: members.iter().map(|member| member.node_id.clone()).collect(),
        },
    )?;
    Ok(members)
}

/// Builds the `kiss.jev-request.v1` request for `run`: every node of the project except the run's
/// own `task_node` (oldest first, flagged with current membership) and the edges between them.
pub fn classification_request(
    tx: &Tx<'_>,
    run: &Run,
    task_node: &NodeId,
    message: &str,
) -> Result<JevRequest, StoreError> {
    let in_context: BTreeSet<NodeId> =
        tx.context(&run.id)?.into_iter().map(|member| member.node_id).collect();
    let candidates: Vec<JevCandidate> = tx
        .nodes(&run.project_id)?
        .into_iter()
        .filter(|node| node.id != *task_node)
        .map(|node| JevCandidate {
            in_context: in_context.contains(&node.id),
            node_id: node.id,
            node_type: node.node_type,
            payload: node.payload,
        })
        .collect();
    let candidate_ids: BTreeSet<&NodeId> =
        candidates.iter().map(|candidate| &candidate.node_id).collect();
    let edges = tx
        .edges(&run.project_id)?
        .into_iter()
        .filter(|edge| {
            candidate_ids.contains(&edge.from_node_id) && candidate_ids.contains(&edge.to_node_id)
        })
        .map(|edge| JevEdge {
            from: edge.from_node_id,
            to: edge.to_node_id,
            edge_type: edge.edge_type,
        })
        .collect();
    Ok(JevRequest { schema: JevRequestSchema, message: message.to_owned(), candidates, edges })
}

/// Validates raw Jev output against the contract and against the request it answers.
///
/// Every classified node must be one of the request's candidates, at most once. Candidates the
/// output leaves out keep their current membership.
pub fn validate_classification(
    request: &JevRequest,
    raw: &str,
) -> Result<ContextClassification, ClassificationError> {
    let output: ContextClassification = Contract::JevContext.parse(raw)?;
    let candidates: BTreeSet<&NodeId> =
        request.candidates.iter().map(|candidate| &candidate.node_id).collect();
    let mut seen = BTreeSet::new();
    for entry in &output.classifications {
        if !candidates.contains(&entry.node_id) {
            return Err(ClassificationError::UnknownCandidate(entry.node_id.clone()));
        }
        if !seen.insert(&entry.node_id) {
            return Err(ClassificationError::DuplicateClassification(entry.node_id.clone()));
        }
    }
    Ok(output)
}

/// Applies a validated classification to a running run's active context: `active` makes a node a
/// member (source `jev`), `inactive` removes its membership. Records `jev.classified`, then one
/// `context.added` / `context.removed` event per membership change.
pub fn apply_classification(
    tx: &Tx<'_>,
    run_id: &RunId,
    classification: &ContextClassification,
) -> Result<ContextChanges, StoreError> {
    let before = tx.context(run_id)?;
    let mut membership: BTreeMap<NodeId, ContextSource> =
        before.iter().map(|member| (member.node_id.clone(), member.source)).collect();
    for entry in &classification.classifications {
        match entry.classification {
            Classification::Active => {
                membership.insert(entry.node_id.clone(), ContextSource::Jev);
            }
            Classification::Inactive => {
                membership.remove(&entry.node_id);
            }
        }
    }
    let members = tx.replace_context(run_id, &membership.into_iter().collect::<Vec<_>>())?;

    let before_ids: BTreeSet<&NodeId> = before.iter().map(|member| &member.node_id).collect();
    let after_ids: BTreeSet<&NodeId> = members.iter().map(|member| &member.node_id).collect();
    let added: Vec<NodeId> = members
        .iter()
        .filter(|member| !before_ids.contains(&member.node_id))
        .map(|member| member.node_id.clone())
        .collect();
    let removed: Vec<NodeId> = before
        .iter()
        .filter(|member| !after_ids.contains(&member.node_id))
        .map(|member| member.node_id.clone())
        .collect();

    tx.append_event(run_id, EventData::JevClassified { classification: classification.clone() })?;
    for node_id in &added {
        tx.append_event(run_id, EventData::ContextAdded { node_id: node_id.clone() })?;
    }
    for node_id in &removed {
        tx.append_event(run_id, EventData::ContextRemoved { node_id: node_id.clone() })?;
    }
    Ok(ContextChanges { added, removed, members })
}
