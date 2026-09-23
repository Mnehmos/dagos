//! Projects and conversations over the HTTP API: chats as threads of runs, projects as separate
//! durable DAGs.

use std::sync::Arc;
use std::time::Duration;

use dagos::keys::KeyStore;
use dagos::server;
use dagos::workspace::{self, Workspace};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use tokio::net::TcpListener;

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

    /// Sends `body` to `POST /api/runs` and waits for the run to finish.
    async fn say(&self, body: Value) -> Value {
        let started = self.ok(Method::POST, "/api/runs", Some(body)).await;
        let id = started["run"]["id"].as_str().unwrap().to_owned();
        for _ in 0..200 {
            let detail = self.ok(Method::GET, &format!("/api/runs/{id}"), None).await;
            if detail["run"]["status"] != "running" {
                return detail["run"].clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("run {id} did not finish");
    }
}

#[tokio::test]
async fn chats_are_threads_of_runs_shown_as_turns() {
    let api = Api::start().await;
    let first =
        api.say(json!({"message": "Plan the storage layer", "new_conversation": true})).await;
    let conversation = first["conversation_id"].as_str().unwrap().to_owned();
    api.say(json!({"message": "Use SQLite?", "conversation_id": conversation})).await;
    let failed = api
        .say(json!({"message": "Break it", "conversation_id": conversation, "model_id": "fake-malformed"}))
        .await;
    assert_eq!(failed["status"], "failed");

    let view = api.ok(Method::GET, &format!("/api/conversations/{conversation}"), None).await;
    assert_eq!(view["conversation"]["title"], "Plan the storage layer");
    let turns = view["turns"].as_array().unwrap();
    let messages: Vec<&str> = turns.iter().map(|t| t["message"].as_str().unwrap()).collect();
    assert_eq!(messages, ["Plan the storage layer", "Use SQLite?", "Break it"]);
    assert!(turns[0]["prose"].as_str().unwrap().contains("Plan the storage layer"));
    assert_eq!(turns[0]["jev_id"], "fake-jev");
    assert_eq!(turns[0]["jev_fallback"], false);
    assert!(turns[1]["context_size"].as_u64().unwrap() > 0, "the thread carries context");
    assert_eq!(turns[2]["failure"]["error_code"], "response_invalid");
    assert_eq!(turns[2]["failure"]["rejected_stage"], "response");

    // Rename, archive, restore.
    let path = format!("/api/conversations/{conversation}");
    let renamed = api.ok(Method::PATCH, &path, Some(json!({"title": "Storage"}))).await;
    assert_eq!(renamed["conversation"]["title"], "Storage");
    let archived = api.ok(Method::PATCH, &path, Some(json!({"archived": true}))).await;
    assert!(archived["conversation"]["archived_at"].is_string());
    let restored = api.ok(Method::PATCH, &path, Some(json!({"archived": false}))).await;
    assert!(restored["conversation"]["archived_at"].is_null());
    let (status, _) = api.call(Method::PATCH, &path, Some(json!({"title": "  "}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = api.call(Method::GET, "/api/conversations/conv_missing", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // A new chat starts its own thread; the overview lists both, most recent first.
    let other = api.say(json!({"message": "Write the README", "new_conversation": true})).await;
    assert_ne!(other["conversation_id"], conversation.as_str());
    let overview = api.ok(Method::GET, "/api/overview", None).await;
    let titles: Vec<&str> = overview["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles, ["Write the README", "Storage"]);
}

#[tokio::test]
async fn projects_keep_separate_dags_and_their_own_configuration() {
    let api = Api::start().await;
    api.say(json!({"message": "Remember the default project"})).await;

    let created =
        api.ok(Method::POST, "/api/projects", Some(json!({"name": "  Side quest "}))).await;
    let project = created["project"]["id"].as_str().unwrap().to_owned();
    assert_eq!(created["project"]["name"], "Side quest");
    let (status, _) = api.call(Method::POST, "/api/projects", Some(json!({"name": " "}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let run = api.say(json!({"message": "Hello side quest", "project_id": project})).await;
    assert_eq!(run["project_id"], project.as_str());

    let side = api.ok(Method::GET, &format!("/api/projects/{project}/overview"), None).await;
    let nodes = side["dag"]["nodes"].as_array().unwrap();
    assert!(nodes.iter().any(|node| node["payload"]["text"] == "Hello side quest"));
    assert!(!side.to_string().contains("Remember the default project"), "DAGs are separate");
    assert_eq!(side["run_defaults"]["provider_id"], "fake");

    let config = json!({"provider_id": "fake", "model_id": "fake-cycle", "system_prompt": "Side."});
    api.ok(Method::PUT, &format!("/api/projects/{project}/config"), Some(config)).await;
    let side = api.ok(Method::GET, &format!("/api/projects/{project}/overview"), None).await;
    assert_eq!(side["run_defaults"]["model_id"], "fake-cycle");
    let main = api.ok(Method::GET, "/api/overview", None).await;
    assert_eq!(main["run_defaults"]["model_id"], "fake-echo", "configuration is per project");

    let renamed = api
        .ok(Method::PATCH, &format!("/api/projects/{project}"), Some(json!({"name": "Quest"})))
        .await;
    assert_eq!(renamed["project"]["name"], "Quest");
    let list = api.ok(Method::GET, "/api/projects", None).await;
    let names: Vec<&str> = list["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["project"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["demo", "Quest"]);
    assert_eq!(list["projects"][1]["conversations"], 1);
    let (status, _) = api.call(Method::GET, "/api/projects/proj_missing/overview", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}
