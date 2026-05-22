//! `rust_no_code_studio` — library crate for the studio server.
//!
//! This crate exposes the router builder and the typed error surface used by
//! the binary entry point (`src/main.rs`) and by integration tests. Keeping
//! the router construction in `lib.rs` (rather than `main.rs`) is deliberate:
//! tests exercise endpoints via `tower::ServiceExt::oneshot` without binding
//! a socket, which keeps the test suite deterministic and free of port
//! collisions.

pub mod build;
pub mod codegen;
pub mod error;
pub mod projects;
pub mod run;
pub mod templates;

use std::sync::Arc;

use axum::{routing::get, Json, Router};
use serde::Serialize;
use tower_http::{cors::CorsLayer, trace::TraceLayer};

use crate::projects::ProjectStore;
use crate::run::RunManager;
use crate::templates::TemplateRegistry;

/// Bundled state shared across every handler that needs persistence + the
/// template registry. Introduced in S3 to keep handler signatures short
/// (a single `State<AppState>` extractor) and to give a single place to
/// add new global services in future sections (S11's Claude subprocess
/// pool, S6's build orchestrator, S13's debug-bridge router will all land
/// here).
///
/// `Clone` is cheap — both fields are reference-counted internally.
#[derive(Clone)]
pub struct AppState {
    pub store: ProjectStore,
    pub registry: Arc<TemplateRegistry>,
    pub build_manager: Arc<crate::build::BuildManager>,
    pub run_manager: Arc<RunManager>,
}

impl AppState {
    pub fn new(store: ProjectStore, registry: Arc<TemplateRegistry>) -> Self {
        Self {
            store,
            registry,
            build_manager: Arc::new(crate::build::BuildManager::new()),
            run_manager: Arc::new(RunManager::new()),
        }
    }
}

/// JSON body returned by `GET /health`.
///
/// Kept stable across versions — the frontend parses this shape to decide
/// whether the studio backend is reachable and what version it is talking to.
#[derive(Debug, Serialize)]
pub struct HealthResponse {
    /// Always the literal string `"ok"` when the server can serve a request.
    /// A non-`ok` value is reserved for future readiness checks (e.g. when
    /// the project loader is still rehydrating from disk on cold start).
    pub status: &'static str,
    /// The studio crate's `CARGO_PKG_VERSION` at compile time. Used by the UI
    /// to detect version skew between frontend and backend during development.
    pub version: &'static str,
}

/// Build the studio's HTTP router.
///
/// This function is the single entry point for HTTP route configuration —
/// `main` calls it once at startup; tests call it once per case. Adding a new
/// route means editing this function and nowhere else, which keeps routing
/// auditable.
///
/// Middleware applied here:
/// - `TraceLayer` for per-request `tracing` spans (request id, latency).
/// - `CorsLayer::permissive` because the frontend dev server runs on a
///   different origin (`127.0.0.1:5173`) than the backend (`127.0.0.1:7878`).
///   This will be tightened in a later section when authentication lands.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ws/build/:slug", get(build::build_ws))
        .route("/ws/run/:slug", get(run::run_ws))
        .nest("/api", projects::projects_router())
        .nest("/api", templates::templates_router())
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Handler for `GET /health`. Responds immediately — no I/O, no locks,
/// no awaits on shared state — so it is safe to use as a liveness probe.
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}
