//! MCP discovery against a real stdio server (the `dagos-mcp-fixture` binary), and proof that
//! DAGOS runs are unaffected by MCP being absent, broken, or present.

use std::sync::Arc;
use std::time::Duration;

use dagos_core::context::FakeJev;
use dagos_core::domain::{EventData, IrTool, ModelId, ProviderId, RunConfig, RunStatus};
use dagos_core::provider::FakeProvider;
use dagos_core::runtime::Runtime;
use dagos_core::store::Store;
use dagos_mcp::{McpConfig, McpError, McpServer, discover, list_tools};

const TIMEOUT: Duration = Duration::from_secs(10);

fn fixture(id: &str, args: &[&str]) -> McpServer {
    McpServer {
        id: id.into(),
        command: env!("CARGO_BIN_EXE_dagos-mcp-fixture").into(),
        args: args.iter().map(|arg| arg.to_string()).collect(),
    }
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
    let missing =
        McpServer { id: "gone".into(), command: "dagos-no-such-command".into(), args: vec![] };
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
            McpServer {
                id: "offline".into(),
                command: "dagos-no-such-command".into(),
                args: vec![],
            },
        ],
    };
    let capabilities = discover(&config, TIMEOUT).await;
    let names: Vec<&str> = capabilities.tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, ["files.read_file", "files.search"]);
    assert_eq!(capabilities.servers.len(), 2);
    assert_eq!((capabilities.servers[0].tools, &capabilities.servers[0].error), (2, &None));
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
        r#"{"servers": [{"id": "fs", "command": "x", "env": {}}]}"#,
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
    let offline = McpConfig {
        servers: vec![McpServer {
            id: "offline".into(),
            command: "dagos-no-such-command".into(),
            args: vec![],
        }],
    };
    let none = discover(&offline, TIMEOUT).await;
    assert!(none.tools.is_empty());
    assert_eq!(run_with_tools(none.tools).await, (RunStatus::Completed, vec![]));

    // MCP available: its tool descriptions reach the provider through IR, and nothing else changes.
    let available = discover(&McpConfig { servers: vec![fixture("files", &[])] }, TIMEOUT).await;
    let (status, tools) = run_with_tools(available.tools.clone()).await;
    assert_eq!(status, RunStatus::Completed);
    assert_eq!(tools, available.tools);
}
