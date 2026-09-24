//! How DAGOS inference IR becomes a Chat Completions request.
//!
//! The system message is the IR's editable system prompt followed by the fixed DAGOS protocol
//! instructions and the response schema. The user message is the IR itself, as compact JSON: the
//! model receives machine-readable input and must answer with one machine-readable document.

use dagos_core::context::NoulQuestion;
use dagos_core::context::recall::RecallChunk;
use dagos_core::contracts::Contract;
use dagos_core::domain::{InferenceIr, JevRequest, ModelId, NodeType};
use serde_json::{Value, json};

const PROTOCOL: &str = "\
You are the inference endpoint of DAGOS, a DAG operating system for coding workflows.

The user message is a kiss.inference-ir.v1 JSON document:
- task.message is the request to answer; task.node_id is its durable node.
- context lists the durable DAG nodes that are active for this request, with their relations.
- recent_events is the conversation so far, oldest first: each earlier turn's request (the user's \
message) and your reply (prose), or why that turn failed. It holds only the most recent turns.
- recalled, if present, are older turns, from this chat or other chats of the project, that DAGOS \
judged relevant to this request, verbatim.
- tools, if present, are tools you may use. To use them, list calls in \"tool_calls\" (name exactly \
as listed, arguments matching its input_schema). DAGOS runs the calls the person permits and sends \
you a new IR whose tool_results say what each returned, failed with, or why it was denied; keep \
going until you can answer, then reply without tool_calls. Every reply, including one that only \
calls tools, is still exactly one JSON object: say what you are doing in presentation.prose, never \
before or after the object. Some tools act on the person's computer: call only what the request \
needs.
- tool_results, if present, are this request's earlier tool calls and their outcomes. An output \
of {\"omitted\": ...} is a large result DAGOS left out because it is not needed for the current \
step; it comes back if it becomes relevant, and you can call the tool again if you need it now.
- review, if present, means you are not done yet: DAGOS linted the functions your tool calls \
changed against the project's plain-English rules, and each finding is a rule its judge found to \
apply (with the probability). Fix the findings with your tools, then reply again; DAGOS reviews \
the new code each time you reply without tool calls, up to review.max_rounds. If a finding is \
wrong, do not change the code for it: say why in presentation.prose.

Reply with exactly one JSON object that satisfies kiss.inference-response.v1 and nothing else: no \
markdown fences and no text outside the object.
- Write \"presentation\" first. presentation.prose is your reply to the person. It is shown to them \
but never stored as project state.
- \"emissions\" is the only way to record durable project state. Emit nodes for tasks, decisions, \
artifacts, observations, and results worth remembering. Give each node emission a unique \"ref\" \
(letters, digits, \"_\" or \"-\", not starting with \"node_\").
- Edge emissions may only connect refs declared in this response or node_ids that appear in the \
IR. Edges must not form cycles.
- Use an empty emissions array when nothing needs recording.

The response schema:
";

/// The system message: the run's system prompt, then the DAGOS protocol and response schema.
pub fn system_message(ir: &InferenceIr) -> String {
    let schema = Contract::InferenceResponse.schema_source().trim();
    if ir.system_prompt.trim().is_empty() {
        format!("{PROTOCOL}{schema}")
    } else {
        format!("{}\n\n{PROTOCOL}{schema}", ir.system_prompt.trim_end())
    }
}

/// The user message: the IR document itself.
pub fn user_message(ir: &InferenceIr) -> String {
    serde_json::to_string(ir).expect("IR serializes")
}

/// The streaming Chat Completions request body for `ir`.
pub fn request_body(model_id: &ModelId, ir: &InferenceIr, json_mode: bool) -> Value {
    let mut body = json!({
        "model": model_id.as_str(),
        "stream": true,
        "messages": [
            {"role": "system", "content": system_message(ir)},
            {"role": "user", "content": user_message(ir)},
        ],
    });
    if json_mode {
        body["response_format"] = json!({"type": "json_object"});
    }
    body
}

const JEV_PROTOCOL: &str = "\
You are Jev, the context classifier of DAGOS. You only classify.

The user message is a kiss.jev-request.v1 JSON document: the user's `message`, the `candidates` \
(durable DAG nodes, oldest first, each with `in_context` telling whether it is in the active \
context carried over from the previous run), the `edges` between them, and optionally the `tools` \
the assistant could be given.

For each candidate, decide whether it belongs in the active context for answering `message`: \
`active` if it is relevant, `inactive` if it is not (for example superseded by a newer node, \
stale, or unrelated). Candidates you leave out keep their current membership.

If the request lists `tools`, label each one in `tools`: `active` if answering `message` may need \
it, `inactive` if not. Only active (and unlabeled) tools are shown to the assistant this turn.

You never answer the message, plan, choose providers or models, call tools, or explain yourself. \
Reply with exactly one JSON object that satisfies kiss.jev-context.v1 and nothing else: no \
markdown fences, no text outside the object, no fields beyond the schema. Use only node_ids from \
`candidates`, each at most once.

The output schema:
";

/// The system message for a Jev classification: the classifier-only instructions and schema.
pub fn jev_system_message() -> String {
    format!("{JEV_PROTOCOL}{}", Contract::JevContext.schema_source().trim())
}

/// The Chat Completions request body for classifying `request` with `model_id`.
pub fn jev_request_body(model_id: &ModelId, request: &JevRequest, json_mode: bool) -> Value {
    let mut body = json!({
        "model": model_id.as_str(),
        "stream": true,
        "temperature": 0,
        "messages": [
            {"role": "system", "content": jev_system_message()},
            {"role": "user", "content": serde_json::to_string(request).expect("requests serialize")},
        ],
    });
    if json_mode {
        body["response_format"] = json!({"type": "json_object"});
    }
    body
}

/// A `noul` answer at or above this probability classifies the node `active`.
pub const ACTIVE_THRESHOLD: f64 = 0.5;

/// The Decisions API request for classifying `request` with a decisions model (e.g. TypeSafe's
/// Jev): one `noul` question per candidate, keyed by node ID, over the request as shared state.
pub fn decisions_body(model_id: &ModelId, request: &JevRequest) -> Value {
    let count = request.candidates.len();
    let mut questions: serde_json::Map<String, Value> = request
        .candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            let question = json!({
                "type": "noul",
                "instructions": {
                    "task": "Decide whether this durable project node belongs in the active \
                             context used to answer the user's message in `state.message`.",
                    "node": candidate,
                    // Candidates are oldest first; 1 is the most recent node before the message.
                    "recency": count - index,
                },
                "criteria": {
                    "true": "The node is relevant to answering the message and still current, \
                             or the message refers back to the recent conversation and this node \
                             is one of its latest turns (low `recency`).",
                    "false": "The node is unrelated to the message, stale, or superseded by a \
                              newer node (see `state.edges`).",
                },
            });
            (candidate.node_id.to_string(), question)
        })
        .collect();
    for tool in &request.tools {
        let question = json!({
            "type": "noul",
            "instructions": {
                "task": "Decide whether answering the user's message in `state.message` may need \
                         this tool. Only tools judged needed are shown to the assistant.",
                "tool": tool,
            },
            "criteria": {
                "true": "The message asks for something this tool does or helps find out, or the \
                         recent conversation (`state.latest_messages`) is about work it serves.",
                "false": "Answering the message will not need this tool.",
            },
        });
        questions.insert(tool_key(&tool.name), question);
    }
    json!({
        "model": model_id.as_str(),
        "questions": questions,
        "state": {
            "message": request.message,
            "latest_messages": latest_messages(request),
            "edges": request.edges,
        },
    })
}

/// How many of the latest user messages the Decisions state includes for orientation.
const LATEST_MESSAGES: usize = 4;

/// The most recent user messages among the candidates, oldest first.
fn latest_messages(request: &JevRequest) -> Vec<&str> {
    let mut messages: Vec<&str> = request
        .candidates
        .iter()
        .rev()
        .filter(|candidate| candidate.node_type == NodeType::Conversation)
        .filter(|candidate| candidate.payload.get("role").and_then(Value::as_str) == Some("user"))
        .filter_map(|candidate| candidate.payload.get("text").and_then(Value::as_str))
        .take(LATEST_MESSAGES)
        .collect();
    messages.reverse();
    messages
}

/// Turns a Decisions API response into a `kiss.jev-context.v1` document: each candidate's `noul`
/// probability becomes `active` (at least [`ACTIVE_THRESHOLD`]) or `inactive`; candidates without
/// an answer are left out and keep their membership. The runtime validates the result as usual.
pub fn classification_from_decisions(
    request: &JevRequest,
    response: &Value,
) -> Result<String, String> {
    let answers = response
        .get("answers")
        .and_then(Value::as_object)
        .ok_or("the Decisions response has no `answers` object")?;
    let mut classifications = Vec::new();
    for candidate in &request.candidates {
        let Some(answer) = answers.get(candidate.node_id.as_str()) else { continue };
        let probability =
            answer.get("noul").and_then(Value::as_f64).filter(|p| (0.0..=1.0).contains(p));
        let Some(probability) = probability else {
            return Err(format!(
                "the answer for {} is not a noul probability: {answer}",
                candidate.node_id
            ));
        };
        let label = if probability >= ACTIVE_THRESHOLD { "active" } else { "inactive" };
        classifications.push(json!({"node_id": candidate.node_id, "classification": label}));
    }
    let mut tools = Vec::new();
    for tool in &request.tools {
        let Some(answer) = answers.get(&tool_key(&tool.name)) else { continue };
        let probability =
            answer.get("noul").and_then(Value::as_f64).filter(|p| (0.0..=1.0).contains(p));
        let Some(probability) = probability else {
            return Err(format!(
                "the answer for tool {} is not a noul probability: {answer}",
                tool.name
            ));
        };
        let label = if probability >= ACTIVE_THRESHOLD { "active" } else { "inactive" };
        tools.push(json!({"name": tool.name, "classification": label}));
    }
    let mut document = json!({"schema": "kiss.jev-context.v1", "classifications": classifications});
    if !tools.is_empty() {
        document["tools"] = json!(tools);
    }
    Ok(document.to_string())
}

/// The most chunk text one recall Decisions request carries; larger searches are split.
pub const RECALL_BATCH_CHARS: usize = 40_000;

/// Splits `chunks` into consecutive batches of at most [`RECALL_BATCH_CHARS`] characters of text
/// (a batch always holds at least one chunk).
pub fn recall_batches(chunks: &[RecallChunk]) -> Vec<&[RecallChunk]> {
    let mut batches = Vec::new();
    let mut start = 0;
    let mut size = 0;
    for (index, chunk) in chunks.iter().enumerate() {
        let length = chunk.text.chars().count();
        if index > start && size + length > RECALL_BATCH_CHARS {
            batches.push(&chunks[start..index]);
            start = index;
            size = 0;
        }
        size += length;
    }
    if start < chunks.len() {
        batches.push(&chunks[start..]);
    }
    batches
}

/// The Decisions API request judging which of `chunks` (earlier turns of the project's chats, or
/// tool results from earlier steps of a run) hold information relevant to `query`: one `noul`
/// question per chunk, keyed by its ID.
pub fn recall_body(model_id: &ModelId, query: &str, chunks: &[RecallChunk]) -> Value {
    let questions: serde_json::Map<String, Value> = chunks
        .iter()
        .map(|chunk| {
            let question = json!({
                "type": "noul",
                "instructions": {
                    "task": "Decide whether this item from the project's history (an earlier \
                             chat turn, or a tool result from an earlier step) holds information \
                             needed to answer `state.query`.",
                    "item": chunk.text,
                },
                "criteria": {
                    "true": "The item states, shows, asks, or decides something that answering \
                             the query needs.",
                    "false": "Answering the query does not need anything in this item.",
                },
            });
            (chunk.id.clone(), question)
        })
        .collect();
    json!({"model": model_id.as_str(), "questions": questions, "state": {"query": query}})
}

/// Each chunk's relevance from a Decisions API response to [`recall_body`], in chunk order.
pub fn relevance_from_decisions(
    chunks: &[RecallChunk],
    response: &Value,
) -> Result<Vec<f64>, String> {
    let answers = response
        .get("answers")
        .and_then(Value::as_object)
        .ok_or("the Decisions response has no `answers` object")?;
    chunks
        .iter()
        .map(|chunk| {
            let answer = answers
                .get(&chunk.id)
                .ok_or_else(|| format!("the Decisions response has no answer for {}", chunk.id))?;
            answer
                .get("noul")
                .and_then(Value::as_f64)
                .filter(|p| (0.0..=1.0).contains(p))
                .ok_or_else(|| {
                    format!("the answer for {} is not a noul probability: {answer}", chunk.id)
                })
        })
        .collect()
}

/// The Decisions API request asking `questions` about `state`: one `noul` question each.
pub fn noul_body(model_id: &ModelId, state: &Value, questions: &[NoulQuestion]) -> Value {
    let questions: serde_json::Map<String, Value> = questions
        .iter()
        .map(|question| {
            let body = json!({
                "type": "noul",
                "instructions": question.instructions,
                "criteria": {"true": question.if_true, "false": question.if_false},
            });
            (question.key.clone(), body)
        })
        .collect();
    json!({"model": model_id.as_str(), "questions": questions, "state": state})
}

/// Each question's probability from a Decisions API response to [`noul_body`], in order.
pub fn noul_answers(questions: &[NoulQuestion], response: &Value) -> Result<Vec<f64>, String> {
    let answers = response
        .get("answers")
        .and_then(Value::as_object)
        .ok_or("the Decisions response has no `answers` object")?;
    questions
        .iter()
        .map(|question| {
            let answer = answers.get(&question.key).ok_or_else(|| {
                format!("the Decisions response has no answer for {}", question.key)
            })?;
            answer
                .get("noul")
                .and_then(Value::as_f64)
                .filter(|p| (0.0..=1.0).contains(p))
                .ok_or_else(|| {
                    format!("the answer for {} is not a noul probability: {answer}", question.key)
                })
        })
        .collect()
}

/// The Decisions question key for a tool; the prefix keeps it apart from node IDs.
fn tool_key(name: &str) -> String {
    format!("tool:{name}")
}

/// The longest plain-text preamble removed before a response document.
const MAX_PREAMBLE_CHARS: usize = 1_000;

/// The response document inside a model's final output, for two harmless quirks of chat models:
/// the whole document wrapped in a Markdown code fence, or a short plain-text preamble (a note on
/// what the model is about to do, usually a paraphrase of its own `presentation.prose`) before
/// the object. The document must be everything after the preamble and the preamble must hold no
/// JSON, so only presentation text is dropped. Any other output is returned unchanged, so
/// validation still fails closed on it.
pub fn unwrap_document(output: &str) -> String {
    let trimmed = output.trim();
    if let Some(inner) = strip_fence(trimmed)
        && matches!(serde_json::from_str::<Value>(inner), Ok(Value::Object(_)))
    {
        return inner.to_owned();
    }
    let Some(start) = trimmed.find('{') else { return output.to_owned() };
    let preamble = &trimmed[..start];
    if start == 0 || preamble.contains('}') || preamble.chars().count() > MAX_PREAMBLE_CHARS {
        return output.to_owned();
    }
    let document = &trimmed[start..];
    match serde_json::from_str::<Value>(document) {
        Ok(Value::Object(_)) => document.to_owned(),
        _ => output.to_owned(),
    }
}

/// The content of a whole-output Markdown code fence (```` ```json … ``` ````), if it is one.
fn strip_fence(text: &str) -> Option<&str> {
    let body = text.strip_prefix("```")?.strip_suffix("```")?;
    let (language, content) = body.split_once('\n')?;
    (language.trim().is_empty() || language.trim().eq_ignore_ascii_case("json"))
        .then(|| content.trim())
}

#[cfg(test)]
mod tests {
    use super::unwrap_document;

    const DOCUMENT: &str = r#"{"schema":"kiss.inference-response.v1","presentation":{"prose":"I'll check the system information."},"emissions":[]}"#;

    #[test]
    fn a_short_plain_text_preamble_is_removed() {
        let output = format!("I'll check the system information.\n\n{DOCUMENT}");
        assert_eq!(unwrap_document(&output), DOCUMENT);
        let paraphrase =
            format!("I'll run a safe smoke test first, nothing destructive.\n{DOCUMENT}");
        assert_eq!(unwrap_document(&paraphrase), DOCUMENT, "a paraphrase of the prose is fine too");
    }

    #[test]
    fn whole_output_code_fences_are_removed() {
        for fence in ["```json", "```", "```JSON"] {
            let output = format!("{fence}\n{DOCUMENT}\n```");
            assert_eq!(unwrap_document(&output), DOCUMENT, "{fence}");
        }
    }

    #[test]
    fn anything_else_is_left_for_validation_to_reject() {
        let json_before = format!("{{\"a\": 1}} and then\n{DOCUMENT}");
        assert_eq!(unwrap_document(&json_before), json_before, "a preamble holding JSON is kept");
        let long = format!("{}\n{DOCUMENT}", "word ".repeat(300));
        assert_eq!(unwrap_document(&long), long, "long preambles are not presentation notes");
        let broken = "Sure!\n{\"schema\": ";
        assert_eq!(unwrap_document(broken), broken);
        let trailing = format!("{DOCUMENT}\nDone!");
        assert_eq!(unwrap_document(&trailing), trailing);
        assert_eq!(unwrap_document("not json at all"), "not json at all");
        let fenced_text = "```python\nprint(1)\n```";
        assert_eq!(unwrap_document(fenced_text), fenced_text);
        assert_eq!(unwrap_document(DOCUMENT), DOCUMENT);
    }
}
