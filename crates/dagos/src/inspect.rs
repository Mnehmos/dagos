//! Read-only views of a workspace: the durable DAG, runs, events, active context, Jev output, IR,
//! and responses, assembled from the store so neither people nor tools have to read tables.
//!
//! Views are projections of recorded state built from core domain types, so they serialize
//! exactly as the DAGOS contracts describe. Nothing here mutates the store.

use dagos_core::domain::{
    ContextClassification, ContextMember, DagEdge, DagNode, EdgeId, Event, EventData, InferenceIr,
    InferenceResponse, JevRequest, ModelId, NodeId, Project, ProjectId, ProviderId, Run, RunConfig,
    RunId,
};
use dagos_core::store::{Store, StoreError};
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
    /// The durable DAG.
    pub dag: DagView,
    /// Every run, oldest first.
    pub runs: Vec<RunSummary>,
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

/// Everything recorded about one run.
#[derive(Debug, Serialize)]
pub struct RunDetail {
    pub run: Run,
    pub message: Option<MessageView>,
    /// The run's active context in IR order, with why each node is a member.
    pub context: Vec<ContextMember>,
    /// Nodes carried over from the previous run's context before Jev ran.
    pub carried: Vec<NodeId>,
    pub jev_request: Option<JevRequest>,
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

/// Builds the overview of `workspace`.
pub fn overview(workspace: &Workspace) -> Result<Overview, StoreError> {
    let project_id = &workspace.project.id;
    let (dag, runs) = workspace.store.transaction(|tx| {
        let dag = DagView { nodes: tx.nodes(project_id)?, edges: tx.edges(project_id)? };
        let mut runs = Vec::new();
        for run in tx.runs(project_id)? {
            let message = tx.events(&run.id)?.into_iter().find_map(|event| match event.data {
                EventData::MessageRecorded { text, .. } => Some(text),
                _ => None,
            });
            runs.push(RunSummary { run, message });
        }
        Ok::<_, StoreError>((dag, runs))
    })?;
    let run_defaults = workspace.run_config()?;
    let providers = workspace
        .runtime
        .providers()
        .map(|provider| ProviderInfo {
            id: provider.id().clone(),
            suggested_models: provider.suggested_models(),
        })
        .collect();
    Ok(Overview {
        project: workspace.project.clone(),
        run_defaults,
        providers,
        jev: workspace.runtime.jev().id().to_owned(),
        dag,
        runs,
    })
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
        jev_request: None,
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
    for event in &events {
        match &event.data {
            EventData::MessageRecorded { node_id, text } => {
                detail.message = Some(MessageView { node_id: node_id.clone(), text: text.clone() });
            }
            EventData::ContextCarried { node_ids, .. } => detail.carried = node_ids.clone(),
            EventData::JevRequested { request, .. } => detail.jev_request = Some(request.clone()),
            EventData::JevClassified { classification } => {
                detail.classification = Some(classification.clone());
            }
            EventData::JevRejected { reason, .. } => rejection = Some(("jev", reason.clone())),
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
