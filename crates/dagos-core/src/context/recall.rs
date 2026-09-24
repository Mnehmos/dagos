//! Recall: Jev decides, every run, which of the project's earlier turns the model sees, and, every
//! step, which of the run's earlier tool results it still sees.
//!
//! Chats are how people organize work; the project DAG is what agents remember. Durable nodes of
//! every chat are already Jev candidates on every run. What is not a node is the turns
//! themselves: replies and tool results. IR carries the chat's most recent turns; instead of
//! summarizing the rest (and losing what a summary leaves out), DAGOS keeps every turn of every
//! chat and asks Jev one relevance question per turn. Relevant turns reach IR verbatim as
//! `recalled`. Inside a long run, the same question decides which large tool results from earlier
//! rounds stay in `tool_results`; the others are left out until they become relevant again. Jev
//! only classifies relevance; DAGOS reads the turns and the model never has to ask.

use serde_json::Value;

pub use crate::domain::tool_output_text as output_text;
use crate::domain::{EventData, IrRecalledTurn, RunId, RunStatus};
use crate::store::{StoreError, Tx};

/// A chunk at or above this relevance is recalled or kept.
pub const RECALL_THRESHOLD: f64 = 0.5;

/// The most turns one run recalls.
pub const RECALL_MAX_TURNS: usize = 5;

/// The most earlier turns Jev judges per run, newest first.
pub const RECALL_SEARCH_TURNS: usize = 300;

/// The longest text one chunk carries; longer turns and tool results are cut.
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

/// The longest excerpt of one tool output inside a turn.
const TOOL_OUTPUT_CHARS: usize = 500;

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
/// first, at most [`RECALL_SEARCH_TURNS`].
pub fn recall_candidates(
    tx: &Tx<'_>,
    run_id: &RunId,
    window: usize,
) -> Result<Vec<IrRecalledTurn>, StoreError> {
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
    let mut turns = Vec::new();
    for turn in &runs[runs.len().saturating_sub(RECALL_SEARCH_TURNS)..] {
        let events: Vec<EventData> = tx
            .events_of_types(&turn.id, TURN_EVENTS)?
            .into_iter()
            .map(|event| event.data)
            .collect();
        turns.push(IrRecalledTurn {
            run_id: turn.id.clone(),
            chat: titles.get(&turn.conversation_id).cloned().unwrap_or_default(),
            text: turn_text(&events),
        });
    }
    Ok(turns)
}

/// A turn as Jev judges it: its chat and its text.
pub fn turn_chunk(turn: &IrRecalledTurn) -> RecallChunk {
    RecallChunk::new(turn.run_id.as_str(), format!("chat “{}”\n{}", turn.chat, turn.text))
}

/// The turns to recall: those at or above [`RECALL_THRESHOLD`], the [`RECALL_MAX_TURNS`] most
/// relevant, oldest first.
pub fn select_recalled(turns: Vec<IrRecalledTurn>, scores: &[f64]) -> Vec<IrRecalledTurn> {
    let mut ranked: Vec<(usize, f64)> = scores
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, score)| *score >= RECALL_THRESHOLD)
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.0.cmp(&a.0)));
    ranked.truncate(RECALL_MAX_TURNS);
    let mut chosen: Vec<usize> = ranked.into_iter().map(|(index, _)| index).collect();
    chosen.sort_unstable();
    turns
        .into_iter()
        .enumerate()
        .filter(|(index, _)| chosen.binary_search(index).is_ok())
        .map(|(_, turn)| turn)
        .collect()
}

/// One turn as text: the user's message, its tool calls with an excerpt of each result, and the
/// reply or failure. Cut at [`RECALL_CHUNK_CHARS`].
fn turn_text(events: &[EventData]) -> String {
    let mut lines = Vec::new();
    for event in events {
        match event {
            EventData::MessageRecorded { text, .. } => lines.push(format!("user: {text}")),
            EventData::ToolRequested { name, arguments, .. } => {
                lines.push(format!("tool call {name} {}", Value::Object(arguments.clone())));
            }
            EventData::ToolCompleted { output, is_error, .. } => {
                let label = if *is_error { "tool error" } else { "tool result" };
                lines.push(format!("{label}: {}", cut(&output_text(output), TOOL_OUTPUT_CHARS)));
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
    cut(&lines.join("\n"), RECALL_CHUNK_CHARS)
}

/// `text` cut to at most `limit` characters, marked with `…` when cut.
pub fn cut(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}
