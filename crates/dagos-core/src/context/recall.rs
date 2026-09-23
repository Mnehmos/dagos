//! Recall: searching a conversation's earlier turns, including those IR no longer carries.
//!
//! IR carries only a conversation's most recent turns. Instead of summarizing older turns (and
//! losing what the summary leaves out), DAGOS keeps them on disk and offers the model the
//! [`RECALL_TOOL`]: every earlier turn is one chunk, Jev judges which chunks are relevant to the
//! model's query, and the model gets the relevant turns verbatim, with cursors to page to their
//! neighbours. Jev only classifies relevance here too; it never sees the store or answers.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::domain::{EventData, IrTool, Payload, RunId, RunStatus};
use crate::store::{StoreError, Tx};

/// The name under which the model sees the recall tool.
pub const RECALL_TOOL: &str = "dagos.recall";

/// A chunk at or above this relevance is a match.
pub const RECALL_THRESHOLD: f64 = 0.5;

/// The most matches one search returns.
pub const RECALL_MAX_MATCHES: usize = 3;

/// The most earlier turns one search reads, newest first.
pub const RECALL_MAX_TURNS: usize = 200;

/// The longest text one chunk carries; longer turns are cut.
pub const RECALL_CHUNK_CHARS: usize = 4000;

/// The longest excerpt of one tool output inside a chunk.
const TOOL_OUTPUT_CHARS: usize = 500;

/// One earlier conversation turn as recall sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallChunk {
    /// The turn's run ID, which is also its cursor.
    pub id: String,
    /// The turn as text: the user's message, the reply or failure, and its tool calls.
    pub text: String,
}

/// The recall tool's description for IR.
pub fn recall_tool() -> IrTool {
    let schema = json!({
        "type": "object",
        "properties": {
            "query": {
                "type": "string",
                "description": "What to look for, e.g. \"sister food allergy\"."
            },
            "cursor": {
                "type": "string",
                "description": "A `before` or `after` cursor from an earlier recall result: \
                                returns that turn instead of searching."
            }
        },
        "additionalProperties": false
    });
    IrTool {
        name: RECALL_TOOL.to_owned(),
        description: "Search this conversation's earlier turns, including those no longer in \
                      recent_events, and return the relevant ones verbatim. Pass `query` to \
                      search, or a `cursor` from a result to read the turn before or after it."
            .to_owned(),
        input_schema: match schema {
            Value::Object(map) => map,
            _ => unreachable!("the recall schema is an object"),
        },
    }
}

/// A recall call's arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecallQuery {
    Search(String),
    Cursor(String),
}

impl RecallQuery {
    /// Reads the tool call's arguments: a non-empty `query`, or a `cursor`.
    pub fn parse(arguments: &Payload) -> Result<Self, String> {
        let text = |key: &str| {
            arguments.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty())
        };
        match (text("cursor"), text("query")) {
            (Some(cursor), _) => Ok(Self::Cursor(cursor.to_owned())),
            (None, Some(query)) => Ok(Self::Search(query.to_owned())),
            (None, None) => Err("pass a non-empty `query` or a `cursor`".to_owned()),
        }
    }
}

/// The conversation's turns before `run_id`, oldest first: at most [`RECALL_MAX_TURNS`].
pub fn conversation_chunks(tx: &Tx<'_>, run_id: &RunId) -> Result<Vec<RecallChunk>, StoreError> {
    let mut chunks = Vec::new();
    let mut cursor = run_id.clone();
    while chunks.len() < RECALL_MAX_TURNS {
        let Some(previous) = tx.previous_run(&cursor)? else { break };
        cursor = previous.id.clone();
        if previous.status == RunStatus::Running {
            continue;
        }
        let events: Vec<EventData> =
            tx.events(&previous.id)?.into_iter().map(|event| event.data).collect();
        let text = turn_text(&events);
        chunks.push(RecallChunk { id: previous.id.to_string(), text });
    }
    chunks.reverse();
    Ok(chunks)
}

/// How many turns the conversation has before `run_id`, counting at most `limit`.
pub fn earlier_turns(tx: &Tx<'_>, run_id: &RunId, limit: usize) -> Result<usize, StoreError> {
    let mut count = 0;
    let mut cursor = run_id.clone();
    while count < limit {
        let Some(previous) = tx.previous_run(&cursor)? else { break };
        cursor = previous.id;
        count += 1;
    }
    Ok(count)
}

/// One turn as text, cut at [`RECALL_CHUNK_CHARS`].
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
            _ => {}
        }
    }
    cut(&lines.join("\n"), RECALL_CHUNK_CHARS)
}

/// A tool output's text parts, or its JSON when it has none.
fn output_text(output: &Value) -> String {
    let parts: Vec<&str> = output
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    if parts.is_empty() { output.to_string() } else { parts.join("\n") }
}

fn cut(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_owned(),
    }
}

/// Deterministic relevance without a model: the share of the query's words (compared by their
/// first five letters, so `allergy` meets `allergic`) that appear in each chunk.
pub fn lexical_relevance(query: &str, chunks: &[RecallChunk]) -> Vec<f64> {
    let terms = stems(query);
    chunks
        .iter()
        .map(|chunk| {
            if terms.is_empty() {
                return 0.0;
            }
            let words = stems(&chunk.text);
            let hits = terms.iter().filter(|term| words.contains(*term)).count();
            hits as f64 / terms.len() as f64
        })
        .collect()
}

/// Lowercase word stems of three or more letters, minus common words.
fn stems(text: &str) -> BTreeSet<String> {
    const COMMON: &[&str] =
        &["the", "and", "for", "with", "that", "this", "what", "about", "you", "did", "was", "are"];
    text.split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| word.chars().count() >= 3 && !COMMON.contains(&word.as_str()))
        .map(|word| word.chars().take(5).collect())
        .collect()
}

/// A search result: the matching turns, best first, with cursors to their neighbours.
/// `scored_by` names what judged relevance.
pub fn search_output(
    query: &str,
    chunks: &[RecallChunk],
    scores: &[f64],
    scored_by: &str,
) -> Value {
    let mut ranked: Vec<(usize, f64)> = scores
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, score)| *score >= RECALL_THRESHOLD)
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(b.0.cmp(&a.0)));
    ranked.truncate(RECALL_MAX_MATCHES);
    let matches: Vec<Value> =
        ranked.iter().map(|(index, score)| turn_json(chunks, *index, Some(*score))).collect();
    let structured = json!({
        "query": query,
        "matches": matches,
        "searched": chunks.len(),
        "dropped": chunks.len() - matches.len(),
        "scored_by": scored_by,
    });
    let mut text = format!(
        "Searched {} earlier turns for “{query}”: {} relevant.",
        chunks.len(),
        matches.len()
    );
    for entry in &matches {
        text.push_str(&format!(
            "\n\n**{}** · relevance {:.2}\n\n{}",
            entry["turn"].as_str().unwrap_or_default(),
            entry["score"].as_f64().unwrap_or_default(),
            quoted(entry["text"].as_str().unwrap_or_default()),
        ));
    }
    output(text, structured)
}

/// The turn a cursor names, with cursors to its neighbours, or `None` if the cursor names no
/// earlier turn of this conversation.
pub fn cursor_output(cursor: &str, chunks: &[RecallChunk]) -> Option<Value> {
    let index = chunks.iter().position(|chunk| chunk.id == cursor)?;
    let entry = turn_json(chunks, index, None);
    let text = format!("**{cursor}**\n\n{}", quoted(entry["text"].as_str().unwrap_or_default()));
    Some(output(text, json!({"matches": [entry], "searched": 1, "dropped": 0})))
}

fn turn_json(chunks: &[RecallChunk], index: usize, score: Option<f64>) -> Value {
    let mut entry = json!({"turn": chunks[index].id, "text": chunks[index].text});
    if let Some(score) = score {
        entry["score"] = json!((score * 100.0).round() / 100.0);
    }
    if let Some(before) = index.checked_sub(1) {
        entry["before"] = json!(chunks[before].id);
    }
    if let Some(after) = chunks.get(index + 1) {
        entry["after"] = json!(after.id);
    }
    entry
}

fn quoted(text: &str) -> String {
    text.lines().map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n")
}

/// Tool output in the shape MCP tools produce: text for people, `structured` for the model.
fn output(text: String, structured: Value) -> Value {
    json!({"content": [{"type": "text", "text": text}], "structured": structured})
}
