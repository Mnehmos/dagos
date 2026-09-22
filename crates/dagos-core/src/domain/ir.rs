//! Inference IR v1: the only document an inference provider receives (`kiss.inference-ir.v1`).
//!
//! IR is a compiled projection, not a view of storage. Its types are distinct from the DAG records
//! they are compiled from and carry no storage fields (timestamps, project scopes), so a provider
//! that accepts [`InferenceIr`] cannot receive a raw DAG record.

use serde::{Deserialize, Serialize};

use super::dag::{EdgeType, NodeType, Payload, closed_enum};
use super::ids::{NodeId, RunId};
use super::run::ErrorCode;
use super::schema::InferenceIrSchema;

/// A compiled inference request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InferenceIr {
    pub schema: InferenceIrSchema,
    pub system_prompt: String,
    pub task: IrTask,
    /// The run's active context in stable order.
    pub context: Vec<IrContextItem>,
    /// How the project's most recent earlier runs ended, oldest first.
    pub recent_events: Vec<IrEvent>,
    /// Optional capability descriptions; omitted when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<IrTool>,
}

/// The run's user message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrTask {
    pub node_id: NodeId,
    pub message: String,
}

/// One active-context node, projected for inference.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrContextItem {
    pub node_id: NodeId,
    #[serde(rename = "type")]
    pub node_type: NodeType,
    pub payload: Payload,
    /// Outgoing edges to other nodes in the same IR, oldest first.
    pub relations: Vec<IrRelation>,
}

/// An outgoing edge: `<context item> <type> <to>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrRelation {
    #[serde(rename = "type")]
    pub edge_type: EdgeType,
    pub to: NodeId,
}

closed_enum!(
    /// The run outcomes IR reports.
    IrEventType, "IR event type" {
        RunCompleted => "run.completed",
        RunFailed => "run.failed",
    }
);

/// How an earlier run ended. Failures carry their code and message, so a model can see why its
/// previous output was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrEvent {
    pub run_id: RunId,
    #[serde(rename = "type")]
    pub event_type: IrEventType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// A capability description. Descriptive only: DAGOS v0.1 never executes tools.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrTool {
    pub name: String,
    pub description: String,
    pub input_schema: Payload,
}
