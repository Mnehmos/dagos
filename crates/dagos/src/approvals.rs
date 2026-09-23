//! Tool-call approvals: the [`ToolGate`] between a run and the tools it asks for.
//!
//! Each tool's policy decides first: `allow` runs, `off` is refused, and `ask` waits for a person
//! to approve or deny the call in the app (or for the approval timeout, which denies it). Without
//! an app to ask in (the CLI), `ask` calls are denied with a note saying where to approve them.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use dagos_core::domain::{RunId, ToolDecider};
use dagos_core::tools::{ToolDecision, ToolGate, ToolRequest};
use dagos_mcp::{McpConfig, Policy};
use tokio::sync::oneshot;

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
    pending: Mutex<BTreeMap<(RunId, String), oneshot::Sender<Answer>>>,
    interactive: AtomicBool,
    timeout: Duration,
}

impl Approvals {
    /// Approvals that deny `ask` calls until [`Approvals::set_interactive`] enables asking.
    pub fn new(timeout: Duration) -> Self {
        Self {
            config: RwLock::new(McpConfig::default()),
            pending: Mutex::new(BTreeMap::new()),
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

#[async_trait]
impl ToolGate for Approvals {
    async fn decide(&self, run_id: &RunId, request: &ToolRequest) -> ToolDecision {
        match self.policy_of(&request.name) {
            Policy::Allow => return ToolDecision::Allow { by: ToolDecider::Policy },
            Policy::Off => {
                return ToolDecision::Deny {
                    by: ToolDecider::Policy,
                    reason: format!("`{}` is turned off", request.name),
                };
            }
            Policy::Ask => {}
        }
        if !self.interactive.load(Ordering::SeqCst) {
            return ToolDecision::Deny {
                by: ToolDecider::Policy,
                reason: format!(
                    "`{}` needs approval; approve it in the app (`dagos serve`) or set it to allow",
                    request.name
                ),
            };
        }
        let (sender, receiver) = oneshot::channel();
        let key = (run_id.clone(), request.call_id.clone());
        self.pending.lock().unwrap_or_else(PoisonError::into_inner).insert(key.clone(), sender);
        let answer = tokio::time::timeout(self.timeout, receiver).await;
        self.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&key);
        match answer {
            Ok(Ok(Answer::Allow)) => ToolDecision::Allow { by: ToolDecider::User },
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
        assert_eq!(decide("ooda.read_file").await, ToolDecision::Allow { by: ToolDecider::Policy });
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
        assert_eq!(waiting.await.unwrap(), ToolDecision::Allow { by: ToolDecider::User });
        assert!(!approvals.answer(&run, "call_1", Answer::Deny), "nothing is waiting any more");

        let timed_out = approvals.decide(&run, &request("ooda.exec_cli")).await;
        assert!(matches!(timed_out, ToolDecision::Deny { by: ToolDecider::Timeout, .. }));
        assert!(!approvals.is_pending(&run, "call_1"));
    }
}
