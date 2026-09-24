//! The OpenAI-compatible adapter against a local mock endpoint: request formatting, streaming,
//! error mapping, and a full DAGOS run through the common provider interface. No network needed.

use std::sync::Arc;
use std::time::Duration;

use dagos_core::context::FakeJev;
use dagos_core::domain::{
    ErrorCode, EventData, InferenceIr, InferenceIrSchema, IrTask, ModelId, NodeId, NodeType,
    ProviderId, RunConfig, RunStatus,
};
use dagos_core::provider::FakeProvider;
use dagos_core::provider::{CollectDeltas, InferenceProvider, InferenceRequest, ProviderError};
use dagos_core::runtime::Runtime;
use dagos_core::store::Store;
use dagos_openai::{
    DecisionsJev, OpenAiCompatible, OpenAiCompatibleConfig, OpenAiCompatibleJev, decisions_url,
    is_decisions_model,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// What the mock endpoint received.
struct Captured {
    head: String,
    body: Value,
}

/// Serves one HTTP response (written in `parts`, so the client sees several chunks) and returns
/// the base URL plus a handle yielding the captured request.
async fn mock(
    status: &str,
    content_type: &str,
    parts: Vec<String>,
) -> (String, JoinHandle<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let head =
        format!("HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\nconnection: close\r\n\r\n");
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let captured = read_request(&mut socket).await;
        socket.write_all(head.as_bytes()).await.unwrap();
        for part in parts {
            socket.write_all(part.as_bytes()).await.unwrap();
            socket.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        socket.shutdown().await.ok();
        captured
    });
    (base_url, handle)
}

async fn read_request(socket: &mut TcpStream) -> Captured {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let read = socket.read(&mut chunk).await.unwrap();
        assert!(read > 0, "client closed the connection early");
        buffer.extend_from_slice(&chunk[..read]);
        let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&buffer[..end]).to_string();
        let length: usize = head
            .lines()
            .find_map(|line| {
                line.to_ascii_lowercase().strip_prefix("content-length:").map(str::to_owned)
            })
            .map_or(0, |value| value.trim().parse().unwrap());
        while buffer.len() < end + 4 + length {
            let read = socket.read(&mut chunk).await.unwrap();
            buffer.extend_from_slice(&chunk[..read]);
        }
        let bytes = &buffer[end + 4..end + 4 + length];
        let body =
            if bytes.is_empty() { Value::Null } else { serde_json::from_slice(bytes).unwrap() };
        return Captured { head, body };
    }
}

/// SSE chunks streaming `pieces` as Chat Completions content deltas, with a keep-alive comment.
fn sse(pieces: &[&str]) -> Vec<String> {
    let mut parts = vec![": keep-alive\n\n".to_owned()];
    parts.push("data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n".to_owned());
    for piece in pieces {
        let event = json!({"choices": [{"delta": {"content": piece}}]});
        parts.push(format!("data: {event}\n\n"));
    }
    parts.push("data: [DONE]\n\n".to_owned());
    parts
}

fn provider(base_url: &str, json_mode: bool) -> OpenAiCompatible {
    OpenAiCompatible::new(OpenAiCompatibleConfig {
        id: ProviderId::parse("mock").unwrap(),
        base_url: base_url.to_owned(),
        api_key: Some("test-key".into()),
        models: vec![ModelId::parse("mock-1").unwrap()],
        json_mode,
        native_tools: false,
        model_windows: false,
    })
}

fn native_provider(base_url: &str) -> OpenAiCompatible {
    OpenAiCompatible::new(OpenAiCompatibleConfig {
        id: ProviderId::parse("mock").unwrap(),
        base_url: base_url.to_owned(),
        api_key: Some("test-key".into()),
        models: vec![],
        json_mode: false,
        native_tools: true,
        model_windows: true,
    })
}

fn ir() -> InferenceIr {
    InferenceIr {
        schema: InferenceIrSchema,
        system_prompt: "Answer tersely.".into(),
        task: IrTask { node_id: NodeId::parse("node_000001").unwrap(), message: "Status?".into() },
        context: vec![],
        recent_events: vec![],
        tools: vec![],
        tool_results: vec![],
        recalled: vec![],
        review: None,
    }
}

const DOCUMENT: &str = r#"{"schema":"kiss.inference-response.v1","presentation":{"prose":"All green."},"emissions":[{"kind":"node","ref":"d1","type":"decision","payload":{"text":"Keep SQLite"}},{"kind":"node","ref":"t1","type":"task","payload":{"title":"Add restart tests"}},{"kind":"edge","from":"t1","to":"d1","type":"depends_on"}]}"#;

fn pieces(document: &str, size: usize) -> Vec<String> {
    let chars: Vec<char> = document.chars().collect();
    chars.chunks(size).map(|chunk| chunk.iter().collect()).collect()
}

#[tokio::test]
async fn streams_prose_and_returns_the_raw_document() {
    let chunks = pieces(DOCUMENT, 7);
    let chunk_refs: Vec<&str> = chunks.iter().map(String::as_str).collect();
    let (base_url, request) = mock("200 OK", "text/event-stream", sse(&chunk_refs)).await;
    let ir = ir();
    let model = ModelId::parse("mock-1").unwrap();
    let mut deltas = CollectDeltas::default();
    let output = provider(&base_url, true)
        .infer(InferenceRequest { model_id: &model, ir: &ir }, &mut deltas)
        .await
        .unwrap();

    assert_eq!(output, DOCUMENT, "the raw document is returned verbatim for validation");
    assert_eq!(deltas.0.concat(), "All green.", "only presentation prose streams");
    assert!(deltas.0.len() > 1, "prose streamed incrementally");

    let captured = request.await.unwrap();
    assert!(captured.head.starts_with("POST /v1/chat/completions HTTP/1.1"), "{}", captured.head);
    assert!(captured.head.to_ascii_lowercase().contains("authorization: bearer test-key"));
    assert_eq!(captured.body["model"], "mock-1");
    assert_eq!(captured.body["stream"], true);
    assert_eq!(captured.body["response_format"], json!({"type": "json_object"}));
    let system = captured.body["messages"][0]["content"].as_str().unwrap();
    assert!(system.starts_with("Answer tersely.\n\nYou are the inference endpoint of DAGOS"));
    assert!(system.contains("kiss://schemas/inference-response/v1"));
    // The model's input is the compiled IR itself.
    let user: Value =
        serde_json::from_str(captured.body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(user, serde_json::to_value(&ir).unwrap());
}

async fn call(base_url: String) -> Result<String, ProviderError> {
    let (ir, model) = (ir(), ModelId::parse("mock-1").unwrap());
    provider(&base_url, false)
        .infer(InferenceRequest { model_id: &model, ir: &ir }, &mut CollectDeltas::default())
        .await
}

#[tokio::test]
async fn http_errors_stream_errors_and_unreachable_endpoints_fail_cleanly() {
    let body = r#"{"error":{"message":"invalid api key"}}"#.to_owned();
    let (base_url, request) = mock("401 Unauthorized", "application/json", vec![body]).await;
    let error = call(base_url).await.unwrap_err();
    assert!(
        matches!(&error, ProviderError::Failed(message) if message.contains("HTTP 401") && message.contains("invalid api key")),
        "{error}"
    );
    assert!(request.await.unwrap().body.get("response_format").is_none(), "json mode off");

    let stream_error = vec!["data: {\"error\":{\"message\":\"overloaded\"}}\n\n".to_owned()];
    let (base_url, _) = mock("200 OK", "text/event-stream", stream_error).await;
    let error = call(base_url).await.unwrap_err();
    assert!(
        matches!(&error, ProviderError::Failed(message) if message.contains("overloaded")),
        "{error}"
    );

    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", closed.local_addr().unwrap());
    drop(closed);
    assert!(matches!(call(base_url).await, Err(ProviderError::Failed(_))));
}

fn setup_runtime(base_url: &str) -> (Arc<Store>, Runtime) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let runtime = Runtime::new(store.clone(), Arc::new(FakeJev::new()))
        .with_provider(Arc::new(provider(base_url, true)))
        .with_inference_timeout(Duration::from_secs(10));
    (store, runtime)
}

fn config() -> RunConfig {
    RunConfig {
        provider_id: ProviderId::parse("mock").unwrap(),
        model_id: ModelId::parse("mock-1").unwrap(),
        system_prompt: "Answer tersely.".into(),
    }
}

#[tokio::test]
async fn a_dagos_run_completes_through_the_real_adapter_with_the_core_unchanged() {
    let chunks = pieces(DOCUMENT, 11);
    let chunk_refs: Vec<&str> = chunks.iter().map(String::as_str).collect();
    let (base_url, _) = mock("200 OK", "text/event-stream", sse(&chunk_refs)).await;
    let (store, runtime) = setup_runtime(&base_url);
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;

    let run = runtime.run(&project, "Status?", &config()).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed, "{run:?}");
    assert_eq!((run.provider_id.as_str(), run.model_id.as_str()), ("mock", "mock-1"));
    let nodes = store.transaction(|tx| tx.nodes(&project)).unwrap();
    let types: Vec<NodeType> = nodes.iter().map(|node| node.node_type).collect();
    assert_eq!(types, [NodeType::Conversation, NodeType::Decision, NodeType::Task]);
    assert_eq!(store.transaction(|tx| tx.edges(&project)).unwrap().len(), 1);
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    let streamed: String = events
        .iter()
        .filter_map(|event| match &event.data {
            EventData::InferenceDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(streamed, "All green.");
}

#[tokio::test]
async fn endpoint_failures_become_common_runtime_error_events() {
    let (base_url, _) = mock("500 Internal Server Error", "text/plain", vec!["boom".into()]).await;
    let (store, runtime) = setup_runtime(&base_url);
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let run = runtime.run(&project, "Status?", &config()).await.unwrap();
    assert_eq!(run.error_code, Some(ErrorCode::ProviderFailed));
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    let EventData::RunFailed { message, .. } = &events.last().unwrap().data else {
        panic!("last event must be run.failed")
    };
    assert!(message.contains("HTTP 500") && message.contains("boom"), "{message}");

    // Non-JSON model output is a response failure, not a provider failure.
    let (base_url, _) = mock("200 OK", "text/event-stream", sse(&["Sure! Here you go:"])).await;
    let (store, runtime) = setup_runtime(&base_url);
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let run = runtime.run(&project, "Status?", &config()).await.unwrap();
    assert_eq!(run.error_code, Some(ErrorCode::ResponseInvalid));
}

/// Opt-in live check against a real endpoint:
/// `DAGOS_LIVE_BASE_URL=https://openrouter.ai/api/v1 DAGOS_LIVE_MODEL=<model>
///  DAGOS_LIVE_API_KEY=<key> cargo test -p dagos-openai --test mock_endpoint -- --ignored`
#[tokio::test]
#[ignore = "needs a live endpoint: set DAGOS_LIVE_BASE_URL and DAGOS_LIVE_MODEL"]
async fn live_endpoint_completes_a_run() {
    let (Ok(base_url), Ok(model)) =
        (std::env::var("DAGOS_LIVE_BASE_URL"), std::env::var("DAGOS_LIVE_MODEL"))
    else {
        panic!("set DAGOS_LIVE_BASE_URL and DAGOS_LIVE_MODEL to run the live check");
    };
    let live = OpenAiCompatible::new(OpenAiCompatibleConfig {
        id: ProviderId::parse("live").unwrap(),
        base_url,
        api_key: std::env::var("DAGOS_LIVE_API_KEY").ok(),
        models: vec![],
        json_mode: std::env::var("DAGOS_LIVE_JSON_MODE").map_or(true, |value| value != "0"),
        native_tools: std::env::var("DAGOS_LIVE_NATIVE_TOOLS").map_or(true, |value| value != "0"),
        model_windows: true,
    });
    let jev: Arc<dyn dagos_core::context::JevClassifier> =
        match std::env::var("DAGOS_LIVE_JEV_MODEL") {
            Ok(jev_model) => {
                Arc::new(OpenAiCompatibleJev::new(live.clone(), ModelId::parse(jev_model).unwrap()))
            }
            Err(_) => Arc::new(FakeJev::new()),
        };
    let store = Arc::new(Store::open_in_memory().unwrap());
    let runtime = Runtime::new(store.clone(), jev)
        .with_provider(Arc::new(live))
        .with_inference_timeout(Duration::from_secs(120));
    let project = store.transaction(|tx| tx.create_project("live")).unwrap().id;
    let config = RunConfig {
        provider_id: ProviderId::parse("live").unwrap(),
        model_id: ModelId::parse(model).unwrap(),
        system_prompt: "You are a concise coding assistant.".into(),
    };
    let message =
        "Record a decision to use SQLite for durable state, then confirm in one sentence.";
    let run = runtime.run(&project, message, &config).await.unwrap();
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    assert_eq!(run.status, RunStatus::Completed, "{:#?}", events.last());
}

/// A store with two decision nodes for Jev to classify, and a fake-provider run config.
fn jev_setup() -> (Arc<Store>, dagos_core::domain::ProjectId, [NodeId; 2], RunConfig) {
    let store = Arc::new(Store::open_in_memory().unwrap());
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let nodes = ["Use SQLite", "Use JSON files"].map(|text| {
        let mut payload = serde_json::Map::new();
        payload.insert("text".into(), json!(text));
        store.transaction(|tx| tx.insert_node(&project, NodeType::Decision, payload)).unwrap().id
    });
    let config = RunConfig {
        provider_id: ProviderId::parse("fake").unwrap(),
        model_id: ModelId::parse("fake-echo").unwrap(),
        system_prompt: String::new(),
    };
    (store, project, nodes, config)
}

fn jev_runtime(store: &Arc<Store>, base_url: &str) -> Runtime {
    let jev =
        OpenAiCompatibleJev::new(provider(base_url, true), ModelId::parse("mock-jev").unwrap());
    Runtime::new(store.clone(), Arc::new(jev)).with_provider(Arc::new(FakeProvider::new()))
}

#[tokio::test]
async fn a_model_backed_jev_classifies_context_through_the_contract() {
    let (store, project, [keep, drop], config) = jev_setup();
    let output = json!({"schema": "kiss.jev-context.v1", "classifications": [
        {"node_id": keep.as_str(), "classification": "active"},
        {"node_id": drop.as_str(), "classification": "inactive"}
    ]})
    .to_string();
    let chunks = pieces(&output, 9);
    let chunk_refs: Vec<&str> = chunks.iter().map(String::as_str).collect();
    let (base_url, request) = mock("200 OK", "text/event-stream", sse(&chunk_refs)).await;

    let run =
        jev_runtime(&store, &base_url).run(&project, "Which storage?", &config).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed, "{run:?}");
    let context = store.transaction(|tx| tx.context(&run.id)).unwrap();
    assert_eq!(context.iter().map(|member| &member.node_id).collect::<Vec<_>>(), [&keep]);
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.data,
        EventData::JevRequested { jev_id, .. } if jev_id == "mock-jev:mock-jev"
    )));

    // The classifier received the versioned request and classifier-only instructions.
    let captured = request.await.unwrap();
    assert_eq!(captured.body["temperature"], 0);
    let system = captured.body["messages"][0]["content"].as_str().unwrap();
    assert!(system.starts_with("You are Jev, the context classifier of DAGOS. You only classify."));
    let user: Value =
        serde_json::from_str(captured.body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(user["schema"], "kiss.jev-request.v1");
    assert_eq!(user["candidates"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_model_jev_that_plans_instead_of_classifying_is_rejected() {
    let (store, project, _, config) = jev_setup();
    let output =
        r#"{"schema":"kiss.jev-context.v1","classifications":[],"plan":["rewrite it all"]}"#;
    let (base_url, _) = mock("200 OK", "text/event-stream", sse(&[output])).await;

    let run =
        jev_runtime(&store, &base_url).run(&project, "Which storage?", &config).await.unwrap();
    assert_eq!(run.error_code, Some(ErrorCode::JevInvalidOutput));
    assert!(store.transaction(|tx| tx.context(&run.id)).unwrap().is_empty());
}

#[tokio::test]
async fn connection_checks_list_models_and_report_rejected_keys() {
    let models = r#"{"data":[{"id":"z/model"},{"id":"a/model"},{"id":"a/model"},{"object":"x"}]}"#;
    let (base_url, request) = mock("200 OK", "application/json", vec![models.into()]).await;
    let listed = provider(&base_url, true).check(None).await.unwrap();
    assert_eq!(listed, ["a/model", "z/model"]);
    let captured = request.await.unwrap();
    assert!(captured.head.starts_with("GET /v1/models HTTP/1.1"), "{}", captured.head);
    assert!(captured.head.to_ascii_lowercase().contains("authorization: bearer test-key"));

    // With a key check, a rejected key fails before the (public) model list is consulted.
    let denied = r#"{"error":{"message":"No auth credentials found"}}"#;
    let (base_url, request) =
        mock("401 Unauthorized", "application/json", vec![denied.into()]).await;
    let error = provider(&base_url, true).check(Some("/key")).await.unwrap_err();
    assert!(error.contains("HTTP 401") && error.contains("No auth credentials"), "{error}");
    assert!(request.await.unwrap().head.starts_with("GET /v1/key HTTP/1.1"));
}

#[test]
fn decisions_models_and_their_endpoint_are_recognised() {
    for (model, decisions) in
        [("~typesafe/jev-latest", true), ("typesafe/jev-1.13", true), ("openai/gpt-5", false)]
    {
        assert_eq!(is_decisions_model(&ModelId::parse(model).unwrap()), decisions, "{model}");
    }
    assert_eq!(
        decisions_url("https://openrouter.ai/api/v1/"),
        "https://openrouter.ai/api/alpha/decisions"
    );
    assert_eq!(decisions_url("http://127.0.0.1:9/gw"), "http://127.0.0.1:9/gw/alpha/decisions");
}

#[tokio::test]
async fn typesafe_jev_classifies_through_the_decisions_api() {
    let (store, project, [keep, drop], config) = jev_setup();
    let drop = &drop;
    let response = json!({
        "id": "gen-dec-1", "model": "typesafe/jev-1.13", "provider": "TypeSafe",
        "answers": {
            keep.as_str(): {"type": "noul", "noul": 0.93},
            drop.as_str(): {"type": "noul", "noul": 0.08}
        },
        "usage": {"input_tokens": 400, "output_tokens": 20, "cost": 0.00002}
    })
    .to_string();
    let (base_url, request) = mock("200 OK", "application/json", vec![response]).await;
    let jev = DecisionsJev::new(
        provider(&base_url, true),
        ModelId::parse("~typesafe/jev-latest").unwrap(),
    );
    let runtime =
        Runtime::new(store.clone(), Arc::new(jev)).with_provider(Arc::new(FakeProvider::new()));

    let run = runtime.run(&project, "Which storage?", &config).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed, "{run:?}");
    let context = store.transaction(|tx| tx.context(&run.id)).unwrap();
    assert_eq!(context.iter().map(|member| &member.node_id).collect::<Vec<_>>(), [&keep]);

    let captured = request.await.unwrap();
    assert!(captured.head.starts_with("POST /alpha/decisions HTTP/1.1"), "{}", captured.head);
    assert!(captured.head.to_ascii_lowercase().contains("authorization: bearer test-key"));
    assert_eq!(captured.body["model"], "~typesafe/jev-latest");
    assert_eq!(captured.body["state"]["message"], "Which storage?");
    let question = &captured.body["questions"][keep.as_str()];
    assert_eq!(question["type"], "noul");
    assert_eq!(question["instructions"]["node"]["node_id"], keep.as_str());
    assert!(question["criteria"]["true"].is_string() && question["criteria"]["false"].is_string());
    assert_eq!(captured.body["questions"].as_object().unwrap().len(), 2);
    assert_eq!(question["instructions"]["recency"], 2, "the oldest of two candidates");
    assert_eq!(captured.body["questions"][drop.as_str()]["instructions"]["recency"], 1);
    assert_eq!(captured.body["state"]["latest_messages"], json!([]), "no user turns among them");
}

#[tokio::test]
async fn malformed_decisions_answers_fail_the_jev_not_the_dag() {
    let (store, project, [keep, _], config) = jev_setup();
    let response =
        json!({"answers": {keep.as_str(): {"type": "choice", "choice": "yes"}}}).to_string();
    let (base_url, _) = mock("200 OK", "application/json", vec![response]).await;
    let jev =
        DecisionsJev::new(provider(&base_url, true), ModelId::parse("typesafe/jev-1.13").unwrap());
    let runtime = Runtime::new(store.clone(), Arc::new(jev))
        .with_provider(Arc::new(FakeProvider::new()))
        .with_jev_fallback(Arc::new(FakeJev::new()));
    let run = runtime.run(&project, "Which storage?", &config).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed, "the offline policy took over");
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.data,
        EventData::JevFallback { reason, .. } if reason.contains("not a noul probability")
    )));
}

#[tokio::test]
async fn typesafe_jev_also_decides_which_tools_the_model_sees() {
    let (store, project, [keep, _], config) = jev_setup();
    let tool = |name: &str| dagos_core::domain::IrTool {
        name: name.into(),
        description: format!("The {name} tool."),
        input_schema: serde_json::Map::new(),
    };
    let response = json!({"answers": {
        keep.as_str(): {"type": "noul", "noul": 0.9},
        "tool:ooda.read_file": {"type": "noul", "noul": 0.97},
        "tool:ooda.mouse_click": {"type": "noul", "noul": 0.02}
    }})
    .to_string();
    let (base_url, request) = mock("200 OK", "application/json", vec![response]).await;
    let jev = DecisionsJev::new(
        provider(&base_url, true),
        ModelId::parse("~typesafe/jev-latest").unwrap(),
    );
    let runtime = Runtime::new(store.clone(), Arc::new(jev))
        .with_provider(Arc::new(FakeProvider::new()))
        .with_tools(vec![tool("ooda.read_file"), tool("ooda.mouse_click"), tool("ooda.exec_cli")]);

    let run = runtime.run(&project, "Read the README", &config).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed, "{run:?}");
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    let ir_tools: Vec<String> = events
        .into_iter()
        .find_map(|event| match event.data {
            EventData::IrCompiled { ir } => {
                Some(ir.tools.into_iter().map(|tool| tool.name).collect())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(ir_tools, ["ooda.read_file", "ooda.exec_cli"], "unanswered tools stay exposed");

    let captured = request.await.unwrap();
    let question = &captured.body["questions"]["tool:ooda.mouse_click"];
    assert_eq!(question["type"], "noul");
    assert_eq!(question["instructions"]["tool"]["name"], "ooda.mouse_click");
    assert_eq!(captured.body["questions"].as_object().unwrap().len(), 5, "2 nodes + 3 tools");
}

fn recall_chunk(id: &str, text: &str) -> dagos_core::context::recall::RecallChunk {
    dagos_core::context::recall::RecallChunk::new(id, text)
}

#[tokio::test]
async fn typesafe_jev_judges_recall_relevance_one_question_per_turn() {
    use dagos_core::context::JevClassifier;
    let chunks = [
        recall_chunk("run_000001", "user: Plan a trip to Lisbon"),
        recall_chunk("run_000002", "user: My sister is allergic to shellfish"),
    ];
    let response = json!({"answers": {
        "run_000001": {"type": "noul", "noul": 0.08},
        "run_000002": {"type": "noul", "noul": 0.97}
    }})
    .to_string();
    let (base_url, request) = mock("200 OK", "application/json", vec![response]).await;
    let jev =
        DecisionsJev::new(provider(&base_url, true), ModelId::parse("typesafe/jev-1.13").unwrap());
    let scores = jev.relevance("sister food allergy", &chunks).await.unwrap();
    assert_eq!(scores, Some(vec![0.08, 0.97]));

    let captured = request.await.unwrap();
    assert!(captured.head.starts_with("POST /alpha/decisions HTTP/1.1"), "{}", captured.head);
    assert_eq!(captured.body["state"], json!({"query": "sister food allergy"}));
    let question = &captured.body["questions"]["run_000002"];
    assert_eq!(question["type"], "noul");
    assert_eq!(question["instructions"]["item"], "user: My sister is allergic to shellfish");
}

#[test]
fn recall_searches_are_split_into_judge_sized_batches() {
    let big = "x".repeat(dagos_openai::RECALL_BATCH_CHARS / 2 + 1);
    let chunks: Vec<_> = (1..=5).map(|i| recall_chunk(&format!("run_00000{i}"), &big)).collect();
    let sizes: Vec<usize> = dagos_openai::recall_batches(&chunks).iter().map(|b| b.len()).collect();
    assert_eq!(sizes, [1, 1, 1, 1, 1]);
    let small: Vec<_> = (1..=5).map(|i| recall_chunk(&format!("run_00000{i}"), "hi")).collect();
    assert_eq!(dagos_openai::recall_batches(&small).len(), 1);
    assert!(dagos_openai::recall_batches(&[]).is_empty());

    let missing = json!({"answers": {"run_000001": {"type": "noul", "noul": 0.5}}});
    let error = dagos_openai::relevance_from_decisions(&small[..2], &missing).unwrap_err();
    assert!(error.contains("no answer for run_000002"), "{error}");
}

/// Serves `responses` to consecutive connections, one each; returns the base URL and the
/// captured requests.
async fn mock_sequence(
    responses: Vec<(&'static str, Vec<String>)>,
) -> (String, JoinHandle<Vec<Captured>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = tokio::spawn(async move {
        let mut captured = Vec::new();
        for (status, parts) in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            captured.push(read_request(&mut socket).await);
            let kind =
                if status.starts_with("200") { "text/event-stream" } else { "application/json" };
            let head =
                format!("HTTP/1.1 {status}\r\ncontent-type: {kind}\r\nconnection: close\r\n\r\n");
            socket.write_all(head.as_bytes()).await.unwrap();
            for part in parts {
                socket.write_all(part.as_bytes()).await.unwrap();
            }
            socket.shutdown().await.ok();
        }
        captured
    });
    (base_url, handle)
}

fn ir_with_tool() -> InferenceIr {
    let mut ir = ir();
    ir.tools = vec![dagos_core::domain::IrTool {
        name: "ooda.exec_cli".into(),
        description: "Run a command.".into(),
        input_schema: json!({"type": "object", "properties": {"command": {"type": "string"}}})
            .as_object()
            .unwrap()
            .clone(),
    }];
    ir
}

#[tokio::test]
async fn native_tool_calls_come_back_as_a_response_document() {
    let events = vec![
        "data: {\"choices\":[{\"delta\":{\"content\":\"Listing the files.\"}}]}\n\n".to_owned(),
        format!(
            "data: {}\n\n",
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "c1", "type": "function", "function": {"name": "ooda__exec_cli", "arguments": "{\"comm"}}]}}]})
        ),
        format!(
            "data: {}\n\n",
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "function": {"arguments": "and\": \"ls\"}"}}]}}]})
        ),
        "data: [DONE]\n\n".to_owned(),
    ];
    let (base_url, requests) = mock_sequence(vec![("200 OK", events)]).await;
    let provider = native_provider(&base_url);
    let model = ModelId::parse("mock-1").unwrap();
    let ir = ir_with_tool();
    let mut deltas = CollectDeltas::default();
    let output =
        provider.infer(InferenceRequest { model_id: &model, ir: &ir }, &mut deltas).await.unwrap();
    let document: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(document["presentation"]["prose"], "Listing the files.");
    assert_eq!(
        document["tool_calls"],
        json!([{"name": "ooda.exec_cli", "arguments": {"command": "ls"}}])
    );
    assert_eq!(deltas.0.concat(), "Listing the files.", "the note streams as prose");

    let captured = requests.await.unwrap();
    let body = &captured[0].body;
    assert_eq!(body["tools"][0]["function"]["name"], "ooda__exec_cli");
    assert_eq!(
        body["tools"][0]["function"]["parameters"]["properties"]["command"]["type"],
        "string"
    );
    let user: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert!(user.get("tools").is_none(), "tools travel natively, not in the IR text");
    assert!(body["messages"][0]["content"].as_str().unwrap().contains("tool-calling interface"));
}

#[tokio::test]
async fn a_model_that_refuses_native_tools_gets_the_json_protocol_and_is_remembered() {
    let refusal = r#"{"error":{"message":"No endpoints found that support tool use."}}"#.to_owned();
    let (base_url, requests) = mock_sequence(vec![
        ("404 Not Found", vec![refusal]),
        ("200 OK", sse(&[DOCUMENT])),
        ("200 OK", sse(&[DOCUMENT])),
    ])
    .await;
    let provider = native_provider(&base_url);
    let model = ModelId::parse("mock-1").unwrap();
    let ir = ir_with_tool();
    for _ in 0..2 {
        let output = provider
            .infer(InferenceRequest { model_id: &model, ir: &ir }, &mut CollectDeltas::default())
            .await
            .unwrap();
        assert_eq!(output, DOCUMENT);
    }
    let captured = requests.await.unwrap();
    assert!(captured[0].body.get("tools").is_some(), "native first");
    assert!(captured[1].body.get("tools").is_none(), "then the JSON protocol");
    assert!(captured[2].body.get("tools").is_none(), "and the refusal is remembered");
}

#[tokio::test]
async fn model_windows_come_from_the_model_list_once() {
    let models = json!({"data": [
        {"id": "openai/gpt-6-luna", "context_length": 400000},
        {"id": "typesafe/jev-latest", "top_provider": {"context_length": 32000}},
        {"id": "no-window"}
    ]})
    .to_string();
    let (base_url, request) = mock("200 OK", "application/json", vec![models]).await;
    let provider = native_provider(&base_url);
    let window = |id: &str| {
        let provider = provider.clone();
        let model = ModelId::parse(id).unwrap();
        async move { provider.context_window(&model).await }
    };
    assert_eq!(window("openai/gpt-6-luna").await, Some(400_000));
    assert_eq!(window("~typesafe/jev-latest").await, Some(32_000), "the ~ alias is matched");
    assert_eq!(window("no-window").await, None);
    assert_eq!(window("unlisted").await, None, "answered from the list fetched once");
    assert!(request.await.unwrap().head.starts_with("GET /v1/models"));
}
