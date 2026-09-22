//! A minimal MCP client over the stdio transport: just enough to list a server's tools.
//!
//! Messages are newline-delimited JSON-RPC 2.0. The session is `initialize`, then
//! `notifications/initialized`, then `tools/list` (following `nextCursor` pages), after which the
//! server process is stopped. Nothing is ever called.

use std::process::Stdio;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use crate::McpServer;

/// The MCP protocol revision DAGOS asks for. Servers may answer with another revision; listing
/// tools works the same way in all of them.
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

/// Why a server's tools could not be listed.
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

/// Starts `server`, lists its tools, and stops it; the whole session must finish within `timeout`.
pub async fn list_tools(server: &McpServer, timeout: Duration) -> Result<Vec<McpTool>, McpError> {
    let mut child = Command::new(&server.command)
        .args(&server.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| McpError::Spawn {
            command: server.command.clone(),
            reason: error.to_string(),
        })?;
    let listed = tokio::time::timeout(timeout, session(&mut child))
        .await
        .unwrap_or(Err(McpError::Timeout(timeout)));
    let _ = child.kill().await;
    listed
}

async fn session(child: &mut Child) -> Result<Vec<McpTool>, McpError> {
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let mut lines = BufReader::new(child.stdout.take().expect("stdout is piped")).lines();

    let client_info = json!({"name": "dagos", "version": env!("CARGO_PKG_VERSION")});
    let initialize =
        json!({"protocolVersion": PROTOCOL_VERSION, "capabilities": {}, "clientInfo": client_info});
    request(&mut stdin, 1, "initialize", initialize).await?;
    receive(&mut lines, 1).await?;
    send(&mut stdin, json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await?;

    let mut tools = Vec::new();
    let mut cursor: Option<String> = None;
    for id in 2..2 + MAX_PAGES {
        let params = match &cursor {
            Some(cursor) => json!({"cursor": cursor}),
            None => json!({}),
        };
        request(&mut stdin, id, "tools/list", params).await?;
        let result = receive(&mut lines, id).await?;
        let page = result["tools"]
            .as_array()
            .ok_or_else(|| McpError::Protocol("tools/list result has no `tools` array".into()))?;
        for tool in page {
            tools.push(parse_tool(tool)?);
        }
        match result.get("nextCursor").and_then(Value::as_str) {
            Some(next) => cursor = Some(next.to_owned()),
            None => return Ok(tools),
        }
    }
    Err(McpError::Protocol(format!("tools/list did not finish within {MAX_PAGES} pages")))
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

async fn request(
    stdin: &mut ChildStdin,
    id: u64,
    method: &str,
    params: Value,
) -> Result<(), McpError> {
    send(stdin, json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})).await
}

async fn send(stdin: &mut ChildStdin, message: Value) -> Result<(), McpError> {
    let line = format!("{message}\n");
    stdin.write_all(line.as_bytes()).await.map_err(|_| McpError::Closed)?;
    stdin.flush().await.map_err(|_| McpError::Closed)
}

/// Reads until the response to `id`, skipping notifications and server requests.
async fn receive(lines: &mut Lines<BufReader<ChildStdout>>, id: u64) -> Result<Value, McpError> {
    loop {
        let line = lines
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
        if message.get("id") != Some(&json!(id)) {
            continue;
        }
        if let Some(error) = message.get("error") {
            let text = error["message"].as_str().map_or_else(|| error.to_string(), str::to_owned);
            return Err(McpError::Server(text));
        }
        return message
            .get("result")
            .cloned()
            .ok_or_else(|| McpError::Protocol(format!("response {id} has no result")));
    }
}
