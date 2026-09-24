//! The lint reviewer against real files: it remembers what a tool call names before the call
//! runs, and judges only the functions that changed, once each.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dagos_core::context::{JevClassifier, JevError, NoulQuestion};
use dagos_core::domain::{JevRequest, RunId};
use dagos_core::tools::{Reviewer, ToolRequest};
use dagos_lint::{LintConfig, LintReviewer, Rule};
use serde_json::{Value, json};

/// Says "swallows errors" for any function whose source contains `.ok();`; records each state.
#[derive(Default)]
struct RuleJudge {
    asked: Mutex<Vec<String>>,
}

#[async_trait]
impl JevClassifier for RuleJudge {
    fn id(&self) -> &str {
        "rule-judge"
    }

    async fn classify(&self, _request: &JevRequest) -> Result<String, JevError> {
        Err(JevError("not used".into()))
    }

    async fn decide(
        &self,
        state: &Value,
        questions: &[NoulQuestion],
    ) -> Result<Option<Vec<f64>>, JevError> {
        self.asked.lock().unwrap().push(state["function"].as_str().unwrap().to_owned());
        let swallows = state["source"].as_str().unwrap().contains(".ok();");
        Ok(Some(
            questions
                .iter()
                .map(|q| if q.key == "swallows-errors" && swallows { 0.95 } else { 0.05 })
                .collect(),
        ))
    }
}

fn config() -> LintConfig {
    LintConfig {
        rules: vec![
            Rule {
                id: "swallows-errors".into(),
                text: "Swallows errors.".into(),
                applies: None,
                except: None,
            },
            Rule {
                id: "too-long".into(),
                text: "Is too long.".into(),
                applies: None,
                except: None,
            },
        ],
        ..LintConfig::default()
    }
}

fn call(arguments: Value) -> ToolRequest {
    ToolRequest {
        call_id: "call_1".into(),
        name: "ooda.exec_cli".into(),
        arguments: arguments.as_object().unwrap().clone(),
    }
}

#[tokio::test]
async fn only_functions_the_run_changed_are_judged_and_findings_name_them() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("src")).unwrap();
    let lib = dir.path().join("src").join("lib.rs");
    std::fs::write(&lib, "fn keep() -> u8 {\n    1\n}\n\nfn load() {\n    read();\n}\n").unwrap();
    let judge = Arc::new(RuleJudge::default());
    let reviewer = LintReviewer::new(dir.path(), judge.clone(), config());
    let run = RunId::parse("run_000001").unwrap();

    // Nothing touched yet: nothing to review.
    assert_eq!(reviewer.review(&run).await.unwrap().judged, 0);

    // A shell command names the file; the reviewer remembers it before the command runs.
    reviewer.before_call(&run, &call(json!({"command": "sed -i 's/x/y/' src/lib.rs"}))).await;
    std::fs::write(
        &lib,
        "fn keep() -> u8 {\n    1\n}\n\nfn load() {\n    read().ok();\n}\n\nfn added() {}\n",
    )
    .unwrap();

    let review = reviewer.review(&run).await.unwrap();
    assert_eq!(review.judged, 2, "load changed and added is new; keep is untouched");
    assert_eq!(*judge.asked.lock().unwrap(), ["load", "added"]);
    assert_eq!(review.findings.len(), 1);
    let finding = &review.findings[0];
    assert_eq!(
        (finding.rule.as_str(), finding.file.as_str(), finding.function.as_str(), finding.line),
        ("swallows-errors", "src/lib.rs", "load", 5)
    );
    assert_eq!(finding.probability, 0.95);

    // The same code again: cached answers, the same fingerprint.
    let again = reviewer.review(&run).await.unwrap();
    assert_eq!(again.fingerprint, review.fingerprint);
    assert_eq!(judge.asked.lock().unwrap().len(), 2, "unchanged functions are not asked again");

    // Fixed: judged again, clean, and a new fingerprint.
    std::fs::write(&lib, "fn keep() -> u8 {\n    1\n}\n\nfn load() {\n    read()?;\n}\n").unwrap();
    let fixed = reviewer.review(&run).await.unwrap();
    assert!(fixed.findings.is_empty());
    assert_ne!(fixed.fingerprint, review.fingerprint);

    reviewer.end(&run);
    assert_eq!(reviewer.review(&run).await.unwrap().judged, 0, "an ended run is forgotten");
}

#[tokio::test]
async fn new_files_count_and_files_outside_the_project_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let reviewer = LintReviewer::new(dir.path(), Arc::new(RuleJudge::default()), config());
    let run = RunId::parse("run_000002").unwrap();
    let absolute = dir.path().join("new.py").to_string_lossy().replace('\\', "/");
    let foreign = outside.path().join("x.py").to_string_lossy().replace('\\', "/");
    let arguments = json!({"path": absolute, "also": [foreign, "notes.md"]});
    let named = reviewer.named_files(arguments.as_object().unwrap());
    assert_eq!(named.len(), 1, "{named:?}");

    reviewer.before_call(&run, &call(arguments)).await;
    std::fs::write(dir.path().join("new.py"), "def fresh():\n    return 1\n").unwrap();
    std::fs::write(outside.path().join("x.py"), "def other():\n    pass\n").unwrap();
    let review = reviewer.review(&run).await.unwrap();
    assert_eq!(review.judged, 1);
}

#[tokio::test]
async fn dismissed_findings_are_not_handed_back() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib.rs");
    std::fs::write(&lib, "fn load() {\n    read();\n}\n").unwrap();
    let mut config = config();
    config.dismissed.push(dagos_lint::Dismissal {
        rule: "swallows-errors".into(),
        file: "lib.rs".into(),
        function: "load".into(),
    });
    let reviewer = LintReviewer::new(dir.path(), Arc::new(RuleJudge::default()), config);
    let run = RunId::parse("run_000003").unwrap();
    reviewer.before_call(&run, &call(json!({"path": "lib.rs"}))).await;
    std::fs::write(&lib, "fn load() {\n    read().ok();\n}\n").unwrap();
    let review = reviewer.review(&run).await.unwrap();
    assert_eq!(review.judged, 1);
    assert!(review.findings.is_empty(), "a person said it is not a problem here");
}

#[tokio::test]
async fn files_a_command_changed_without_naming_them_are_reviewed_too() {
    let dir = tempfile::tempdir().unwrap();
    let judge = Arc::new(RuleJudge::default());
    let reviewer = LintReviewer::new(dir.path(), judge.clone(), config());
    let run = RunId::parse("run_000004").unwrap();
    // A command that names no file (e.g. a generator script) still runs through a call.
    reviewer.before_call(&run, &call(json!({"command": "python generate.py"}))).await;
    std::fs::create_dir(dir.path().join("target")).unwrap();
    std::fs::write(dir.path().join("target").join("built.rs"), "fn skip() {}\n").unwrap();
    std::fs::write(dir.path().join("made.rs"), "fn made() {\n    load().ok();\n}\n").unwrap();
    let review = reviewer.review(&run).await.unwrap();
    assert_eq!(review.judged, 1, "made.rs counts; build folders are skipped");
    assert_eq!(review.findings[0].function, "made");
}

#[tokio::test]
async fn judged_answers_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("lint-cache.json");
    let lib = dir.path().join("lib.rs");
    let run = RunId::parse("run_000005").unwrap();
    for (attempt, expected_questions) in [(0, 1), (1, 1)] {
        std::fs::write(&lib, "fn old() {}\n").unwrap();
        let judge = Arc::new(RuleJudge::default());
        let reviewer =
            LintReviewer::new(dir.path(), judge.clone(), config()).with_cache_file(cache.clone());
        reviewer.before_call(&run, &call(json!({"path": "lib.rs"}))).await;
        std::fs::write(&lib, "fn changed() {\n    read().ok();\n}\n").unwrap();
        reviewer.review(&run).await.unwrap();
        let asked = judge.asked.lock().unwrap().len();
        if attempt == 0 {
            assert_eq!(asked, expected_questions, "first time: judged");
        } else {
            assert_eq!(asked, 0, "after a restart: answered from the saved cache");
        }
    }
    assert!(cache.exists());
}
