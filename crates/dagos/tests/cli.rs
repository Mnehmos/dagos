//! The `dagos` binary end to end: offline runs, streamed prose, explicit failures, persistent
//! run configuration, and crash recovery.

use std::path::Path;
use std::process::{Command, Output};

use dagos_core::domain::{ModelId, ProviderId, RunConfig};
use dagos_core::store::Store;
use serde_json::Value;

fn dagos(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_dagos"))
        .arg("--dir")
        .arg(dir)
        .args(args)
        .env_remove("OPENROUTER_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("DAGOS_INFERENCE_TIMEOUT")
        .output()
        .expect("dagos runs")
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!("stdout is not JSON ({error}): {}\nstderr: {}", stdout(output), stderr(output))
    })
}

fn workspace() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join(".dagos");
    let init = dagos(&dir, &["init", "--name", "demo"]);
    assert!(init.status.success(), "{}", stderr(&init));
    assert!(stdout(&init).contains("Initialized DAGOS project `demo`"), "{}", stdout(&init));
    (root, dir)
}

#[test]
fn a_fake_run_streams_prose_and_records_its_emissions_offline() {
    let (_root, dir) = workspace();
    let run = dagos(&dir, &["run", "Add restart tests"]);
    assert_eq!(run.status.code(), Some(0), "{}", stderr(&run));
    assert!(stdout(&run).contains("fake-echo received: \"Add restart tests\""), "{}", stdout(&run));
    assert!(stderr(&run).contains("fake / fake-echo"), "{}", stderr(&run));
    assert!(stderr(&run).contains("completed · 1 node(s), 1 edge(s) recorded"), "{}", stderr(&run));

    let again = dagos(&dir, &["init"]);
    assert!(stdout(&again).contains("Already initialized"), "init is idempotent");
}

#[test]
fn failed_runs_are_explicit_and_exit_non_zero() {
    let (_root, dir) = workspace();
    let run = dagos(&dir, &["run", "x", "--model", "fake-invalid-schema", "--json"]);
    assert_eq!(run.status.code(), Some(1));
    let report = json(&run);
    assert_eq!(report["run"]["status"], "failed");
    assert_eq!(report["run"]["error_code"], "response_invalid");
    assert_eq!(report["emitted"]["nodes"], 0);

    let timeout = dagos(&dir, &["--inference-timeout", "1", "run", "x", "--model", "fake-timeout"]);
    assert_eq!(timeout.status.code(), Some(1));
    assert!(stderr(&timeout).contains("failed [provider_timeout]"), "{}", stderr(&timeout));
}

#[test]
fn run_configuration_persists_and_drives_subsequent_runs() {
    let (_root, dir) = workspace();
    let changed = dagos(&dir, &["config", "--model", "fake-error", "--system-prompt", "Terse."]);
    assert!(changed.status.success(), "{}", stderr(&changed));
    let config = json(&changed);
    assert_eq!(config["run_defaults"]["model_id"], "fake-error");
    assert_eq!(config["providers"][0]["id"], "fake");

    let run = json(&dagos(&dir, &["run", "status?", "--json"]));
    assert_eq!(run["run"]["error_code"], "provider_failed");
    assert_eq!(run["run"]["system_prompt"], "Terse.");

    // A per-run override does not change the defaults.
    let overridden = json(&dagos(&dir, &["run", "status?", "--model", "fake-echo", "--json"]));
    assert_eq!(overridden["run"]["status"], "completed");
    assert_eq!(json(&dagos(&dir, &["config"]))["run_defaults"]["model_id"], "fake-error");

    let unknown = dagos(&dir, &["config", "--provider", "openrouter"]);
    assert_eq!(unknown.status.code(), Some(2));
    assert!(stderr(&unknown).contains("provider `openrouter` is not registered"));
}

#[test]
fn commands_explain_how_to_start_without_a_workspace() {
    let root = tempfile::tempdir().unwrap();
    let output = dagos(&root.path().join(".dagos"), &["run", "hello"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(stderr(&output).contains("run `dagos init` first"), "{}", stderr(&output));
}

#[test]
fn recover_fails_runs_left_running_by_a_crash() {
    let (_root, dir) = workspace();
    {
        let store = Store::open(dir.join("dagos.sqlite3")).unwrap();
        let project = store.transaction(|tx| tx.projects()).unwrap().remove(0);
        let config = RunConfig {
            provider_id: ProviderId::parse("fake").unwrap(),
            model_id: ModelId::parse("fake-echo").unwrap(),
            system_prompt: String::new(),
        };
        store.transaction(|tx| tx.create_run(&project.id, &config)).unwrap();
    }
    let blocked = dagos(&dir, &["run", "hello"]);
    assert_eq!(blocked.status.code(), Some(2));
    assert!(stderr(&blocked).contains("still running"), "{}", stderr(&blocked));

    let recover = dagos(&dir, &["recover"]);
    assert!(stdout(&recover).contains("Marked 1 interrupted run(s) as failed."));
    assert_eq!(dagos(&dir, &["run", "hello"]).status.code(), Some(0));
}

#[test]
fn inspect_prints_recorded_state_as_json() {
    let (_root, dir) = workspace();
    assert!(dagos(&dir, &["run", "Add restart tests"]).status.success());

    let overview = json(&dagos(&dir, &["inspect"]));
    assert_eq!(overview["dag"]["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(overview["runs"][0]["message"], "Add restart tests");

    let run = json(&dagos(&dir, &["inspect", "run", "latest"]));
    assert_eq!(run["run"]["status"], "completed");
    let ir = json(&dagos(&dir, &["inspect", "ir"]));
    assert_eq!(ir["schema"], "kiss.inference-ir.v1");
    assert_eq!(ir["task"]["message"], "Add restart tests");
    let response = json(&dagos(&dir, &["inspect", "response"]));
    assert_eq!(response["schema"], "kiss.inference-response.v1");
    let events = json(&dagos(&dir, &["inspect", "events"]));
    assert_eq!(events[0]["type"], "run.started");
    assert!(json(&dagos(&dir, &["inspect", "dag"]))["edges"].is_array());

    let missing = dagos(&dir, &["inspect", "run", "run_nope"]);
    assert_eq!(missing.status.code(), Some(2));
    assert!(stderr(&missing).contains("run `run_nope` not found"));
}

#[test]
fn runs_succeed_when_configured_mcp_servers_are_unavailable() {
    let (_root, dir) = workspace();
    let config = r#"{"servers": [{"id": "offline", "command": "dagos-no-such-mcp-server"}]}"#;
    std::fs::write(dir.join("mcp.json"), config).unwrap();

    let run = dagos(&dir, &["run", "hello"]);
    assert_eq!(run.status.code(), Some(0), "{}", stderr(&run));
    assert!(stderr(&run).contains("MCP server `offline` unavailable"), "{}", stderr(&run));
    let ir = json(&dagos(&dir, &["inspect", "ir"]));
    assert!(ir.get("tools").is_none(), "no tools reach the IR when MCP is unavailable");

    std::fs::write(dir.join("mcp.json"), "not json").unwrap();
    let broken = dagos(&dir, &["run", "hello"]);
    assert_eq!(broken.status.code(), Some(2));
    assert!(stderr(&broken).contains("invalid"), "{}", stderr(&broken));
}
