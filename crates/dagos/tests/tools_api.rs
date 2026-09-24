//! Tools over the HTTP API with a real stdio MCP server (`dagos-mcp`'s fixture): discovery,
//! policies, approvals that wait for a person, and results that reach the model and the chat.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dagos::keys::KeyStore;
use dagos::server;
use dagos::workspace::{self, Workspace};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use tokio::net::TcpListener;

/// The `dagos-mcp-fixture` binary, built on demand next to this test's own binary.
fn fixture() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let path = dir.join(format!("dagos-mcp-fixture{}", std::env::consts::EXE_SUFFIX));
    if !path.exists() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let status = std::process::Command::new(cargo)
            .args(["build", "-p", "dagos-mcp", "--bin", "dagos-mcp-fixture"])
            .status()
            .unwrap();
        assert!(status.success(), "building the MCP fixture failed");
    }
    path
}

struct Api {
    base: String,
    client: Client,
    _dir: tempfile::TempDir,
}

impl Api {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let dagos_dir = dir.path().join(".dagos");
        workspace::init(&dagos_dir, Some("demo")).unwrap();
        let keys = KeyStore::at(dir.path().join("keys.json"));
        let workspace =
            Workspace::open_with_keys(&dagos_dir, None, Duration::from_secs(10), Some(keys))
                .unwrap();
        workspace.approvals().set_interactive(true);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(server::serve(listener, Arc::new(workspace), server::EventHub::new()));
        Self { base, client: Client::new(), _dir: dir }
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut request = self.client.request(method, format!("{}{path}", self.base));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn ok(&self, method: Method, path: &str, body: Option<Value>) -> Value {
        let (status, value) = self.call(method, path, body).await;
        assert!(status.is_success(), "{path}: {status} {value}");
        value
    }

    async fn start_run(&self, message: &str) -> String {
        let body = json!({"message": message, "model_id": "fake-tool"});
        let started = self.ok(Method::POST, "/api/runs", Some(body)).await;
        started["run"]["id"].as_str().unwrap().to_owned()
    }

    /// Polls the run until `done` holds for its detail.
    async fn until(&self, run: &str, done: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..300 {
            let detail = self.ok(Method::GET, &format!("/api/runs/{run}"), None).await;
            if done(&detail) {
                return detail;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("run {run} never reached the expected state");
    }
}

fn finished(detail: &Value) -> bool {
    detail["run"]["status"] != "running"
}

#[tokio::test]
async fn servers_are_added_approved_and_their_results_reach_the_chat() {
    let api = Api::start().await;
    let server = json!({"command": fixture().to_string_lossy()});
    let tools = api.ok(Method::PUT, "/api/tools/servers/files", Some(server)).await;
    let status = &tools["capabilities"]["servers"][0];
    assert_eq!(status["id"], "files");
    assert_eq!(status["tools"].as_array().unwrap().len(), 2);
    assert_eq!(status["tools"][0]["policy"], "ask", "new tools ask first");

    // `ask`: the run waits until a person answers.
    let run = api.start_run(r#"files.read_file {"path": "README.md"}"#).await;
    let waiting = api.until(&run, |detail| detail["tool_calls"][0]["status"] == "awaiting").await;
    assert_eq!(waiting["run"]["status"], "running");
    let path = format!("/api/runs/{run}/tools/call_1");
    let (status, _) = api.call(Method::POST, &path, Some(json!({"decision": "maybe"}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    api.ok(Method::POST, &path, Some(json!({"decision": "allow", "remember": true}))).await;
    let detail = api.until(&run, finished).await;
    assert_eq!(detail["run"]["status"], "completed");
    let call = &detail["tool_calls"][0];
    assert_eq!(
        (call["status"].as_str(), call["decided_by"].as_str()),
        (Some("completed"), Some("user"))
    );
    assert_eq!(call["output"]["content"][0]["text"], "contents of README.md");
    let (status, _) = api.call(Method::POST, &path, Some(json!({"decision": "deny"}))).await;
    assert_eq!(status, StatusCode::CONFLICT, "nothing is waiting any more");

    // The chat shows the call between the two replies.
    let conversation = detail["run"]["conversation_id"].as_str().unwrap();
    let view = api.ok(Method::GET, &format!("/api/conversations/{conversation}"), None).await;
    let kinds: Vec<&str> = view["turns"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["prose", "tool", "prose"]);

    // "Always allow" was remembered: the next call runs without asking.
    let tools = api.ok(Method::GET, "/api/tools", None).await;
    assert_eq!(tools["config"]["servers"][0]["tools"]["read_file"], "allow");
    let run = api.start_run(r#"files.read_file {"path": "b.md"}"#).await;
    let detail = api.until(&run, finished).await;
    assert_eq!(detail["tool_calls"][0]["decided_by"], "policy");
    assert_eq!(detail["tool_calls"][0]["output"]["content"][0]["text"], "contents of b.md");
}

#[tokio::test]
async fn denied_and_disabled_tools_never_run() {
    let api = Api::start().await;
    let server = json!({"command": fixture().to_string_lossy()});
    api.ok(Method::PUT, "/api/tools/servers/files", Some(server)).await;

    let run = api.start_run("files.search").await;
    api.until(&run, |detail| detail["tool_calls"][0]["status"] == "awaiting").await;
    let path = format!("/api/runs/{run}/tools/call_1");
    api.ok(Method::POST, &path, Some(json!({"decision": "deny"}))).await;
    let detail = api.until(&run, finished).await;
    assert_eq!(detail["tool_calls"][0]["status"], "denied");
    assert!(detail["tool_calls"][0]["output"].is_null());

    // `off` for the whole server: models are no longer offered its tools.
    let off = json!({"policy": "off"});
    let tools = api.ok(Method::PUT, "/api/tools/servers/files/policy", Some(off)).await;
    assert_eq!(tools["capabilities"]["tools"], json!([]));
    let run = api.start_run("files.search").await;
    let detail = api.until(&run, finished).await;
    assert!(detail["tool_calls"].as_array().unwrap().is_empty(), "no tools were offered");
    assert!(detail["ir"]["tools"].is_null());

    // One tool back on, set to `allow`.
    let allow = json!({"tool": "search", "policy": "allow"});
    let tools = api.ok(Method::PUT, "/api/tools/servers/files/policy", Some(allow)).await;
    let names: Vec<&str> = tools["capabilities"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["files.search"]);

    let (status, _) = api.call(Method::DELETE, "/api/tools/servers/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let tools = api.ok(Method::DELETE, "/api/tools/servers/files", None).await;
    assert_eq!(tools["config"]["servers"], json!([]));
    let bad = json!({"command": "x"});
    let (status, _) = api.call(Method::PUT, "/api/tools/servers/bad%20id", Some(bad)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn runs_can_wait_while_tool_servers_start() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(".dagos");
    workspace::init(&dir, Some("demo")).unwrap();
    let config = json!({"servers": [{"id": "files", "command": fixture(), "args": ["files"], "policy": "allow"}]});
    std::fs::write(dir.join("mcp.json"), config.to_string()).unwrap();
    let workspace = Workspace::open_with_keys(&dir, None, Duration::from_secs(10), None).unwrap();
    assert!(workspace.tools_starting(), "servers are configured and not started yet");
    assert!(workspace.runtime().tools().is_empty());

    let workspace = Arc::new(workspace);
    let starter = workspace.clone();
    let started = tokio::spawn(async move { starter.start_tools(|_| {}).await });
    workspace.wait_for_tools(Duration::from_secs(30)).await;
    assert!(!workspace.tools_starting());
    assert!(!workspace.runtime().tools().is_empty(), "the runtime now has the tools");
    started.await.unwrap().unwrap();

    let plain = tempfile::tempdir().unwrap();
    let plain_dir = plain.path().join(".dagos");
    workspace::init(&plain_dir, Some("plain")).unwrap();
    let without =
        Workspace::open_with_keys(&plain_dir, None, Duration::from_secs(10), None).unwrap();
    assert!(!without.tools_starting(), "no servers configured: nothing to wait for");
}
