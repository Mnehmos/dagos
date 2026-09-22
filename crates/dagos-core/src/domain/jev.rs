//! The Jev contract: a classification request in, node classifications out.
//!
//! Jev is only a classifier. Its request carries what it may consider (`kiss.jev-request.v1`) and
//! its output carries one label per node and nothing else (`kiss.jev-context.v1`). There is no
//! field for plans, routes, provider choices, tool calls, or actions, and unknown fields are
//! rejected, so the boundary cannot quietly grow.

use serde::{Deserialize, Serialize};

use super::context::Classification;
use super::dag::{EdgeType, NodeType, Payload};
use super::ids::NodeId;
use super::schema::{JevContextSchema, JevRequestSchema};

/// What Jev may consider when classifying one run's context (`kiss.jev-request.v1`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JevRequest {
    pub schema: JevRequestSchema,
    /// The run's user message.
    pub message: String,
    /// Every node of the project except the run's own message, oldest first.
    pub candidates: Vec<JevCandidate>,
    /// The edges between candidates, oldest first.
    pub edges: Vec<JevEdge>,
}

/// A node Jev may classify.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JevCandidate {
    pub node_id: NodeId,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub payload: Payload,
    /// Whether the node is in the active context carried over from the previous run.
    pub in_context: bool,
}

/// A relationship between two candidates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JevEdge {
    pub from: NodeId,
    pub to: NodeId,
    #[serde(rename = "type")]
    pub edge_type: EdgeType,
}

/// Jev's entire output (`kiss.jev-context.v1`): one classification per node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextClassification {
    pub schema: JevContextSchema,
    pub classifications: Vec<NodeClassification>,
}

/// One node's classification label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeClassification {
    pub node_id: NodeId,
    pub classification: Classification,
}
