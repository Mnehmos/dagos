//! Recall: Jev decides, every run, which of the project's earlier turns the model sees, and, every
//! step, which of the run's earlier tool results it still sees.
//!
//! Chats are how people organize work; the project DAG is what agents remember. Durable nodes of
//! every chat are already Jev candidates on every run. What is not a node is the turns
//! themselves: replies and tool results. IR carries the chat's most recent turns; instead of
//! summarizing the rest (and losing what a summary leaves out), DAGOS keeps every turn of every
//! chat and asks Jev one relevance question per turn. Every turn it judges relevant reaches IR,
//! whole, as `recalled`: context is managed by relevance, never rationed by a count. Inside a long run, the same question decides which large tool results from earlier
//! rounds stay in `tool_results`; the others are left out until they become relevant again. Jev
//! only classifies relevance; DAGOS reads the turns and the model never has to ask.

use serde_json::Value;

pub use crate::domain::tool_output_text as output_text;
use crate::domain::{EventData, IrRecalledTurn, RunId, RunStatus};
use crate::store::{StoreError, Tx};

/// A chunk at or above this relevance is recalled or kept.
pub const RECALL_THRESHOLD: f64 = 0.5;

/// The most text Jev reads of one turn or tool result when judging it (its own input limit). The
/// model always gets a recalled turn whole.
pub const RECALL_CHUNK_CHARS: usize = 4000;

/// Tool results shorter than this always stay in IR: judging them costs more than it saves.
pub const COMPACT_MIN_CHARS: usize = 2000;

/// The events a turn's text is made of (never the many `inference.delta` events).
const TURN_EVENTS: &[&str] = &[
    "message.recorded",
    "tool.requested",
    "tool.completed",
    "response.validated",
    "review.completed",
    "run.failed",
];

/// The longest excerpt of one tool output in the copy of a turn Jev judges.
const TOOL_OUTPUT_CHARS: usize = 500;

/// An earlier turn that may be recalled: what the model would get, and what Jev judges.
#[derive(Debug, Clone, PartialEq)]
pub struct RecallCandidate {
    /// The whole turn, as IR carries it when recalled.
    pub turn: IrRecalledTurn,
    /// A trimmed copy for Jev to judge.
    pub chunk: RecallChunk,
}

/// One piece of history Jev judges: an earlier turn (`run_…`) or a tool result (`call_…`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallChunk {
    pub id: String,
    pub text: String,
}

impl RecallChunk {
    pub fn new(id: impl Into<String>, text: impl Into<String>) -> Self {
        Self { id: id.into(), text: text.into() }
    }
}

/// The project's finished turns that `run_id`'s IR does not already carry: every chat's turns
/// except this run and the `window` turns of its own chat that `recent_events` holds. Oldest
/// first, all of them.
pub fn recall_candidates(
    tx: &Tx<'_>,
    run_id: &RunId,
    window: usize,
) -> Result<Vec<RecallCandidate>, StoreError> {
    let run = tx
        .run(run_id)?
        .ok_or_else(|| StoreError::NotFound { kind: "run", id: run_id.to_string() })?;
    let mut in_ir = vec![run.id.clone()];
    let mut cursor = run.id.clone();
    while in_ir.len() <= window {
        let Some(previous) = tx.previous_run(&cursor)? else { break };
        cursor = previous.id.clone();
        in_ir.push(previous.id);
    }
    let titles: std::collections::BTreeMap<_, _> = tx
        .conversations(&run.project_id)?
        .into_iter()
        .map(|conversation| (conversation.id, conversation.title))
        .collect();
    let runs: Vec<_> = tx
        .runs(&run.project_id)?
        .into_iter()
        .filter(|other| !in_ir.contains(&other.id) && other.status != RunStatus::Running)
        .collect();
    let mut candidates = Vec::with_capacity(runs.len());
    for turn in &runs {
        let events: Vec<EventData> = tx
            .events_of_types(&turn.id, TURN_EVENTS)?
            .into_iter()
            .map(|event| event.data)
            .collect();
        let chat = titles.get(&turn.conversation_id).cloned().unwrap_or_default();
        let judged = cut(&turn_text(&events, Some(TOOL_OUTPUT_CHARS)), RECALL_CHUNK_CHARS);
        candidates.push(RecallCandidate {
            chunk: RecallChunk::new(turn.id.as_str(), format!("chat “{chat}”\n{judged}")),
            turn: IrRecalledTurn { run_id: turn.id.clone(), chat, text: turn_text(&events, None) },
        });
    }
    Ok(candidates)
}

/// The turns to recall: every one Jev judged at or above [`RECALL_THRESHOLD`], oldest first.
pub fn select_recalled(candidates: Vec<RecallCandidate>, scores: &[f64]) -> Vec<IrRecalledTurn> {
    candidates
        .into_iter()
        .zip(scores)
        .filter(|(_, score)| **score >= RECALL_THRESHOLD)
        .map(|(candidate, _)| candidate.turn)
        .collect()
}

/// One turn as text: the user's message, its tool calls and their results (each cut to `excerpt`
/// characters, if given), and the reply or failure.
fn turn_text(events: &[EventData], excerpt: Option<usize>) -> String {
    let mut lines = Vec::new();
    for event in events {
        match event {
            EventData::MessageRecorded { text, .. } => lines.push(format!("user: {text}")),
            EventData::ToolRequested { name, arguments, .. } => {
                lines.push(format!("tool call {name} {}", Value::Object(arguments.clone())));
            }
            EventData::ToolCompleted { output, is_error, .. } => {
                let label = if *is_error { "tool error" } else { "tool result" };
                let text = output_text(output);
                let text = excerpt.map_or(text.clone(), |limit| cut(&text, limit));
                lines.push(format!("{label}: {text}"));
            }
            EventData::ResponseValidated { response } => {
                lines.push(format!("assistant: {}", response.presentation.prose));
            }
            EventData::RunFailed { message, .. } => lines.push(format!("failed: {message}")),
            EventData::ReviewCompleted { round, findings, .. } => {
                for finding in findings {
                    lines.push(format!(
                        "lint round {round}: {} {} ({}:{})",
                        finding.text, finding.function, finding.file, finding.line
                    ));
                }
            }
            _ => {}
        }
    }
    lines.join("\n")
}

/// `text` cut to at most `limit` characters, marked with `…` when cut.
pub fn cut(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}
