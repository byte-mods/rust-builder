//! HTTP handlers for project CRUD.
//!
//! Each handler is a thin shell: extract typed inputs (`Slug` from the URL
//! path, request bodies via `Json<T>`), call the `ProjectStore`, map the
//! result through `Result<_, ApiError>`. Domain logic lives in `store.rs`;
//! response shaping lives in `error.rs`. This separation makes the wire
//! surface trivially auditable — every handler is small enough to read in
//! one screen.
//!
//! The subrouter exposed by [`projects_router`] is mounted under `/api`
//! from `crate::router()`.

use axum::{
    extract::{rejection::JsonRejection, Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::codegen::{GenerateReport, Generator};
use crate::error::ApiError;
use crate::projects::{Graph, Project, ProjectMeta, Slug};
use crate::AppState;
use std::sync::Arc;

/// Build the projects subrouter. State is provided by the parent router
/// via `with_state` in `crate::router`.
pub fn projects_router() -> Router<AppState> {
    Router::new()
        .route("/projects", post(create_project).get(list_projects))
        .route(
            "/projects/:slug",
            get(get_project).delete(delete_project),
        )
        .route(
            "/projects/:slug/graph",
            get(get_graph).put(put_graph),
        )
        .route("/projects/:slug/regen", post(regen_project))
        .route("/projects/:slug/build", post(build_project))
        .route("/projects/:slug/run", post(run_project))
        .route("/projects/:slug/stop", post(stop_project))
        .route("/projects/:slug/status", get(run_status))
}

/// Request body for `POST /api/projects`. `slug` deserialises through the
/// `Slug` validator, so malformed slugs are rejected at the request boundary
/// with `invalid_slug` rather than reaching the store.
#[derive(Debug, Deserialize)]
struct CreateProjectBody {
    slug: Slug,
    /// Human-readable name. Currently unconstrained except by JSON's own
    /// limits; the studio surfaces it verbatim in the UI list.
    name: String,
}

/// Response body for `GET /api/projects` — a flat array of metadata
/// headers. Wrapping in a named struct (rather than returning the raw
/// `Vec`) keeps room to add pagination fields without a breaking change.
#[derive(Debug, Serialize)]
struct ListProjectsResponse {
    projects: Vec<ProjectMeta>,
}

/// `POST /api/projects` — create a new project. 201 + the created `Project`.
///
/// Body extraction goes through `Result<Json<_>, JsonRejection>` rather than
/// the bare `Json` extractor so a malformed body (invalid JSON, wrong
/// shape, failed `Slug` validation inside the body) surfaces as the
/// sanitised `ApiError::InvalidBody` instead of Axum's default plain-text
/// 400 — which would leak serde column numbers and `Slug` validator detail
/// to the client.
async fn create_project(
    State(state): State<AppState>,
    body: Result<Json<CreateProjectBody>, JsonRejection>,
) -> Result<(StatusCode, Json<Project>), ApiError> {
    let Json(body) = body?;
    let project = state.store.create(body.slug, body.name).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

/// `GET /api/projects` — list every project's metadata header. Newest first.
async fn list_projects(
    State(state): State<AppState>,
) -> Result<Json<ListProjectsResponse>, ApiError> {
    let projects = state.store.list().await?;
    Ok(Json(ListProjectsResponse { projects }))
}

/// `GET /api/projects/:slug` — fetch a single project's metadata.
async fn get_project(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<Project>, ApiError> {
    let slug = parse_slug(&slug)?;
    let project = state.store.load(&slug).await?;
    Ok(Json(project))
}

/// `DELETE /api/projects/:slug` — delete the project. 204, no body.
///
/// Destructive and irreversible from the studio's side; the user-project's
/// own git history (if any) is the only safety net.
async fn delete_project(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<StatusCode, ApiError> {
    let slug = parse_slug(&slug)?;
    state.store.delete(&slug).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/projects/:slug/graph` — load the persisted graph.
async fn get_graph(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<Graph>, ApiError> {
    let slug = parse_slug(&slug)?;
    let graph = state.store.load_graph(&slug).await?;
    Ok(Json(graph))
}

/// `PUT /api/projects/:slug/graph` — replace the persisted graph atomically
/// and return the saved value as confirmation.
///
/// The store validates the graph's schema version at the boundary; bodies
/// at unknown versions surface as 422 `invalid_graph`.
async fn put_graph(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    graph: Result<Json<Graph>, JsonRejection>,
) -> Result<Json<Graph>, ApiError> {
    let slug = parse_slug(&slug)?;
    let Json(graph) = graph?;
    state.store.save_graph(&slug, &graph, &state.registry).await?;
    Ok(Json(graph))
}

/// `POST /api/projects/:slug/regen` — regenerate the user-project's Rust
/// source from the current graph. Returns a report of files written,
/// pending templates, and dependencies.
async fn regen_project(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<GenerateReport>, ApiError> {
    let slug = parse_slug(&slug)?;
    let graph = state.store.load_graph(&slug).await?;
    let generator = Generator::new(
        Arc::clone(&state.registry),
        state.store.root().to_path_buf(),
    );
    let report = generator.generate_project(&slug, &graph).await?;
    Ok(Json(report))
}

/// Query parameters for `POST /api/projects/:slug/build`.
#[derive(Debug, Deserialize)]
struct BuildQuery {
    #[serde(default)]
    release: bool,
}

/// `POST /api/projects/:slug/build` — start a `cargo check` (or `cargo build
/// --release`) for the user-project. Returns 202 immediately; output is
/// streamed via the WebSocket endpoint.
async fn build_project(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Query(query): Query<BuildQuery>,
) -> Result<StatusCode, ApiError> {
    let slug = parse_slug(&slug)?;
    // Verify the project exists before spawning cargo — consistent with
    // every other project-scoped endpoint.
    let _ = state.store.load(&slug).await?;
    let project_dir = state.store.root().join(slug.as_str());
    state
        .build_manager
        .start_build(slug.as_str(), project_dir, query.release)
        .await
        .map_err(|e| match e {
            crate::build::BuildError::AlreadyRunning(_) => ApiError::BuildInProgress,
            crate::build::BuildError::Io(io) => io.into(),
        })?;
    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/projects/:slug/run` — start `cargo run` for the user-project.
/// Returns 202 immediately; output is streamed via the run WebSocket endpoint.
async fn run_project(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<StatusCode, ApiError> {
    let slug = parse_slug(&slug)?;
    let _ = state.store.load(&slug).await?;
    let project_dir = state.store.root().join(slug.as_str());
    state
        .run_manager
        .start_run(slug.as_str(), project_dir)
        .await?;
    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/projects/:slug/stop` — stop the active run for the user-project.
/// Returns 204 on success.
async fn stop_project(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<StatusCode, ApiError> {
    let slug = parse_slug(&slug)?;
    state.run_manager.stop_run(slug.as_str()).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/projects/:slug/status` — return the current run status.
async fn run_status(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<crate::run::RunStatus>, ApiError> {
    let slug = parse_slug(&slug)?;
    let _ = state.store.load(&slug).await?;
    Ok(Json(state.run_manager.status(slug.as_str())))
}

/// Helper — `Path<String>` → `Slug`. Centralised so the `invalid_slug`
/// mapping doesn't drift across handlers.
fn parse_slug(raw: &str) -> Result<Slug, ApiError> {
    Slug::new(raw).map_err(|e| ApiError::InvalidSlug(e.to_string()))
}
