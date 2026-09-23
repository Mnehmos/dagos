//! How DAGOS inference IR becomes a Chat Completions request.
//!
//! The system message is the IR's editable system prompt followed by the fixed DAGOS protocol
//! instructions and the response schema. The user message is the IR itself, as compact JSON: the
//! model receives machine-readable input and must answer with one machine-readable document.

use dagos_core::contracts::Contract;
use dagos_core::domain::{InferenceIr, JevRequest, ModelId, NodeType};
use serde_json::{Value, json};

const PROTOCOL: &str = "\
You are the inference endpoint of DAGOS, a DAG operating system for coding workflows.

The user message is a kiss.inference-ir.v1 JSON document:
- task.message is the request to answer; task.node_id is its durable node.
- context lists the durable DAG nodes that are active for this request, with their relations.
- recent_events is the conversation so far, oldest first: each earlier turn's request (the user's \
message) and your reply (prose), or why that turn failed.
- tools, if present, are tools you may use. To use them, list calls in \"tool_calls\" (name exactly \
as listed, arguments matching its input_schema). DAGOS runs the calls the person permits and sends \
you a new IR whose tool_results say what each returned, failed with, or why it was denied; keep \
going until you can answer, then reply without tool_calls. Every reply, including one that only \
calls tools, is still exactly one JSON object: say what you are doing in presentation.prose, never \
before or after the object. Some tools act on the person's computer: call only what the request \
needs.
- tool_results, if present, are this request's earlier tool calls and their outcomes.

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
context carried over from the previous run), and the `edges` between them.

For each candidate, decide whether it belongs in the active context for answering `message`: \
`active` if it is relevant, `inactive` if it is not (for example superseded by a newer node, \
stale, or unrelated). Candidates you leave out keep their current membership.

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
    let questions: serde_json::Map<String, Value> = request
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
    Ok(json!({"schema": "kiss.jev-context.v1", "classifications": classifications}).to_string())
}

/// The response document inside a model's final output, for two harmless quirks of chat models:
/// the whole document wrapped in a Markdown code fence, or a one-line preamble that repeats (part
/// of) the document's own `presentation.prose` before the object. Nothing is lost by removing
/// either. Any other output is returned unchanged, so validation still fails closed on it.
pub fn unwrap_document(output: &str) -> String {
    let trimmed = output.trim();
    if let Some(inner) = strip_fence(trimmed)
        && matches!(serde_json::from_str::<Value>(inner), Ok(Value::Object(_)))
    {
        return inner.to_owned();
    }
    if trimmed.starts_with('{') {
        return output.to_owned();
    }
    for (index, _) in trimmed.match_indices('{') {
        let Ok(document @ Value::Object(_)) = serde_json::from_str::<Value>(&trimmed[index..])
        else {
            continue;
        };
        let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
        let preamble = words(&trimmed[..index]);
        let prose = document.pointer("/presentation/prose").and_then(Value::as_str).unwrap_or("");
        if !preamble.is_empty() && words(prose).contains(&preamble) {
            return trimmed[index..].to_owned();
        }
        break;
    }
    output.to_owned()
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
    fn a_preamble_repeating_the_prose_is_removed() {
        let output = format!("I'll check the system information.\n\n{DOCUMENT}");
        assert_eq!(unwrap_document(&output), DOCUMENT);
        let partial = format!("I'll check  the system\ninformation.\n{DOCUMENT}");
        assert_eq!(unwrap_document(&partial), DOCUMENT, "whitespace differences do not matter");
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
        let different = format!("Here is something else entirely.\n{DOCUMENT}");
        assert_eq!(
            unwrap_document(&different),
            different,
            "text not in the prose is never dropped"
        );
        let trailing = format!("{DOCUMENT}\nDone!");
        assert_eq!(unwrap_document(&trailing), trailing);
        assert_eq!(unwrap_document("not json at all"), "not json at all");
        let fenced_text = "```python\nprint(1)\n```";
        assert_eq!(unwrap_document(fenced_text), fenced_text);
        assert_eq!(unwrap_document(DOCUMENT), DOCUMENT);
    }
}
