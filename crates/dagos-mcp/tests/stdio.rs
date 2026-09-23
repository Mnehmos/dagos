//! MCP discovery against a real stdio server (the `dagos-mcp-fixture` binary), and proof that
//! DAGOS runs are unaffected by MCP being absent, broken, or present.

use std::sync::Arc;
use std::time::Duration;

use dagos_core::context::FakeJev;
use dagos_core::domain::{EventData, IrTool, ModelId, ProviderId, RunConfig, RunStatus};
use dagos_core::provider::FakeProvider;
use dagos_core::runtime::Runtime;
use dagos_core::store::Store;
use dagos_core::tools::{ToolExecutor, ToolRequest};
use dagos_mcp::{
    MAX_TEXT_CHARS, McpConfig, McpError, McpPool, McpServer, Policy, discover, list_tools,
    tool_output,
};
use serde_json::json;

const TIMEOUT: Duration = Duration::from_secs(10);

fn fixture(id: &str, args: &[&str]) -> McpServer {
    McpServer::new(
        id,
        env!("CARGO_BIN_EXE_dagos-mcp-fixture"),
        args.iter().map(|arg| arg.to_string()).collect(),
    )
}

#[tokio::test]
async fn lists_tools_across_pages() {
    let tools = list_tools(&fixture("fixture", &[]), TIMEOUT).await.unwrap();
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, ["read_file", "search"]);
    assert_eq!(tools[0].description, "Read a file from the workspace.");
    assert_eq!(tools[0].input_schema["properties"]["path"]["type"], "string");
}

#[tokio::test]
async fn broken_servers_fail_with_a_reason_instead_of_hanging() {
    let missing = McpServer::new("gone", "dagos-no-such-command", vec![]);
    assert!(matches!(list_tools(&missing, TIMEOUT).await, Err(McpError::Spawn { .. })));
    assert_eq!(list_tools(&fixture("x", &["--exit"]), TIMEOUT).await, Err(McpError::Closed));
    assert!(matches!(
        list_tools(&fixture("x", &["--garbage"]), TIMEOUT).await,
        Err(McpError::Protocol(reason)) if reason.contains("not a JSON-RPC message")
    ));
    let short = Duration::from_millis(500);
    assert_eq!(
        list_tools(&fixture("x", &["--silent"]), short).await,
        Err(McpError::Timeout(short))
    );
}

#[tokio::test]
async fn discovery_namespaces_tools_and_reports_every_server() {
    let config = McpConfig {
        servers: vec![
            fixture("files", &[]),
            McpServer::new("offline", "dagos-no-such-command", vec![]),
        ],
    };
    let capabilities = discover(&config, TIMEOUT).await;
    let names: Vec<&str> = capabilities.tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, ["files.read_file", "files.search"]);
    assert_eq!(capabilities.servers.len(), 2);
    assert_eq!((capabilities.servers[0].tools.len(), &capabilities.servers[0].error), (2, &None));
    assert!(capabilities.servers[1].error.as_ref().unwrap().contains("cannot start"));
}

#[test]
fn configuration_is_optional_and_validated() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    assert_eq!(McpConfig::load(&path), Ok(None), "no file: MCP is simply not configured");
    std::fs::write(&path, r#"{"servers": [{"id": "fs", "command": "npx", "args": ["-y", "x"]}]}"#)
        .unwrap();
    assert_eq!(McpConfig::load(&path).unwrap().unwrap().servers[0].id, "fs");
    for text in [
        "nope",
        r#"{"servers": [{"id": "bad id", "command": "x"}]}"#,
        r#"{"servers": [{"id": "dagos", "command": "x"}]}"#,
        r#"{"servers": [{"id": "fs", "command": "x", "env": {}}]}"#,
        r#"{"servers": [{"id": "fs", "command": "x", "policy": "sometimes"}]}"#,
        r#"{"servers": [{"id": "fs", "command": "x"}, {"id": "fs", "command": "y"}]}"#,
        r#"{"servers": [{"id": "fs", "command": " "}]}"#,
    ] {
        std::fs::write(&path, text).unwrap();
        assert!(McpConfig::load(&path).is_err(), "accepted {text}");
    }
}

async fn run_with_tools(tools: Vec<IrTool>) -> (RunStatus, Vec<IrTool>) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let runtime = Runtime::new(store.clone(), Arc::new(FakeJev::new()))
        .with_provider(Arc::new(FakeProvider::new()))
        .with_tools(tools);
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let config = RunConfig {
        provider_id: ProviderId::parse("fake").unwrap(),
        model_id: ModelId::parse("fake-echo").unwrap(),
        system_prompt: String::new(),
    };
    let run = runtime.run(&project, "What can you use?", &config).await.unwrap();
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    let ir = events
        .into_iter()
        .find_map(|event| match event.data {
            EventData::IrCompiled { ir } => Some(ir),
            _ => None,
        })
        .unwrap();
    (run.status, ir.tools)
}

#[tokio::test]
async fn runs_work_without_mcp_and_carry_its_capabilities_when_configured() {
    // MCP unavailable: every server failed, so there are no tools, and the run still completes.
    let offline =
        McpConfig { servers: vec![McpServer::new("offline", "dagos-no-such-command", vec![])] };
    let none = discover(&offline, TIMEOUT).await;
    assert!(none.tools.is_empty());
    assert_eq!(run_with_tools(none.tools).await, (RunStatus::Completed, vec![]));

    // MCP available: its tool descriptions reach the provider through IR, and nothing else changes.
    let available = discover(&McpConfig { servers: vec![fixture("files", &[])] }, TIMEOUT).await;
    let (status, tools) = run_with_tools(available.tools.clone()).await;
    assert_eq!(status, RunStatus::Completed);
    assert_eq!(tools, available.tools);
}

#[tokio::test]
async fn policies_decide_which_tools_models_see() {
    let mut files = fixture("files", &[]);
    files.policy = Policy::Allow;
    files.tools.insert("search".into(), Policy::Off);
    let config = McpConfig { servers: vec![files] };
    assert_eq!(config.policy_of("files.read_file"), Policy::Allow);
    assert_eq!(config.policy_of("files.search"), Policy::Off);
    assert_eq!(config.policy_of("other.read_file"), Policy::Off);
    assert_eq!(config.policy_of("no-dot"), Policy::Off);

    let capabilities = discover(&config, TIMEOUT).await;
    let names: Vec<&str> = capabilities.tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, ["files.read_file"], "`off` tools are not offered to models");
    let statuses = &capabilities.servers[0].tools;
    assert_eq!(statuses.len(), 2, "the settings still list every tool");
    assert_eq!(statuses[1].policy, Policy::Off);

    // Changing policies needs no restart: the pool recomputes from what it listed.
    let pool = McpPool::new(config.clone(), TIMEOUT);
    assert_eq!(pool.discover().await.tools.len(), 1);
    let mut all = config.clone();
    all.servers[0].tools.clear();
    assert_eq!(pool.set_policies(all).tools.len(), 2);

    let mut disabled = fixture("files", &[]);
    disabled.enabled = false;
    let capabilities = discover(&McpConfig { servers: vec![disabled] }, TIMEOUT).await;
    assert!(capabilities.tools.is_empty());
    assert!(!capabilities.servers[0].enabled && capabilities.servers[0].error.is_none());
}

#[tokio::test]
async fn the_pool_keeps_sessions_and_runs_tool_calls() {
    let pool = McpPool::new(McpConfig { servers: vec![fixture("files", &[])] }, TIMEOUT);
    assert_eq!(pool.discover().await.tools.len(), 2);
    let request = |name: &str, arguments: serde_json::Value| ToolRequest {
        call_id: "call_1".into(),
        name: name.into(),
        arguments: arguments.as_object().unwrap().clone(),
    };

    // The server asks the client for its roots mid-call; the pool declines and gets the answer.
    let read = pool.call(&request("files.read_file", json!({"path": "a.md"}))).await.unwrap();
    assert!(!read.is_error);
    assert_eq!(read.output["content"][0]["text"], "contents of a.md");
    let again = pool.call(&request("files.read_file", json!({"path": "b.md"}))).await.unwrap();
    assert_eq!(again.output["content"][0]["text"], "contents of b.md", "the session is reused");

    let search = pool.call(&request("files.search", json!({}))).await.unwrap();
    assert!(search.is_error);
    assert_eq!(search.output["content"][0]["type"], "image");
    assert!(search.output["content"][0].get("data").is_none(), "image data is never kept");

    assert!(pool.call(&request("other.read_file", json!({}))).await.is_err());
    assert!(pool.call(&request("nodot", json!({}))).await.is_err());
}

#[test]
fn long_text_is_cut_and_structured_content_kept() {
    let long = "x".repeat(MAX_TEXT_CHARS + 5);
    let (output, is_error) = tool_output(&json!({
        "content": [{"type": "text", "text": long}, {"type": "resource", "resource": {"uri": "file:///a", "text": "hi"}}],
        "structuredContent": {"count": 2}
    }));
    assert!(!is_error);
    let text = output["content"][0]["text"].as_str().unwrap();
    assert!(text.ends_with("(5 more characters cut)"));
    assert_eq!(output["content"][1], json!({"type": "resource", "uri": "file:///a", "text": "hi"}));
    assert_eq!(output["structured"], json!({"count": 2}));
}

#[test]
fn configuration_round_trips_with_policies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mcp.json");
    let mut server = McpServer::new("ooda", "node", vec!["dist/index.js".into()]);
    server.cwd = Some("C:/tools/ooda".into());
    server.tools.insert("exec_cli".into(), Policy::Ask);
    server.tools.insert("read_file".into(), Policy::Allow);
    let config = McpConfig { servers: vec![server] };
    config.save(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(!text.contains("\"enabled\""), "defaults are not written: {text}");
    assert_eq!(McpConfig::load(&path).unwrap().unwrap(), config);
}
