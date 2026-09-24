//! Tool-call approvals: the [`ToolGate`] between a run and the tools it asks for.
//!
//! Each tool's policy decides first: `allow` runs, `off` is refused, and `ask` waits for a person
//! to approve or deny the call in the app (or for the approval timeout, which denies it). Without
//! an app to ask in (the CLI), `ask` calls are denied with a note saying where to approve them.
//!
//! Two checks can make a call stricter than its own policy, never looser:
//! - A call that names other tools of its server in its arguments (a meta-tool such as
//!   `batch_tools`) gets the strictest policy among itself and the tools it names.
//! - The [`Guard`] asks Jev about the call's risks; a likely risk, or a check that could not run,
//!   turns `allow` into a question for the person, with the reason shown.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use std::sync::Arc;

use dagos_core::domain::{IrTool, RunId, ToolDecider};
use dagos_core::tools::{ToolDecision, ToolGate, ToolRequest};
use dagos_mcp::{McpConfig, Policy};
use tokio::sync::oneshot;

use crate::guard::{Assessment, Guard, named_tools};

/// How long a call waits for a person before it is denied.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// A person's answer to a pending call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Allow,
    Deny,
}

pub struct Approvals {
    config: RwLock<McpConfig>,
    /// Every offered tool's name and description.
    tools: RwLock<BTreeMap<String, String>>,
    guard: RwLock<Option<Arc<Guard>>>,
    pending: Mutex<BTreeMap<(RunId, String), oneshot::Sender<Answer>>>,
    /// Why each pending call needs a person, when it is more than its policy.
    notes: Mutex<BTreeMap<(RunId, String), String>>,
    interactive: AtomicBool,
    timeout: Duration,
}

impl Approvals {
    /// Approvals that deny `ask` calls until [`Approvals::set_interactive`] enables asking.
    pub fn new(timeout: Duration) -> Self {
        Self {
            config: RwLock::new(McpConfig::default()),
            tools: RwLock::new(BTreeMap::new()),
            guard: RwLock::new(None),
            pending: Mutex::new(BTreeMap::new()),
            notes: Mutex::new(BTreeMap::new()),
            interactive: AtomicBool::new(false),
            timeout,
        }
    }

    /// Whether `ask` calls wait for a person (the app is serving) or are denied (the CLI).
    pub fn set_interactive(&self, interactive: bool) {
        self.interactive.store(interactive, Ordering::SeqCst);
    }

    /// The tool policies to decide by.
    pub fn set_config(&self, config: McpConfig) {
        *self.config.write().unwrap_or_else(PoisonError::into_inner) = config;
    }

    /// The tools on offer, for meta-tool checks and the guard's descriptions.
    pub fn set_tools(&self, tools: &[IrTool]) {
        let tools =
            tools.iter().map(|tool| (tool.name.clone(), tool.description.clone())).collect();
        *self.tools.write().unwrap_or_else(PoisonError::into_inner) = tools;
    }

    /// The guard that asks Jev about each call's risks; `None` lets policies decide alone.
    pub fn set_guard(&self, guard: Option<Arc<Guard>>) {
        *self.guard.write().unwrap_or_else(PoisonError::into_inner) = guard;
    }

    /// Why the pending call `call_id` of `run_id` needs a person, beyond its policy.
    pub fn note(&self, run_id: &RunId, call_id: &str) -> Option<String> {
        let notes = self.notes.lock().unwrap_or_else(PoisonError::into_inner);
        notes.get(&(run_id.clone(), call_id.to_owned())).cloned()
    }

    /// The strictest policy among `request`'s tool and the tools its arguments name, and which
    /// named tool made it stricter.
    fn effective_policy(&self, request: &ToolRequest) -> (Policy, Option<String>) {
        let own = self.policy_of(&request.name);
        let tools = self.tools.read().unwrap_or_else(PoisonError::into_inner);
        let mut strictest = (own, None);
        for name in named_tools(request, tools.keys().map(String::as_str)) {
            let policy = self.policy_of(&name);
            if strictness(policy) > strictness(strictest.0) {
                strictest = (policy, Some(name));
            }
        }
        strictest
    }

    /// Answers the pending call `call_id` of `run_id`; false if nothing is waiting for it.
    pub fn answer(&self, run_id: &RunId, call_id: &str, answer: Answer) -> bool {
        let sender = self
            .pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(run_id.clone(), call_id.to_owned()));
        sender.is_some_and(|sender| sender.send(answer).is_ok())
    }

    /// Whether the call `call_id` of `run_id` is waiting for a person.
    pub fn is_pending(&self, run_id: &RunId, call_id: &str) -> bool {
        let pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        pending.contains_key(&(run_id.clone(), call_id.to_owned()))
    }

    fn policy_of(&self, name: &str) -> Policy {
        self.config.read().unwrap_or_else(PoisonError::into_inner).policy_of(name)
    }
}

fn strictness(policy: Policy) -> u8 {
    match policy {
        Policy::Allow => 0,
        Policy::Ask => 1,
        Policy::Off => 2,
    }
}

#[async_trait]
impl ToolGate for Approvals {
    async fn decide(&self, run_id: &RunId, request: &ToolRequest) -> ToolDecision {
        let (policy, via) = self.effective_policy(request);
        if policy == Policy::Off {
            let reason = match via {
                Some(named) => {
                    format!("`{}` would run `{named}`, which is turned off", request.name)
                }
                None => format!("`{}` is turned off", request.name),
            };
            return ToolDecision::Deny { by: ToolDecider::Policy, reason };
        }
        let guard = self.guard.read().unwrap_or_else(PoisonError::into_inner).clone();
        let assessment = match guard {
            Some(guard) => {
                let description = self
                    .tools
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&request.name)
                    .cloned()
                    .unwrap_or_default();
                guard.assess(run_id, request, &description).await
            }
            None => Assessment::Unchecked,
        };
        let mut concerns: Vec<String> = Vec::new();
        if let Some(named) = &via {
            concerns.push(format!("it would run `{named}`, which is set to ask"));
        }
        concerns.extend(assessment.concern());
        if policy == Policy::Allow && concerns.is_empty() {
            return ToolDecision::Allow { by: ToolDecider::Policy, note: None };
        }
        let note = (!concerns.is_empty()).then(|| concerns.join("; "));
        if !self.interactive.load(Ordering::SeqCst) {
            let why = note.as_ref().map(|note| format!(" ({note})")).unwrap_or_default();
            return ToolDecision::Deny {
                by: ToolDecider::Policy,
                reason: format!(
                    "`{}` needs approval{why}; approve it in the app (`dagos serve`){}",
                    request.name,
                    if policy == Policy::Ask { " or set it to allow" } else { "" }
                ),
            };
        }
        let (sender, receiver) = oneshot::channel();
        let key = (run_id.clone(), request.call_id.clone());
        if let Some(note) = &note {
            self.notes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(key.clone(), note.clone());
        }
        self.pending.lock().unwrap_or_else(PoisonError::into_inner).insert(key.clone(), sender);
        let answer = tokio::time::timeout(self.timeout, receiver).await;
        self.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        self.notes.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        match answer {
            Ok(Ok(Answer::Allow)) => ToolDecision::Allow { by: ToolDecider::User, note },
            Ok(Ok(Answer::Deny)) => {
                ToolDecision::Deny { by: ToolDecider::User, reason: "the user denied it".into() }
            }
            Ok(Err(_)) | Err(_) => ToolDecision::Deny {
                by: ToolDecider::Timeout,
                reason: format!("nobody approved it within {:?}", self.timeout),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use dagos_mcp::McpServer;
    use serde_json::json;

    use super::*;

    fn request(name: &str) -> ToolRequest {
        ToolRequest { call_id: "call_1".into(), name: name.into(), arguments: Default::default() }
    }

    fn approvals(timeout: Duration) -> Arc<Approvals> {
        let approvals = Arc::new(Approvals::new(timeout));
        let mut server = McpServer::new("ooda", "node", vec![]);
        server.tools.insert("read_file".into(), Policy::Allow);
        server.tools.insert("mouse_click".into(), Policy::Off);
        approvals.set_config(McpConfig { servers: vec![server] });
        approvals
    }

    #[tokio::test]
    async fn policies_decide_before_anyone_is_asked() {
        let approvals = approvals(Duration::from_secs(5));
        let run = RunId::parse("run_1").unwrap();
        let decide = |name: &'static str| {
            let approvals = approvals.clone();
            let run = run.clone();
            async move { approvals.decide(&run, &request(name)).await }
        };
        assert_eq!(
            decide("ooda.read_file").await,
            ToolDecision::Allow { by: ToolDecider::Policy, note: None }
        );
        assert!(matches!(
            decide("ooda.mouse_click").await,
            ToolDecision::Deny { by: ToolDecider::Policy, .. }
        ));
        match decide("ooda.exec_cli").await {
            ToolDecision::Deny { by: ToolDecider::Policy, reason } => {
                assert!(reason.contains("dagos serve"))
            }
            other => panic!("the CLI cannot ask: {other:?}"),
        }
    }

    #[tokio::test]
    async fn ask_waits_for_a_person_or_times_out() {
        let approvals = approvals(Duration::from_millis(200));
        approvals.set_interactive(true);
        let run = RunId::parse("run_1").unwrap();

        let waiting = {
            let (approvals, run) = (approvals.clone(), run.clone());
            tokio::spawn(async move { approvals.decide(&run, &request("ooda.exec_cli")).await })
        };
        while !approvals.is_pending(&run, "call_1") {
            tokio::task::yield_now().await;
        }
        assert!(approvals.answer(&run, "call_1", Answer::Allow));
        assert_eq!(
            waiting.await.unwrap(),
            ToolDecision::Allow { by: ToolDecider::User, note: None }
        );
        assert!(!approvals.answer(&run, "call_1", Answer::Deny), "nothing is waiting any more");

        let timed_out = approvals.decide(&run, &request("ooda.exec_cli")).await;
        assert!(matches!(timed_out, ToolDecision::Deny { by: ToolDecider::Timeout, .. }));
        assert!(!approvals.is_pending(&run, "call_1"));
    }

    /// Flags `destroys-data` for any call whose arguments mention `rm -rf`; or fails; or cannot
    /// judge at all.
    enum Judge {
        Flags,
        Fails,
        Cannot,
    }

    #[async_trait]
    impl dagos_core::context::JevClassifier for Judge {
        fn id(&self) -> &str {
            "judge"
        }

        async fn classify(
            &self,
            _request: &dagos_core::domain::JevRequest,
        ) -> Result<String, dagos_core::context::JevError> {
            Err(dagos_core::context::JevError("unused".into()))
        }

        async fn decide(
            &self,
            state: &serde_json::Value,
            questions: &[dagos_core::context::NoulQuestion],
        ) -> Result<Option<Vec<f64>>, dagos_core::context::JevError> {
            match self {
                Judge::Cannot => Ok(None),
                Judge::Fails => Err(dagos_core::context::JevError("offline".into())),
                Judge::Flags => {
                    let risky = state["arguments"].as_str().unwrap().contains("rm -rf");
                    Ok(Some(
                        questions
                            .iter()
                            .map(|q| if risky && q.key == "destroys-data" { 0.93 } else { 0.02 })
                            .collect(),
                    ))
                }
            }
        }
    }

    fn call(name: &str, arguments: serde_json::Value) -> ToolRequest {
        ToolRequest {
            call_id: "call_1".into(),
            name: name.into(),
            arguments: arguments.as_object().unwrap().clone(),
        }
    }

    /// Approvals where every `ooda` tool is `allow` except `mouse_click` (off) and `exec_cli`
    /// (ask), with `judge` as the guard's Jev.
    fn guarded(judge: Judge) -> Arc<Approvals> {
        let approvals = Arc::new(Approvals::new(Duration::from_secs(5)));
        let mut server = McpServer::new("ooda", "node", vec![]);
        server.policy = Policy::Allow;
        server.tools.insert("mouse_click".into(), Policy::Off);
        server.tools.insert("exec_cli".into(), Policy::Ask);
        approvals.set_config(McpConfig { servers: vec![server] });
        let tool = |name: &str| IrTool {
            name: format!("ooda.{name}"),
            description: format!("The {name} tool."),
            input_schema: Default::default(),
        };
        approvals.set_tools(&[
            tool("batch_tools"),
            tool("exec_cli"),
            tool("mouse_click"),
            tool("read_file"),
        ]);
        let store = Arc::new(dagos_core::store::Store::open_in_memory().unwrap());
        let guard = Guard::new(Arc::new(judge), store, std::path::PathBuf::from("/project"));
        approvals.set_guard(Some(Arc::new(guard)));
        approvals
    }

    #[tokio::test]
    async fn meta_tools_get_the_strictest_policy_of_the_tools_they_name() {
        let approvals = guarded(Judge::Flags);
        let run = RunId::parse("run_1").unwrap();
        let off = call("ooda.batch_tools", json!({"operations": [{"tool": "mouse_click"}]}));
        match approvals.decide(&run, &off).await {
            ToolDecision::Deny { by: ToolDecider::Policy, reason } => {
                assert!(
                    reason.contains("would run `ooda.mouse_click`, which is turned off"),
                    "{reason}"
                )
            }
            other => panic!("{other:?}"),
        }
        // Naming an `ask` tool makes the allowed meta-tool ask; the CLI cannot, so it is denied.
        let ask = call(
            "ooda.batch_tools",
            json!({"operations": [{"tool": "exec_cli", "args": {"command": "ls"}}]}),
        );
        match approvals.decide(&run, &ask).await {
            ToolDecision::Deny { reason, .. } => {
                assert!(reason.contains("which is set to ask"), "{reason}")
            }
            other => panic!("{other:?}"),
        }
        let fine = call("ooda.batch_tools", json!({"operations": [{"tool": "read_file"}]}));
        assert_eq!(
            approvals.decide(&run, &fine).await,
            ToolDecision::Allow { by: ToolDecider::Policy, note: None }
        );
    }

    #[tokio::test]
    async fn the_guard_turns_risky_allowed_calls_into_questions() {
        let approvals = guarded(Judge::Flags);
        approvals.set_interactive(true);
        let run = RunId::parse("run_1").unwrap();
        let risky = call(
            "ooda.batch_tools",
            json!({"operations": [{"tool": "read_file", "args": {"path": "rm -rf /"}}]}),
        );
        let waiting = {
            let (approvals, run) = (approvals.clone(), run.clone());
            tokio::spawn(async move { approvals.decide(&run, &risky).await })
        };
        while !approvals.is_pending(&run, "call_1") {
            tokio::task::yield_now().await;
        }
        let note = approvals.note(&run, "call_1").expect("the person sees why");
        assert_eq!(note, "Jev flagged it: destroys-data 0.93");
        assert!(approvals.answer(&run, "call_1", Answer::Allow));
        assert_eq!(
            waiting.await.unwrap(),
            ToolDecision::Allow { by: ToolDecider::User, note: Some(note) },
            "the flag is recorded with the person's decision"
        );
        assert!(approvals.note(&run, "call_1").is_none());

        let safe = call("ooda.read_file", json!({"path": "README.md"}));
        assert_eq!(
            approvals.decide(&run, &safe).await,
            ToolDecision::Allow { by: ToolDecider::Policy, note: None }
        );
    }

    #[tokio::test]
    async fn a_failed_check_asks_and_a_jev_that_cannot_judge_leaves_policies_alone() {
        let run = RunId::parse("run_1").unwrap();
        let read = call("ooda.read_file", json!({"path": "README.md"}));
        match guarded(Judge::Fails).decide(&run, &read).await {
            ToolDecision::Deny { reason, .. } => {
                assert!(reason.contains("the safety check could not run"), "{reason}")
            }
            other => panic!("fails closed: {other:?}"),
        }
        assert_eq!(
            guarded(Judge::Cannot).decide(&run, &read).await,
            ToolDecision::Allow { by: ToolDecider::Policy, note: None }
        );
    }
}
