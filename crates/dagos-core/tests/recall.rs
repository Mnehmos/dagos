//! Recall: IR carries only the conversation's latest turns, and `dagos.recall` finds the older
//! ones verbatim. Jev judges relevance when it can; word overlap judges otherwise.

mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::memory_store;
use dagos_core::context::recall::{
    RECALL_TOOL, RecallChunk, cursor_output, lexical_relevance, search_output,
};
use dagos_core::context::{FakeJev, JevClassifier, JevError};
use dagos_core::domain::{
    EventData, InferenceIr, JevRequest, ModelId, ProjectId, ProviderId, Run, RunConfig, RunStatus,
    ToolDecider,
};
use dagos_core::provider::FakeProvider;
use dagos_core::runtime::Runtime;
use dagos_core::store::Store;
use serde_json::{Value, json};

fn config(model: &str) -> RunConfig {
    RunConfig {
        provider_id: ProviderId::parse("fake").unwrap(),
        model_id: ModelId::parse(model).unwrap(),
        system_prompt: String::new(),
    }
}

/// Classifies like the fake Jev and judges relevance with scripted scores: turns mentioning
/// `shellfish` are relevant. Or fails to judge.
struct JudgingJev {
    fail: bool,
}

#[async_trait]
impl JevClassifier for JudgingJev {
    fn id(&self) -> &str {
        "judging-jev"
    }

    async fn classify(&self, request: &JevRequest) -> Result<String, JevError> {
        FakeJev::new().classify(request).await
    }

    async fn relevance(&self, _query: &str, chunks: &[RecallChunk]) -> Result<Vec<f64>, JevError> {
        if self.fail {
            return Err(JevError("offline".into()));
        }
        Ok(chunks.iter().map(|c| if c.text.contains("shellfish") { 0.97 } else { 0.04 }).collect())
    }

    fn judges_relevance(&self) -> bool {
        true
    }
}

const TURNS: [&str; 4] = [
    "Help me plan a trip to Lisbon in October with my sister.",
    "My sister is allergic to shellfish, keep that in mind for restaurants.",
    "Can you draft a packing list too?",
    "Also, what is a good gift for my mum's birthday next week?",
];

/// A conversation of [`TURNS`] under a one-turn window.
async fn conversation(jev: Arc<dyn JevClassifier>) -> (Arc<Store>, Runtime, ProjectId, Vec<Run>) {
    let store = Arc::new(memory_store());
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let runtime = Runtime::new(store.clone(), jev)
        .with_provider(Arc::new(FakeProvider::new()))
        .with_conversation_window(1);
    let mut runs = Vec::new();
    for turn in TURNS {
        let run = runtime.run(&project, turn, &config("fake-echo")).await.unwrap();
        assert_eq!(run.status, RunStatus::Completed, "{run:?}");
        runs.push(run);
    }
    (store, runtime, project, runs)
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

/// The recall call's decision and output in `run`.
fn recall_result(store: &Store, run: &Run) -> (ToolDecider, Value, bool) {
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    let decided = events.iter().find_map(|event| match &event.data {
        EventData::ToolDecided { by, allowed: true, .. } => Some(*by),
        _ => None,
    });
    let completed = events.into_iter().find_map(|event| match event.data {
        EventData::ToolCompleted { output, is_error, .. } => Some((output, is_error)),
        _ => None,
    });
    let (output, is_error) = completed.expect("the recall call completed");
    (decided.expect("the recall call was allowed"), output, is_error)
}

#[tokio::test]
async fn ir_keeps_the_window_and_recall_is_offered_only_when_older_turns_exist() {
    let (store, runtime, project, runs) = conversation(Arc::new(FakeJev::new())).await;
    let first = &irs(&store, &runs[1])[0];
    assert_eq!(first.recent_events.len(), 1);
    assert!(first.tools.is_empty(), "one earlier turn fits the window: {:?}", first.tools);

    let later = &irs(&store, &runs[3])[0];
    assert_eq!(later.recent_events.len(), 1, "only the window reaches IR");
    assert_eq!(later.recent_events[0].request.as_deref(), Some(TURNS[2]));
    let names: Vec<&str> = later.tools.iter().map(|tool| tool.name.as_str()).collect();
    assert_eq!(names, [RECALL_TOOL]);

    // A conversation that has not outgrown the window is not offered recall.
    let fresh = runtime
        .start_in(dagos_core::runtime::Thread::New(&project), "hello", &config("fake-echo"))
        .unwrap();
    let fresh = runtime.finish(fresh).await.unwrap();
    assert!(irs(&store, &fresh)[0].tools.is_empty());
}

#[tokio::test]
async fn jev_finds_the_relevant_turn_verbatim_with_cursors() {
    let (store, runtime, project, runs) = conversation(Arc::new(JudgingJev { fail: false })).await;
    let message = r#"dagos.recall {"query": "sister food allergy"}"#;
    let run = runtime.run(&project, message, &config("fake-tool")).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed, "{run:?}");

    let (by, output, is_error) = recall_result(&store, &run);
    assert_eq!(by, ToolDecider::Policy, "recall reads only this conversation: no approval");
    assert!(!is_error);
    let result = &output["structured"];
    assert_eq!(result["scored_by"], "judging-jev");
    assert_eq!(result["searched"], 4);
    assert_eq!(result["dropped"], 3);
    let hit = &result["matches"][0];
    assert_eq!(hit["turn"], runs[1].id.as_str());
    assert_eq!(hit["score"], 0.97);
    assert!(hit["text"].as_str().unwrap().contains(&format!("user: {}", TURNS[1])));
    assert_eq!(hit["before"], runs[0].id.as_str());
    assert_eq!(hit["after"], runs[2].id.as_str());
    let text = output["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("allergic to shellfish"), "{text}");

    // The result reaches the model through the next IR.
    let second = &irs(&store, &run)[1];
    assert_eq!(second.tool_results[0].name, RECALL_TOOL);
    assert_eq!(second.tool_results[0].output.as_ref(), Some(&output));
}

#[tokio::test]
async fn cursors_page_to_neighbouring_turns() {
    let (store, runtime, project, runs) = conversation(Arc::new(FakeJev::new())).await;
    let message = format!(r#"dagos.recall {{"cursor": "{}"}}"#, runs[0].id);
    let run = runtime.run(&project, &message, &config("fake-tool")).await.unwrap();
    let (_, output, is_error) = recall_result(&store, &run);
    assert!(!is_error);
    let turn = &output["structured"]["matches"][0];
    assert_eq!(turn["turn"], runs[0].id.as_str());
    assert!(turn.get("before").is_none(), "the first turn has nothing before it");
    assert_eq!(turn["after"], runs[1].id.as_str());

    let message = r#"dagos.recall {"cursor": "run_999999"}"#;
    let run = runtime.run(&project, message, &config("fake-tool")).await.unwrap();
    let (_, output, is_error) = recall_result(&store, &run);
    assert!(is_error, "{output}");
    assert!(output["error"].as_str().unwrap().contains("not an earlier turn"));
}

#[tokio::test]
async fn without_a_judging_jev_word_overlap_decides_and_says_so() {
    for (jev, scored_by) in [
        (Arc::new(FakeJev::new()) as Arc<dyn JevClassifier>, "word overlap"),
        (
            Arc::new(JudgingJev { fail: true }),
            "word overlap (Jev failed: Jev classifier unavailable: offline)",
        ),
    ] {
        let (store, runtime, project, runs) = conversation(jev).await;
        let message = r#"dagos.recall {"query": "sister allergy"}"#;
        let run = runtime.run(&project, message, &config("fake-tool")).await.unwrap();
        let (_, output, _) = recall_result(&store, &run);
        let result = &output["structured"];
        assert_eq!(result["scored_by"], scored_by);
        assert_eq!(result["matches"][0]["turn"], runs[1].id.as_str(), "{result}");
    }
}

#[test]
fn word_overlap_compares_word_stems() {
    let chunk = |id: &str, text: &str| RecallChunk { id: id.into(), text: text.into() };
    let chunks = [
        chunk("run_000001", "My sister is allergic to shellfish."),
        chunk("run_000002", "Here is a packing list."),
    ];
    assert_eq!(lexical_relevance("sister allergy", &chunks), [1.0, 0.0]);
    assert_eq!(
        lexical_relevance("the and", &chunks),
        [0.0, 0.0],
        "common words alone match nothing"
    );

    let output = search_output("sister allergy", &chunks, &[1.0, 0.0], "word overlap");
    assert_eq!(output["structured"]["matches"].as_array().unwrap().len(), 1);
    assert_eq!(output["structured"]["matches"][0]["after"], "run_000002");
    assert!(cursor_output("run_000003", &chunks).is_none());
    assert_eq!(json!(cursor_output("run_000002", &chunks).unwrap()["structured"]["searched"]), 1);
}
