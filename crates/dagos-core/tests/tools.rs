//! Tool calls: a validated response asks, the gate decides, the executor runs permitted calls,
//! and the results come back through the next IR. Every step is an event, and runs stay bounded.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use common::{memory_store, payload};
use dagos_core::context::FakeJev;
use dagos_core::domain::{
    EventData, InferenceIr, IrTool, IrToolStatus, ModelId, ProjectId, ProviderId, Run, RunConfig,
    RunId, RunStatus, ToolDecider,
};
use dagos_core::provider::{
    DeltaSink, FakeProvider, InferenceProvider, InferenceRequest, ProviderError,
};
use dagos_core::runtime::Runtime;
use dagos_core::store::Store;
use dagos_core::tools::{AllowAll, ToolDecision, ToolExecutor, ToolGate, ToolOutput, ToolRequest};
use serde_json::json;

fn config(model: &str) -> RunConfig {
    RunConfig {
        provider_id: ProviderId::parse("fake").unwrap(),
        model_id: ModelId::parse(model).unwrap(),
        system_prompt: String::new(),
    }
}

fn tool(name: &str) -> IrTool {
    IrTool {
        name: name.into(),
        description: format!("The {name} tool."),
        input_schema: payload(json!({"type": "object"})),
    }
}

/// Records the calls it runs and answers with their arguments, or fails as configured.
#[derive(Default)]
struct Recorder {
    calls: Mutex<Vec<ToolRequest>>,
    fail: Option<String>,
    hang: bool,
}

#[async_trait]
impl ToolExecutor for Recorder {
    async fn call(&self, request: &ToolRequest) -> Result<ToolOutput, String> {
        self.calls.lock().unwrap().push(request.clone());
        if self.hang {
            std::future::pending::<()>().await;
        }
        match &self.fail {
            Some(error) => Err(error.clone()),
            None => Ok(ToolOutput { output: json!({"echo": request.arguments}), is_error: false }),
        }
    }
}

struct DenyAll;

#[async_trait]
impl ToolGate for DenyAll {
    async fn decide(&self, _run_id: &RunId, _request: &ToolRequest) -> ToolDecision {
        ToolDecision::Deny { by: ToolDecider::User, reason: "not now".into() }
    }
}

fn setup(runtime: impl FnOnce(Runtime) -> Runtime) -> (Arc<Store>, Runtime, ProjectId) {
    let store = Arc::new(memory_store());
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let base = Runtime::new(store.clone(), Arc::new(FakeJev::new()))
        .with_provider(Arc::new(FakeProvider::new()))
        .with_tools(vec![tool("files.read"), tool("files.write")]);
    (store, runtime(base), project)
}

fn types(store: &Store, run: &Run) -> Vec<String> {
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    events
        .iter()
        .map(|event| event.data.event_type())
        .filter(|kind| kind != "inference.delta")
        .collect()
}

fn irs(store: &Store, run: &Run) -> Vec<InferenceIr> {
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    events
        .into_iter()
        .filter_map(|event| match event.data {
            EventData::IrCompiled { ir } => Some(ir),
            _ => None,
        })
        .collect()
}

fn final_prose(store: &Store, run: &Run) -> String {
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    events
        .into_iter()
        .rev()
        .find_map(|event| match event.data {
            EventData::ResponseValidated { response } => Some(response.presentation.prose),
            _ => None,
        })
        .unwrap()
}

#[tokio::test]
async fn permitted_calls_run_and_their_results_reach_the_next_ir() {
    let recorder = Arc::new(Recorder::default());
    let (store, runtime, project) =
        setup(|runtime| runtime.with_tool_runner(recorder.clone(), Arc::new(AllowAll)));
    let message = r#"files.write {"path": "notes.md", "text": "hi"}"#;
    let run = runtime.run(&project, message, &config("fake-tool")).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed, "{run:?}");

    let calls = recorder.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!((calls[0].call_id.as_str(), calls[0].name.as_str()), ("call_1", "files.write"));
    assert_eq!(calls[0].arguments["path"], "notes.md");

    let timeline = types(&store, &run);
    let tail: Vec<&str> =
        timeline.iter().map(String::as_str).skip_while(|t| *t != "ir.compiled").collect();
    assert_eq!(
        tail,
        [
            "ir.compiled",
            "inference.started",
            "inference.completed",
            "response.validated",
            "tool.requested",
            "tool.decided",
            "tool.completed",
            "ir.compiled",
            "inference.started",
            "inference.completed",
            "response.validated",
            "run.completed",
        ]
    );
    let irs = irs(&store, &run);
    assert!(irs[0].tool_results.is_empty());
    let result = &irs[1].tool_results[0];
    assert_eq!((result.call_id.as_str(), result.status), ("call_1", IrToolStatus::Completed));
    assert_eq!(result.output.as_ref().unwrap()["echo"]["text"], "hi");
    assert!(final_prose(&store, &run).starts_with("`files.write` returned completed"));
}

#[tokio::test]
async fn denied_calls_never_run_and_the_model_learns_why() {
    let recorder = Arc::new(Recorder::default());
    let (store, runtime, project) =
        setup(|runtime| runtime.with_tool_runner(recorder.clone(), Arc::new(DenyAll)));
    let run = runtime.run(&project, "files.read", &config("fake-tool")).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert!(recorder.calls.lock().unwrap().is_empty());
    assert!(!types(&store, &run).contains(&"tool.completed".to_owned()));
    let result = &irs(&store, &run)[1].tool_results[0];
    assert_eq!(result.status, IrToolStatus::Denied);
    assert_eq!(result.reason.as_deref(), Some("not now"));
    assert_eq!(final_prose(&store, &run), "`files.read` was denied: not now");
}

#[tokio::test]
async fn without_a_runner_calls_are_recorded_and_denied_as_unavailable() {
    let (store, runtime, project) = setup(|runtime| runtime);
    let run = runtime.run(&project, "files.read", &config("fake-tool")).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    assert!(events.iter().any(|event| matches!(
        &event.data,
        EventData::ToolDecided { allowed: false, by: ToolDecider::Unavailable, .. }
    )));
}

#[tokio::test]
async fn tool_failures_and_timeouts_come_back_as_failed_results() {
    for recorder in [
        Recorder { fail: Some("server crashed".into()), ..Recorder::default() },
        Recorder { hang: true, ..Recorder::default() },
    ] {
        let (store, runtime, project) = setup(|runtime| {
            runtime
                .with_tool_runner(Arc::new(recorder), Arc::new(AllowAll))
                .with_tool_timeout(Duration::from_millis(50))
        });
        let run = runtime.run(&project, "files.read", &config("fake-tool")).await.unwrap();
        assert_eq!(run.status, RunStatus::Completed, "a failed tool does not fail the run");
        let result = &irs(&store, &run)[1].tool_results[0];
        assert_eq!(result.status, IrToolStatus::Failed);
        let error = result.output.as_ref().unwrap()["error"].as_str().unwrap().to_owned();
        assert!(error == "server crashed" || error.starts_with("no result within"), "{error}");
    }
}

/// Asks for an unknown tool on every step, to exercise the availability check and the limit.
struct Insistent {
    id: ProviderId,
}

#[async_trait]
impl InferenceProvider for Insistent {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn suggested_models(&self) -> Vec<ModelId> {
        Vec::new()
    }

    async fn infer(
        &self,
        _request: InferenceRequest<'_>,
        _deltas: &mut dyn DeltaSink,
    ) -> Result<String, ProviderError> {
        Ok(json!({
            "schema": "kiss.inference-response.v1",
            "presentation": {"prose": "Again."},
            "emissions": [],
            "tool_calls": [{"name": "nope.tool", "arguments": {}}]
        })
        .to_string())
    }
}

#[tokio::test]
async fn unknown_tools_are_unavailable_and_runs_stop_at_the_step_limit() {
    let recorder = Arc::new(Recorder::default());
    let (store, runtime, project) = setup(|runtime| {
        runtime
            .with_provider(Arc::new(Insistent { id: ProviderId::parse("insistent").unwrap() }))
            .with_tool_runner(recorder.clone(), Arc::new(AllowAll))
            .with_max_tool_steps(2)
    });
    let mut insist = config("any");
    insist.provider_id = ProviderId::parse("insistent").unwrap();
    let run = runtime.run(&project, "go", &insist).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);
    assert!(recorder.calls.lock().unwrap().is_empty());

    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    let decisions: Vec<(String, ToolDecider)> = events
        .into_iter()
        .filter_map(|event| match event.data {
            EventData::ToolDecided { call_id, by, allowed: false, .. } => Some((call_id, by)),
            _ => None,
        })
        .collect();
    assert_eq!(
        decisions,
        [
            ("call_1".to_owned(), ToolDecider::Unavailable),
            ("call_2".to_owned(), ToolDecider::Unavailable),
            ("call_3".to_owned(), ToolDecider::Limit),
        ]
    );
    assert_eq!(irs(&store, &run).len(), 3, "two tool rounds, then the limit ends the run");
}

fn with_jev(jev: FakeJev) -> (Arc<Store>, Runtime, ProjectId) {
    let store = Arc::new(memory_store());
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let runtime = Runtime::new(store.clone(), Arc::new(jev))
        .with_provider(Arc::new(FakeProvider::new()))
        .with_tools(vec![tool("files.read"), tool("files.write")])
        .with_tool_runner(Arc::new(Recorder::default()), Arc::new(AllowAll));
    (store, runtime, project)
}

#[tokio::test]
async fn jev_decides_which_tools_the_model_sees_in_a_run() {
    let hide_write = r#"{"schema":"kiss.jev-context.v1","classifications":[],
        "tools":[{"name":"files.write","classification":"inactive"}]}"#;
    let (store, runtime, project) = with_jev(FakeJev::scripted(hide_write));
    let run = runtime.run(&project, "files.write {}", &config("fake-tool")).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed);

    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    let request = events
        .iter()
        .find_map(|event| match &event.data {
            EventData::JevRequested { request, .. } => Some(request.clone()),
            _ => None,
        })
        .unwrap();
    let offered: Vec<&str> = request.tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(offered, ["files.read", "files.write"], "Jev sees every offered tool");

    let irs = irs(&store, &run);
    let exposed: Vec<&str> = irs[0].tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(exposed, ["files.read"], "hidden tools never reach the model");
    // fake-tool falls back to the first exposed tool, so the hidden one is never even requested.
    assert_eq!(irs[1].tool_results[0].name, "files.read");
}

#[tokio::test]
async fn unlabeled_tools_stay_exposed_and_bad_tool_labels_are_rejected() {
    let (store, runtime, project) = with_jev(FakeJev::new());
    let run = runtime.run(&project, "hi", &config("fake-echo")).await.unwrap();
    assert_eq!(irs(&store, &run)[0].tools.len(), 2, "the offline policy labels no tools");

    for (label, reason) in [
        (r#"[{"name":"shell.rm","classification":"active"}]"#, "was not a candidate"),
        (
            r#"[{"name":"files.read","classification":"active"},{"name":"files.read","classification":"inactive"}]"#,
            "more than once",
        ),
        (r#"[{"name":"files.read","classification":"run"}]"#, ""),
    ] {
        let raw =
            format!(r#"{{"schema":"kiss.jev-context.v1","classifications":[],"tools":{label}}}"#);
        let (store, runtime, project) = with_jev(FakeJev::scripted(raw));
        let run = runtime.run(&project, "hi", &config("fake-echo")).await.unwrap();
        assert_eq!(
            run.error_code,
            Some(dagos_core::domain::ErrorCode::JevInvalidOutput),
            "{label}"
        );
        let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
        let rejected = events.iter().find_map(|event| match &event.data {
            EventData::JevRejected { reason, .. } => Some(reason.clone()),
            _ => None,
        });
        assert!(rejected.unwrap().contains(reason), "{label}");
    }
}
