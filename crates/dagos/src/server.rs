//! The local HTTP server: the DAGOS inspector UI, its JSON API, and a live stream of committed
//! events.
//!
//! The server is transport only. Views come from [`crate::inspect`], runs go through the core
//! runtime, and the event stream relays exactly what the store committed. By default it binds to
//! loopback and refuses requests addressed to any other host name, so web pages elsewhere cannot
//! reach it through DNS rebinding; mutations also require JSON bodies.

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, patch, post, put};
use dagos_core::domain::{ConversationId, Event, ModelId, ProjectId, ProviderId, RunConfig, RunId};
use dagos_core::runtime::{RuntimeError, Thread};
use dagos_core::store::{EventListener, StoreError};
use dagos_mcp::{McpServer, Policy};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

use crate::approvals::Answer;
use crate::inspect::{self, ConversationView, Overview, RunDetail, TurnItem};
use crate::keys::KeyStore;
use crate::providers::{
    self, JevEntry, Origin, ProviderEntry, ProviderKind, ProviderSetting, Settings,
};
use crate::workspace::Workspace;

/// Fans committed events out to live subscribers of `GET /api/stream`.
#[derive(Clone)]
pub struct EventHub {
    sender: broadcast::Sender<Event>,
}

impl EventHub {
    pub fn new() -> Self {
        Self { sender: broadcast::channel(4096).0 }
    }

    /// A store listener that publishes every committed event to the hub.
    pub fn listener(&self) -> EventListener {
        let sender = self.sender.clone();
        Box::new(move |events| {
            for event in events {
                // No subscribers is fine: the events are durable in the store regardless.
                let _ = sender.send(event.clone());
            }
        })
    }
}

impl Default for EventHub {
    fn default() -> Self {
        Self::new()
    }
}

struct AppState {
    workspace: Arc<Workspace>,
    hub: EventHub,
    /// Runs this process is executing right now; a `running` run outside this set is stale.
    executing: Mutex<BTreeSet<RunId>>,
    loopback_only: bool,
}

impl AppState {
    fn executing(&self) -> std::sync::MutexGuard<'_, BTreeSet<RunId>> {
        self.executing.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The UI and API routes over `workspace`. `loopback_only` enables the host-name guard.
fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/assets/{*path}", get(asset))
        .route("/api/health", get(health))
        .route("/api/overview", get(overview))
        .route("/api/runs", post(start_run))
        .route("/api/runs/{run}", get(run))
        .route("/api/config", put(update_config))
        .route("/api/recover", post(recover))
        .route("/api/stream", get(stream))
        .route("/api/projects", get(list_projects).post(create_project))
        .route("/api/projects/{project}", patch(rename_project))
        .route("/api/projects/{project}/overview", get(project_overview))
        .route("/api/projects/{project}/config", put(update_project_config))
        .route("/api/conversations/{conversation}", get(conversation).patch(update_conversation))
        .route("/api/runs/{run}/tools/{call}", post(answer_tool_call))
        .route("/api/tools", get(tools))
        .route("/api/tools/import", get(import_candidates))
        .route("/api/tools/servers/{id}", put(save_tool_server).delete(remove_tool_server))
        .route("/api/tools/servers/{id}/policy", put(set_tool_policy))
        .route("/api/settings", get(settings))
        .route("/api/settings/providers/{id}", put(save_provider).delete(remove_provider))
        .route("/api/settings/providers/{id}/key", put(save_key).delete(remove_key))
        .route("/api/settings/providers/{id}/check", post(check_provider))
        .route("/api/settings/jev", put(save_jev).delete(clear_jev))
        .layer(middleware::from_fn_with_state(state.clone(), guard_host))
        .with_state(state)
}

/// Serves the UI and API for `workspace` on an already bound listener. `hub` must be the hub
/// whose listener the workspace's store was opened with, so the stream sees its events.
pub async fn serve(
    listener: TcpListener,
    workspace: Arc<Workspace>,
    hub: EventHub,
) -> std::io::Result<()> {
    // The app can start processes and run tools on this computer and has no sign-in, so it never
    // listens beyond loopback; reach it remotely through something that authenticates (an SSH
    // tunnel, or a reverse proxy with sign-in).
    if !listener.local_addr()?.ip().is_loopback() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "DAGOS serves only on a loopback address (127.0.0.1 or ::1): the app can run commands              on this computer and has no sign-in. To reach it from elsewhere, use an SSH tunnel              or a reverse proxy that authenticates.",
        ));
    }
    let state =
        Arc::new(AppState { workspace, hub, executing: Mutex::default(), loopback_only: true });
    axum::serve(listener, router(state)).await
}

/// The host part of a `Host` header value, e.g. `127.0.0.1` for `127.0.0.1:7420`.
fn hostname(host: &str) -> &str {
    if host.starts_with('[') {
        return host.find(']').map_or(host, |end| &host[..=end]);
    }
    host.rsplit_once(':').map_or(host, |(name, _)| name)
}

async fn guard_host(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    if state.loopback_only {
        let host = request.headers().get(header::HOST).and_then(|value| value.to_str().ok());
        let allowed = host
            .map(hostname)
            .is_some_and(|name| matches!(name, "localhost" | "127.0.0.1" | "[::1]"));
        if !allowed {
            let message = "this DAGOS server only answers requests addressed to a loopback host";
            return ApiError::Forbidden(message.into()).into_response();
        }
    }
    next.run(request).await
}

/// The UI files, embedded at build time. `DAGOS_UI_DIR` serves them from disk instead (for UI
/// development without rebuilding).
const ASSETS: &[(&str, &str, &str)] = &[
    ("index.html", "text/html; charset=utf-8", include_str!("../ui/index.html")),
    ("styles.css", "text/css; charset=utf-8", include_str!("../ui/styles.css")),
    ("js/app.js", "text/javascript; charset=utf-8", include_str!("../ui/js/app.js")),
    ("js/api.js", "text/javascript; charset=utf-8", include_str!("../ui/js/api.js")),
    ("js/model.js", "text/javascript; charset=utf-8", include_str!("../ui/js/model.js")),
    ("js/view.js", "text/javascript; charset=utf-8", include_str!("../ui/js/view.js")),
    ("js/settings.js", "text/javascript; charset=utf-8", include_str!("../ui/js/settings.js")),
    ("js/chat.js", "text/javascript; charset=utf-8", include_str!("../ui/js/chat.js")),
    ("js/markdown.js", "text/javascript; charset=utf-8", include_str!("../ui/js/markdown.js")),
];

fn asset_response(path: &str) -> Response {
    let Some((name, content_type, embedded)) = ASSETS.iter().find(|(name, _, _)| *name == path)
    else {
        return ApiError::NotFound(format!("no asset `{path}`")).into_response();
    };
    let body = match std::env::var_os("DAGOS_UI_DIR") {
        Some(dir) => std::fs::read_to_string(PathBuf::from(dir).join(name))
            .unwrap_or_else(|_| (*embedded).to_owned()),
        None => (*embedded).to_owned(),
    };
    let mut response = Response::new(Body::from(body));
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; img-src 'self' data:; frame-ancestors 'none'",
        ),
    );
    response
}

async fn index() -> Response {
    asset_response("index.html")
}

async fn asset(Path(path): Path<String>) -> Response {
    asset_response(&path)
}

async fn health(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let schema_version = state.workspace.store.schema_version()?;
    let project = &state.workspace.project.id;
    Ok(Json(json!({"ok": true, "schema_version": schema_version, "project": project})))
}

/// The overview plus the runs this server is executing, so clients can tell a live run from one
/// a crashed process left behind.
#[derive(Serialize)]
struct ServerOverview {
    #[serde(flatten)]
    overview: Overview,
    executing: Vec<RunId>,
}

async fn overview(State(state): State<Arc<AppState>>) -> Result<Json<ServerOverview>, ApiError> {
    let overview = inspect::overview(&state.workspace)?;
    let executing = state.executing().iter().cloned().collect();
    Ok(Json(ServerOverview { overview, executing }))
}

async fn run(
    State(state): State<Arc<AppState>>,
    Path(reference): Path<String>,
) -> Result<Json<RunDetail>, ApiError> {
    let workspace = &state.workspace;
    let not_found = || ApiError::NotFound(format!("run `{reference}` not found"));
    // A run ID names a run in any project; `latest` means the default project's latest run.
    let id = match RunId::parse(reference.as_str()) {
        Ok(id) => id,
        Err(_) => inspect::resolve_run(&workspace.store, &workspace.project.id, &reference)?
            .ok_or_else(not_found)?,
    };
    let mut detail = inspect::run_detail(&workspace.store, &id)?.ok_or_else(not_found)?;
    for call in &mut detail.tool_calls {
        call.pending = workspace.approvals().is_pending(&id, &call.call_id);
        let escalation = workspace.approvals().escalation(&id, &call.call_id);
        call.pending_note = escalation.as_ref().and_then(|e| e.note.clone());
        call.pending_guarded = escalation.is_some_and(|e| e.guarded);
    }
    Ok(Json(detail))
}

/// `POST /api/runs`: the message, plus optional one-off overrides of the run defaults.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartRequest {
    message: String,
    /// Continue this conversation.
    conversation_id: Option<String>,
    /// Otherwise run in this project (default: the workspace's default project)...
    project_id: Option<String>,
    /// ...in a new conversation titled after the message, or else its latest conversation.
    #[serde(default)]
    new_conversation: bool,
    provider_id: Option<String>,
    model_id: Option<String>,
    system_prompt: Option<String>,
}

async fn start_run(
    State(state): State<Arc<AppState>>,
    Json(request): Json<StartRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let message = request.message.trim();
    if message.is_empty() {
        return Err(ApiError::BadRequest("the message is empty".into()));
    }
    let conversation_id = request
        .conversation_id
        .map(|id| ConversationId::parse(id).map_err(ApiError::bad_request))
        .transpose()?;
    let project_id = match (&conversation_id, request.project_id) {
        (Some(id), _) => {
            let conversation = state.workspace.store.transaction(|tx| tx.conversation(id))?;
            conversation
                .ok_or_else(|| ApiError::NotFound(format!("conversation `{id}` not found")))?
                .project_id
        }
        (None, Some(id)) => existing_project(&state, &id)?,
        (None, None) => state.workspace.project.id.clone(),
    };
    let thread = match &conversation_id {
        Some(id) => Thread::Conversation(id),
        None if request.new_conversation => Thread::New(&project_id),
        None => Thread::Latest(&project_id),
    };
    let mut config = state.workspace.run_config_for(&project_id)?;
    if let Some(provider) = request.provider_id {
        config.provider_id = ProviderId::parse(provider).map_err(ApiError::bad_request)?;
    }
    if let Some(model) = request.model_id {
        config.model_id = ModelId::parse(model).map_err(ApiError::bad_request)?;
    }
    if let Some(prompt) = request.system_prompt {
        config.system_prompt = prompt;
    }
    let runtime = state.workspace.runtime();
    // Recording the run and marking it executing happen under one lock, so a concurrent
    // `/api/recover` can never see it running but not executing and fail it as interrupted.
    let started = {
        let mut executing = state.executing();
        let started = runtime.start_in(thread, message, &config)?;
        executing.insert(started.run.id.clone());
        started
    };
    let run = started.run.clone();
    let task_state = state.clone();
    tokio::spawn(async move {
        let id = started.run.id.clone();
        // The outcome is recorded on the run and its events; a store failure leaves the run
        // `running`, where recovery will find it.
        let _ = runtime.finish(started).await;
        task_state.executing().remove(&id);
    });
    Ok((StatusCode::ACCEPTED, Json(json!({"run": run}))))
}

/// `PUT /api/config`: the provider, model, and system prompt for subsequent runs.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigRequest {
    provider_id: String,
    model_id: String,
    system_prompt: String,
}

async fn update_config(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ConfigRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let project = state.workspace.project.id.clone();
    save_config(&state, &project, request)
}

/// `PUT /api/projects/{project}/config`: that project's configuration for subsequent runs.
async fn update_project_config(
    State(state): State<Arc<AppState>>,
    Path(project): Path<String>,
    Json(request): Json<ConfigRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let project = existing_project(&state, &project)?;
    save_config(&state, &project, request)
}

fn save_config(
    state: &AppState,
    project: &ProjectId,
    request: ConfigRequest,
) -> Result<Json<serde_json::Value>, ApiError> {
    let config = RunConfig {
        provider_id: ProviderId::parse(request.provider_id).map_err(ApiError::bad_request)?,
        model_id: ModelId::parse(request.model_id).map_err(ApiError::bad_request)?,
        system_prompt: request.system_prompt,
    };
    state.workspace.runtime().set_defaults(project, &config)?;
    Ok(Json(json!({"run_defaults": state.workspace.run_config_for(project)?})))
}

/// The ID of an existing project, or a 404.
fn existing_project(state: &AppState, id: &str) -> Result<ProjectId, ApiError> {
    let id = ProjectId::parse(id).map_err(ApiError::bad_request)?;
    match state.workspace.store.transaction(|tx| tx.project(&id))? {
        Some(project) => Ok(project.id),
        None => Err(ApiError::NotFound(format!("project `{id}` not found"))),
    }
}

/// `GET /api/projects`: every project, oldest first.
async fn list_projects(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let projects = inspect::projects(&state.workspace.store)?;
    Ok(Json(json!({"projects": projects, "default": state.workspace.project.id})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectRequest {
    name: String,
}

/// `POST /api/projects`: a new project (its own DAG), starting with the default project's run
/// configuration.
async fn create_project(
    State(state): State<Arc<AppState>>,
    Json(request): Json<ProjectRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let defaults = state.workspace.run_config()?;
    let project =
        state.workspace.create_project(&request.name, &defaults).map_err(ApiError::BadRequest)?;
    Ok((StatusCode::CREATED, Json(json!({"project": project}))))
}

/// `PATCH /api/projects/{project}`: renames a project.
async fn rename_project(
    State(state): State<Arc<AppState>>,
    Path(project): Path<String>,
    Json(request): Json<ProjectRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let id = existing_project(&state, &project)?;
    let project = state.workspace.store.transaction(|tx| tx.rename_project(&id, &request.name))?;
    Ok(Json(json!({"project": project})))
}

/// `GET /api/projects/{project}/overview`.
async fn project_overview(
    State(state): State<Arc<AppState>>,
    Path(project): Path<String>,
) -> Result<Json<ServerOverview>, ApiError> {
    let id = existing_project(&state, &project)?;
    let overview = inspect::project_overview(&state.workspace, &id)?
        .ok_or_else(|| ApiError::NotFound(format!("project `{id}` not found")))?;
    let executing = state.executing().iter().cloned().collect();
    Ok(Json(ServerOverview { overview, executing }))
}

/// `GET /api/conversations/{conversation}`: the conversation as chat turns.
async fn conversation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<ConversationView>, ApiError> {
    let id = ConversationId::parse(id).map_err(ApiError::bad_request)?;
    let mut view = inspect::conversation(&state.workspace.store, &id)?
        .ok_or_else(|| ApiError::NotFound(format!("conversation `{id}` not found")))?;
    let approvals = state.workspace.approvals();
    for turn in &mut view.turns {
        for item in &mut turn.items {
            if let TurnItem::Tool(call) = item {
                call.pending = approvals.is_pending(&turn.run.id, &call.call_id);
                let escalation = approvals.escalation(&turn.run.id, &call.call_id);
                call.pending_note = escalation.as_ref().and_then(|e| e.note.clone());
                call.pending_guarded = escalation.is_some_and(|e| e.guarded);
            }
        }
    }
    Ok(Json(view))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationRequest {
    title: Option<String>,
    archived: Option<bool>,
}

/// `PATCH /api/conversations/{conversation}`: renames, archives, or restores a conversation.
async fn update_conversation(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<ConversationRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let id = ConversationId::parse(id).map_err(ApiError::bad_request)?;
    let conversation = state.workspace.store.transaction(|tx| {
        let mut conversation = tx
            .conversation(&id)?
            .ok_or_else(|| StoreError::NotFound { kind: "conversation", id: id.to_string() })?;
        if let Some(title) = &request.title {
            conversation = tx.rename_conversation(&id, title)?;
        }
        if let Some(archived) = request.archived {
            conversation = tx.set_conversation_archived(&id, archived)?;
        }
        Ok::<_, StoreError>(conversation)
    })?;
    Ok(Json(json!({"conversation": conversation})))
}

/// `POST /api/recover`: fails runs left `running` by a process that is gone.
async fn recover(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    // Held while recovering, so no run can start (and not yet be marked executing) meanwhile.
    let executing = state.executing();
    let recovered = state.workspace.runtime().recover_runs_except(&executing)?;
    drop(executing);
    Ok(Json(json!({"recovered": recovered})))
}

/// `GET /api/settings`: providers, key status (never keys), and the Jev.
async fn settings(State(state): State<Arc<AppState>>) -> Json<Settings> {
    Json(state.workspace.settings())
}

/// Applies a configuration edit, reloads the providers, and returns the new settings.
fn edit(
    state: &AppState,
    change: impl FnOnce(&Workspace) -> Result<(), ApiError>,
) -> Result<Json<Settings>, ApiError> {
    let workspace = &state.workspace;
    let _guard = workspace.edit_lock();
    change(workspace)?;
    workspace.reload().map_err(ApiError::BadRequest)?;
    Ok(Json(workspace.settings()))
}

/// A provider shown in the settings, other than the built-in fake.
fn configurable(workspace: &Workspace, id: &str) -> Result<ProviderSetting, ApiError> {
    let id = ProviderId::parse(id).map_err(ApiError::bad_request)?;
    let setting = workspace.settings().providers.into_iter().find(|setting| setting.id == id);
    match setting {
        Some(setting) if setting.origin == Origin::Builtin => {
            Err(ApiError::BadRequest(format!("`{id}` is built in and needs no configuration")))
        }
        Some(setting) => Ok(setting),
        None => Err(ApiError::NotFound(format!("no provider `{id}`"))),
    }
}

fn key_store(workspace: &Workspace) -> Result<&KeyStore, ApiError> {
    workspace.keys().ok_or_else(|| {
        let message = "no user configuration directory to save keys in; set DAGOS_CONFIG_DIR";
        ApiError::Conflict(message.into())
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyRequest {
    key: String,
}

/// `PUT /api/settings/providers/{id}/key`: saves a key for this user.
async fn save_key(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<KeyRequest>,
) -> Result<Json<Settings>, ApiError> {
    edit(&state, |workspace| {
        let provider = configurable(workspace, &id)?;
        key_store(workspace)?.set(&provider.id, &request.key).map_err(ApiError::BadRequest)
    })
}

/// `DELETE /api/settings/providers/{id}/key`: forgets the saved key.
async fn remove_key(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Settings>, ApiError> {
    edit(&state, |workspace| {
        let provider = configurable(workspace, &id)?;
        key_store(workspace)?.remove(&provider.id).map_err(ApiError::Internal)?;
        Ok(())
    })
}

/// `PUT /api/settings/providers/{id}`: a custom OpenAI-compatible endpoint.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderRequest {
    base_url: String,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default)]
    models: Vec<String>,
    #[serde(default = "enabled")]
    json_mode: bool,
}

fn enabled() -> bool {
    true
}

async fn save_provider(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<ProviderRequest>,
) -> Result<Json<Settings>, ApiError> {
    let id = ProviderId::parse(id).map_err(ApiError::bad_request)?;
    let models = request
        .models
        .iter()
        .map(|model| model.trim())
        .filter(|model| !model.is_empty())
        .map(ModelId::parse)
        .collect::<Result<Vec<_>, _>>()
        .map_err(ApiError::bad_request)?;
    let api_key_env =
        request.api_key_env.map(|name| name.trim().to_owned()).filter(|name| !name.is_empty());
    let entry = ProviderEntry {
        id,
        kind: ProviderKind::OpenaiCompatible,
        base_url: request.base_url.trim().to_owned(),
        api_key_env,
        models,
        json_mode: request.json_mode,
    };
    edit(&state, |workspace| {
        providers::save_custom(workspace.dir(), entry).map_err(ApiError::BadRequest)
    })
}

/// `DELETE /api/settings/providers/{id}`: removes a custom endpoint and its saved key.
async fn remove_provider(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<Settings>, ApiError> {
    edit(&state, |workspace| {
        let provider = configurable(workspace, &id)?;
        if provider.origin != Origin::Custom {
            return Err(ApiError::BadRequest(format!("`{id}` is a preset and cannot be removed")));
        }
        providers::remove_custom(workspace.dir(), &provider.id).map_err(ApiError::Internal)?;
        // An overridden preset keeps its key; a custom endpoint's key goes with it.
        let is_preset = providers::PRESETS.iter().any(|preset| preset.id == provider.id.as_str());
        if let (false, Some(keys)) = (is_preset, workspace.keys()) {
            keys.remove(&provider.id).map_err(ApiError::Internal)?;
        }
        Ok(())
    })
}

/// `POST /api/settings/providers/{id}/check`: tests the connection and key, listing models.
async fn check_provider(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let provider = configurable(&state.workspace, &id)?;
    let endpoint = state.workspace.endpoint(&provider.id).ok_or_else(|| {
        ApiError::BadRequest(format!("`{id}` has no API key yet; add one to connect"))
    })?;
    let models = endpoint.chat.check(endpoint.key_check).await.map_err(ApiError::BadGateway)?;
    Ok(Json(json!({"ok": true, "models": models})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JevChoice {
    provider: String,
    model: String,
}

/// `PUT /api/settings/jev`: classify context with a model; the offline policy stays the fallback.
async fn save_jev(
    State(state): State<Arc<AppState>>,
    Json(request): Json<JevChoice>,
) -> Result<Json<Settings>, ApiError> {
    let model = ModelId::parse(request.model.trim()).map_err(ApiError::bad_request)?;
    edit(&state, |workspace| {
        let provider = configurable(workspace, &request.provider)?;
        let entry = JevEntry { provider: provider.id, model };
        providers::set_jev(workspace.dir(), Some(entry)).map_err(ApiError::Internal)
    })
}

/// `DELETE /api/settings/jev`: classify with the offline policy only.
async fn clear_jev(State(state): State<Arc<AppState>>) -> Result<Json<Settings>, ApiError> {
    edit(&state, |workspace| providers::set_jev(workspace.dir(), None).map_err(ApiError::Internal))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolAnswer {
    /// `allow` or `deny`.
    decision: String,
    /// With `allow`: also set the tool's policy to `allow`, so later calls run without asking.
    #[serde(default)]
    remember: bool,
}

/// `POST /api/runs/{run}/tools/{call}`: a person's answer to a call waiting for approval.
async fn answer_tool_call(
    State(state): State<Arc<AppState>>,
    Path((run, call)): Path<(String, String)>,
    Json(request): Json<ToolAnswer>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let run = RunId::parse(run).map_err(ApiError::bad_request)?;
    let answer = match request.decision.as_str() {
        "allow" => Answer::Allow,
        "deny" => Answer::Deny,
        other => return Err(ApiError::BadRequest(format!("unknown decision `{other}`"))),
    };
    let workspace = &state.workspace;
    if !workspace.approvals().is_pending(&run, &call) {
        return Err(ApiError::Conflict(format!("call `{call}` of run `{run}` is not waiting")));
    }
    if request.remember && answer == Answer::Allow {
        let escalation = workspace.approvals().escalation(&run, &call).unwrap_or_default();
        if escalation.guarded {
            return Err(ApiError::Conflict(
                "Jev flagged this call, so it asks every time; allow it once instead".into(),
            ));
        }
        let name = inspect::run_detail(&workspace.store, &run)?
            .and_then(|detail| detail.tool_calls.into_iter().find(|c| c.call_id == call))
            .map(|call| call.name)
            .ok_or_else(|| ApiError::NotFound(format!("call `{call}` not found")))?;
        let _edit = workspace.tool_edit_lock().await;
        let mut config = workspace.mcp_config().map_err(ApiError::Internal)?;
        // The call's own tool, and the tools it names that made it ask.
        for name in std::iter::once(&name).chain(&escalation.asking) {
            if let Some((server_id, tool)) = name.split_once('.')
                && let Some(server) = config.servers.iter_mut().find(|s| s.id == server_id)
            {
                server.tools.insert(tool.to_owned(), Policy::Allow);
            }
        }
        workspace.set_tool_policies(config).map_err(ApiError::Internal)?;
    }
    let answered = workspace.approvals().answer(&run, &call, answer);
    Ok(Json(json!({"answered": answered})))
}

/// The tools view: the configuration, what each server offers, and where it is stored.
fn tools_view(workspace: &Workspace) -> Result<Json<serde_json::Value>, ApiError> {
    let config = workspace.mcp_config().map_err(ApiError::Internal)?;
    Ok(Json(json!({
        "config": config,
        "capabilities": workspace.capabilities(),
        "file": workspace.dir().join(crate::workspace::MCP_FILE),
    })))
}

/// `GET /api/tools`.
async fn tools(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    tools_view(&state.workspace)
}

/// Claude Desktop's configuration file for this user, if the platform has one.
fn claude_desktop_config() -> Option<PathBuf> {
    let var = |name| std::env::var_os(name).filter(|value| !value.is_empty()).map(PathBuf::from);
    let path = if cfg!(windows) {
        var("APPDATA")?.join("Claude")
    } else if cfg!(target_os = "macos") {
        var("HOME")?.join("Library/Application Support/Claude")
    } else {
        var("XDG_CONFIG_HOME")
            .or_else(|| var("HOME").map(|home| home.join(".config")))?
            .join("Claude")
    };
    Some(path.join("claude_desktop_config.json"))
}

/// MCP servers another app already configures, offered for import: id, command, args, and
/// working directory only. Environment values are never read or copied (they often hold keys);
/// servers that rely on them are flagged.
fn import_candidates_from(
    path: &std::path::Path,
    existing: &[McpServer],
) -> Vec<serde_json::Value> {
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(config) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    let Some(servers) = config.get("mcpServers").and_then(serde_json::Value::as_object) else {
        return Vec::new();
    };
    servers
        .iter()
        .filter_map(|(name, server)| {
            let command = server.get("command")?.as_str()?.to_owned();
            let id: String = name
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
                .collect();
            let args: Vec<String> = server
                .get("args")
                .and_then(serde_json::Value::as_array)
                .map(|args| args.iter().filter_map(|arg| arg.as_str().map(str::to_owned)).collect())
                .unwrap_or_default();
            let needs_env = server
                .get("env")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|env| !env.is_empty());
            Some(json!({
                "id": id,
                "name": name,
                "command": command,
                "args": args,
                "cwd": server.get("cwd").and_then(serde_json::Value::as_str),
                "needs_env": needs_env,
                "added": existing.iter().any(|existing| existing.id == id),
            }))
        })
        .collect()
}

/// `GET /api/tools/import`: MCP servers configured in Claude Desktop, for one-click import.
async fn import_candidates(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let existing = state.workspace.mcp_config().map_err(ApiError::Internal)?.servers;
    let source = claude_desktop_config();
    let candidates =
        source.as_deref().map(|path| import_candidates_from(path, &existing)).unwrap_or_default();
    Ok(Json(json!({"source": source, "servers": candidates})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolServerRequest {
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default = "enabled")]
    enabled: bool,
}

/// `PUT /api/tools/servers/{id}`: adds or changes an MCP server (keeping its tool policies) and
/// restarts the tools.
async fn save_tool_server(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<ToolServerRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !dagos_mcp::valid_server_id(&id) {
        return Err(ApiError::BadRequest(format!(
            "invalid server id `{id}`: use letters, digits, - and _ (not `dagos`)"
        )));
    }
    let workspace = &state.workspace;
    let _edit = workspace.tool_edit_lock().await;
    let mut config = workspace.mcp_config().map_err(ApiError::Internal)?;
    let cwd = request.cwd.map(|cwd| cwd.trim().to_owned()).filter(|cwd| !cwd.is_empty());
    let args: Vec<String> = request.args.into_iter().filter(|arg| !arg.is_empty()).collect();
    match config.servers.iter_mut().find(|server| server.id == id) {
        Some(server) => {
            server.command = request.command.trim().to_owned();
            server.args = args;
            server.cwd = cwd;
            server.enabled = request.enabled;
        }
        None => {
            let mut server = McpServer::new(id, request.command.trim(), args);
            server.cwd = cwd;
            server.enabled = request.enabled;
            config.servers.push(server);
        }
    }
    config.save(&workspace.dir().join(crate::workspace::MCP_FILE)).map_err(ApiError::BadRequest)?;
    workspace.start_tools(|_| {}).await.map_err(ApiError::Internal)?;
    tools_view(workspace)
}

/// `DELETE /api/tools/servers/{id}`: removes an MCP server and restarts the tools.
async fn remove_tool_server(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace = &state.workspace;
    let _edit = workspace.tool_edit_lock().await;
    let mut config = workspace.mcp_config().map_err(ApiError::Internal)?;
    let before = config.servers.len();
    config.servers.retain(|server| server.id != id);
    if config.servers.len() == before {
        return Err(ApiError::NotFound(format!("no MCP server `{id}`")));
    }
    config.save(&workspace.dir().join(crate::workspace::MCP_FILE)).map_err(ApiError::Internal)?;
    workspace.start_tools(|_| {}).await.map_err(ApiError::Internal)?;
    tools_view(workspace)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyRequest {
    /// One tool, by the server's own name; without it, every tool of the server.
    #[serde(default)]
    tool: Option<String>,
    policy: Policy,
}

/// `PUT /api/tools/servers/{id}/policy`: sets one tool's policy, or every tool's, without
/// restarting the server.
async fn set_tool_policy(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(request): Json<PolicyRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let workspace = &state.workspace;
    let _edit = workspace.tool_edit_lock().await;
    let mut config = workspace.mcp_config().map_err(ApiError::Internal)?;
    let server = config
        .servers
        .iter_mut()
        .find(|server| server.id == id)
        .ok_or_else(|| ApiError::NotFound(format!("no MCP server `{id}`")))?;
    match request.tool {
        Some(tool) => {
            server.tools.insert(tool, request.policy);
        }
        None => {
            server.policy = request.policy;
            server.tools.clear();
        }
    }
    workspace.set_tool_policies(config).map_err(ApiError::Internal)?;
    tools_view(workspace)
}

/// `GET /api/stream`: server-sent events. Each `event` message carries one committed event as
/// JSON; `resync` tells a client that fell behind to reload.
async fn stream(
    State(state): State<Arc<AppState>>,
) -> Sse<impl tokio_stream::Stream<Item = Result<SseEvent, Infallible>>> {
    let events = BroadcastStream::new(state.hub.sender.subscribe()).map(|item| {
        Ok(match item {
            Ok(event) => SseEvent::default()
                .event("event")
                .json_data(&event)
                .unwrap_or_else(|_| SseEvent::default().event("resync").data("{}")),
            Err(BroadcastStreamRecvError::Lagged(_)) => {
                SseEvent::default().event("resync").data("{}")
            }
        })
    });
    Sse::new(events).keep_alive(KeepAlive::default())
}

/// An API failure, reported as `{"error": message}` with a matching status.
#[derive(Debug)]
enum ApiError {
    BadRequest(String),
    Forbidden(String),
    NotFound(String),
    Conflict(String),
    Internal(String),
    /// An upstream endpoint failed a connection check.
    BadGateway(String),
}

impl ApiError {
    fn bad_request(error: impl ToString) -> Self {
        ApiError::BadRequest(error.to_string())
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::RunInProgress { .. } => ApiError::Conflict(error.to_string()),
            StoreError::Invalid(_) => ApiError::BadRequest(error.to_string()),
            StoreError::NotFound { .. } => ApiError::NotFound(error.to_string()),
            other => ApiError::Internal(other.to_string()),
        }
    }
}

impl From<RuntimeError> for ApiError {
    fn from(error: RuntimeError) -> Self {
        match error {
            RuntimeError::UnknownProvider(_) => ApiError::BadRequest(error.to_string()),
            RuntimeError::Store(error) => error.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            ApiError::Forbidden(message) => (StatusCode::FORBIDDEN, message),
            ApiError::NotFound(message) => (StatusCode::NOT_FOUND, message),
            ApiError::Conflict(message) => (StatusCode::CONFLICT, message),
            ApiError::Internal(message) => (StatusCode::INTERNAL_SERVER_ERROR, message),
            ApiError::BadGateway(message) => (StatusCode::BAD_GATEWAY, message),
        };
        (status, Json(json!({"error": message}))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::{McpServer, hostname, import_candidates_from};

    #[test]
    fn claude_desktop_servers_import_without_their_environment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude_desktop_config.json");
        std::fs::write(
            &path,
            r#"{"mcpServers": {
                "ooda-computer": {"command": "node", "args": ["C:/OODA/dist/index.js"], "cwd": "C:/OODA"},
                "github": {"command": "npx", "args": ["-y", "gh-mcp"], "env": {"GITHUB_TOKEN": "secret-token"}},
                "broken": {"args": []}
            }}"#,
        )
        .unwrap();
        let existing = [McpServer::new("github", "npx", vec![])];
        let candidates = import_candidates_from(&path, &existing);
        assert_eq!(candidates.len(), 2, "entries without a command are skipped");
        let ooda = candidates.iter().find(|c| c["name"] == "ooda-computer").unwrap();
        assert_eq!(ooda["id"], "ooda-computer");
        assert_eq!(ooda["cwd"], "C:/OODA");
        assert_eq!(
            (ooda["needs_env"].as_bool(), ooda["added"].as_bool()),
            (Some(false), Some(false))
        );
        let github = candidates.iter().find(|c| c["name"] == "github").unwrap();
        assert_eq!(
            (github["needs_env"].as_bool(), github["added"].as_bool()),
            (Some(true), Some(true))
        );
        assert!(!serde_json::to_string(&candidates).unwrap().contains("secret-token"));
        assert!(import_candidates_from(&dir.path().join("missing.json"), &[]).is_empty());
    }

    #[test]
    fn host_names_are_extracted_from_host_headers() {
        assert_eq!(hostname("127.0.0.1:7420"), "127.0.0.1");
        assert_eq!(hostname("localhost"), "localhost");
        assert_eq!(hostname("[::1]:7420"), "[::1]");
        assert_eq!(hostname("[::1]"), "[::1]");
        assert_eq!(hostname("evil.example:7420"), "evil.example");
    }
}
