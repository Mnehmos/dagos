//! Read-only views of a workspace: the durable DAG, runs, events, active context, Jev output, IR,
//! and responses, assembled from the store so neither people nor tools have to read tables.
//!
//! Views are projections of recorded state built from core domain types, so they serialize
//! exactly as the DAGOS contracts describe. Nothing here mutates the store.

use dagos_core::domain::{
    ContextClassification, ContextMember, Conversation, ConversationId, DagEdge, DagNode, EdgeId,
    Event, EventData, InferenceIr, InferenceResponse, JevRequest, ModelId, NodeId, Project,
    ProjectId, ProviderId, Run, RunConfig, RunId, RunStatus,
};
use dagos_core::store::{Store, StoreError};
use dagos_mcp::Capabilities;
use serde::Serialize;

use crate::workspace::Workspace;

/// The project at a glance.
#[derive(Debug, Serialize)]
pub struct Overview {
    pub project: Project,
    /// The configuration new runs use.
    pub run_defaults: RunConfig,
    pub providers: Vec<ProviderInfo>,
    /// The Jev classifier in use.
    pub jev: String,
    /// MCP capabilities compiled into new runs' IR, when this process discovered them.
    pub capabilities: Option<Capabilities>,
    /// The durable DAG.
    pub dag: DagView,
    /// Every run, oldest first.
    pub runs: Vec<RunSummary>,
    /// The project's conversations, most recently active first (archived ones included).
    pub conversations: Vec<Conversation>,
}

/// A project in the project switcher.
#[derive(Debug, Serialize)]
pub struct ProjectSummary {
    pub project: Project,
    pub conversations: usize,
    pub nodes: usize,
}

/// A conversation as a chat: every run in order, each shown as a turn.
#[derive(Debug, Serialize)]
pub struct ConversationView {
    pub conversation: Conversation,
    pub turns: Vec<TurnView>,
}

/// One run as a chat turn: the user's message and what came back.
#[derive(Debug, Serialize)]
pub struct TurnView {
    pub run: Run,
    pub message: Option<String>,
    /// The validated reply's prose, or what has streamed so far while the run is running.
    pub prose: String,
    pub failure: Option<FailureView>,
    /// How many durable nodes were in the active context the model saw.
    pub context_size: usize,
    /// The Jev that classified, e.g. `openrouter-jev:<model>` or `fake-jev`.
    pub jev_id: Option<String>,
    /// Whether the offline fallback classified because the model Jev could not.
    pub jev_fallback: bool,
    pub emitted_nodes: usize,
    pub emitted_edges: usize,
}

#[derive(Debug, Serialize)]
pub struct ProviderInfo {
    pub id: ProviderId,
    pub suggested_models: Vec<ModelId>,
}

#[derive(Debug, Serialize)]
pub struct DagView {
    pub nodes: Vec<DagNode>,
    pub edges: Vec<DagEdge>,
}

#[derive(Debug, Serialize)]
pub struct RunSummary {
    pub run: Run,
    /// The user message the run answered.
    pub message: Option<String>,
}

/// The user message a run recorded as its task.
#[derive(Debug, Clone, Serialize)]
pub struct MessageView {
    pub node_id: NodeId,
    pub text: String,
}

/// Why a run stopped short of completion.
#[derive(Debug, Clone, Serialize)]
pub struct FailureView {
    pub error_code: String,
    pub message: String,
    /// The rejection that explains the failure, if output was rejected: `jev` or `response`.
    pub rejected_stage: Option<&'static str>,
    pub rejected_reason: Option<String>,
}

/// The requested Jev failed or was rejected and the fallback classified instead.
#[derive(Debug, Clone, Serialize)]
pub struct JevFallbackView {
    /// The Jev that was asked first.
    pub from: Option<String>,
    /// The fallback that classified.
    pub jev_id: String,
    pub reason: String,
    /// The first Jev's output, if it was rejected (never applied).
    pub rejected_output: Option<String>,
}

/// Everything recorded about one run.
#[derive(Debug, Serialize)]
pub struct RunDetail {
    pub run: Run,
    pub message: Option<MessageView>,
    /// The run's active context in IR order, with why each node is a member.
    pub context: Vec<ContextMember>,
    /// Nodes carried over from the previous run's context before Jev ran.
    pub carried: Vec<NodeId>,
    /// The Jev asked to classify, e.g. `openrouter-jev:<model>` or `fake-jev`.
    pub jev_id: Option<String>,
    pub jev_request: Option<JevRequest>,
    /// Set when the fallback Jev classified instead of the one asked first.
    pub jev_fallback: Option<JevFallbackView>,
    pub classification: Option<ContextClassification>,
    /// Membership changes Jev's classification caused.
    pub context_added: Vec<NodeId>,
    pub context_removed: Vec<NodeId>,
    /// Exactly what the provider received.
    pub ir: Option<InferenceIr>,
    /// Presentation prose as streamed (never canonical state).
    pub streamed: String,
    /// The provider's raw final output, before validation.
    pub output: Option<String>,
    /// The validated response whose emissions became durable state.
    pub response: Option<InferenceResponse>,
    pub emitted_nodes: Vec<NodeId>,
    pub emitted_edges: Vec<EdgeId>,
    pub failure: Option<FailureView>,
    pub events: Vec<Event>,
}

/// Builds the overview of the workspace's default project.
pub fn overview(workspace: &Workspace) -> Result<Overview, StoreError> {
    project_overview(workspace, &workspace.project.id)?.ok_or_else(|| StoreError::NotFound {
        kind: "project",
        id: workspace.project.id.to_string(),
    })
}

/// Every project of the workspace, oldest first.
pub fn projects(store: &Store) -> Result<Vec<ProjectSummary>, StoreError> {
    store.transaction(|tx| {
        tx.projects()?
            .into_iter()
            .map(|project| {
                let conversations = tx.conversations(&project.id)?.len();
                let nodes = tx.nodes(&project.id)?.len();
                Ok(ProjectSummary { project, conversations, nodes })
            })
            .collect()
    })
}

/// Builds the overview of `project_id`; `None` if there is no such project.
pub fn project_overview(
    workspace: &Workspace,
    project_id: &ProjectId,
) -> Result<Option<Overview>, StoreError> {
    let Some(project) = workspace.store.transaction(|tx| tx.project(project_id))? else {
        return Ok(None);
    };
    let (dag, runs, conversations) = workspace.store.transaction(|tx| {
        let dag = DagView { nodes: tx.nodes(project_id)?, edges: tx.edges(project_id)? };
        let mut runs = Vec::new();
        for run in tx.runs(project_id)? {
            let message = tx.events(&run.id)?.into_iter().find_map(|event| match event.data {
                EventData::MessageRecorded { text, .. } => Some(text),
                _ => None,
            });
            runs.push(RunSummary { run, message });
        }
        Ok::<_, StoreError>((dag, runs, tx.conversations(project_id)?))
    })?;
    let run_defaults = workspace.run_config_for(project_id)?;
    let runtime = workspace.runtime();
    let providers = runtime
        .providers()
        .map(|provider| ProviderInfo {
            id: provider.id().clone(),
            suggested_models: provider.suggested_models(),
        })
        .collect();
    Ok(Some(Overview {
        project,
        run_defaults,
        providers,
        jev: workspace.runtime().jev().id().to_owned(),
        capabilities: workspace.capabilities.clone(),
        dag,
        runs,
        conversations,
    }))
}

/// A conversation as a chat; `None` if there is no such conversation.
pub fn conversation(
    store: &Store,
    id: &ConversationId,
) -> Result<Option<ConversationView>, StoreError> {
    store.transaction(|tx| {
        let Some(conversation) = tx.conversation(id)? else { return Ok(None) };
        let mut turns = Vec::new();
        for run in tx.conversation_runs(id)? {
            let events = tx.events(&run.id)?;
            let context_size = tx.context(&run.id)?.len();
            turns.push(turn(run, &events, context_size));
        }
        Ok(Some(ConversationView { conversation, turns }))
    })
}

fn turn(run: Run, events: &[Event], context_size: usize) -> TurnView {
    let mut view = TurnView {
        run,
        message: None,
        prose: String::new(),
        failure: None,
        context_size,
        jev_id: None,
        jev_fallback: false,
        emitted_nodes: 0,
        emitted_edges: 0,
    };
    let mut streamed = String::new();
    let mut validated = None;
    for event in events {
        match &event.data {
            EventData::MessageRecorded { text, .. } => view.message = Some(text.clone()),
            EventData::JevRequested { jev_id, .. } => view.jev_id = Some(jev_id.clone()),
            EventData::JevFallback { .. } => view.jev_fallback = true,
            EventData::InferenceDelta { text } => streamed.push_str(text),
            EventData::ResponseValidated { response } => {
                validated = Some(response.presentation.prose.clone());
            }
            EventData::DagNodeCreated { .. } => view.emitted_nodes += 1,
            EventData::DagEdgeCreated { .. } => view.emitted_edges += 1,
            _ => {}
        }
    }
    view.prose = validated.unwrap_or(streamed);
    if view.run.status == RunStatus::Failed {
        view.failure = failure(events);
    }
    view
}

/// Why a failed run stopped: its `run.failed` event and the rejection that explains it, if any.
fn failure(events: &[Event]) -> Option<FailureView> {
    let mut rejection: Option<(&'static str, String)> = None;
    for event in events {
        match &event.data {
            EventData::JevRejected { reason, .. } => rejection = Some(("jev", reason.clone())),
            EventData::JevFallback { .. } => rejection = None,
            EventData::ResponseRejected { reason } => {
                rejection = Some(("response", reason.clone()));
            }
            EventData::RunFailed { error_code, message } => {
                return Some(FailureView {
                    error_code: error_code.to_string(),
                    message: message.clone(),
                    rejected_stage: rejection.as_ref().map(|(stage, _)| *stage),
                    rejected_reason: rejection.map(|(_, reason)| reason),
                });
            }
            _ => {}
        }
    }
    None
}

/// Resolves `reference` (a run ID or `latest`) to a run of `project_id`.
pub fn resolve_run(
    store: &Store,
    project_id: &ProjectId,
    reference: &str,
) -> Result<Option<RunId>, StoreError> {
    if reference == "latest" {
        let runs = store.transaction(|tx| tx.runs(project_id))?;
        return Ok(runs.into_iter().last().map(|run| run.id));
    }
    let Ok(id) = RunId::parse(reference) else { return Ok(None) };
    let run = store.transaction(|tx| tx.run(&id))?;
    Ok(run.filter(|run| &run.project_id == project_id).map(|run| run.id))
}

/// Everything recorded about run `id`, or `None` if it does not exist.
pub fn run_detail(store: &Store, id: &RunId) -> Result<Option<RunDetail>, StoreError> {
    let Some((run, context, events)) = store.transaction(|tx| {
        let Some(run) = tx.run(id)? else { return Ok::<_, StoreError>(None) };
        Ok(Some((run, tx.context(id)?, tx.events(id)?)))
    })?
    else {
        return Ok(None);
    };

    let mut detail = RunDetail {
        run,
        message: None,
        context,
        carried: Vec::new(),
        jev_id: None,
        jev_request: None,
        jev_fallback: None,
        classification: None,
        context_added: Vec::new(),
        context_removed: Vec::new(),
        ir: None,
        streamed: String::new(),
        output: None,
        response: None,
        emitted_nodes: Vec::new(),
        emitted_edges: Vec::new(),
        failure: None,
        events: Vec::new(),
    };
    let mut rejection: Option<(&'static str, String)> = None;
    let mut rejected_output: Option<String> = None;
    for event in &events {
        match &event.data {
            EventData::MessageRecorded { node_id, text } => {
                detail.message = Some(MessageView { node_id: node_id.clone(), text: text.clone() });
            }
            EventData::ContextCarried { node_ids, .. } => detail.carried = node_ids.clone(),
            EventData::JevRequested { jev_id, request } => {
                detail.jev_id = Some(jev_id.clone());
                detail.jev_request = Some(request.clone());
            }
            EventData::JevFallback { jev_id, reason } => {
                // A rejection the fallback recovered from does not explain a later failure.
                rejection = None;
                detail.jev_fallback = Some(JevFallbackView {
                    from: detail.jev_id.clone(),
                    jev_id: jev_id.clone(),
                    reason: reason.clone(),
                    rejected_output: rejected_output.take(),
                });
            }
            EventData::JevClassified { classification } => {
                detail.classification = Some(classification.clone());
            }
            EventData::JevRejected { reason, output } => {
                rejection = Some(("jev", reason.clone()));
                rejected_output = Some(output.clone());
            }
            EventData::ContextAdded { node_id } => detail.context_added.push(node_id.clone()),
            EventData::ContextRemoved { node_id } => detail.context_removed.push(node_id.clone()),
            EventData::IrCompiled { ir } => detail.ir = Some(ir.clone()),
            EventData::InferenceDelta { text } => detail.streamed.push_str(text),
            EventData::InferenceCompleted { output } => detail.output = Some(output.clone()),
            EventData::ResponseValidated { response } => detail.response = Some(response.clone()),
            EventData::ResponseRejected { reason } => {
                rejection = Some(("response", reason.clone()));
            }
            EventData::DagNodeCreated { node_id, .. } => detail.emitted_nodes.push(node_id.clone()),
            EventData::DagEdgeCreated { edge_id, .. } => detail.emitted_edges.push(edge_id.clone()),
            EventData::RunFailed { error_code, message } => {
                detail.failure = Some(FailureView {
                    error_code: error_code.to_string(),
                    message: message.clone(),
                    rejected_stage: rejection.as_ref().map(|(stage, _)| *stage),
                    rejected_reason: rejection.as_ref().map(|(_, reason)| reason.clone()),
                });
            }
            EventData::RunStarted { .. }
            | EventData::InferenceStarted { .. }
            | EventData::RunCompleted {} => {}
        }
    }
    detail.events = events;
    Ok(Some(detail))
}
