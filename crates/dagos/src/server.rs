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
use axum::routing::{get, post, put};
use dagos_core::domain::{Event, ModelId, ProviderId, RunId};
use dagos_core::runtime::RuntimeError;
use dagos_core::store::{EventListener, StoreError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

use crate::inspect::{self, Overview, RunDetail};
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
    let loopback_only = listener.local_addr()?.ip().is_loopback();
    let state = Arc::new(AppState { workspace, hub, executing: Mutex::default(), loopback_only });
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
    let id = inspect::resolve_run(&workspace.store, &workspace.project.id, &reference)?
        .ok_or_else(not_found)?;
    Ok(Json(inspect::run_detail(&workspace.store, &id)?.ok_or_else(not_found)?))
}

/// `POST /api/runs`: the message, plus optional one-off overrides of the run defaults.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartRequest {
    message: String,
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
    let mut config = state.workspace.run_config()?;
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
    let started = runtime.start(&state.workspace.project.id, message, &config)?;
    let run = started.run.clone();
    state.executing().insert(run.id.clone());
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
    let config = dagos_core::domain::RunConfig {
        provider_id: ProviderId::parse(request.provider_id).map_err(ApiError::bad_request)?,
        model_id: ModelId::parse(request.model_id).map_err(ApiError::bad_request)?,
        system_prompt: request.system_prompt,
    };
    state.workspace.runtime().set_defaults(&state.workspace.project.id, &config)?;
    Ok(Json(json!({"run_defaults": state.workspace.run_config()?})))
}

/// `POST /api/recover`: fails runs left `running` by a process that is gone.
async fn recover(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let executing = state.executing().clone();
    let recovered = state.workspace.runtime().recover_runs_except(&executing)?;
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
    use super::hostname;

    #[test]
    fn host_names_are_extracted_from_host_headers() {
        assert_eq!(hostname("127.0.0.1:7420"), "127.0.0.1");
        assert_eq!(hostname("localhost"), "localhost");
        assert_eq!(hostname("[::1]:7420"), "[::1]");
        assert_eq!(hostname("[::1]"), "[::1]");
        assert_eq!(hostname("evil.example:7420"), "evil.example");
    }
}
