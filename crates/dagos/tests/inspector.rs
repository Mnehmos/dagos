//! The read-only inspection surface: views assembled from recorded state, served as JSON over a
//! loopback HTTP API.

use std::sync::Arc;
use std::time::Duration;

use dagos::inspect;
use dagos::server;
use dagos::workspace::{self, Workspace};
use dagos_core::domain::{ModelId, RunStatus};
use serde_json::Value;

/// A workspace in a temporary directory with two completed runs and one rejected run.
async fn workspace_with_history() -> (tempfile::TempDir, Workspace) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(".dagos");
    workspace::init(&dir, Some("demo")).unwrap();
    // A generous deadline: CI runners run many of these workspaces at once.
    let workspace = Workspace::open_with_keys(&dir, None, Duration::from_secs(120), None).unwrap();
    let mut config = workspace.run_config().unwrap();
    for message in ["Where should durable state live?", "Add restart tests"] {
        let run = workspace.runtime().run(&workspace.project.id, message, &config).await.unwrap();
        let events = workspace.store.transaction(|tx| tx.events(&run.id)).unwrap();
        let last: Vec<_> = events.iter().rev().take(3).map(|event| &event.data).collect();
        assert_eq!(run.status, RunStatus::Completed, "{:?}: {last:?}", run.error_code);
    }
    config.model_id = ModelId::parse("fake-dangling-edge").unwrap();
    let run = workspace.runtime().run(&workspace.project.id, "Link it", &config).await.unwrap();
    assert_eq!(run.status, RunStatus::Failed);
    (root, workspace)
}

#[tokio::test]
async fn the_overview_shows_the_durable_dag_and_every_run() {
    let (_root, workspace) = workspace_with_history().await;
    let overview = inspect::overview(&workspace).unwrap();
    assert_eq!(overview.project.name, "demo");
    assert_eq!(overview.jev, "fake-jev");
    assert_eq!(overview.run_defaults.model_id.as_str(), "fake-echo");
    assert_eq!(overview.providers[0].id.as_str(), "fake");
    // Three user messages plus two observations; the rejected run added nothing else.
    assert_eq!(overview.dag.nodes.len(), 5);
    assert_eq!(overview.dag.edges.len(), 2);
    let messages: Vec<_> = overview.runs.iter().map(|summary| summary.message.as_deref()).collect();
    assert_eq!(
        messages,
        [Some("Where should durable state live?"), Some("Add restart tests"), Some("Link it")]
    );
}

#[tokio::test]
async fn a_completed_run_exposes_every_stage_of_the_pipeline() {
    let (_root, workspace) = workspace_with_history().await;
    let runs = inspect::overview(&workspace).unwrap().runs;
    let id = &runs[1].run.id;
    let detail = inspect::run_detail(&workspace.store, id).unwrap().unwrap();

    assert_eq!(detail.message.as_ref().unwrap().text, "Add restart tests");
    // The first run ended with an empty context (its observation came after), so nothing was
    // carried; Jev then brought the first run's message and observation in.
    assert!(detail.carried.is_empty());
    assert_eq!(detail.context_added.len(), 2);
    assert!(detail.context_removed.is_empty());
    assert_eq!(detail.classification.as_ref().unwrap().classifications.len(), 2);
    assert_eq!(detail.context.len(), 2);
    assert!(detail.jev_request.is_some());
    let ir = detail.ir.as_ref().unwrap();
    assert_eq!(ir.task.message, "Add restart tests");
    assert_eq!(ir.context.len(), 2);
    let response = detail.response.as_ref().unwrap();
    assert_eq!(detail.streamed, response.presentation.prose, "streamed prose is the presentation");
    assert_eq!(
        serde_json::from_str::<Value>(detail.output.as_ref().unwrap()).unwrap()["schema"],
        "kiss.inference-response.v1"
    );
    assert_eq!((detail.emitted_nodes.len(), detail.emitted_edges.len()), (1, 1));
    assert!(detail.failure.is_none());
    assert_eq!(detail.events.first().unwrap().data.event_type(), "run.started");
}

#[tokio::test]
async fn a_rejected_run_explains_itself_and_shows_the_raw_output() {
    let (_root, workspace) = workspace_with_history().await;
    let id =
        inspect::resolve_run(&workspace.store, &workspace.project.id, "latest").unwrap().unwrap();
    let detail = inspect::run_detail(&workspace.store, &id).unwrap().unwrap();

    let failure = detail.failure.as_ref().unwrap();
    assert_eq!(failure.error_code, "response_invalid");
    assert_eq!(failure.rejected_stage, Some("response"));
    assert!(failure.rejected_reason.as_ref().unwrap().contains("node_zzzzzz"));
    assert!(detail.output.as_ref().unwrap().contains("node_zzzzzz"), "raw output is kept");
    assert!(!detail.streamed.is_empty(), "prose streamed before the rejection");
    assert!(detail.response.is_none());
    assert!(detail.emitted_nodes.is_empty() && detail.emitted_edges.is_empty());

    assert_eq!(
        inspect::resolve_run(&workspace.store, &workspace.project.id, "run_nope").unwrap(),
        None
    );
    assert_eq!(
        inspect::resolve_run(&workspace.store, &workspace.project.id, "garbage").unwrap(),
        None
    );
}

#[tokio::test]
async fn the_http_api_serves_the_same_views_on_loopback() {
    let (_root, workspace) = workspace_with_history().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(server::serve(listener, Arc::new(workspace), server::EventHub::new()));
    let get = |path: &str| {
        let url = format!("{base}{path}");
        async move {
            let response = reqwest::get(url).await.unwrap();
            let status = response.status().as_u16();
            (status, response.json::<Value>().await.unwrap())
        }
    };

    let (status, health) = get("/api/health").await;
    assert_eq!((status, &health["ok"]), (200, &Value::Bool(true)));

    let (status, overview) = get("/api/overview").await;
    assert_eq!(status, 200);
    assert_eq!(overview["dag"]["nodes"].as_array().unwrap().len(), 5);
    assert_eq!(overview["runs"][2]["run"]["error_code"], "response_invalid");

    let (status, latest) = get("/api/runs/latest").await;
    assert_eq!(status, 200);
    assert_eq!(latest["failure"]["rejected_stage"], "response");
    let first_id = overview["runs"][0]["run"]["id"].as_str().unwrap().to_owned();
    let (status, first) = get(&format!("/api/runs/{first_id}")).await;
    assert_eq!(status, 200);
    assert_eq!(first["ir"]["schema"], "kiss.inference-ir.v1");
    assert_eq!(first["response"]["schema"], "kiss.inference-response.v1");

    let (status, missing) = get("/api/runs/run_nope").await;
    assert_eq!(status, 404);
    assert!(missing["error"].as_str().unwrap().contains("run_nope"));
}

#[tokio::test]
async fn the_app_refuses_to_listen_beyond_loopback() {
    let (_dir, workspace) = workspace_with_history().await;
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let error = server::serve(listener, Arc::new(workspace), server::EventHub::new())
        .await
        .expect_err("a non-loopback address is refused");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert!(error.to_string().contains("loopback"), "{error}");
}
