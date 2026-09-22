//! The local HTTP server: DAGOS inspection as JSON over HTTP, for the inspector UI and tools.
//!
//! It binds to loopback by default and serves views built from the store; it never reads or writes
//! tables directly and adds no semantics of its own.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use dagos_core::store::StoreError;
use serde_json::json;
use tokio::net::TcpListener;

use crate::inspect::{self, Overview, RunDetail};
use crate::workspace::Workspace;

/// The API routes over `workspace`.
pub fn router(workspace: Arc<Workspace>) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/overview", get(overview))
        .route("/api/runs/{run}", get(run))
        .with_state(workspace)
}

/// Serves `workspace` on an already bound listener until the process stops.
pub async fn serve(listener: TcpListener, workspace: Arc<Workspace>) -> std::io::Result<()> {
    axum::serve(listener, router(workspace)).await
}

async fn health(
    State(workspace): State<Arc<Workspace>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let schema_version = workspace.store.schema_version()?;
    Ok(Json(json!({"ok": true, "schema_version": schema_version, "project": workspace.project.id})))
}

async fn overview(State(workspace): State<Arc<Workspace>>) -> Result<Json<Overview>, ApiError> {
    Ok(Json(inspect::overview(&workspace)?))
}

async fn run(
    State(workspace): State<Arc<Workspace>>,
    Path(reference): Path<String>,
) -> Result<Json<RunDetail>, ApiError> {
    let not_found = || ApiError::NotFound(format!("run `{reference}` not found"));
    let id = inspect::resolve_run(&workspace.store, &workspace.project.id, &reference)?
        .ok_or_else(not_found)?;
    let detail = inspect::run_detail(&workspace.store, &id)?.ok_or_else(not_found)?;
    Ok(Json(detail))
}

/// An API failure, reported as `{"error": message}` with a matching status.
#[derive(Debug)]
enum ApiError {
    NotFound(String),
    Internal(String),
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        ApiError::Internal(error.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::NotFound(message) => (StatusCode::NOT_FOUND, message),
            ApiError::Internal(message) => (StatusCode::INTERNAL_SERVER_ERROR, message),
        };
        (status, Json(json!({"error": message}))).into_response()
    }
}
