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

use crate::build::BuildVerb;
use crate::codegen::{GenerateReport, Generator};
use crate::error::ApiError;
use crate::projects::{Graph, Project, ProjectMeta, Slug};
use crate::projects::llm;
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
        .route("/projects/:slug/test", post(test_project))
        .route("/projects/:slug/debug", post(debug_project))
        .route("/projects/:slug/debug/action", post(debug_action))
        .route("/projects/:slug/llm/generate-flow", post(generate_flow))
        .route("/projects/:slug/llm/refine-flow", post(refine_flow))
        .route("/projects/:slug/audit", post(audit_project))
        .route("/projects/:slug/export", get(export_project))
        .route("/projects/import", post(import_project))
        .route("/projects/:slug/db/schema", post(db_schema))
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
    // Drop the codegen hash cache entry so a recreate with the same slug
    // does not falsely skip its first regen on a hash match against the
    // previous project's last-built graph.
    state.codegen_cache.forget(&slug);
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
    let Json(mut graph) = graph?;

    for node in &mut graph.nodes {
        if node.template_id.as_str() == "custom.block" {
            if let Some(code_val) = node.config.get("code") {
                if let Some(code_str) = code_val.as_str() {
                    match crate::templates::builtins::custom::parse_signature(code_str) {
                        Ok((inputs, outputs)) => {
                            if let serde_json::Value::Object(ref mut map) = node.config {
                                map.insert("inputs".to_string(), serde_json::to_value(inputs).unwrap());
                                map.insert("outputs".to_string(), serde_json::to_value(outputs).unwrap());
                            }
                        }
                        Err(e) => {
                            return Err(ApiError::InvalidGraph(format!("Custom block '{}' code parsing failed: {}", node.id.0, e)));
                        }
                    }
                }
            }
        } else if node.template_id.as_str() == "grpc.server" {
            if let Some(proto_val) = node.config.get("proto_definition") {
                if let Some(proto_str) = proto_val.as_str() {
                    match crate::templates::builtins::grpc::parse_proto_ports(proto_str) {
                        Ok((inputs, outputs)) => {
                            if let serde_json::Value::Object(ref mut map) = node.config {
                                map.insert("inputs".to_string(), serde_json::to_value(inputs).unwrap());
                                map.insert("outputs".to_string(), serde_json::to_value(outputs).unwrap());
                            }
                        }
                        Err(e) => {
                            return Err(ApiError::InvalidGraph(format!("gRPC Server '{}' Proto3 schema parsing failed: {}", node.id.0, e)));
                        }
                    }
                }
            }
        }
    }

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

    // S16: Regen first if the graph has changed since the last build
    let graph = state.store.load_graph(&slug).await?;
    let generator = Generator::new(
        Arc::clone(&state.registry),
        state.store.root().to_path_buf(),
    );
    let _ = state.codegen_cache.regen_if_changed(&generator, &slug, &graph).await?;

    let project_dir = state.store.root().join(slug.as_str());
    let verb = if query.release {
        BuildVerb::BuildRelease
    } else {
        BuildVerb::Check
    };
    state
        .build_manager
        .start_build(slug.as_str(), project_dir, verb, Some(graph))
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

    // S16: Regen first if the graph has changed since the last run
    let graph = state.store.load_graph(&slug).await?;
    let generator = Generator::new(
        Arc::clone(&state.registry),
        state.store.root().to_path_buf(),
    );
    let _ = state.codegen_cache.regen_if_changed(&generator, &slug, &graph).await?;

    let project_dir = state.store.root().join(slug.as_str());
    state
        .run_manager
        .start_run(slug.as_str(), project_dir, &[("STUDIO_PROFILE", "1")])
        .await?;
    Ok(StatusCode::ACCEPTED)
}

/// `POST /api/projects/:slug/test` — start `cargo test` for the user-project.
/// Returns 202 immediately; output is streamed via the build WebSocket endpoint.
async fn test_project(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<StatusCode, ApiError> {
    let slug = parse_slug(&slug)?;
    let _ = state.store.load(&slug).await?;

    // S16: Regen first if the graph has changed since the last test
    let graph = state.store.load_graph(&slug).await?;
    let generator = Generator::new(
        Arc::clone(&state.registry),
        state.store.root().to_path_buf(),
    );
    let _ = state.codegen_cache.regen_if_changed(&generator, &slug, &graph).await?;

    let project_dir = state.store.root().join(slug.as_str());
    state
        .build_manager
        .start_build(slug.as_str(), project_dir, BuildVerb::Test, Some(graph))
        .await
        .map_err(|e| match e {
            crate::build::BuildError::AlreadyRunning(_) => ApiError::BuildInProgress,
            crate::build::BuildError::Io(io) => io.into(),
        })?;
    Ok(StatusCode::ACCEPTED)
}

#[derive(Debug, Deserialize)]
struct StartDebugBody {
    breakpoints: Option<Vec<String>>,
}

/// `POST /api/projects/:slug/debug` — start `cargo run` with debug env vars for the user-project.
/// Returns 202 immediately; output is streamed via the run WebSocket endpoint.
async fn debug_project(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    body: Option<Json<StartDebugBody>>,
) -> Result<StatusCode, ApiError> {
    let slug = parse_slug(&slug)?;
    let _ = state.store.load(&slug).await?;

    // S16: Regen first if the graph has changed since the last debug run
    let graph = state.store.load_graph(&slug).await?;
    let generator = Generator::new(
        Arc::clone(&state.registry),
        state.store.root().to_path_buf(),
    );
    let _ = state.codegen_cache.regen_if_changed(&generator, &slug, &graph).await?;

    let bps_str = body
        .and_then(|Json(b)| b.breakpoints)
        .map(|bps| bps.join(","))
        .unwrap_or_default();

    let project_dir = state.store.root().join(slug.as_str());
    state
        .run_manager
        .start_run(
            slug.as_str(),
            project_dir,
            &[
                ("RUST_LOG", "debug"),
                ("RUST_BACKTRACE", "full"),
                ("STUDIO_DEBUG", "1"),
                ("STUDIO_BREAKPOINTS", &bps_str),
            ],
        )
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

#[derive(Debug, Deserialize)]
struct DebugActionBody {
    action: String,
}

/// `POST /api/projects/:slug/debug/action` — send resume or step commands to the running debug process.
async fn debug_action(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Json(body): Json<DebugActionBody>,
) -> Result<StatusCode, ApiError> {
    let slug = parse_slug(&slug)?;
    let command = match body.action.as_str() {
        "resume" => "resume\n",
        "step" => "step\n",
        other => {
            return Err(ApiError::InvalidBody(format!(
                "invalid debug action `{other}` (must be `resume` or `step`)"
            )));
        }
    };
    state.run_manager.send_stdin(slug.as_str(), command).await?;
    Ok(StatusCode::OK)
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

#[derive(Debug, Deserialize)]
struct GenerateFlowBody {
    prompt: String,
    history: Option<Vec<llm::ChatMessage>>,
    provider: Option<llm::LlmProvider>,
    api_key: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RefineFlowBody {
    prompt: String,
    history: Option<Vec<llm::ChatMessage>>,
    provider: Option<llm::LlmProvider>,
    api_key: Option<String>,
    model: Option<String>,
}

/// `POST /api/projects/:slug/llm/generate-flow` — generate a proposed flow graph using the selected LLM provider.
async fn generate_flow(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    body: Result<Json<GenerateFlowBody>, JsonRejection>,
) -> Result<Json<Graph>, ApiError> {
    let slug = parse_slug(&slug)?;
    let _ = state.store.load(&slug).await?;
    let Json(body) = body?;

    let provider = body.provider.unwrap_or_else(|| {
        if std::env::var("OPENAI_API_KEY").is_ok() {
            llm::LlmProvider::OpenAi
        } else if std::env::var("DEEPSEEK_API_KEY").is_ok() {
            llm::LlmProvider::DeepSeek
        } else if std::env::var("KIMI_API_KEY").is_ok() {
            llm::LlmProvider::Kimi
        } else {
            llm::LlmProvider::Anthropic
        }
    });

    let resolved_key = match body.api_key.as_deref() {
        Some(k) if !k.trim().is_empty() => Some(k.to_string()),
        _ => {
            let env_key = match provider {
                llm::LlmProvider::Anthropic => "ANTHROPIC_API_KEY",
                llm::LlmProvider::OpenAi => "OPENAI_API_KEY",
                llm::LlmProvider::Codex => "CODEX_API_KEY",
                llm::LlmProvider::DeepSeek => "DEEPSEEK_API_KEY",
                llm::LlmProvider::Kimi => "KIMI_API_KEY",
                llm::LlmProvider::ClaudeCli => "",
            };
            if !env_key.is_empty() {
                let val = std::env::var(env_key).map_err(|_| {
                    if provider == llm::LlmProvider::Anthropic {
                        ApiError::ApiKeyMissing
                    } else {
                        ApiError::LlmError(format!("API key environment variable `{env_key}` is not set"))
                    }
                })?;
                if val.trim().is_empty() {
                    return Err(if provider == llm::LlmProvider::Anthropic {
                        ApiError::ApiKeyMissing
                    } else {
                        ApiError::LlmError(format!("API key environment variable `{env_key}` is empty"))
                    });
                }
                Some(val)
            } else {
                None
            }
        }
    };

    let project_dir = state.store.root().join(slug.as_str());
    let current_graph = state.store.load_graph(&slug).await?;

    let context = llm::assemble_context(&project_dir, &current_graph);
    let system_prompt = llm::build_system_prompt(&state.registry);
    
    let user_prompt = format!(
        "The developer wants to completely generate or heavily modify the flow graph.\n\n### Project Context:\n{}\n\n### Developer's Request:\n{}",
        context, body.prompt
    );

    let history_ref = body.history.as_deref();
    let proposed_graph = llm::call_llm_api(
        provider,
        resolved_key.as_deref(),
        body.model.as_deref(),
        &system_prompt,
        &user_prompt,
        history_ref,
    )
    .await?;

    llm::validate_proposed_graph(&proposed_graph, &state.registry)?;

    Ok(Json(proposed_graph))
}

/// `POST /api/projects/:slug/llm/refine-flow` — refine the existing flow graph using the selected LLM provider.
async fn refine_flow(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    body: Result<Json<RefineFlowBody>, JsonRejection>,
) -> Result<Json<Graph>, ApiError> {
    let slug = parse_slug(&slug)?;
    let _ = state.store.load(&slug).await?;
    let Json(body) = body?;

    let provider = body.provider.unwrap_or_else(|| {
        if std::env::var("OPENAI_API_KEY").is_ok() {
            llm::LlmProvider::OpenAi
        } else if std::env::var("DEEPSEEK_API_KEY").is_ok() {
            llm::LlmProvider::DeepSeek
        } else if std::env::var("KIMI_API_KEY").is_ok() {
            llm::LlmProvider::Kimi
        } else {
            llm::LlmProvider::Anthropic
        }
    });

    let resolved_key = match body.api_key.as_deref() {
        Some(k) if !k.trim().is_empty() => Some(k.to_string()),
        _ => {
            let env_key = match provider {
                llm::LlmProvider::Anthropic => "ANTHROPIC_API_KEY",
                llm::LlmProvider::OpenAi => "OPENAI_API_KEY",
                llm::LlmProvider::Codex => "CODEX_API_KEY",
                llm::LlmProvider::DeepSeek => "DEEPSEEK_API_KEY",
                llm::LlmProvider::Kimi => "KIMI_API_KEY",
                llm::LlmProvider::ClaudeCli => "",
            };
            if !env_key.is_empty() {
                let val = std::env::var(env_key).map_err(|_| {
                    if provider == llm::LlmProvider::Anthropic {
                        ApiError::ApiKeyMissing
                    } else {
                        ApiError::LlmError(format!("API key environment variable `{env_key}` is not set"))
                    }
                })?;
                if val.trim().is_empty() {
                    return Err(if provider == llm::LlmProvider::Anthropic {
                        ApiError::ApiKeyMissing
                    } else {
                        ApiError::LlmError(format!("API key environment variable `{env_key}` is empty"))
                    });
                }
                Some(val)
            } else {
                None
            }
        }
    };

    let project_dir = state.store.root().join(slug.as_str());
    let current_graph = state.store.load_graph(&slug).await?;

    let context = llm::assemble_context(&project_dir, &current_graph);
    let system_prompt = llm::build_system_prompt(&state.registry);

    let user_prompt = format!(
        "The developer wants to make minor refinements or tweaks to the existing flow graph.\n\n### Project Context:\n{}\n\n### Developer's Refinement Request:\n{}",
        context, body.prompt
    );

    let history_ref = body.history.as_deref();
    let proposed_graph = llm::call_llm_api(
        provider,
        resolved_key.as_deref(),
        body.model.as_deref(),
        &system_prompt,
        &user_prompt,
        history_ref,
    )
    .await?;

    llm::validate_proposed_graph(&proposed_graph, &state.registry)?;

    Ok(Json(proposed_graph))
}

/// `POST /api/projects/:slug/audit` — run security audit static analysis scans on the user-project
async fn audit_project(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<crate::projects::security::SecurityAuditReport>, ApiError> {
    let slug = parse_slug(&slug)?;
    // Confirm project exists
    let _ = state.store.load(&slug).await?;

    let graph = state.store.load_graph(&slug).await?;
    let project_dir = state.store.root().join(slug.as_str());

    let report = crate::projects::security::run_security_audit(&project_dir, &graph).await;

    Ok(Json(report))
}

/// `GET /api/projects/:slug/export` — export project files compressed inside a portable `.flow` archive.
async fn export_project(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let slug = parse_slug(&slug)?;
    let archive_bytes = state.store.export_archive(&slug).await?;
    
    let filename = format!("{}.flow", slug.as_str());
    
    let response = axum::response::Response::builder()
        .header(axum::http::header::CONTENT_TYPE, "application/octet-stream")
        .header(
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", filename),
        )
        .body(axum::body::Body::from(archive_bytes))
        .map_err(|e| ApiError::Internal(format!("Failed to build response: {}", e)))?;
        
    Ok(response)
}

/// `POST /api/projects/import` — import a portable `.flow` archive and regenerate the target's Rust source codes.
async fn import_project(
    State(state): State<AppState>,
    body: axum::body::Bytes,
) -> Result<Json<Project>, ApiError> {
    let (project, graph) = state.store.import_archive(&body).await?;
    
    let generator = Generator::new(
        Arc::clone(&state.registry),
        state.store.root().to_path_buf(),
    );
    let _report = generator.generate_project(&project.meta.slug, &graph).await?;
    
    Ok(Json(project))
}

#[derive(Debug, Deserialize)]
struct DbSchemaBody {
    connection_string: String,
}

/// `POST /api/projects/:slug/db/schema` — introspect and explore table schemas and foreign key relations.
async fn db_schema(
    State(_state): State<AppState>,
    Path(slug): Path<String>,
    body: Result<Json<DbSchemaBody>, JsonRejection>,
) -> Result<Json<crate::projects::db::DbSchemaReport>, ApiError> {
    let _slug = parse_slug(&slug)?;
    let Json(body) = body?;
    let report = crate::projects::db::introspect_schema(&body.connection_string).await?;
    Ok(Json(report))
}



