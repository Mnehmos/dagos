//! IR layer: deterministic compilation of a run's state into versioned inference IR.
//!
//! IR is the only provider-facing contract. [`compile`] only reads durable state and returns a
//! fresh [`InferenceIr`]: identical state always compiles to identical IR, and storage records
//! never appear in it — every value is projected into IR types.

use std::collections::BTreeSet;

use crate::contracts::{Contract, ContractViolation};
use crate::domain::{
    ConversationTurn, ErrorCode, EventData, InferenceIr, InferenceIrSchema, IrContextItem, IrEvent,
    IrEventType, IrRecalledTurn, IrRelation, IrReview, IrTask, IrTool, IrToolResult, IrToolStatus,
    NodeId, NodeType, Role, RunId, RunStatus,
};
use crate::store::{StoreError, Tx};

/// How many of the conversation's most recent earlier turns IR carries in `recent_events` by
/// default.
pub const RECENT_RUN_OUTCOMES: usize = 8;

/// What Jev decided about history for one IR: how many recent turns it carries, which earlier
/// turns it recalls, and which of the run's tool results it leaves out.
#[derive(Debug, Clone, Copy)]
pub struct IrHistory<'a> {
    /// How many of the conversation's most recent earlier turns `recent_events` holds.
    pub window: usize,
    pub recalled: &'a [IrRecalledTurn],
    /// Call IDs whose outputs `tool_results` replaces with a note.
    pub omitted: &'a BTreeSet<String>,
    /// The latest review of the run's changed code, if it found anything.
    pub review: Option<&'a IrReview>,
}

/// Why IR could not be compiled.
#[derive(Debug, thiserror::Error)]
pub enum CompileError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("task node `{node_id}` is not a user conversation turn")]
    InvalidTask { node_id: NodeId },
    #[error(transparent)]
    Contract(#[from] ContractViolation),
}

/// Compiles the IR for `run_id`: its system prompt, its task (the user message recorded as
/// `task_node`), its active context in stored order with the relations among IR nodes, how the
/// project's most recent earlier runs ended, and the given capability descriptions.
///
/// The result is validated against `kiss.inference-ir.v1` before it is returned.
pub fn compile(
    tx: &Tx<'_>,
    run_id: &RunId,
    task_node: &NodeId,
    tools: &[IrTool],
) -> Result<InferenceIr, CompileError> {
    let omitted = BTreeSet::new();
    let history =
        IrHistory { window: RECENT_RUN_OUTCOMES, recalled: &[], omitted: &omitted, review: None };
    compile_with(tx, run_id, task_node, tools, history)
}

/// [`compile`] with the history Jev decided on: `history.window` recent turns, the recalled
/// turns, and the omitted tool results replaced by a note.
pub fn compile_with(
    tx: &Tx<'_>,
    run_id: &RunId,
    task_node: &NodeId,
    tools: &[IrTool],
    history: IrHistory<'_>,
) -> Result<InferenceIr, CompileError> {
    let run = tx.run(run_id)?.ok_or_else(|| not_found("run", run_id))?;
    let task = tx.node(task_node)?.ok_or_else(|| not_found("node", task_node))?;
    let message = ConversationTurn::from_payload(&task.payload)
        .filter(|turn| task.node_type == NodeType::Conversation && turn.role == Role::User)
        .ok_or_else(|| CompileError::InvalidTask { node_id: task.id.clone() })?
        .text;

    let members = tx.context(run_id)?;
    let in_ir: BTreeSet<&NodeId> =
        members.iter().map(|member| &member.node_id).chain([task_node]).collect();
    let edges = tx.edges(&run.project_id)?;
    let mut context = Vec::with_capacity(members.len());
    for member in &members {
        let node = tx.node(&member.node_id)?.ok_or_else(|| not_found("node", &member.node_id))?;
        let relations = edges
            .iter()
            .filter(|edge| edge.from_node_id == node.id && in_ir.contains(&edge.to_node_id))
            .map(|edge| IrRelation { edge_type: edge.edge_type, to: edge.to_node_id.clone() })
            .collect();
        context.push(IrContextItem {
            node_id: node.id,
            node_type: node.node_type,
            payload: node.payload,
            relations,
        });
    }

    let ir = InferenceIr {
        schema: InferenceIrSchema,
        system_prompt: run.system_prompt.clone(),
        task: IrTask { node_id: task.id, message },
        context,
        recent_events: recent_run_outcomes(tx, run_id, history.window)?,
        tools: tools.to_vec(),
        tool_results: tool_results(tx, run_id, history.omitted)?,
        recalled: history.recalled.to_vec(),
        review: history.review.cloned(),
    };
    Contract::InferenceIr.validate(&serde_json::to_value(&ir).expect("IR serializes"))?;
    Ok(ir)
}

/// The conversation's most recent turns before `run_id`, oldest first: each turn's message and
/// how it ended.
fn recent_run_outcomes(
    tx: &Tx<'_>,
    run_id: &RunId,
    window: usize,
) -> Result<Vec<IrEvent>, StoreError> {
    let mut earlier = Vec::new();
    let mut cursor = run_id.clone();
    while earlier.len() < window {
        let Some(previous) = tx.previous_run(&cursor)? else { break };
        cursor = previous.id.clone();
        earlier.push(previous);
    }
    earlier.reverse();

    let mut outcomes = Vec::with_capacity(earlier.len());
    for run in earlier {
        let events = tx.events(&run.id)?;
        let request = events.iter().find_map(|event| match &event.data {
            EventData::MessageRecorded { text, .. } => Some(text.clone()),
            _ => None,
        });
        let outcome = match run.status {
            RunStatus::Completed => IrEvent {
                run_id: run.id,
                event_type: IrEventType::RunCompleted,
                request,
                error_code: None,
                message: None,
                prose: events.into_iter().rev().find_map(|event| match event.data {
                    EventData::ResponseValidated { response } => Some(response.presentation.prose),
                    _ => None,
                }),
            },
            RunStatus::Failed => IrEvent {
                run_id: run.id,
                event_type: IrEventType::RunFailed,
                request,
                error_code: Some(run.error_code.unwrap_or(ErrorCode::Internal)),
                message: events.into_iter().rev().find_map(|event| match event.data {
                    EventData::RunFailed { message, .. } => Some(message),
                    _ => None,
                }),
                prose: None,
            },
            // Only one run per project runs at a time, so an earlier run is always finished.
            RunStatus::Running => continue,
        };
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

/// The tool calls recorded so far in `run_id`, oldest first, with their outcome; the outputs of
/// calls in `omitted` are replaced by a note.
fn tool_results(
    tx: &Tx<'_>,
    run_id: &RunId,
    omitted: &BTreeSet<String>,
) -> Result<Vec<IrToolResult>, StoreError> {
    let mut results: Vec<IrToolResult> = Vec::new();
    for event in tx.events(run_id)? {
        match event.data {
            EventData::ToolRequested { call_id, name, arguments } => results.push(IrToolResult {
                call_id,
                name,
                arguments,
                status: IrToolStatus::Denied,
                output: None,
                reason: Some("no decision was recorded".to_owned()),
            }),
            EventData::ToolDecided { call_id, allowed, reason, .. } => {
                if let Some(result) = results.iter_mut().find(|result| result.call_id == call_id) {
                    result.reason = if allowed { None } else { reason };
                    if allowed {
                        // Allowed but never completed: the call was interrupted.
                        result.status = IrToolStatus::Failed;
                        result.output =
                            Some(serde_json::json!({"error": "the call did not finish"}));
                    }
                }
            }
            EventData::ToolCompleted { call_id, output, is_error } => {
                if let Some(result) = results.iter_mut().find(|result| result.call_id == call_id) {
                    result.status =
                        if is_error { IrToolStatus::Failed } else { IrToolStatus::Completed };
                    result.output = Some(if omitted.contains(&call_id) {
                        omitted_output(&output)
                    } else {
                        output
                    });
                    result.reason = None;
                }
            }
            _ => {}
        }
    }
    Ok(results)
}

/// The note that stands in for a tool output left out of IR.
fn omitted_output(output: &serde_json::Value) -> serde_json::Value {
    let size = crate::domain::tool_output_text(output).chars().count();
    serde_json::json!({
        "omitted": format!(
            "Left out: Jev judged this {size}-character result not needed for the current step. \
             DAGOS brings it back if it becomes relevant."
        )
    })
}

fn not_found(kind: &'static str, id: impl ToString) -> CompileError {
    CompileError::Store(StoreError::NotFound { kind, id: id.to_string() })
}
