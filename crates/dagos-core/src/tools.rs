//! Tool layer: the interfaces through which a run executes the tool calls a validated response
//! asks for.
//!
//! The core never talks to tools directly. A [`ToolGate`] decides whether each call may run (a
//! person's policy or approval), and a [`ToolExecutor`] runs permitted calls (for example on MCP
//! servers). Both are supplied from outside; without them, requested calls are recorded and
//! denied as unavailable. Tool output reaches the model only through the next IR and never
//! becomes DAG state by itself.

use async_trait::async_trait;

use crate::domain::{IrFinding, Payload, RunId, ToolDecider};

/// A tool call a validated response asked for.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolRequest {
    /// Unique within the run: `call_1`, `call_2`, …
    pub call_id: String,
    /// The tool's name as listed in the IR, e.g. `ooda.read_file`.
    pub name: String,
    pub arguments: Payload,
}

/// Whether a call may run, and who decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolDecision {
    Allow { by: ToolDecider },
    Deny { by: ToolDecider, reason: String },
}

/// What a tool returned. `is_error` means the tool reported a failure.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    pub output: serde_json::Value,
    pub is_error: bool,
}

/// Decides whether a requested call may run. May wait, e.g. for a person to approve.
#[async_trait]
pub trait ToolGate: Send + Sync {
    async fn decide(&self, run_id: &RunId, request: &ToolRequest) -> ToolDecision;
}

/// Runs permitted tool calls.
#[async_trait]
pub trait ToolExecutor: Send + Sync {
    /// Runs `request`. `Err` means the tool could not be reached or run at all.
    async fn call(&self, request: &ToolRequest) -> Result<ToolOutput, String>;
}

/// What a review of a run's changed code found.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Review {
    pub findings: Vec<IrFinding>,
    /// How many functions were judged; 0 means the run changed no code the reviewer checks.
    pub judged: usize,
    /// Identifies the reviewed code; an unchanged fingerprint means nothing changed since the
    /// previous review.
    pub fingerprint: String,
}

/// Reviews the code a run changes, e.g. a semantic linter.
///
/// A model reaches files only through tool calls, and the runtime shows the reviewer every
/// permitted call before it runs, so the reviewer can remember what the files it names looked
/// like. The runtime asks for a review each time the model replies without tool calls, and hands
/// findings back to the model until a review finds nothing, the code stops changing, or the run
/// reaches its review limit.
#[async_trait]
pub trait Reviewer: Send + Sync {
    /// A permitted call is about to run in `run_id` and may change what it names.
    async fn before_call(&self, run_id: &RunId, request: &ToolRequest);

    /// Reviews what the run's calls changed.
    async fn review(&self, run_id: &RunId) -> Result<Review, String>;

    /// The run finished; forget it.
    fn end(&self, run_id: &RunId);
}

/// A gate that allows every call, by policy. For tests and fully trusted setups.
pub struct AllowAll;

#[async_trait]
impl ToolGate for AllowAll {
    async fn decide(&self, _run_id: &RunId, _request: &ToolRequest) -> ToolDecision {
        ToolDecision::Allow { by: ToolDecider::Policy }
    }
}
