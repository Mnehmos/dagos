//! Native tool calling: the IR's tools offered through Chat Completions' `tools`, and the model's
//! native tool calls turned back into a `kiss.inference-response.v1` document.
//!
//! The runtime never sees the difference: whichever way a model calls tools, the adapter returns
//! one response document, which DAGOS validates like any other. Final replies still use the
//! document (emissions need it); only tool calls move to the provider's own mechanism, which
//! models handle more reliably than JSON they must write themselves.

use std::collections::BTreeMap;

use dagos_core::domain::{InferenceIr, ModelId};
use serde_json::{Value, json};

use crate::prose::ProseExtractor;
use crate::protocol::{NATIVE_TOOLS, system_message, unwrap_document, user_message};

/// A native tool call as it streamed: the function name and its raw argument text.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawToolCall {
    pub name: String,
    pub arguments: String,
}

/// Adds one streamed `delta.tool_calls` part to `calls` (parts carry an `index`, and a call's
/// name and arguments may arrive in pieces).
pub(crate) fn accumulate(calls: &mut Vec<RawToolCall>, part: &Value) {
    let index = part.get("index").and_then(Value::as_u64).map_or(calls.len(), |i| i as usize);
    if calls.len() <= index {
        calls.resize(index + 1, RawToolCall::default());
    }
    let call = &mut calls[index];
    if let Some(name) = part.pointer("/function/name").and_then(Value::as_str) {
        call.name.push_str(name);
    }
    if let Some(arguments) = part.pointer("/function/arguments").and_then(Value::as_str) {
        call.arguments.push_str(arguments);
    }
}

/// A function name providers accept (`^[a-zA-Z0-9_-]{1,64}$`) for each IR tool name, unique:
/// `ooda.exec_cli` becomes `ooda__exec_cli`. Returns encoded → IR name.
fn function_names(ir: &InferenceIr) -> BTreeMap<String, String> {
    let mut names = BTreeMap::new();
    for tool in &ir.tools {
        let base: String = tool
            .name
            .replace('.', "__")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
            .take(60)
            .collect();
        let mut name = base.clone();
        let mut suffix = 2;
        while names.contains_key(&name) {
            name = format!("{base}_{suffix}");
            suffix += 1;
        }
        names.insert(name, tool.name.clone());
    }
    names
}

/// The Chat Completions request for `ir` with its tools offered natively, and the map from
/// function names back to IR tool names. The tools leave the IR text (the endpoint carries them),
/// and the system message says how to call them.
pub fn native_request_body(
    model_id: &ModelId,
    ir: &InferenceIr,
    json_mode: bool,
) -> (Value, BTreeMap<String, String>) {
    let names = function_names(ir);
    let tools: Vec<Value> = names
        .iter()
        .filter_map(|(function, name)| {
            let tool = ir.tools.iter().find(|tool| &tool.name == name)?;
            let mut parameters = Value::Object(tool.input_schema.clone());
            if parameters.get("type").is_none() {
                parameters["type"] = json!("object");
            }
            Some(json!({
                "type": "function",
                "function": {"name": function, "description": tool.description, "parameters": parameters},
            }))
        })
        .collect();
    let mut without_tools = ir.clone();
    without_tools.tools.clear();
    let mut body = json!({
        "model": model_id.as_str(),
        "stream": true,
        "messages": [
            {"role": "system", "content": format!("{}{NATIVE_TOOLS}", system_message(ir))},
            {"role": "user", "content": user_message(&without_tools)},
        ],
        "tools": tools,
        "tool_choice": "auto",
    });
    if json_mode {
        body["response_format"] = json!({"type": "json_object"});
    }
    (body, names)
}

/// The response document for a completion made with native tools: without tool calls, the
/// content's document as usual; with them, the content's document (or, if the content is a plain
/// note, a document whose prose is that note) with the calls as `tool_calls`, by IR name.
pub fn native_document(
    content: &str,
    calls: Vec<RawToolCall>,
    names: &BTreeMap<String, String>,
) -> Result<String, String> {
    let calls: Vec<RawToolCall> = calls.into_iter().filter(|call| !call.name.is_empty()).collect();
    if calls.is_empty() {
        return Ok(unwrap_document(content));
    }
    let mut tool_calls = Vec::with_capacity(calls.len());
    for call in calls {
        let name = names.get(&call.name).cloned().unwrap_or(call.name);
        let raw = if call.arguments.trim().is_empty() { "{}" } else { call.arguments.as_str() };
        let arguments = match serde_json::from_str::<Value>(raw) {
            Ok(Value::Object(arguments)) => arguments,
            _ => {
                return Err(format!(
                    "the arguments of the native tool call `{name}` are not a JSON object: {raw}"
                ));
            }
        };
        tool_calls.push(json!({"name": name, "arguments": arguments}));
    }
    let document = match serde_json::from_str::<Value>(&unwrap_document(content)) {
        Ok(Value::Object(document)) if document.contains_key("presentation") => {
            let mut document = Value::Object(document);
            if let Some(existing) = document.get("tool_calls").and_then(Value::as_array) {
                let mut all = existing.clone();
                all.extend(tool_calls);
                tool_calls = all;
            }
            document["tool_calls"] = json!(tool_calls);
            document
        }
        _ => json!({
            "schema": "kiss.inference-response.v1",
            "presentation": {"prose": content.trim()},
            "emissions": [],
            "tool_calls": tool_calls,
        }),
    };
    Ok(document.to_string())
}

/// Whether an endpoint error means the model or endpoint does not take native tools.
pub fn refuses_tools(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    let client_error = ["http 400", "http 404", "http 422"].iter().any(|code| lower.contains(code));
    client_error && (lower.contains("tool") || lower.contains("function"))
}

/// Streams the prose of a completion that may be a plain note (while calling tools) or a
/// response document: plain text passes through until a line begins with `{`; from there the
/// document's `presentation.prose` is extracted as usual.
#[derive(Debug, Default)]
pub struct NativeProse {
    json: Option<ProseExtractor>,
    decided: bool,
    at_line_start: bool,
}

impl NativeProse {
    pub fn push(&mut self, chunk: &str) -> String {
        let mut out = String::new();
        for c in chunk.chars() {
            if let Some(json) = &mut self.json {
                out.push_str(&json.push(&c.to_string()));
                continue;
            }
            let starts_line = !self.decided || self.at_line_start;
            if c == '{' && starts_line {
                let mut json = ProseExtractor::default();
                out.push_str(&json.push("{"));
                self.json = Some(json);
                continue;
            }
            if !c.is_whitespace() {
                self.decided = true;
            }
            if self.decided {
                out.push(c);
            }
            if c == '\n' {
                self.at_line_start = true;
            } else if !c.is_whitespace() {
                self.at_line_start = false;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_parts_assemble_into_calls() {
        let mut calls = Vec::new();
        for part in [
            json!({"index": 0, "id": "a", "function": {"name": "ooda__exec", "arguments": ""}}),
            json!({"index": 0, "function": {"name": "_cli", "arguments": "{\"command\":"}}),
            json!({"index": 1, "function": {"name": "ooda__read_file", "arguments": "{}"}}),
            json!({"index": 0, "function": {"arguments": " \"ls\"}"}}),
        ] {
            accumulate(&mut calls, &part);
        }
        assert_eq!(
            calls[0],
            RawToolCall {
                name: "ooda__exec_cli".into(),
                arguments: "{\"command\": \"ls\"}".into()
            }
        );
        assert_eq!(calls[1].name, "ooda__read_file");
    }

    #[test]
    fn native_calls_become_a_response_document() {
        let names = BTreeMap::from([("ooda__exec_cli".to_owned(), "ooda.exec_cli".to_owned())]);
        let call = || RawToolCall {
            name: "ooda__exec_cli".into(),
            arguments: "{\"command\":\"ls\"}".into(),
        };
        let document: Value = serde_json::from_str(
            &native_document("Listing the files.", vec![call()], &names).unwrap(),
        )
        .unwrap();
        assert_eq!(document["presentation"]["prose"], "Listing the files.");
        assert_eq!(
            document["tool_calls"],
            json!([{"name": "ooda.exec_cli", "arguments": {"command": "ls"}}])
        );
        assert_eq!(document["emissions"], json!([]));

        let with_document = r#"{"schema":"kiss.inference-response.v1","presentation":{"prose":"Checking."},"emissions":[]}"#;
        let document: Value =
            serde_json::from_str(&native_document(with_document, vec![call()], &names).unwrap())
                .unwrap();
        assert_eq!(document["presentation"]["prose"], "Checking.");
        assert_eq!(document["tool_calls"][0]["name"], "ooda.exec_cli");

        assert_eq!(native_document(with_document, vec![], &names).unwrap(), with_document);
        let bad = RawToolCall { name: "ooda__exec_cli".into(), arguments: "[1]".into() };
        assert!(native_document("", vec![bad], &names).unwrap_err().contains("not a JSON object"));
    }

    #[test]
    fn refusals_are_recognised_and_notes_stream() {
        assert!(refuses_tools(
            "HTTP 404 Not Found from x: No endpoints found that support tool use"
        ));
        assert!(refuses_tools("HTTP 400 Bad Request from x: model does not support functions"));
        assert!(!refuses_tools("HTTP 401 Unauthorized from x: bad key"));
        assert!(!refuses_tools("HTTP 500 from x: tool crashed"));

        let mut plain = NativeProse::default();
        let streamed: String =
            ["Listing ", "the files", "."].iter().map(|c| plain.push(c)).collect();
        assert_eq!(streamed, "Listing the files.");
        let mut document = NativeProse::default();
        let streamed: String = ["{\"presentation\":{\"prose\":\"Do", "ne.\"}}"]
            .iter()
            .map(|c| document.push(c))
            .collect();
        assert_eq!(streamed, "Done.");
        let mut mixed = NativeProse::default();
        let streamed = mixed.push("Returns `{}`.\n{\"presentation\":{\"prose\":\"Hi\"}}");
        assert_eq!(
            streamed, "Returns `{}`.\nHi",
            "braces mid-line are prose; a line-start brace opens the document"
        );
    }
}
