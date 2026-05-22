//! HTTP surface for the template registry.
//!
//! Two read-only endpoints:
//!   - `GET /api/templates`        → `{ templates: [TemplateSummary] }`
//!   - `GET /api/templates/:id`    → full `TemplateSummary` for one id
//!
//! Both are pure registry lookups — no I/O, no awaits on shared state, no
//! locks (the registry is built once at startup and never mutated). Safe to
//! call from any request thread.

use axum::{
    extract::{Path, State},
    routing::get,
    Json, Router,
};
use serde::Serialize;

use crate::error::ApiError;
use crate::templates::{TemplateId, TemplateSummary};
use crate::AppState;

/// Build the templates subrouter. Nested under `/api` by `crate::router`.
pub fn templates_router() -> Router<AppState> {
    Router::new()
        .route("/templates", get(list_templates))
        .route("/templates/:id", get(get_template))
}

/// Wire shape for `GET /api/templates`. Wrapped in a struct so future
/// additions (pagination, category facets) don't break callers.
#[derive(Debug, Serialize)]
struct ListTemplatesResponse {
    templates: Vec<TemplateSummary>,
}

async fn list_templates(
    State(state): State<AppState>,
) -> Json<ListTemplatesResponse> {
    Json(ListTemplatesResponse {
        templates: state.registry.summaries(),
    })
}

async fn get_template(
    State(state): State<AppState>,
    Path(raw_id): Path<String>,
) -> Result<Json<TemplateSummary>, ApiError> {
    // Validate the path segment via `TemplateId::new` so a malformed id
    // surfaces as 422 invalid_graph (via TemplateError → ApiError) rather
    // than 404 — the client should know they sent garbage, not just that
    // their valid-looking id wasn't found.
    let id = TemplateId::new(&raw_id).map_err(|_| ApiError::InvalidGraph(format!(
        "template id `{raw_id}` is malformed"
    )))?;
    let summary = state
        .registry
        .summaries()
        .into_iter()
        .find(|s| s.id == id)
        .ok_or(ApiError::NotFound)?;
    Ok(Json(summary))
}
