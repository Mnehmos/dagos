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
    /// The conversation so far: its most recent earlier turns, oldest first.
    pub recent_events: Vec<IrEvent>,
    /// Optional capability descriptions; omitted when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<IrTool>,
    /// Tool calls made earlier in this run and what came of them, oldest first; omitted when
    /// there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_results: Vec<IrToolResult>,
    /// Earlier turns from any chat of the project that are not in `recent_events` but that Jev
    /// judged relevant to this request, oldest first; omitted when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recalled: Vec<IrRecalledTurn>,
}

/// A tool output's text parts (the `content` of MCP-shaped output), or its JSON when it has none.
pub fn tool_output_text(output: &serde_json::Value) -> String {
    let parts: Vec<&str> = output
        .get("content")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(serde_json::Value::as_str))
        .collect();
    if parts.is_empty() { output.to_string() } else { parts.join("\n") }
}

/// An earlier turn Jev recalled: its chat and what was said and done, verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrRecalledTurn {
    pub run_id: RunId,
    /// The chat's title.
    pub chat: String,
    /// The user's message, tool calls with result excerpts, and the reply or failure.
    pub text: String,
}

closed_enum!(
    /// What came of a tool call.
    IrToolStatus, "tool call status" {
        Completed => "completed",
        Failed => "failed",
        Denied => "denied",
    }
);

/// A tool call made earlier in this run: what was asked and what came back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrToolResult {
    pub call_id: String,
    pub name: String,
    pub arguments: Payload,
    pub status: IrToolStatus,
    /// The tool's output, for completed and failed calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    /// Why a call was denied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
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
/// previous output was rejected; completed runs carry the prose they presented, from the event
/// history (never DAG state), so a conversation keeps its thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrEvent {
    pub run_id: RunId,
    #[serde(rename = "type")]
    pub event_type: IrEventType,
    /// The user's message in that turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<ErrorCode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prose: Option<String>,
}

/// A tool the model may call. DAGOS runs the calls a person permits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IrTool {
    pub name: String,
    pub description: String,
    pub input_schema: Payload,
}
