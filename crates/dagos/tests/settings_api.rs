//! In-app provider settings over the HTTP API: saving and removing keys (never echoed back),
//! custom endpoints, connection checks, and an optional model Jev that runs never depend on.

use std::sync::Arc;
use std::time::Duration;

use dagos::keys::KeyStore;
use dagos::server;
use dagos::workspace::{self, Workspace};
use reqwest::{Client, Method, StatusCode};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

struct Api {
    base: String,
    client: Client,
    keys: KeyStore,
    _dir: tempfile::TempDir,
}

impl Api {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let dagos_dir = dir.path().join(".dagos");
        workspace::init(&dagos_dir, Some("demo")).unwrap();
        let keys = KeyStore::at(dir.path().join("config").join("keys.json"));
        let workspace = Workspace::open_with_keys(
            &dagos_dir,
            None,
            Duration::from_secs(10),
            Some(keys.clone()),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(server::serve(listener, Arc::new(workspace), server::EventHub::new()));
        Self { base, client: Client::new(), keys, _dir: dir }
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
}

fn provider<'a>(settings: &'a Value, id: &str) -> &'a Value {
    settings["providers"].as_array().unwrap().iter().find(|p| p["id"] == id).unwrap()
}

/// Serves `body` once as JSON on loopback; returns the base URL.
async fn serve_once(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0u8; 4096];
        let _ = socket.read(&mut buffer).await.unwrap();
        let head = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        socket.write_all(head.as_bytes()).await.unwrap();
        socket.write_all(body.as_bytes()).await.unwrap();
    });
    base
}

#[tokio::test]
async fn keys_are_saved_per_user_masked_and_removable() {
    let api = Api::start().await;
    let settings = api.ok(Method::GET, "/api/settings", None).await;
    for id in ["fake", "openrouter", "openai", "zai"] {
        provider(&settings, id);
    }
    assert_eq!(provider(&settings, "fake")["key"]["source"], "not_needed");
    assert!(
        provider(&settings, "openrouter")["key"]["manage_url"]
            .as_str()
            .unwrap()
            .starts_with("https://")
    );

    // A custom endpoint takes a saved key and becomes usable with it.
    let endpoint = json!({"base_url": "http://127.0.0.1:9/v1", "models": ["m-1"]});
    api.ok(Method::PUT, "/api/settings/providers/acme", Some(endpoint)).await;
    let secret = "acme-secret-key-0042";
    let settings =
        api.ok(Method::PUT, "/api/settings/providers/acme/key", Some(json!({"key": secret}))).await;
    let acme = provider(&settings, "acme");
    assert_eq!(acme["key"]["source"], "saved");
    assert_eq!(acme["key"]["hint"], "acme…0042");
    assert!(!settings.to_string().contains(secret), "keys are never sent back");
    assert_eq!(api.keys.load().unwrap().values().next().unwrap(), secret);
    let overview = api.ok(Method::GET, "/api/overview", None).await;
    assert!(!overview.to_string().contains(secret));

    // Refusals: the fake needs no key, unknown providers, malformed keys.
    let key = Some(json!({"key": "k-123456789012"}));
    let (status, _) = api.call(Method::PUT, "/api/settings/providers/fake/key", key.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = api.call(Method::PUT, "/api/settings/providers/nope/key", key).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let spaced = Some(json!({"key": "two words"}));
    let (status, body) = api.call(Method::PUT, "/api/settings/providers/acme/key", spaced).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let settings = api.ok(Method::DELETE, "/api/settings/providers/acme/key", None).await;
    assert_eq!(provider(&settings, "acme")["key"]["source"], "none");
    assert!(api.keys.load().unwrap().is_empty());

    // Presets cannot be removed; custom endpoints can, together with their saved key.
    api.ok(Method::PUT, "/api/settings/providers/acme/key", Some(json!({"key": secret}))).await;
    let (status, _) = api.call(Method::DELETE, "/api/settings/providers/openai", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let settings = api.ok(Method::DELETE, "/api/settings/providers/acme", None).await;
    assert!(settings["providers"].as_array().unwrap().iter().all(|p| p["id"] != "acme"));
    assert!(api.keys.load().unwrap().is_empty());
}

#[tokio::test]
async fn connection_checks_list_models_or_explain_the_failure() {
    let api = Api::start().await;
    let models = serve_once(r#"{"data":[{"id":"qwen"},{"id":"llama"}]}"#).await;
    api.ok(Method::PUT, "/api/settings/providers/local", Some(json!({"base_url": models}))).await;
    let check = api.ok(Method::POST, "/api/settings/providers/local/check", None).await;
    assert_eq!(check["models"], json!(["llama", "qwen"]));

    let down = json!({"base_url": "http://127.0.0.1:9/v1"});
    api.ok(Method::PUT, "/api/settings/providers/local", Some(down)).await;
    let (status, body) = api.call(Method::POST, "/api/settings/providers/local/check", None).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(body["error"].as_str().unwrap().contains("127.0.0.1:9"), "{body}");

    let bad = json!({"base_url": "ftp://example"});
    let (status, _) = api.call(Method::PUT, "/api/settings/providers/local", Some(bad)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn runs_work_without_the_model_jev_and_record_the_fallback() {
    let api = Api::start().await;
    // A model Jev on an endpoint that is down: runs must still complete, via the offline policy.
    let down = json!({"base_url": "http://127.0.0.1:9/v1"});
    api.ok(Method::PUT, "/api/settings/providers/local", Some(down)).await;
    let jev = json!({"provider": "local", "model": "tiny-classifier"});
    let settings = api.ok(Method::PUT, "/api/settings/jev", Some(jev)).await;
    assert_eq!(settings["jev"]["active"], "local-jev:tiny-classifier");
    assert_eq!(settings["jev"]["fallback"], "fake-jev");

    let started = api.ok(Method::POST, "/api/runs", Some(json!({"message": "hello"}))).await;
    let id = started["run"]["id"].as_str().unwrap().to_owned();
    let mut detail = Value::Null;
    for _ in 0..200 {
        detail = api.ok(Method::GET, &format!("/api/runs/{id}"), None).await;
        if detail["run"]["status"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert_eq!(detail["run"]["status"], "completed", "{detail}");
    assert_eq!(detail["jev_id"], "local-jev:tiny-classifier");
    assert_eq!(detail["jev_fallback"]["from"], "local-jev:tiny-classifier");
    assert_eq!(detail["jev_fallback"]["jev_id"], "fake-jev");
    assert!(detail["classification"].is_object());

    // Without a model Jev the offline policy classifies directly.
    let settings = api.ok(Method::DELETE, "/api/settings/jev", None).await;
    assert_eq!(settings["jev"]["active"], "fake-jev");
    assert!(settings["jev"]["provider"].is_null());
}
