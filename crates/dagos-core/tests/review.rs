//! The review loop: each time the model replies without tool calls, a reviewer judges the code
//! the run changed, and findings go back to the model until the code is clean, stops changing,
//! or the run runs out of reviews. Reviews never fail a run.

mod common;

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::{memory_store, payload};
use dagos_core::context::FakeJev;
use dagos_core::domain::{
    EventData, InferenceIr, IrFinding, IrTool, ModelId, ProviderId, Run, RunConfig, RunId,
    RunStatus,
};
use dagos_core::provider::FakeProvider;
use dagos_core::runtime::Runtime;
use dagos_core::store::Store;
use dagos_core::tools::{AllowAll, Review, Reviewer, ToolExecutor, ToolOutput, ToolRequest};
use serde_json::json;

fn finding(rule: &str) -> IrFinding {
    IrFinding {
        rule: rule.into(),
        text: "Swallows errors.".into(),
        file: "src/lib.rs".into(),
        function: "load".into(),
        line: 3,
        probability: 0.93,
    }
}

/// Answers a scripted list of reviews, then clean reviews; records what it was shown.
#[derive(Default)]
struct Scripted {
    reviews: Mutex<Vec<Result<Review, String>>>,
    seen_calls: Mutex<Vec<String>>,
    ended: Mutex<Vec<RunId>>,
}

impl Scripted {
    fn new(reviews: Vec<Result<Review, String>>) -> Arc<Self> {
        Arc::new(Self { reviews: Mutex::new(reviews), ..Default::default() })
    }
}

#[async_trait]
impl Reviewer for Scripted {
    async fn before_call(&self, _run_id: &RunId, request: &ToolRequest) {
        self.seen_calls.lock().unwrap().push(request.name.clone());
    }

    async fn review(&self, _run_id: &RunId) -> Result<Review, String> {
        let mut reviews = self.reviews.lock().unwrap();
        if reviews.is_empty() {
            return Ok(Review { findings: vec![], judged: 1, fingerprint: "clean".into() });
        }
        reviews.remove(0)
    }

    fn end(&self, run_id: &RunId) {
        self.ended.lock().unwrap().push(run_id.clone());
    }
}

fn review(fingerprint: &str, findings: Vec<IrFinding>) -> Result<Review, String> {
    Ok(Review { findings, judged: 2, fingerprint: fingerprint.into() })
}

struct Echo;

#[async_trait]
impl ToolExecutor for Echo {
    async fn call(&self, _request: &ToolRequest) -> Result<ToolOutput, String> {
        Ok(ToolOutput { output: json!({"ok": true}), is_error: false })
    }
}

async fn run_with(reviewer: Arc<Scripted>, model: &str) -> (Arc<Store>, Run) {
    let store = Arc::new(memory_store());
    let project = store.transaction(|tx| tx.create_project("demo")).unwrap().id;
    let tool = IrTool {
        name: "files.write".into(),
        description: "Write a file.".into(),
        input_schema: payload(json!({"type": "object"})),
    };
    let runtime = Runtime::new(store.clone(), Arc::new(FakeJev::new()))
        .with_provider(Arc::new(FakeProvider::new()))
        .with_tools(vec![tool])
        .with_tool_runner(Arc::new(Echo), Arc::new(AllowAll))
        .with_reviewer(reviewer, 3);
    let config = RunConfig {
        provider_id: ProviderId::parse("fake").unwrap(),
        model_id: ModelId::parse(model).unwrap(),
        system_prompt: String::new(),
    };
    let run =
        runtime.run(&project, "files.write {\"path\": \"src/lib.rs\"}", &config).await.unwrap();
    assert_eq!(run.status, RunStatus::Completed, "{run:?}");
    (store, run)
}

fn reviews(store: &Store, run: &Run) -> Vec<(u32, usize, Option<String>)> {
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    events
        .into_iter()
        .filter_map(|event| match event.data {
            EventData::ReviewCompleted { round, findings, error, .. } => {
                Some((round, findings.len(), error))
            }
            _ => None,
        })
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

#[tokio::test]
async fn findings_go_back_to_the_model_until_a_review_is_clean() {
    let reviewer = Scripted::new(vec![review("v1", vec![finding("swallows-errors")])]);
    let (store, run) = run_with(reviewer.clone(), "fake-tool").await;

    assert_eq!(*reviewer.seen_calls.lock().unwrap(), ["files.write"], "shown before it ran");
    assert_eq!(reviews(&store, &run), [(1, 1, None), (2, 0, None)]);
    let steps = irs(&store, &run);
    // Step 1 calls the tool, step 2 reports, step 3 answers the review, which then is clean.
    assert_eq!(steps.len(), 3);
    assert!(steps[0].review.is_none() && steps[1].review.is_none());
    let handed_back = steps[2].review.as_ref().expect("the review reached the model");
    assert_eq!((handed_back.round, handed_back.max_rounds), (1, 3));
    assert_eq!(handed_back.findings, [finding("swallows-errors")]);
    assert_eq!(
        *reviewer.ended.lock().unwrap(),
        std::slice::from_ref(&run.id),
        "the reviewer forgets the run"
    );
}

#[tokio::test]
async fn the_loop_stops_when_the_code_stops_changing_or_reviews_run_out() {
    // The model changed nothing after the first review: the same fingerprint ends the loop.
    let reviewer = Scripted::new(vec![
        review("same", vec![finding("a")]),
        review("same", vec![finding("a")]),
        review("never", vec![]),
    ]);
    let (store, run) = run_with(reviewer, "fake-echo").await;
    assert_eq!(reviews(&store, &run), [(1, 1, None), (2, 1, None)]);

    // Every review finds something new: three reviews, then the run completes regardless.
    let reviewer = Scripted::new(vec![
        review("v1", vec![finding("a")]),
        review("v2", vec![finding("b")]),
        review("v3", vec![finding("c")]),
        review("v4", vec![finding("d")]),
    ]);
    let (store, run) = run_with(reviewer, "fake-echo").await;
    assert_eq!(reviews(&store, &run), [(1, 1, None), (2, 1, None), (3, 1, None)]);
    assert_eq!(irs(&store, &run).len(), 3, "the third review's findings stay recorded, not sent");
}

#[tokio::test]
async fn nothing_to_review_records_nothing_and_errors_do_not_fail_runs() {
    let reviewer = Scripted::new(vec![Ok(Review::default())]);
    let (store, run) = run_with(reviewer, "fake-echo").await;
    assert!(reviews(&store, &run).is_empty(), "no changed code: no review event");

    let reviewer = Scripted::new(vec![Err("Jev is unreachable".into())]);
    let (store, run) = run_with(reviewer, "fake-echo").await;
    assert_eq!(reviews(&store, &run), [(1, 0, Some("Jev is unreachable".into()))]);
    assert_eq!(irs(&store, &run).len(), 1);
}

/// The lint-finding nodes in `run`'s project, as (rule, file, function).
fn finding_nodes(store: &Store, run: &Run) -> Vec<(String, String, String)> {
    let nodes = store.transaction(|tx| tx.nodes(&run.project_id)).unwrap();
    nodes
        .into_iter()
        .filter(|node| node.payload.get("kind").and_then(|k| k.as_str()) == Some("lint_finding"))
        .map(|node| {
            let field = |key: &str| node.payload[key].as_str().unwrap().to_owned();
            (field("rule"), field("file"), field("function"))
        })
        .collect()
}

#[tokio::test]
async fn findings_left_open_become_project_nodes_once() {
    // Clean at the end: nothing to remember.
    let reviewer = Scripted::new(vec![review("v1", vec![finding("swallows-errors")])]);
    let (store, run) = run_with(reviewer, "fake-tool").await;
    assert!(finding_nodes(&store, &run).is_empty());

    // Disputed (the code did not change): the open finding is remembered, once, as an observation.
    let reviewer = Scripted::new(vec![
        review("same", vec![finding("swallows-errors"), finding("dead-code")]),
        review("same", vec![finding("swallows-errors"), finding("dead-code")]),
    ]);
    let (store, run) = run_with(reviewer, "fake-echo").await;
    let nodes = finding_nodes(&store, &run);
    assert_eq!(nodes.len(), 2, "{nodes:?}");
    assert_eq!(nodes[0], ("swallows-errors".into(), "src/lib.rs".into(), "load".into()));
    let events = store.transaction(|tx| tx.events(&run.id)).unwrap();
    let created = events
        .iter()
        .filter(|event| match &event.data {
            EventData::DagNodeCreated { emission_ref, .. } => {
                emission_ref.as_str().starts_with("review-finding-")
            }
            _ => false,
        })
        .count();
    assert_eq!(created, 2, "each node is announced like an emission");

    // Out of reviews: the last review's findings are remembered.
    let reviewer = Scripted::new(vec![
        review("v1", vec![finding("a")]),
        review("v2", vec![finding("b")]),
        review("v3", vec![finding("c")]),
    ]);
    let (store, run) = run_with(reviewer, "fake-echo").await;
    let rules: Vec<String> =
        finding_nodes(&store, &run).into_iter().map(|(rule, ..)| rule).collect();
    assert_eq!(rules, ["c"]);
}
