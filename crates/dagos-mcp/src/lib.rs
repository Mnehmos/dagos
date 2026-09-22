//! Optional MCP capability discovery for DAGOS.
//!
//! DAGOS works without MCP. When a workspace names MCP servers, this crate asks each one (stdio
//! transport) for its tools and turns them into IR capability descriptions. Discovery is
//! descriptive only: DAGOS v0.1 never calls MCP tools, and nothing about MCP enters DAGOS state.
//! Every server is time-boxed, and a server that is missing, crashes, hangs, or speaks garbage is
//! reported and skipped; it can never block a run.

mod client;

use std::path::Path;
use std::time::Duration;

use dagos_core::domain::IrTool;
use serde::{Deserialize, Serialize};

pub use client::{McpError, McpTool, PROTOCOL_VERSION, list_tools};

/// MCP servers to ask for capabilities (`mcp.json`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    pub servers: Vec<McpServer>,
}

/// One MCP server, started as a child process speaking MCP over stdio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    /// Names the server's tools in IR: `<id>.<tool>`.
    pub id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
}

impl McpConfig {
    /// Reads `path`; a missing file means MCP is not configured.
    pub fn load(path: &Path) -> Result<Option<Self>, String> {
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let config: Self = serde_json::from_str(&text)
            .map_err(|error| format!("invalid {}: {error}", path.display()))?;
        for server in &config.servers {
            let valid_id = !server.id.is_empty()
                && server.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
            if !valid_id {
                return Err(format!(
                    "invalid MCP server id `{}`: use letters, digits, - and _",
                    server.id
                ));
            }
        }
        Ok(Some(config))
    }
}

/// What discovery found: capability descriptions for IR and how each server answered.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Capabilities {
    pub tools: Vec<IrTool>,
    pub servers: Vec<ServerStatus>,
}

/// How one MCP server answered discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServerStatus {
    pub id: String,
    /// How many tools it described.
    pub tools: usize,
    /// Why it could not be used, if it could not.
    pub error: Option<String>,
}

/// Asks every configured server for its tools, allowing each at most `timeout`.
pub async fn discover(config: &McpConfig, timeout: Duration) -> Capabilities {
    let mut capabilities = Capabilities::default();
    for server in &config.servers {
        match list_tools(server, timeout).await {
            Ok(tools) => {
                capabilities.servers.push(ServerStatus {
                    id: server.id.clone(),
                    tools: tools.len(),
                    error: None,
                });
                capabilities.tools.extend(tools.into_iter().map(|tool| IrTool {
                    name: format!("{}.{}", server.id, tool.name),
                    description: tool.description,
                    input_schema: tool.input_schema,
                }));
            }
            Err(error) => capabilities.servers.push(ServerStatus {
                id: server.id.clone(),
                tools: 0,
                error: Some(error.to_string()),
            }),
        }
    }
    capabilities
}
