//! Recall and compaction: Jev decides automatically which earlier turns of the project's chats
//! reach IR, and which of a long run's earlier tool results stay in it. The model never asks.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::{memory_store, payload};
use dagos_core::context::recall::RecallChunk;
use dagos_core::context::{FakeJev, JevClassifier, JevError};
use dagos_core::domain::{
    EventData, InferenceIr, IrTool, JevRequest, ModelId, ProjectId, ProviderId, Run, RunConfig,
    RunStatus,
};
use dagos_core::provider::{
    DeltaSink, FakeProvider, InferenceProvider, InferenceRequest, ProviderError,
};
use dagos_core::runtime::{Runtime, Thread};
use dagos_core::store::Store;
use dagos_core::tools::{AllowAll, ToolExecutor, ToolOutput, ToolRequest};
use serde_json::json;

fn config(provider: &str, model: &str) -> RunConfig {
    RunConfig {
        provider_id: ProviderId::parse(provider).unwrap(),
        model_id: ModelId::parse(model).unwrap(),
        system_prompt: String::new(),
    }
}

/// Classifies like the fake Jev and judges relevance by a marker: chunks containing `relevant`
/// (e.g. `shellfish`) score 0.97, the rest 0.04. Records every query it is asked.
struct JudgingJev {
    relevant: &'static str,
    queries: Mutex<Vec<(String, usize)>>,
}

impl JudgingJev {
    fn new(relevant: &'static str) -> Arc<Self> {
        Arc::new(Self { relevant, queries: Mutex::new(Vec::new()) })
    }
}

#[async_trait]
impl JevClassifier for JudgingJev {
    fn id(&self) -> &str {
        "judging-jev"
    }

    async fn classify(&self, request: &JevRequest) -> Result<String, JevError> {
        FakeJev::new().classify(request).await
    }

    async fn relevance(
        &self,
        query: &str,
        chunks: &[RecallChunk],
    ) -> Result<Option<Vec<f64>>, JevError> {
        self.queries.lock().unwrap().push((query.to_owned(), chunks.len()));
        let score =
            |chunk: &RecallChunk| if chunk.text.contains(self.relevant) { 0.97 } else { 0.04 };
        Ok(Some(chunks.iter().map(score).collect()))
    }
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

const TRIP: [&str; 4] = [
    "Help me plan a trip to Lisbon in October with my sister.",
    "My sister is allergic to shellfish, keep that in mind for restaurants.",
    "Can you draft a packing list too?",
    "Also, what is a good gift for my mum's birthday next week?",
];

/// A chat of [`TRIP`] under a one-turn window.
async fn trip(jev: Arc<dyn JevClassifier>) -> (Arc<Store>, Runtime, ProjectId, Vec<Run>) {
    let store = Arc::new(memory_store());
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let runtime = Runtime::new(store.clone(), jev)
        .with_provider(Arc::new(FakeProvider::new()))
        .with_conversation_window(1);
    let mut runs = Vec::new();
    for turn in TRIP {
        let run = runtime.run(&project, turn, &config("fake", "fake-echo")).await.unwrap();
        assert_eq!(run.status, RunStatus::Completed, "{run:?}");
        runs.push(run);
    }
    (store, runtime, project, runs)
}

#[tokio::test]
async fn jev_recalls_relevant_turns_from_any_chat_verbatim() {
    let jev = JudgingJev::new("shellfish");
    let (store, runtime, project, runs) = trip(jev.clone()).await;

    // Within the chat: the shellfish turn left the one-turn window and comes back as recalled.
    let ir = &irs(&store, &runs[3])[0];
    assert_eq!(ir.recent_events.len(), 1);
    assert_eq!(ir.recalled.len(), 1);
    assert_eq!(ir.recalled[0].run_id, runs[1].id);
    assert!(ir.recalled[0].text.contains(&format!("user: {}", TRIP[1])), "verbatim");
    assert!(ir.recalled[0].text.contains("assistant: "), "with the reply");
    // Only turns outside the window are judged: the run before, in recent_events, is not.
    let (query, judged) = jev.queries.lock().unwrap().last().cloned().unwrap();
    assert_eq!((query.as_str(), judged), (TRIP[3], 2));

    // A new chat of the same project recalls it too: chats organize, the project remembers.
    let started = runtime
        .start_in(
            Thread::New(&project),
            "Where should we eat tonight?",
            &config("fake", "fake-echo"),
        )
        .unwrap();
    let dinner = runtime.finish(started).await.unwrap();
    let ir = &irs(&store, &dinner)[0];
    assert!(ir.recent_events.is_empty(), "a new chat has no turns of its own");
    assert_eq!(ir.recalled.len(), 1);
    assert_eq!(ir.recalled[0].run_id, runs[1].id);
    assert_eq!(ir.recalled[0].chat, TRIP[0], "the chat is titled after its first message");
}

#[tokio::test]
async fn without_a_judging_jev_nothing_is_recalled() {
    let (store, _, _, runs) = trip(Arc::new(FakeJev::new())).await;
    let ir = &irs(&store, &runs[3])[0];
    assert_eq!(ir.recent_events.len(), 1);
    assert!(ir.recalled.is_empty());
    assert!(!serde_json::to_value(ir).unwrap().as_object().unwrap().contains_key("recalled"));
}

/// Reads three files, one per step, then answers.
struct Reader {
    id: ProviderId,
}

#[async_trait]
impl InferenceProvider for Reader {
    fn id(&self) -> &ProviderId {
        &self.id
    }

    fn suggested_models(&self) -> Vec<ModelId> {
        Vec::new()
    }

    async fn infer(
        &self,
        request: InferenceRequest<'_>,
        _deltas: &mut dyn DeltaSink,
    ) -> Result<String, ProviderError> {
        let step = request.ir.tool_results.len();
        let mut response = json!({
            "schema": "kiss.inference-response.v1",
            "presentation": {"prose": format!("Reading file {}.", step + 1)},
            "emissions": [],
        });
        if step < 3 {
            response["tool_calls"] =
                json!([{"name": "files.read", "arguments": {"path": format!("f{}", step + 1)}}]);
        } else {
            response["presentation"]["prose"] = json!("Done.");
        }
        Ok(response.to_string())
    }
}

/// `f1` holds the answer, `f2` is large noise, `f3` is small.
struct Files;

#[async_trait]
impl ToolExecutor for Files {
    async fn call(&self, request: &ToolRequest) -> Result<ToolOutput, String> {
        let text = match request.arguments["path"].as_str().unwrap() {
            "f1" => format!("the answer is 42 {}", "a".repeat(3000)),
            "f2" => format!("unrelated {}", "b".repeat(3000)),
            _ => "short".to_owned(),
        };
        Ok(ToolOutput {
            output: json!({"content": [{"type": "text", "text": text}]}),
            is_error: false,
        })
    }
}

async fn read_files(jev: Arc<dyn JevClassifier>) -> (Arc<Store>, Run) {
    let store = Arc::new(memory_store());
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let tool = IrTool {
        name: "files.read".into(),
        description: "Read a file.".into(),
        input_schema: payload(json!({"type": "object"})),
    };
    let runtime = Runtime::new(store.clone(), jev)
        .with_provider(Arc::new(Reader { id: ProviderId::parse("reader").unwrap() }))
        .with_tools(vec![tool])
        .with_tool_runner(Arc::new(Files), Arc::new(AllowAll));
    let run = runtime.run(&project, "What is the answer?", &config("reader", "any")).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed, "{run:?}");
    (store, run)
}

#[tokio::test]
async fn large_earlier_tool_results_jev_judges_irrelevant_are_left_out() {
    let jev = JudgingJev::new("answer is 42");
    let (store, run) = read_files(jev.clone()).await;
    let steps = irs(&store, &run);
    assert_eq!(steps.len(), 4);
    let outputs = |ir: &InferenceIr| -> Vec<bool> {
        ir.tool_results
            .iter()
            .map(|result| result.output.as_ref().unwrap().get("omitted").is_none())
            .collect()
    };
    assert_eq!(outputs(&steps[1]), [true], "the latest round is always kept");
    assert_eq!(outputs(&steps[2]), [true, true], "f1 is relevant, f2 is the latest");
    assert_eq!(outputs(&steps[3]), [true, false, true], "f2 is left out; f3 is too small to judge");
    let note = steps[3].tool_results[1].output.as_ref().unwrap()["omitted"].as_str().unwrap();
    assert!(note.contains("3010-character"), "{note}");

    // Each step judges afresh, against the request and the model's latest step.
    let queries = jev.queries.lock().unwrap().clone();
    assert_eq!(queries.len(), 2, "steps 3 and 4: step 2 had nothing older to judge");
    assert!(queries[1].0.starts_with("What is the answer?"));
    assert!(queries[1].0.contains("The assistant's latest step: Reading file 3."));
    assert_eq!(queries[1].1, 2, "f1 and f2; f3 is the latest round");

    // The full output stays in the event history and the inspector.
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    let completed =
        events.iter().filter(|event| matches!(event.data, EventData::ToolCompleted { .. })).count();
    assert_eq!(completed, 3);
}

#[tokio::test]
async fn without_a_judging_jev_every_tool_result_is_kept() {
    let (store, run) = read_files(Arc::new(FakeJev::new())).await;
    let last = irs(&store, &run).pop().unwrap();
    assert!(
        last.tool_results.iter().all(|result| result
            .output
            .as_ref()
            .unwrap()
            .get("omitted")
            .is_none())
    );
}

#[tokio::test]
async fn every_relevant_turn_is_recalled_whole_with_no_count_limit() {
    let jev = JudgingJev::new("shellfish");
    let store = Arc::new(memory_store());
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let runtime = Runtime::new(store.clone(), jev.clone())
        .with_provider(Arc::new(FakeProvider::new()))
        .with_conversation_window(1);
    let long = format!("shellfish notes: {}", "detail ".repeat(1500));
    let mut messages: Vec<String> = (1..=8).map(|i| format!("shellfish fact {i}")).collect();
    messages.push(long.clone());
    messages.push("unrelated".into());
    for message in &messages {
        runtime.run(&project, message, &config("fake", "fake-echo")).await.unwrap();
    }
    let last =
        runtime.run(&project, "What do we know?", &config("fake", "fake-echo")).await.unwrap();
    let ir = &irs(&store, &last)[0];
    // Ten earlier turns: the latest ("unrelated") is in recent_events, and the other nine (eight
    // facts and the long note) all mention shellfish, so all nine are relevant.
    assert_eq!(ir.recalled.len(), 9, "no cap on how many relevant turns are recalled");
    let whole = ir.recalled.iter().find(|turn| turn.text.contains("shellfish notes")).unwrap();
    assert!(whole.text.contains(&long), "a recalled turn arrives whole, not cut");
}
