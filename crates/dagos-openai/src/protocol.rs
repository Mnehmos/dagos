//! How DAGOS inference IR becomes a Chat Completions request.
//!
//! The system message is the IR's editable system prompt followed by the fixed DAGOS protocol
//! instructions and the response schema. The user message is the IR itself, as compact JSON: the
//! model receives machine-readable input and must answer with one machine-readable document.

use dagos_core::contracts::Contract;
use dagos_core::domain::{InferenceIr, JevRequest, ModelId};
use serde_json::{Value, json};

const PROTOCOL: &str = "\
You are the inference endpoint of DAGOS, a DAG operating system for coding workflows.

The user message is a kiss.inference-ir.v1 JSON document:
- task.message is the request to answer; task.node_id is its durable node.
- context lists the durable DAG nodes that are active for this request, with their relations.
- recent_events says how recent runs ended, including your earlier replies and rejection reasons.
- tools, if present, only describe capabilities; nothing has been executed.

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
