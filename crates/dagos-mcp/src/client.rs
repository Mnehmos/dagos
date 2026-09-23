//! A minimal MCP client over the stdio transport: start a server, list its tools, call them.
//!
//! Messages are newline-delimited JSON-RPC 2.0. A session is `initialize`, then
//! `notifications/initialized`, then any number of `tools/list` and `tools/call` requests. Requests
//! the server sends to the client (e.g. `roots/list`) are answered with "method not found": DAGOS
//! offers servers no capabilities of its own.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::McpServer;

/// The MCP protocol revision DAGOS asks for. Servers may answer with another revision; listing and
/// calling tools work the same way in all of them.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Page limit for `tools/list`, so a misbehaving server cannot keep discovery going forever.
const MAX_PAGES: u64 = 50;

/// A tool as an MCP server describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    pub input_schema: Map<String, Value>,
}

/// Why a server could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpError {
    #[error("cannot start `{command}`: {reason}")]
    Spawn { command: String, reason: String },
    #[error("no answer within {0:?}")]
    Timeout(Duration),
    #[error("the server closed its output before answering")]
    Closed,
    #[error("the server reported an error: {0}")]
    Server(String),
    #[error("protocol violation: {0}")]
    Protocol(String),
}

/// A running MCP server and its initialized session. The process is killed when the session is
/// dropped.
pub struct Session {
    child: Child,
    stdin: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

impl Session {
    /// Starts `server` and initializes a session within `timeout`.
    pub async fn start(server: &McpServer, timeout: Duration) -> Result<Self, McpError> {
        let mut command = Command::new(&server.command);
        command
            .args(&server.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(cwd) = &server.cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn().map_err(|error| McpError::Spawn {
            command: server.command.clone(),
            reason: error.to_string(),
        })?;
        let stdin = child.stdin.take().expect("stdin is piped");
        let lines = BufReader::new(child.stdout.take().expect("stdout is piped")).lines();
        let mut session = Self { child, stdin, lines, next_id: 1 };
        let client_info = json!({"name": "dagos", "version": env!("CARGO_PKG_VERSION")});
        let initialize = json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": client_info,
        });
        within(timeout, session.request("initialize", initialize)).await?;
        within(
            timeout,
            session.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})),
        )
        .await?;
        Ok(session)
    }

    /// Whether the server process is still running.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Every tool the server offers, following `nextCursor` pages, within `timeout`.
    pub async fn list_tools(&mut self, timeout: Duration) -> Result<Vec<McpTool>, McpError> {
        within(timeout, async {
            let mut tools = Vec::new();
            let mut cursor: Option<String> = None;
            for _ in 0..MAX_PAGES {
                let params = match &cursor {
                    Some(cursor) => json!({"cursor": cursor}),
                    None => json!({}),
                };
                let result = self.request("tools/list", params).await?;
                let page = result["tools"].as_array().ok_or_else(|| {
                    McpError::Protocol("tools/list result has no `tools` array".into())
                })?;
                for tool in page {
                    tools.push(parse_tool(tool)?);
                }
                match result.get("nextCursor").and_then(Value::as_str) {
                    Some(next) => cursor = Some(next.to_owned()),
                    None => return Ok(tools),
                }
            }
            Err(McpError::Protocol(format!("tools/list did not finish within {MAX_PAGES} pages")))
        })
        .await
    }

    /// Calls tool `name` with `arguments` and returns the raw `tools/call` result.
    pub async fn call_tool(
        &mut self,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, McpError> {
        self.request("tools/call", json!({"name": name, "arguments": arguments})).await
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).await?;
        self.receive(id).await
    }

    async fn send(&mut self, message: Value) -> Result<(), McpError> {
        let line = format!("{message}\n");
        self.stdin.write_all(line.as_bytes()).await.map_err(|_| McpError::Closed)?;
        self.stdin.flush().await.map_err(|_| McpError::Closed)
    }

    /// Reads until the response to `id`, skipping notifications and stale responses and
    /// declining requests from the server.
    async fn receive(&mut self, id: u64) -> Result<Value, McpError> {
        loop {
            let line = self
                .lines
                .next_line()
                .await
                .map_err(|error| McpError::Protocol(error.to_string()))?
                .ok_or(McpError::Closed)?;
            if line.trim().is_empty() {
                continue;
            }
            let message: Value = serde_json::from_str(&line).map_err(|_| {
                let excerpt: String = line.chars().take(80).collect();
                McpError::Protocol(format!("not a JSON-RPC message: {excerpt}"))
            })?;
            if let (Some(request_id), Some(_)) = (message.get("id"), message.get("method")) {
                let declined = json!({
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "error": {"code": -32601, "message": "DAGOS does not offer this capability"},
                });
                self.send(declined).await?;
                continue;
            }
            if message.get("id") != Some(&json!(id)) {
                continue;
            }
            if let Some(error) = message.get("error") {
                let text =
                    error["message"].as_str().map_or_else(|| error.to_string(), str::to_owned);
                return Err(McpError::Server(text));
            }
            return message
                .get("result")
                .cloned()
                .ok_or_else(|| McpError::Protocol(format!("response {id} has no result")));
        }
    }
}

async fn within<T>(
    timeout: Duration,
    future: impl Future<Output = Result<T, McpError>>,
) -> Result<T, McpError> {
    tokio::time::timeout(timeout, future).await.unwrap_or(Err(McpError::Timeout(timeout)))
}

/// Starts `server`, lists its tools, and stops it; the whole session must finish within `timeout`.
pub async fn list_tools(server: &McpServer, timeout: Duration) -> Result<Vec<McpTool>, McpError> {
    let started = std::time::Instant::now();
    let mut session = Session::start(server, timeout).await?;
    session.list_tools(timeout.saturating_sub(started.elapsed())).await
}

fn parse_tool(tool: &Value) -> Result<McpTool, McpError> {
    let name = tool["name"]
        .as_str()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| McpError::Protocol(format!("tool without a name: {tool}")))?;
    let input_schema = match tool.get("inputSchema") {
        Some(Value::Object(schema)) => schema.clone(),
        None => Map::new(),
        Some(other) => {
            return Err(McpError::Protocol(format!(
                "tool `{name}` has a non-object inputSchema: {other}"
            )));
        }
    };
    Ok(McpTool {
        name: name.to_owned(),
        description: tool["description"].as_str().unwrap_or_default().to_owned(),
        input_schema,
    })
}

/// The longest text kept from one piece of tool output; longer text is cut and marked.
pub const MAX_TEXT_CHARS: usize = 20_000;

/// A `tools/call` result as DAGOS records it and returns it to the model: text (truncated at
/// [`MAX_TEXT_CHARS`]), resources, and structured content are kept; image and audio data are
/// replaced by a note, because models receive JSON and events should stay small. The flag is the
/// result's `isError`.
pub fn tool_output(result: &Value) -> (Value, bool) {
    let is_error = result.get("isError").and_then(Value::as_bool).unwrap_or(false);
    let content: Vec<Value> = result
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(content_part)
        .collect();
    let mut output = json!({"content": content});
    if let Some(structured) = result.get("structuredContent") {
        let size = structured.to_string().chars().count();
        output["structured"] = if size <= MAX_TEXT_CHARS {
            structured.clone()
        } else {
            json!(format!("(structured content omitted: {size} characters)"))
        };
    }
    (output, is_error)
}

fn truncate(text: &str) -> String {
    let count = text.chars().count();
    if count <= MAX_TEXT_CHARS {
        return text.to_owned();
    }
    let kept: String = text.chars().take(MAX_TEXT_CHARS).collect();
    format!("{kept}\n… ({} more characters cut)", count - MAX_TEXT_CHARS)
}

fn content_part(part: &Value) -> Value {
    let kind = part["type"].as_str().unwrap_or("unknown");
    match kind {
        "text" => {
            json!({"type": "text", "text": truncate(part["text"].as_str().unwrap_or_default())})
        }
        "image" | "audio" => json!({
            "type": kind,
            "mimeType": part["mimeType"],
            "note": format!(
                "{kind} data ({} base64 characters) is not passed to models",
                part["data"].as_str().map_or(0, str::len)
            ),
        }),
        "resource" => {
            let resource = &part["resource"];
            let mut kept = json!({"type": "resource", "uri": resource["uri"]});
            if let Some(text) = resource["text"].as_str() {
                kept["text"] = json!(truncate(text));
            }
            kept
        }
        "resource_link" => {
            json!({"type": "resource_link", "uri": part["uri"], "name": part["name"]})
        }
        other => json!({"type": other}),
    }
}
