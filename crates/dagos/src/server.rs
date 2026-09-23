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
    let started = state.workspace.runtime.start(&state.workspace.project.id, message, &config)?;
    let run = started.run.clone();
    state.executing().insert(run.id.clone());
    let task_state = state.clone();
    tokio::spawn(async move {
        let id = started.run.id.clone();
        // The outcome is recorded on the run and its events; a store failure leaves the run
        // `running`, where recovery will find it.
        let _ = task_state.workspace.runtime.finish(started).await;
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
    state.workspace.runtime.set_defaults(&state.workspace.project.id, &config)?;
    Ok(Json(json!({"run_defaults": state.workspace.run_config()?})))
}

/// `POST /api/recover`: fails runs left `running` by a process that is gone.
async fn recover(State(state): State<Arc<AppState>>) -> Result<Json<serde_json::Value>, ApiError> {
    let executing = state.executing().clone();
    let recovered = state.workspace.runtime.recover_runs_except(&executing)?;
    Ok(Json(json!({"recovered": recovered})))
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
