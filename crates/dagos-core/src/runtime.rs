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
use std::sync::Arc;
use std::time::Duration;

use crate::context::{
    JevClassifier, apply_classification, carry_context, classification_request,
    validate_classification,
};
use crate::domain::{
    ContextClassification, ConversationId, ConversationTurn, Emission, EmissionRef, Endpoint,
    ErrorCode, EventData, InferenceIr, IrTool, JevRequest, NodeId, NodeType, ProjectId, ProviderId,
    Run, RunConfig, RunId,
};
use crate::ir::{CompileError, compile};
use crate::provider::{DeltaSink, InferenceProvider, InferenceRequest};
use crate::response::{ValidatedResponse, validate_response};
use crate::store::{Store, StoreError, Tx};

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
    inference_timeout: Duration,
    jev_timeout: Duration,
}

impl Runtime {
    pub fn new(store: Arc<Store>, jev: Arc<dyn JevClassifier>) -> Self {
        Self {
            store,
            jev,
            jev_fallback: None,
            providers: BTreeMap::new(),
            tools: Vec::new(),
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

    /// Executes the rest of a started run's pipeline and returns the finished run.
    pub async fn finish(&self, started: StartedRun) -> Result<Run, RuntimeError> {
        let StartedRun { run, task, message } = started;
        let outcome = match self.providers.get(&run.provider_id).cloned() {
            Some(provider) => self.execute(&run, &task, &message, provider.as_ref()).await,
            None => Err(StageFailure::new(
                ErrorCode::ProviderFailed,
                format!("provider `{}` is not registered", run.provider_id),
            )),
        };
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
        self.classify_context(run, task, message).await?;
        let ir = self.compile_ir(run, task)?;
        let raw = self.infer(run, &ir, provider).await?;
        let validated = validate_response(&raw, &ir).map_err(|error| {
            StageFailure::new(ErrorCode::ResponseInvalid, format!("response rejected: {error}"))
                .with_evidence(EventData::ResponseRejected { reason: error.to_string() })
        })?;
        self.store.transaction(|tx| apply_response(tx, run, &validated)).map_err(
            |error| match error {
                StoreError::Dag(violation) => StageFailure::new(
                    ErrorCode::EmissionRejected,
                    format!("emissions rejected: {violation}"),
                )
                .with_evidence(EventData::ResponseRejected { reason: violation.to_string() }),
                other => StageFailure::from(other),
            },
        )
    }

    /// Asks Jev to classify the run's candidates and applies valid output to the active context.
    async fn classify_context(
        &self,
        run: &Run,
        task: &NodeId,
        message: &str,
    ) -> Result<(), StageFailure> {
        let request = self.store.transaction(|tx| {
            let request = classification_request(tx, run, task, message)?;
            let jev_id = self.jev.id().to_owned();
            tx.append_event(&run.id, EventData::JevRequested { jev_id, request: request.clone() })?;
            Ok::<_, StoreError>(request)
        })?;
        let failure = match self.classify_with(self.jev.as_ref(), &request).await {
            Ok(classification) => return self.apply(run, &classification),
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
        self.apply(run, &classification)
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

    fn compile_ir(&self, run: &Run, task: &NodeId) -> Result<InferenceIr, StageFailure> {
        self.store.transaction(|tx| {
            let ir = compile(tx, &run.id, task, &self.tools).map_err(|error| match error {
                CompileError::Contract(violation) => {
                    StageFailure::new(ErrorCode::IrInvalid, violation.to_string())
                }
                other => StageFailure::new(ErrorCode::Internal, other.to_string()),
            })?;
            tx.append_event(&run.id, EventData::IrCompiled { ir: ir.clone() })?;
            Ok(ir)
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

/// Records `response.validated`, creates the response's nodes and then its edges, and completes
/// the run. Runs inside one transaction, so any DAG violation leaves no trace of the response.
fn apply_response(
    tx: &Tx<'_>,
    run: &Run,
    validated: &ValidatedResponse,
) -> Result<Run, StoreError> {
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
    tx.complete_run(&run.id)
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
