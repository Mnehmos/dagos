//! Runtime layer: the run lifecycle.
//!
//! A run records the user message, asks Jev to classify context, projects active context,
//! compiles IR, invokes the selected provider, validates the structured response, applies valid
//! emissions to the DAG, and records every transition as an ordered event.
//!
//! Pipeline failures are run state, not Rust errors: [`Runtime::run`] returns the finished run
//! whether it completed or failed, and a failed run carries an error code plus the evidence that
//! explains it. Each stage commits its own events, so everything recorded before a failure stays
//! inspectable; a response's emissions, its `response.validated` event, and `run.completed` are
//! committed together or not at all.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::context::recall::{
    COMPACT_MIN_CHARS, RECALL_CHUNK_CHARS, RECALL_THRESHOLD, RecallChunk, cut, output_text,
    recall_candidates, select_recalled,
};
use crate::context::{
    JevClassifier, apply_classification, carry_context, classification_request,
    validate_classification,
};
use crate::domain::{Classification, JevToolCandidate};
use crate::domain::{
    ContextClassification, ConversationId, ConversationTurn, Emission, EmissionRef, Endpoint,
    ErrorCode, EventData, InferenceIr, IrFinding, IrRecalledTurn, IrReview, IrTool, JevRequest,
    NodeId, NodeType, Payload, ProjectId, ProviderId, Run, RunConfig, RunId, ToolDecider,
};
use crate::ir::{CompileError, IrHistory, RECENT_RUN_OUTCOMES, compile_with};
use crate::provider::{DeltaSink, InferenceProvider, InferenceRequest};
use crate::response::{ValidatedResponse, validate_response};
use crate::store::{Store, StoreError, Tx};
use crate::tools::{Reviewer, ToolDecision, ToolExecutor, ToolGate, ToolOutput, ToolRequest};

/// How long a provider may take to return its final output, unless configured otherwise.
pub const DEFAULT_INFERENCE_TIMEOUT: Duration = Duration::from_secs(180);

/// How long Jev may take to classify, unless configured otherwise.
pub const DEFAULT_JEV_TIMEOUT: Duration = Duration::from_secs(60);

/// A run could not be started or recorded at all. (A run that started and then failed is
/// returned as a failed [`Run`] instead.)
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("provider `{0}` is not registered")]
    UnknownProvider(ProviderId),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// A run that [`Runtime::start`] recorded and [`Runtime::finish`] has yet to execute.
#[derive(Debug)]
pub struct StartedRun {
    /// The run as recorded: `running`, with its provider, model, and system prompt.
    pub run: Run,
    task: NodeId,
    message: String,
}

/// Which conversation a new run belongs to.
#[derive(Debug, Clone, Copy)]
pub enum Thread<'a> {
    /// The project's most recently active conversation, or a new one if it has none.
    Latest(&'a ProjectId),
    /// This conversation.
    Conversation(&'a ConversationId),
    /// A new conversation in the project, titled after the message.
    New(&'a ProjectId),
}

/// Wires the store, Jev, and the registered providers into the DAGOS pipeline.
pub struct Runtime {
    store: Arc<Store>,
    jev: Arc<dyn JevClassifier>,
    jev_fallback: Option<Arc<dyn JevClassifier>>,
    providers: BTreeMap<ProviderId, Arc<dyn InferenceProvider>>,
    tools: Vec<IrTool>,
    tool_runner: Option<(Arc<dyn ToolExecutor>, Arc<dyn ToolGate>)>,
    max_tool_steps: usize,
    tool_timeout: Duration,
    conversation_window: usize,
    reviewer: Option<Arc<dyn Reviewer>>,
    max_review_rounds: usize,
    /// Stop signals of the runs this runtime is executing.
    stops: Mutex<BTreeMap<RunId, Arc<tokio::sync::Notify>>>,
    inference_timeout: Duration,
    jev_timeout: Duration,
}

/// How many reviews one run may have by default.
pub const DEFAULT_MAX_REVIEW_ROUNDS: usize = 3;

/// How many rounds of tool calls one run may execute by default.
pub const DEFAULT_MAX_TOOL_STEPS: usize = 8;

/// How long one tool call may take by default.
pub const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(120);

impl Runtime {
    pub fn new(store: Arc<Store>, jev: Arc<dyn JevClassifier>) -> Self {
        Self {
            store,
            jev,
            jev_fallback: None,
            providers: BTreeMap::new(),
            tools: Vec::new(),
            tool_runner: None,
            max_tool_steps: DEFAULT_MAX_TOOL_STEPS,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
            conversation_window: RECENT_RUN_OUTCOMES,
            reviewer: None,
            max_review_rounds: DEFAULT_MAX_REVIEW_ROUNDS,
            stops: Mutex::default(),
            inference_timeout: DEFAULT_INFERENCE_TIMEOUT,
            jev_timeout: DEFAULT_JEV_TIMEOUT,
        }
    }

    /// Registers a provider under its own ID, replacing any provider with the same ID.
    pub fn with_provider(mut self, provider: Arc<dyn InferenceProvider>) -> Self {
        self.providers.insert(provider.id().clone(), provider);
        self
    }

    /// Capability descriptions compiled into every run's IR (descriptive only).
    pub fn with_tools(mut self, tools: Vec<IrTool>) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_inference_timeout(mut self, timeout: Duration) -> Self {
        self.inference_timeout = timeout;
        self
    }

    /// Classifies with `fallback` whenever the primary Jev fails, times out, or is rejected, so a
    /// model-backed Jev improves runs without being required for them. The fallback's output is
    /// validated exactly like the primary's, and `jev.fallback` records why it was used.
    pub fn with_jev_fallback(mut self, fallback: Arc<dyn JevClassifier>) -> Self {
        self.jev_fallback = Some(fallback);
        self
    }

    /// Lets runs execute the tool calls their validated responses ask for: `gate` decides whether
    /// each call may run and `executor` runs it. Without a runner, requested calls are recorded
    /// and denied as unavailable.
    pub fn with_tool_runner(
        mut self,
        executor: Arc<dyn ToolExecutor>,
        gate: Arc<dyn ToolGate>,
    ) -> Self {
        self.tool_runner = Some((executor, gate));
        self
    }

    /// The most rounds of tool calls one run executes; further requests are denied.
    pub fn with_max_tool_steps(mut self, steps: usize) -> Self {
        self.max_tool_steps = steps;
        self
    }

    pub fn with_tool_timeout(mut self, timeout: Duration) -> Self {
        self.tool_timeout = timeout;
        self
    }

    /// Reviews the code runs change: each time the model replies without tool calls, `reviewer`
    /// judges what the run changed, and its findings go back to the model in the next IR until a
    /// review finds nothing, the code stops changing, or `max_rounds` reviews have run.
    pub fn with_reviewer(mut self, reviewer: Arc<dyn Reviewer>, max_rounds: usize) -> Self {
        self.reviewer = Some(reviewer);
        self.max_review_rounds = max_rounds;
        self
    }

    /// How many of the conversation's most recent earlier turns IR carries. Jev recalls older
    /// turns, and turns of other chats, when they are relevant.
    pub fn with_conversation_window(mut self, turns: usize) -> Self {
        self.conversation_window = turns;
        self
    }

    pub fn with_jev_timeout(mut self, timeout: Duration) -> Self {
        self.jev_timeout = timeout;
        self
    }

    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }

    pub fn jev(&self) -> &dyn JevClassifier {
        self.jev.as_ref()
    }

    /// The primary Jev, shared, e.g. for a linter that asks it questions of its own.
    pub fn shared_jev(&self) -> Arc<dyn JevClassifier> {
        self.jev.clone()
    }

    /// Registered providers, ordered by ID.
    pub fn providers(&self) -> impl Iterator<Item = &Arc<dyn InferenceProvider>> {
        self.providers.values()
    }

    pub fn tools(&self) -> &[IrTool] {
        &self.tools
    }

    /// The project's configuration for new runs, if one has been chosen.
    pub fn defaults(&self, project_id: &ProjectId) -> Result<Option<RunConfig>, StoreError> {
        self.store.transaction(|tx| tx.run_defaults(project_id))
    }

    /// Chooses the provider, model, and system prompt for the project's subsequent runs. The
    /// provider must be registered; the model ID is passed to it unchanged.
    pub fn set_defaults(
        &self,
        project_id: &ProjectId,
        config: &RunConfig,
    ) -> Result<(), RuntimeError> {
        if !self.providers.contains_key(&config.provider_id) {
            return Err(RuntimeError::UnknownProvider(config.provider_id.clone()));
        }
        Ok(self.store.transaction(|tx| tx.set_run_defaults(project_id, config))?)
    }

    /// Fails every run a previous process left `running` with error code `interrupted`. Call only
    /// when this process is executing no runs, e.g. at startup.
    pub fn recover_interrupted_runs(&self) -> Result<Vec<Run>, StoreError> {
        self.recover_runs_except(&BTreeSet::new())
    }

    /// Fails every `running` run except those this process is still `executing`, with error code
    /// `interrupted`.
    pub fn recover_runs_except(&self, executing: &BTreeSet<RunId>) -> Result<Vec<Run>, StoreError> {
        self.store.transaction(|tx| {
            tx.running_runs()?
                .into_iter()
                .filter(|run| !executing.contains(&run.id))
                .map(|run| {
                    tx.fail_run(
                        &run.id,
                        ErrorCode::Interrupted,
                        "DAGOS stopped before the run finished",
                    )
                })
                .collect()
        })
    }

    /// Runs the whole pipeline for `message` in `project_id` with `config` and returns the
    /// finished run, completed or failed.
    pub async fn run(
        &self,
        project_id: &ProjectId,
        message: &str,
        config: &RunConfig,
    ) -> Result<Run, RuntimeError> {
        let started = self.start(project_id, message, config)?;
        self.finish(started).await
    }

    /// Starts a run in the project's latest conversation (see [`Runtime::start_in`]).
    pub fn start(
        &self,
        project_id: &ProjectId,
        message: &str,
        config: &RunConfig,
    ) -> Result<StartedRun, RuntimeError> {
        self.start_in(Thread::Latest(project_id), message, config)
    }

    /// Starts a run in `thread`: records it, records `message` as its task node, and carries
    /// context from the conversation's previous run. The returned run is `running`;
    /// [`Runtime::finish`] executes the rest of the pipeline. Splitting the two lets a caller
    /// learn the run ID before inference begins.
    pub fn start_in(
        &self,
        thread: Thread<'_>,
        message: &str,
        config: &RunConfig,
    ) -> Result<StartedRun, RuntimeError> {
        if !self.providers.contains_key(&config.provider_id) {
            return Err(RuntimeError::UnknownProvider(config.provider_id.clone()));
        }
        let (run, task) = self.store.transaction(|tx| {
            let conversation_id = match thread {
                Thread::Conversation(id) => id.clone(),
                Thread::Latest(project_id) => match tx.latest_conversation(project_id)? {
                    Some(conversation) => conversation.id,
                    None => tx.create_conversation(project_id, message)?.id,
                },
                Thread::New(project_id) => tx.create_conversation(project_id, message)?.id,
            };
            let run = tx.create_run_in(&conversation_id, config)?;
            let turn = ConversationTurn::user(message).to_payload();
            let task = tx.insert_node(&run.project_id, NodeType::Conversation, turn)?;
            tx.append_event(
                &run.id,
                EventData::MessageRecorded { node_id: task.id.clone(), text: message.to_owned() },
            )?;
            carry_context(tx, &run.id)?;
            Ok::<_, StoreError>((run, task.id))
        })?;
        Ok(StartedRun { run, task, message: message.to_owned() })
    }

    /// Stops `run_id` if this runtime is executing it: the run fails with `cancelled` at its
    /// current step (waiting for a provider, a tool, a person, or Jev), and nothing it had not
    /// committed yet is recorded. Returns whether the run was executing here.
    pub fn stop(&self, run_id: &RunId) -> bool {
        let stops = self.stops.lock().unwrap_or_else(PoisonError::into_inner);
        stops.get(run_id).map(|stop| stop.notify_one()).is_some()
    }

    /// Executes the rest of a started run's pipeline and returns the finished run.
    pub async fn finish(&self, started: StartedRun) -> Result<Run, RuntimeError> {
        let StartedRun { run, task, message } = started;
        let stop = Arc::new(tokio::sync::Notify::new());
        self.stops
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(run.id.clone(), stop.clone());
        let outcome = match self.providers.get(&run.provider_id).cloned() {
            Some(provider) => tokio::select! {
                outcome = self.execute(&run, &task, &message, provider.as_ref()) => outcome,
                () = stop.notified() => {
                    Err(StageFailure::new(ErrorCode::Cancelled, "stopped by the person"))
                }
            },
            None => Err(StageFailure::new(
                ErrorCode::ProviderFailed,
                format!("provider `{}` is not registered", run.provider_id),
            )),
        };
        self.stops.lock().unwrap_or_else(PoisonError::into_inner).remove(&run.id);
        if let Some(reviewer) = &self.reviewer {
            reviewer.end(&run.id);
        }
        match outcome {
            Ok(completed) => Ok(completed),
            Err(failure) => Ok(self.store.transaction(|tx| {
                if let Some(evidence) = failure.evidence {
                    tx.append_event(&run.id, *evidence)?;
                }
                tx.fail_run(&run.id, failure.code, &failure.message)
            })?),
        }
    }

    async fn execute(
        &self,
        run: &Run,
        task: &NodeId,
        message: &str,
        provider: &dyn InferenceProvider,
    ) -> Result<Run, StageFailure> {
        let classification = self.classify_context(run, task, message).await?;
        let tools = exposed_tools(&self.tools, &classification);
        let recalled = self.recall_turns(run, message).await?;
        let budget = provider.context_window(&run.model_id).await.map(ir_budget);
        let mut latest_round = BTreeSet::new();
        let mut latest_step = String::new();
        let mut omitted = BTreeSet::new();
        let mut review: Option<IrReview> = None;
        let mut reviews = ReviewState::default();
        let mut rounds = 0;
        let mut next_call = 1;
        loop {
            if rounds > 0 {
                omitted =
                    self.compact_tool_results(run, message, &latest_step, &latest_round).await?;
            }
            let step = Step {
                recalled: &recalled,
                omitted: &omitted,
                latest_round: &latest_round,
                review: review.as_ref(),
                budget,
            };
            let ir = self.compile_ir(run, task, &tools, step)?;
            let raw = self.infer(run, &ir, provider).await?;
            let validated = validate_response(&raw, &ir).map_err(|error| {
                StageFailure::new(ErrorCode::ResponseInvalid, format!("response rejected: {error}"))
                    .with_evidence(EventData::ResponseRejected { reason: error.to_string() })
            })?;
            let calls = validated.response().tool_calls.clone();
            self.store.transaction(|tx| apply_response(tx, run, &validated)).map_err(|error| {
                match error {
                    StoreError::Dag(violation) => StageFailure::new(
                        ErrorCode::EmissionRejected,
                        format!("emissions rejected: {violation}"),
                    )
                    .with_evidence(EventData::ResponseRejected { reason: violation.to_string() }),
                    other => StageFailure::from(other),
                }
            })?;
            if calls.is_empty() {
                match self.review(run, &mut reviews).await? {
                    Some(next) => {
                        review = Some(next);
                        continue;
                    }
                    None => break,
                }
            }
            rounds += 1;
            let over_limit = rounds > self.max_tool_steps;
            latest_step = validated.response().presentation.prose.clone();
            latest_round.clear();
            for call in calls {
                let request = ToolRequest {
                    call_id: format!("call_{next_call}"),
                    name: call.name,
                    arguments: call.arguments,
                };
                latest_round.insert(request.call_id.clone());
                next_call += 1;
                self.run_tool(run, &ir, request, over_limit).await?;
            }
            if over_limit {
                break;
            }
        }
        Ok(self.store.transaction(|tx| tx.complete_run(&run.id))?)
    }

    /// Records a requested call, asks the gate, runs it if allowed, and records the outcome.
    async fn run_tool(
        &self,
        run: &Run,
        ir: &InferenceIr,
        request: ToolRequest,
        over_limit: bool,
    ) -> Result<(), StageFailure> {
        let call_id = request.call_id.clone();
        self.store.transaction(|tx| {
            tx.append_event(
                &run.id,
                EventData::ToolRequested {
                    call_id: call_id.clone(),
                    name: request.name.clone(),
                    arguments: request.arguments.clone(),
                },
            )
        })?;
        let listed = ir.tools.iter().any(|tool| tool.name == request.name);
        let decision = match &self.tool_runner {
            _ if over_limit => ToolDecision::Deny {
                by: ToolDecider::Limit,
                reason: format!("the run reached its limit of {} tool rounds", self.max_tool_steps),
            },
            Some((_, gate)) if listed => gate.decide(&run.id, &request).await,
            _ => ToolDecision::Deny {
                by: ToolDecider::Unavailable,
                reason: format!("no tool named `{}` is available to this run", request.name),
            },
        };
        let (allowed, by, reason) = match decision {
            ToolDecision::Allow { by, note } => (true, by, note),
            ToolDecision::Deny { by, reason } => (false, by, Some(reason)),
        };
        self.store.transaction(|tx| {
            tx.append_event(
                &run.id,
                EventData::ToolDecided { call_id: call_id.clone(), allowed, by, reason },
            )
        })?;
        let Some((executor, _)) = self.tool_runner.as_ref().filter(|_| allowed) else {
            return Ok(());
        };
        if let Some(reviewer) = &self.reviewer {
            reviewer.before_call(&run.id, &request).await;
        }
        let output = match tokio::time::timeout(self.tool_timeout, executor.call(&request)).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                ToolOutput { output: serde_json::json!({"error": error}), is_error: true }
            }
            Err(_elapsed) => ToolOutput {
                output: serde_json::json!({
                    "error": format!("no result within {:?}", self.tool_timeout)
                }),
                is_error: true,
            },
        };
        self.store.transaction(|tx| {
            tx.append_event(
                &run.id,
                EventData::ToolCompleted {
                    call_id,
                    output: output.output,
                    is_error: output.is_error,
                },
            )
        })?;
        Ok(())
    }

    /// Reviews the code the run changed after the model replied without tool calls, records the
    /// review, and returns it when the model must address it: it found something, the code
    /// changed since the previous review, and the run has reviews left. A review that judged
    /// nothing (no code changed) is not recorded.
    async fn review(
        &self,
        run: &Run,
        state: &mut ReviewState,
    ) -> Result<Option<IrReview>, StageFailure> {
        let Some(reviewer) = &self.reviewer else { return Ok(None) };
        if state.rounds >= self.max_review_rounds {
            return Ok(None);
        }
        let outcome = tokio::time::timeout(self.tool_timeout, reviewer.review(&run.id))
            .await
            .unwrap_or_else(|_elapsed| Err(format!("no review within {:?}", self.tool_timeout)));
        let (review, error) = match outcome {
            Ok(review) => (review, None),
            Err(error) => (Default::default(), Some(error)),
        };
        if review.judged == 0 && error.is_none() {
            return Ok(None);
        }
        state.rounds += 1;
        let round = state.rounds as u32;
        self.store.transaction(|tx| {
            tx.append_event(
                &run.id,
                EventData::ReviewCompleted {
                    round,
                    judged: review.judged as u32,
                    findings: review.findings.clone(),
                    error,
                },
            )
        })?;
        let unchanged = state.fingerprint.as_deref() == Some(review.fingerprint.as_str());
        if review.findings.is_empty() {
            return Ok(None);
        }
        if unchanged || state.rounds >= self.max_review_rounds {
            // The run ends with these findings open: the project remembers them.
            self.store.transaction(|tx| record_findings(tx, run, &review.findings))?;
            return Ok(None);
        }
        state.fingerprint = Some(review.fingerprint);
        Ok(Some(IrReview {
            round,
            max_rounds: self.max_review_rounds as u32,
            findings: review.findings,
        }))
    }

    /// Asks Jev which of the project's earlier turns outside this run's recent window (any chat)
    /// are relevant to `message`; every relevant one reaches IR, whole, as `recalled`. Nothing is recalled
    /// when Jev does not judge relevance or fails to.
    async fn recall_turns(
        &self,
        run: &Run,
        message: &str,
    ) -> Result<Vec<(IrRecalledTurn, f64)>, StageFailure> {
        let window = self.conversation_window;
        let candidates = self.store.transaction(|tx| recall_candidates(tx, &run.id, window))?;
        if candidates.is_empty() {
            return Ok(Vec::new());
        }
        let chunks: Vec<RecallChunk> =
            candidates.iter().map(|candidate| candidate.chunk.clone()).collect();
        match self.judge(message, &chunks).await {
            Some(scores) => Ok(select_recalled(candidates, &scores)),
            None => Ok(Vec::new()),
        }
    }

    /// Before a step, asks Jev which large tool results from earlier rounds of this run are still
    /// relevant to the request and the model's latest step, and returns the call IDs of the rest,
    /// which IR leaves out. Every step judges afresh, so a result comes back when it becomes
    /// relevant again. The latest round's results are always kept, and without a judging Jev, or
    /// when it fails, every result is kept.
    async fn compact_tool_results(
        &self,
        run: &Run,
        message: &str,
        latest_step: &str,
        latest_round: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, StageFailure> {
        let events = self
            .store
            .transaction(|tx| tx.events_of_types(&run.id, &["tool.requested", "tool.completed"]))?;
        let mut requests = BTreeMap::new();
        let mut chunks = Vec::new();
        for event in events {
            match event.data {
                EventData::ToolRequested { call_id, name, arguments } => {
                    requests.insert(call_id, (name, arguments));
                }
                EventData::ToolCompleted { call_id, output, .. } => {
                    let text = output_text(&output);
                    if latest_round.contains(&call_id) || text.chars().count() < COMPACT_MIN_CHARS {
                        continue;
                    }
                    let Some((name, arguments)) = requests.get(&call_id) else { continue };
                    let arguments = serde_json::Value::Object(arguments.clone());
                    let text = cut(&text, RECALL_CHUNK_CHARS);
                    chunks.push(RecallChunk::new(
                        call_id,
                        format!("tool call {name} {arguments}\nresult:\n{text}"),
                    ));
                }
                _ => {}
            }
        }
        if chunks.is_empty() {
            return Ok(BTreeSet::new());
        }
        let query = format!("{message}\n\nThe assistant's latest step: {latest_step}");
        let Some(scores) = self.judge(&query, &chunks).await else {
            return Ok(BTreeSet::new());
        };
        Ok(chunks
            .into_iter()
            .zip(scores)
            .filter(|(_, score)| *score < RECALL_THRESHOLD)
            .map(|(chunk, _)| chunk.id)
            .collect())
    }

    /// Jev's relevance of each chunk to `query`, under the Jev deadline and checked for shape;
    /// `None` if Jev does not judge relevance, fails, or answers malformed scores.
    async fn judge(&self, query: &str, chunks: &[RecallChunk]) -> Option<Vec<f64>> {
        let scores = tokio::time::timeout(self.jev_timeout, self.jev.relevance(query, chunks))
            .await
            .ok()?
            .ok()??;
        let valid = scores.len() == chunks.len() && scores.iter().all(|p| (0.0..=1.0).contains(p));
        valid.then_some(scores)
    }

    /// Asks Jev to classify the run's candidates and applies valid output to the active context.
    async fn classify_context(
        &self,
        run: &Run,
        task: &NodeId,
        message: &str,
    ) -> Result<ContextClassification, StageFailure> {
        let request = self.store.transaction(|tx| {
            let mut request = classification_request(tx, run, task, message)?;
            request.tools = self.tools.iter().map(tool_candidate).collect();
            let jev_id = self.jev.id().to_owned();
            tx.append_event(&run.id, EventData::JevRequested { jev_id, request: request.clone() })?;
            Ok::<_, StoreError>(request)
        })?;
        let failure = match self.classify_with(self.jev.as_ref(), &request).await {
            Ok(classification) => {
                self.apply(run, &classification)?;
                return Ok(classification);
            }
            Err(failure) => failure,
        };
        let Some(fallback) = &self.jev_fallback else { return Err(failure) };
        self.store.transaction(|tx| {
            if let Some(evidence) = &failure.evidence {
                tx.append_event(&run.id, (**evidence).clone())?;
            }
            let jev_id = fallback.id().to_owned();
            tx.append_event(&run.id, EventData::JevFallback { jev_id, reason: failure.message })
        })?;
        let classification = self.classify_with(fallback.as_ref(), &request).await?;
        self.apply(run, &classification)?;
        Ok(classification)
    }

    /// One classification attempt by `jev`: under the deadline, then validated against the
    /// contract and the request.
    async fn classify_with(
        &self,
        jev: &dyn JevClassifier,
        request: &JevRequest,
    ) -> Result<ContextClassification, StageFailure> {
        let raw = tokio::time::timeout(self.jev_timeout, jev.classify(request))
            .await
            .map_err(|_elapsed| {
                let message = format!("no classification within {:?}", self.jev_timeout);
                StageFailure::new(ErrorCode::JevFailed, message)
            })?
            .map_err(|error| StageFailure::new(ErrorCode::JevFailed, error.to_string()))?;
        validate_classification(request, &raw).map_err(|error| {
            StageFailure::new(ErrorCode::JevInvalidOutput, format!("Jev output rejected: {error}"))
                .with_evidence(EventData::JevRejected { reason: error.to_string(), output: raw })
        })
    }

    fn apply(&self, run: &Run, classification: &ContextClassification) -> Result<(), StageFailure> {
        self.store.transaction(|tx| apply_classification(tx, &run.id, classification))?;
        Ok(())
    }

    /// Compiles the step's IR and, when the model's window is known and everything relevant does
    /// not fit, fits it: the least relevant recalled turns go first, then the outputs of earlier
    /// rounds' tool calls (oldest first; the model can call again), then the oldest turns of the
    /// chat. Nothing is left out while everything fits.
    fn compile_ir(
        &self,
        run: &Run,
        task: &NodeId,
        tools: &[IrTool],
        step: Step<'_>,
    ) -> Result<InferenceIr, StageFailure> {
        let failed = |error: CompileError| match error {
            CompileError::Contract(violation) => {
                StageFailure::new(ErrorCode::IrInvalid, violation.to_string())
            }
            other => StageFailure::new(ErrorCode::Internal, other.to_string()),
        };
        self.store.transaction(|tx| {
            // Recalled turns in chronological order, and the order they are left out in: least
            // relevant first.
            let mut kept: Vec<bool> = vec![true; step.recalled.len()];
            let mut drop_order: Vec<usize> = (0..step.recalled.len()).collect();
            drop_order.sort_by(|a, b| step.recalled[*a].1.total_cmp(&step.recalled[*b].1));
            let mut drop_order = drop_order.into_iter();
            let mut omitted = step.omitted.clone();
            let mut window = self.conversation_window;
            let mut exhausted = false;
            loop {
                let turns: Vec<IrRecalledTurn> = step
                    .recalled
                    .iter()
                    .zip(&kept)
                    .filter(|(_, kept)| **kept)
                    .map(|((turn, _), _)| turn.clone())
                    .collect();
                let history =
                    IrHistory { window, recalled: &turns, omitted: &omitted, review: step.review };
                let ir = compile_with(tx, &run.id, task, tools, history).map_err(failed)?;
                let size = serde_json::to_string(&ir).map_or(0, |text| text.len());
                let over = step.budget.and_then(|budget| size.checked_sub(budget));
                let Some(over) = over.filter(|_| !exhausted) else {
                    // It fits, or nothing more can be left out: send it (a provider may refuse
                    // what is still too large).
                    tx.append_event(&run.id, EventData::IrCompiled { ir: ir.clone() })?;
                    return Ok(ir);
                };
                // Leave out enough, by size, to get under the budget, then compile again.
                let mut freed = 0;
                while freed <= over {
                    if let Some(index) = drop_order.next() {
                        kept[index] = false;
                        freed += step.recalled[index].0.text.len();
                        continue;
                    }
                    let earlier = ir.tool_results.iter().find(|result| {
                        !step.latest_round.contains(&result.call_id)
                            && !omitted.contains(&result.call_id)
                            && result.output.is_some()
                    });
                    if let Some(result) = earlier {
                        omitted.insert(result.call_id.clone());
                        freed +=
                            result.output.as_ref().map_or(0, |output| output.to_string().len());
                        continue;
                    }
                    if window > 0 && !ir.recent_events.is_empty() {
                        window = ir.recent_events.len() - 1;
                        freed = over + 1;
                        continue;
                    }
                    exhausted = true;
                    break;
                }
            }
        })
    }

    /// Calls the provider under the inference deadline, recording each delta as it streams.
    async fn infer(
        &self,
        run: &Run,
        ir: &InferenceIr,
        provider: &dyn InferenceProvider,
    ) -> Result<String, StageFailure> {
        self.store.transaction(|tx| {
            tx.append_event(
                &run.id,
                EventData::InferenceStarted {
                    provider_id: run.provider_id.clone(),
                    model_id: run.model_id.clone(),
                },
            )
        })?;
        let mut deltas = RecordDeltas { store: &self.store, run_id: &run.id, failure: None };
        let request = InferenceRequest { model_id: &run.model_id, ir };
        let outcome =
            tokio::time::timeout(self.inference_timeout, provider.infer(request, &mut deltas))
                .await;
        if let Some(error) = deltas.failure {
            return Err(error.into());
        }
        let raw = match outcome {
            Err(_elapsed) => {
                return Err(StageFailure::new(
                    ErrorCode::ProviderTimeout,
                    format!("no final response within {:?}", self.inference_timeout),
                ));
            }
            Ok(Err(error)) => {
                return Err(StageFailure::new(ErrorCode::ProviderFailed, error.to_string()));
            }
            Ok(Ok(raw)) => raw,
        };
        self.store.transaction(|tx| {
            tx.append_event(&run.id, EventData::InferenceCompleted { output: raw.clone() })
        })?;
        Ok(raw)
    }
}

/// Records findings a run ended with as durable `observation` nodes (payload `kind:
/// "lint_finding"`), so later runs in any chat can draw on them through Jev's classification. A
/// finding the project already has a node for (same rule, file, and function) is not recorded
/// twice. Each node is announced with `dag.node_created`, like an emission.
fn record_findings(tx: &Tx<'_>, run: &Run, findings: &[IrFinding]) -> Result<(), StoreError> {
    let same = |payload: &Payload, finding: &IrFinding| {
        payload.get("kind").and_then(|v| v.as_str()) == Some("lint_finding")
            && payload.get("rule").and_then(|v| v.as_str()) == Some(finding.rule.as_str())
            && payload.get("file").and_then(|v| v.as_str()) == Some(finding.file.as_str())
            && payload.get("function").and_then(|v| v.as_str()) == Some(finding.function.as_str())
    };
    let existing: Vec<Payload> = tx
        .nodes(&run.project_id)?
        .into_iter()
        .filter(|node| node.node_type == NodeType::Observation)
        .map(|node| node.payload)
        .collect();
    for (index, finding) in findings.iter().enumerate() {
        if existing.iter().any(|payload| same(payload, finding)) {
            continue;
        }
        let serde_json::Value::Object(payload) = serde_json::json!({
            "kind": "lint_finding",
            "text": format!(
                "Lint: {} `{}` ({}:{})",
                finding.text, finding.function, finding.file, finding.line
            ),
            "rule": finding.rule,
            "file": finding.file,
            "function": finding.function,
            "line": finding.line,
            "probability": finding.probability,
        }) else {
            unreachable!("a JSON object literal")
        };
        let node = tx.insert_node(&run.project_id, NodeType::Observation, payload)?;
        let reference = EmissionRef::parse(format!("review-finding-{}", index + 1))
            .expect("review finding refs are valid");
        tx.append_event(
            &run.id,
            EventData::DagNodeCreated {
                node_id: node.id,
                node_type: NodeType::Observation,
                emission_ref: reference,
            },
        )?;
    }
    Ok(())
}

/// What one step's IR is made of, beyond the durable state.
struct Step<'a> {
    /// Recalled turns, oldest first, with Jev's relevance.
    recalled: &'a [(IrRecalledTurn, f64)],
    omitted: &'a BTreeSet<String>,
    latest_round: &'a BTreeSet<String>,
    review: Option<&'a IrReview>,
    /// The most IR characters the model's window takes, if known.
    budget: Option<usize>,
}

/// Tokens of a model's window kept free for the system message (protocol and schema) and the
/// reply.
const RESERVED_TOKENS: usize = 16_000;

/// A conservative characters-per-token estimate for JSON-heavy IR.
const CHARS_PER_TOKEN: usize = 3;

/// The most IR characters a model with a window of `tokens` takes.
fn ir_budget(tokens: usize) -> usize {
    tokens.saturating_sub(RESERVED_TOKENS) * CHARS_PER_TOKEN
}

/// The reviews a run has had so far.
#[derive(Default)]
struct ReviewState {
    rounds: usize,
    /// The fingerprint of the code the latest review handed back.
    fingerprint: Option<String>,
}

/// The longest tool description Jev is shown; the model still gets the full description.
const JEV_TOOL_DESCRIPTION_CHARS: usize = 300;

/// A tool as Jev sees it: its name and the start of its description.
fn tool_candidate(tool: &IrTool) -> JevToolCandidate {
    let mut description: String =
        tool.description.chars().take(JEV_TOOL_DESCRIPTION_CHARS).collect();
    if description.len() < tool.description.len() {
        description.push('…');
    }
    JevToolCandidate { name: tool.name.clone(), description }
}

/// The tools the model sees in a run: every offered tool Jev did not classify `inactive`.
fn exposed_tools(tools: &[IrTool], classification: &ContextClassification) -> Vec<IrTool> {
    let hidden: BTreeSet<&str> = classification
        .tools
        .iter()
        .filter(|entry| entry.classification == Classification::Inactive)
        .map(|entry| entry.name.as_str())
        .collect();
    tools.iter().filter(|tool| !hidden.contains(tool.name.as_str())).cloned().collect()
}

/// Records `response.validated` and creates the response's nodes and then its edges. Runs inside
/// one transaction, so any DAG violation leaves no trace of the response.
fn apply_response(tx: &Tx<'_>, run: &Run, validated: &ValidatedResponse) -> Result<(), StoreError> {
    let response = validated.response().clone();
    tx.append_event(&run.id, EventData::ResponseValidated { response })?;
    let mut created: BTreeMap<&EmissionRef, NodeId> = BTreeMap::new();
    for emission in validated.emissions() {
        if let Emission::Node { reference, node_type, payload } = emission {
            let node = tx.insert_node(&run.project_id, *node_type, payload.clone())?;
            tx.append_event(
                &run.id,
                EventData::DagNodeCreated {
                    node_id: node.id.clone(),
                    node_type: *node_type,
                    emission_ref: reference.clone(),
                },
            )?;
            created.insert(reference, node.id);
        }
    }
    let resolve = |endpoint: &Endpoint| match endpoint {
        Endpoint::Ref(reference) => {
            created.get(reference).cloned().expect("validated refs name emitted nodes")
        }
        Endpoint::Node(node_id) => node_id.clone(),
    };
    for emission in validated.emissions() {
        if let Emission::Edge { from, to, edge_type } = emission {
            let edge = tx.insert_edge(&run.project_id, &resolve(from), &resolve(to), *edge_type)?;
            tx.append_event(
                &run.id,
                EventData::DagEdgeCreated {
                    edge_id: edge.id,
                    from: edge.from_node_id,
                    to: edge.to_node_id,
                    edge_type: edge.edge_type,
                },
            )?;
        }
    }
    Ok(())
}

/// A failed pipeline stage, recorded on the run rather than returned to the caller.
struct StageFailure {
    code: ErrorCode,
    message: String,
    /// An event explaining a rejection, recorded right before `run.failed`.
    evidence: Option<Box<EventData>>,
}

impl StageFailure {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), evidence: None }
    }

    fn with_evidence(mut self, evidence: EventData) -> Self {
        self.evidence = Some(Box::new(evidence));
        self
    }
}

impl From<StoreError> for StageFailure {
    fn from(error: StoreError) -> Self {
        Self::new(ErrorCode::Internal, error.to_string())
    }
}

/// Persists each streamed delta as an `inference.delta` event the moment it arrives.
struct RecordDeltas<'a> {
    store: &'a Store,
    run_id: &'a RunId,
    failure: Option<StoreError>,
}

impl DeltaSink for RecordDeltas<'_> {
    fn delta(&mut self, text: &str) {
        if self.failure.is_some() {
            return;
        }
        let recorded = self.store.transaction(|tx| {
            tx.append_event(self.run_id, EventData::InferenceDelta { text: text.to_owned() })
        });
        if let Err(error) = recorded {
            self.failure = Some(error);
        }
    }
}
