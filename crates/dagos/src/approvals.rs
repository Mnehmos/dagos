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

use dagos_core::domain::{RunId, ToolDecider};
use dagos_core::tools::{ToolDecision, ToolGate, ToolRequest};
use dagos_mcp::{Capabilities, McpConfig, Policy};
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
    /// Why each pending call needs a person.
    escalations: Mutex<BTreeMap<(RunId, String), Escalation>>,
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
            escalations: Mutex::new(BTreeMap::new()),
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

    /// Every tool the servers described, `off` ones included (a meta-tool may name them), with
    /// descriptions for the guard.
    pub fn set_tools(&self, capabilities: &Capabilities) {
        let mut tools = BTreeMap::new();
        for server in &capabilities.servers {
            for tool in &server.tools {
                tools.insert(format!("{}.{}", server.id, tool.name), tool.description.clone());
            }
        }
        for tool in &capabilities.tools {
            tools.insert(tool.name.clone(), tool.description.clone());
        }
        *self.tools.write().unwrap_or_else(PoisonError::into_inner) = tools;
    }

    /// The guard that asks Jev about each call's risks; `None` lets policies decide alone.
    pub fn set_guard(&self, guard: Option<Arc<Guard>>) {
        *self.guard.write().unwrap_or_else(PoisonError::into_inner) = guard;
    }

    /// Why the pending call `call_id` of `run_id` needs a person, beyond its policy.
    pub fn note(&self, run_id: &RunId, call_id: &str) -> Option<String> {
        self.escalation(run_id, call_id).and_then(|escalation| escalation.note)
    }

    /// Why the pending call `call_id` of `run_id` needs a person.
    pub fn escalation(&self, run_id: &RunId, call_id: &str) -> Option<Escalation> {
        let escalations = self.escalations.lock().unwrap_or_else(PoisonError::into_inner);
        escalations.get(&(run_id.clone(), call_id.to_owned())).cloned()
    }

    /// The strictest policy among `request`'s tool and the tools its arguments name, the named
    /// tool that is `off` (if any), and the named tools set to `ask`.
    fn effective_policy(&self, request: &ToolRequest) -> (Policy, Option<String>, Vec<String>) {
        let tools = self.tools.read().unwrap_or_else(PoisonError::into_inner);
        let mut policy = self.policy_of(&request.name);
        let (mut off, mut asking) = (None, Vec::new());
        for name in named_tools(request, tools.keys().map(String::as_str)) {
            let named = self.policy_of(&name);
            match named {
                Policy::Off if off.is_none() => off = Some(name),
                Policy::Ask => asking.push(name),
                _ => {}
            }
            if strictness(named) > strictness(policy) {
                policy = named;
            }
        }
        (policy, off, asking)
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

/// A call waiting for a person; forgets it when dropped.
struct Waiting<'a> {
    approvals: &'a Approvals,
    key: (RunId, String),
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        let approvals = self.approvals;
        approvals.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.key);
        approvals.escalations.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.key);
    }
}

/// Why a pending call needs a person.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Escalation {
    /// Why, beyond the call's own policy, if anything.
    pub note: Option<String>,
    /// Tools of the same server that the call's arguments name and that are set to ask:
    /// "always allow" allows these too.
    pub asking: Vec<String>,
    /// The guard flagged the call or could not check it. It will ask again next time whatever
    /// the policies, so "always allow" cannot help.
    pub guarded: bool,
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
        let (policy, off, asking) = self.effective_policy(request);
        if policy == Policy::Off {
            let reason = match off {
                Some(named) => {
                    format!("`{}` would run `{named}`, which is turned off", request.name)
                }
                None => format!("`{}` is turned off", request.name),
            };
            return ToolDecision::Deny { by: ToolDecider::Policy, reason };
        }
        let settings = self.config.read().unwrap_or_else(PoisonError::into_inner).guard;
        let guard = self.guard.read().unwrap_or_else(PoisonError::into_inner).clone();
        let assessment = match guard.filter(|_| settings.enabled) {
            Some(guard) => {
                let description = self
                    .tools
                    .read()
                    .unwrap_or_else(PoisonError::into_inner)
                    .get(&request.name)
                    .cloned()
                    .unwrap_or_default();
                guard.assess(run_id, request, &description, settings.threshold).await
            }
            None => Assessment::Unchecked,
        };
        let mut concerns: Vec<String> = Vec::new();
        if !asking.is_empty() {
            let names: Vec<String> = asking.iter().map(|name| format!("`{name}`")).collect();
            let verb = if asking.len() == 1 { "is" } else { "are" };
            concerns.push(format!("it would run {}, which {verb} set to ask", names.join(", ")));
        }
        let guarded = assessment.concern();
        let is_guarded = guarded.is_some();
        concerns.extend(guarded);
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
        let escalation = Escalation { note: note.clone(), asking, guarded: is_guarded };
        self.escalations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key.clone(), escalation);
        self.pending.lock().unwrap_or_else(PoisonError::into_inner).insert(key.clone(), sender);
        // Removed when this wait ends, however it ends: answered, timed out, or dropped because
        // the run was stopped.
        let _waiting = Waiting { approvals: self, key };
        let answer = tokio::time::timeout(self.timeout, receiver).await;
        let why = note.as_ref().map(|note| format!(" ({note})")).unwrap_or_default();
        match answer {
            Ok(Ok(Answer::Allow)) => ToolDecision::Allow { by: ToolDecider::User, note },
            Ok(Ok(Answer::Deny)) => ToolDecision::Deny {
                by: ToolDecider::User,
                reason: format!("the user denied it{why}"),
            },
            Ok(Err(_)) | Err(_) => ToolDecision::Deny {
                by: ToolDecider::Timeout,
                reason: format!("nobody approved it within {:?}{why}", self.timeout),
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
        approvals.set_config(McpConfig { servers: vec![server], ..Default::default() });
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
        approvals.set_config(McpConfig { servers: vec![server], ..Default::default() });
        // As in production: the server describes every tool, but `off` ones are not offered.
        let described = ["batch_tools", "exec_cli", "mouse_click", "read_file"];
        let status = |name: &str| dagos_mcp::ToolStatus {
            name: name.into(),
            description: format!("The {name} tool."),
            policy: if name == "mouse_click" { Policy::Off } else { Policy::Allow },
        };
        let offered = |name: &str| dagos_core::domain::IrTool {
            name: format!("ooda.{name}"),
            description: format!("The {name} tool."),
            input_schema: Default::default(),
        };
        approvals.set_tools(&Capabilities {
            tools: described.iter().filter(|n| **n != "mouse_click").map(|n| offered(n)).collect(),
            servers: vec![dagos_mcp::ServerStatus {
                id: "ooda".into(),
                enabled: true,
                tools: described.iter().map(|n| status(n)).collect(),
                error: None,
            }],
        });
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

    #[tokio::test]
    async fn a_dropped_wait_forgets_the_pending_call() {
        let approvals = approvals(Duration::from_secs(60));
        approvals.set_interactive(true);
        let run = RunId::parse("run_1").unwrap();
        let waiting = {
            let (approvals, run) = (approvals.clone(), run.clone());
            tokio::spawn(async move { approvals.decide(&run, &request("ooda.exec_cli")).await })
        };
        while !approvals.is_pending(&run, "call_1") {
            tokio::task::yield_now().await;
        }
        // Stopping the run drops its wait for a person.
        waiting.abort();
        let _ = waiting.await;
        assert!(!approvals.is_pending(&run, "call_1"), "nothing waits for an answer any more");
        assert!(approvals.escalation(&run, "call_1").is_none());
    }
}
