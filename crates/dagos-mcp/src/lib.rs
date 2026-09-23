//! Optional MCP tools for DAGOS.
//!
//! DAGOS works without MCP. When a workspace names MCP servers (`mcp.json`), this crate keeps a
//! session with each enabled server, describes their tools to models as IR capabilities, and runs
//! the calls a person permits. Every tool has a policy: `off` (never offered to models), `ask`
//! (a person approves each call), or `allow` (runs without asking). A server that is missing,
//! crashes, hangs, or speaks garbage is reported and skipped; it can never block a run.

mod client;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use dagos_core::domain::IrTool;
use dagos_core::tools::{ToolExecutor, ToolOutput, ToolRequest};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

pub use client::{
    MAX_TEXT_CHARS, McpError, McpTool, PROTOCOL_VERSION, Session, list_tools, tool_output,
};

/// Whether a tool is offered to models and whether its calls need approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    /// Not offered to models at all.
    Off,
    /// Offered; a person approves each call.
    Ask,
    /// Offered; calls run without asking.
    Allow,
}

fn ask() -> Policy {
    Policy::Ask
}

fn enabled() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_ask(value: &Policy) -> bool {
    *value == Policy::Ask
}

/// MCP servers and their tool policies (`mcp.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// The working directory to start the server in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Disabled servers are not started.
    #[serde(default = "enabled", skip_serializing_if = "is_true")]
    pub enabled: bool,
    /// The policy of every tool without its own entry in `tools`.
    #[serde(default = "ask", skip_serializing_if = "is_ask")]
    pub policy: Policy,
    /// Per-tool policies, by the server's own tool name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tools: BTreeMap<String, Policy>,
}

impl McpServer {
    /// A server with the default settings: enabled, every tool `ask`.
    pub fn new(id: impl Into<String>, command: impl Into<String>, args: Vec<String>) -> Self {
        Self {
            id: id.into(),
            command: command.into(),
            args,
            cwd: None,
            enabled: true,
            policy: Policy::Ask,
            tools: BTreeMap::new(),
        }
    }

    /// The policy of the server's tool `tool`.
    pub fn policy_of(&self, tool: &str) -> Policy {
        self.tools.get(tool).copied().unwrap_or(self.policy)
    }
}

/// Whether `id` can name a server: letters, digits, `-` and `_`.
pub fn valid_server_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
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
        config.validate()?;
        Ok(Some(config))
    }

    /// Writes the configuration to `path`.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        let text = serde_json::to_string_pretty(self).expect("MCP configuration serializes");
        std::fs::write(path, format!("{text}\n"))
            .map_err(|error| format!("cannot save {}: {error}", path.display()))
    }

    fn validate(&self) -> Result<(), String> {
        let mut seen = std::collections::BTreeSet::new();
        for server in &self.servers {
            if !valid_server_id(&server.id) {
                return Err(format!(
                    "invalid MCP server id `{}`: use letters, digits, - and _",
                    server.id
                ));
            }
            if !seen.insert(&server.id) {
                return Err(format!("MCP server id `{}` is used twice", server.id));
            }
            if server.command.trim().is_empty() {
                return Err(format!("MCP server `{}` has no command", server.id));
            }
        }
        Ok(())
    }

    /// The policy of the IR tool `name` (`<server>.<tool>`); `Off` if there is no such server.
    pub fn policy_of(&self, name: &str) -> Policy {
        let Some((server, tool)) = name.split_once('.') else { return Policy::Off };
        self.servers
            .iter()
            .find(|candidate| candidate.id == server && candidate.enabled)
            .map_or(Policy::Off, |server| server.policy_of(tool))
    }
}

/// What discovery found: the tools offered to models and how each server answered.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Capabilities {
    /// Tools offered to models (every tool whose policy is not `off`).
    pub tools: Vec<IrTool>,
    pub servers: Vec<ServerStatus>,
}

/// How one MCP server answered discovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServerStatus {
    pub id: String,
    pub enabled: bool,
    /// Every tool the server described, with its policy.
    pub tools: Vec<ToolStatus>,
    /// Why it could not be used, if it could not.
    pub error: Option<String>,
}

/// One of a server's tools, as the settings screen shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolStatus {
    pub name: String,
    pub description: String,
    pub policy: Policy,
}

/// Asks every enabled server in `config` for its tools with a one-off session each, allowing each
/// at most `timeout`.
pub async fn discover(config: &McpConfig, timeout: Duration) -> Capabilities {
    McpPool::new(config.clone(), timeout).discover().await
}

/// Live sessions with the configured servers: discovery keeps them running, and tool calls reuse
/// them (restarting a server that has exited). Dropping the pool stops every server.
pub struct McpPool {
    config: RwLock<McpConfig>,
    start_timeout: Duration,
    sessions: Mutex<BTreeMap<String, Arc<Mutex<Session>>>>,
    /// What each enabled server listed at the last discovery, or why it could not.
    listed: RwLock<BTreeMap<String, Result<Vec<McpTool>, String>>>,
}

impl McpPool {
    /// A pool over `config`; each server gets `start_timeout` to start and describe its tools.
    pub fn new(config: McpConfig, start_timeout: Duration) -> Self {
        Self {
            config: RwLock::new(config),
            start_timeout,
            sessions: Mutex::new(BTreeMap::new()),
            listed: RwLock::new(BTreeMap::new()),
        }
    }

    pub fn config(&self) -> McpConfig {
        self.config.read().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Replaces the tool policies without restarting servers. `config` must name the same servers
    /// with the same commands; start a new pool when those change.
    pub fn set_policies(&self, config: McpConfig) -> Capabilities {
        *self.config.write().unwrap_or_else(PoisonError::into_inner) = config;
        self.capabilities()
    }

    /// Starts every enabled server, lists its tools, and returns the capabilities.
    pub async fn discover(&self) -> Capabilities {
        for server in self.config().servers.into_iter().filter(|server| server.enabled) {
            let listed = match self.session(&server).await {
                Ok(session) => session.lock().await.list_tools(self.start_timeout).await,
                Err(error) => Err(error),
            };
            if listed.is_err() {
                self.sessions.lock().await.remove(&server.id);
            }
            self.listed
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(server.id.clone(), listed.map_err(|error| error.to_string()));
        }
        self.capabilities()
    }

    /// The capabilities from the last discovery under the current policies.
    pub fn capabilities(&self) -> Capabilities {
        let config = self.config();
        let listed = self.listed.read().unwrap_or_else(PoisonError::into_inner);
        let mut capabilities = Capabilities::default();
        for server in &config.servers {
            let (tools, error) = match listed.get(&server.id) {
                _ if !server.enabled => (Vec::new(), None),
                Some(Ok(tools)) => (tools.clone(), None),
                Some(Err(error)) => (Vec::new(), Some(error.clone())),
                None => (Vec::new(), Some("not started yet".to_owned())),
            };
            capabilities.tools.extend(
                tools.iter().filter(|tool| server.policy_of(&tool.name) != Policy::Off).map(
                    |tool| IrTool {
                        name: format!("{}.{}", server.id, tool.name),
                        description: tool.description.clone(),
                        input_schema: tool.input_schema.clone(),
                    },
                ),
            );
            capabilities.servers.push(ServerStatus {
                id: server.id.clone(),
                enabled: server.enabled,
                tools: tools
                    .iter()
                    .map(|tool| ToolStatus {
                        name: tool.name.clone(),
                        description: tool.description.clone(),
                        policy: server.policy_of(&tool.name),
                    })
                    .collect(),
                error,
            });
        }
        capabilities
    }

    /// The running session with `server`, started (or restarted) if needed.
    async fn session(&self, server: &McpServer) -> Result<Arc<Mutex<Session>>, McpError> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(&server.id)
            && session.lock().await.is_running()
        {
            return Ok(session.clone());
        }
        let session = Arc::new(Mutex::new(Session::start(server, self.start_timeout).await?));
        sessions.insert(server.id.clone(), session.clone());
        Ok(session)
    }
}

#[async_trait]
impl ToolExecutor for McpPool {
    async fn call(&self, request: &ToolRequest) -> Result<ToolOutput, String> {
        let (server_id, tool) = request
            .name
            .split_once('.')
            .ok_or_else(|| format!("`{}` does not name an MCP tool", request.name))?;
        let server = self
            .config()
            .servers
            .into_iter()
            .find(|server| server.id == server_id && server.enabled)
            .ok_or_else(|| format!("MCP server `{server_id}` is not configured"))?;
        let session = self.session(&server).await.map_err(|error| error.to_string())?;
        let result = session
            .lock()
            .await
            .call_tool(tool, &request.arguments)
            .await
            .map_err(|error| error.to_string())?;
        let (output, is_error) = tool_output(&result);
        Ok(ToolOutput { output, is_error })
    }
}
