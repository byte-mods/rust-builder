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
use crate::projects::{Graph, Package, PackageId, PackageSlug, Project, ProjectMeta, Slug};
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
        // Package CRUD (Section 1 T3) — the project's package tree.
        .route(
            "/projects/:slug/packages",
            get(list_packages).post(create_package),
        )
        .route(
            "/projects/:slug/packages/:pkg",
            axum::routing::patch(rename_package).delete(delete_package),
        )
        .route(
            "/projects/:slug/packages/:pkg/graph",
            get(get_package_graph).put(put_package_graph),
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
        .route("/projects/:slug/marketplace", get(get_marketplace))
        .route("/projects/:slug/marketplace/install", post(install_marketplace_package))
        .route("/projects/:slug/marketplace/uninstall", post(uninstall_marketplace_package))
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
    /// Optional template identifier to seed the initial graph
    template: Option<String>,
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
    let project = state.store.create(body.slug, body.name, body.template).await?;
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
    let project = state.store.load(&slug).await?;

    // T4: load every package's graph so the codegen tree walker can
    // emit nested module files. Missing per-package graph files are
    // tolerated — `generate_project_tree` falls back to an empty
    // graph, which produces a stub `mod.rs` for that package.
    let mut graphs: std::collections::HashMap<PackageSlug, Graph> =
        std::collections::HashMap::with_capacity(project.packages.len());
    for pkg in &project.packages {
        match state.store.load_graph_for_package(&slug, &pkg.slug).await {
            Ok(g) => {
                graphs.insert(pkg.slug.clone(), g);
            }
            // Missing graph file is the expected state for a freshly
            // created child package that has never had a PUT. The
            // store's `read_json` helper maps `ENOENT` to
            // `ApiError::NotFound`, so that's what we catch here —
            // `Io(NotFound)` is never produced by this path.
            Err(ApiError::NotFound) => {
                // generate_project_tree falls back to an empty graph
                // for missing entries and writes a stub mod.rs.
            }
            Err(err) => return Err(err),
        }
    }

    let generator = Generator::new(
        Arc::clone(&state.registry),
        state.store.root().to_path_buf(),
    );
    let report = generator
        .generate_project_tree(&slug, &project, &graphs)
        .await?;
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

    // S16+T4: regen the full package tree before cargo, only if
    // something changed since the last build.
    regen_project_tree_for_cargo(&state, &slug).await?;

    // BuildManager needs the root graph for its diagnostic mapper.
    // Reload after regen so the value reflects whatever the regen
    // pass may have rewritten (currently a no-op, but keeps the
    // semantics future-proof).
    let graph = state.store.load_graph(&slug).await?;

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
    // S16+T6: regen the full package tree before cargo. Must use the
    // tree-aware path so the shared cache key doesn't fight Build's
    // tree-hash with Run's root-only hash — otherwise multi-package
    // projects flip between layouts (super-qa MAJOR caught at S1
    // section close).
    regen_project_tree_for_cargo(&state, &slug).await?;

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
    // S16+T6: tree-aware regen (see run_project for the rationale).
    regen_project_tree_for_cargo(&state, &slug).await?;
    let graph = state.store.load_graph(&slug).await?;

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
    // S16+T6: tree-aware regen (see run_project for the rationale).
    regen_project_tree_for_cargo(&state, &slug).await?;
    let graph = state.store.load_graph(&slug).await?;
    // Suppress unused warning in branches that don't read `graph`.
    let _ = &graph;

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

#[derive(Debug, Deserialize)]
struct MarketplaceActionBody {
    package: String,
}

/// `GET /api/projects/:slug/marketplace` — fetch the list of installed marketplace packages.
async fn get_marketplace(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<Vec<String>>, ApiError> {
    let slug = parse_slug(&slug)?;
    let packages = state.store.load_marketplace(&slug).await?;
    Ok(Json(packages))
}

/// `POST /api/projects/:slug/marketplace/install` — install a marketplace package.
async fn install_marketplace_package(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    body: Result<Json<MarketplaceActionBody>, JsonRejection>,
) -> Result<Json<Vec<String>>, ApiError> {
    let slug = parse_slug(&slug)?;
    let Json(body) = body?;
    
    let mut packages = state.store.load_marketplace(&slug).await?;
    if !packages.contains(&body.package) {
        packages.push(body.package.clone());
        state.store.save_marketplace(&slug, &packages).await?;
    }
    
    Ok(Json(packages))
}

/// `POST /api/projects/:slug/marketplace/uninstall` — uninstall a marketplace package.
async fn uninstall_marketplace_package(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    body: Result<Json<MarketplaceActionBody>, JsonRejection>,
) -> Result<Json<Vec<String>>, ApiError> {
    let slug = parse_slug(&slug)?;
    let Json(body) = body?;
    
    let mut packages = state.store.load_marketplace(&slug).await?;
    if let Some(pos) = packages.iter().position(|x| *x == body.package) {
        packages.remove(pos);
        state.store.save_marketplace(&slug, &packages).await?;
    }
    
    Ok(Json(packages))
}

/// Helper — `Path<String>` → `Slug`. Centralised so the `invalid_slug`
/// mapping doesn't drift across handlers.
fn parse_slug(raw: &str) -> Result<Slug, ApiError> {
    Slug::new(raw).map_err(|e| ApiError::InvalidSlug(e.to_string()))
}

/// Shared regen-before-cargo path for Build / Run / Test / Debug.
///
/// Loads the full project + every package's graph (tolerating missing
/// per-package graph files as empty), then routes through the
/// tree-aware codegen cache so multi-package projects produce nested
/// `src/<path>/mod.rs` files before cargo runs. Replaces the
/// pre-Section-1 single-graph `regen_if_changed` flow.
async fn regen_project_tree_for_cargo(
    state: &AppState,
    slug: &Slug,
) -> Result<(), ApiError> {
    let project = state.store.load(slug).await?;
    let mut graphs: std::collections::HashMap<PackageSlug, Graph> =
        std::collections::HashMap::with_capacity(project.packages.len());
    for pkg in &project.packages {
        match state.store.load_graph_for_package(slug, &pkg.slug).await {
            Ok(g) => {
                graphs.insert(pkg.slug.clone(), g);
            }
            // Missing per-package graph file is the expected state for
            // a freshly-created child package — treat as empty graph.
            Err(ApiError::NotFound) => {}
            Err(err) => return Err(err),
        }
    }
    let generator = Generator::new(
        Arc::clone(&state.registry),
        state.store.root().to_path_buf(),
    );
    let _ = state
        .codegen_cache
        .regen_if_changed_tree(&generator, slug, &project, &graphs)
        .await?;
    Ok(())
}

/// Helper — `Path<String>` → `PackageSlug`. Reuses the same `invalid_slug`
/// error code as project slugs since both share the validator.
fn parse_pkg_slug(raw: &str) -> Result<PackageSlug, ApiError> {
    PackageSlug::new(raw).map_err(|e| ApiError::InvalidSlug(e.to_string()))
}

// ----- Package CRUD (Section 1 T3) -----

/// Response body for `GET /api/projects/:slug/packages`.
#[derive(Debug, Serialize)]
struct ListPackagesResponse {
    packages: Vec<Package>,
}

/// Request body for `POST /api/projects/:slug/packages`.
///
/// The server assigns the `id` (UUID v4) so the client cannot supply an
/// empty or duplicate id. `parent_id` defaults to the root package when
/// omitted, matching the common case of "add a sibling at the top
/// level".
#[derive(Debug, Deserialize)]
struct CreatePackageBody {
    slug: PackageSlug,
    /// Parent's `PackageId.0`. Defaults to the root package's id when
    /// omitted.
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    label: Option<String>,
}

/// Request body for `PATCH /api/projects/:slug/packages/:pkg`. All fields
/// optional; missing fields leave the existing value unchanged.
#[derive(Debug, Deserialize)]
struct PatchPackageBody {
    /// New slug. Renames the on-disk folder atomically.
    #[serde(default)]
    slug: Option<PackageSlug>,
    /// New label. `None` here means "leave unchanged"; the JSON literal
    /// `null` is treated identically. To clear a label, send the empty
    /// string and the handler will store it as `None`.
    #[serde(default)]
    label: Option<String>,
}

/// `GET /api/projects/:slug/packages` — list every package in the tree.
async fn list_packages(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<ListPackagesResponse>, ApiError> {
    let slug = parse_slug(&slug)?;
    let project = state.store.load(&slug).await?;
    Ok(Json(ListPackagesResponse { packages: project.packages }))
}

/// `POST /api/projects/:slug/packages` — create a new package as a child
/// of an existing parent. Returns 201 + the new `Package`.
///
/// Failure modes mapped to HTTP:
/// - sibling slug collision → 409 `conflict`
/// - parent id not found    → 404 `not_found`
/// - malformed body or slug → 400 `invalid_body` (the typed `PackageSlug`
///   field's deserialiser rejects bad slugs before the handler runs, and
///   Axum's `JsonRejection` is mapped to `InvalidBody` — both surface
///   under the same 400 envelope)
async fn create_package(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    body: Result<Json<CreatePackageBody>, JsonRejection>,
) -> Result<(StatusCode, Json<Package>), ApiError> {
    let slug = parse_slug(&slug)?;
    let Json(body) = body?;

    // Mint a fresh server-side id so the tree never carries
    // client-controlled empty / duplicate ids. Addresses the T1 MINOR.
    let new_id = PackageId(format!("pkg-{}", uuid::Uuid::new_v4()));
    let new_id_for_search = new_id.clone();

    let new_pkg = Package {
        id: new_id,
        slug: body.slug,
        parent_id: None, // resolved below inside the mutator under the lock
        label: body.label,
    };
    let new_pkg_for_mutator = new_pkg.clone();

    let updated = state
        .store
        .mutate_project(&slug, move |project| {
            let mut pkg = new_pkg_for_mutator;

            // Resolve parent: explicit id, or the root package as default.
            let parent_id = match body.parent_id {
                Some(raw) => {
                    let pid = PackageId(raw);
                    if !project.packages.iter().any(|p| p.id == pid) {
                        return Err(ApiError::NotFound);
                    }
                    Some(pid)
                }
                None => project
                    .packages
                    .iter()
                    .find(|p| p.parent_id.is_none())
                    .map(|p| p.id.clone()),
            };
            pkg.parent_id = parent_id.clone();

            // Sibling-slug collision pre-check so we surface the precise
            // conflict reason rather than the generic
            // `DuplicateSiblingSlug` from the tree validator.
            let parent_key = parent_id.as_ref().map(|p| p.0.as_str());
            for existing in &project.packages {
                let ek = existing.parent_id.as_ref().map(|p| p.0.as_str());
                if ek == parent_key && existing.slug == pkg.slug {
                    return Err(ApiError::Conflict(format!(
                        "package slug {} already exists under this parent",
                        pkg.slug.as_str()
                    )));
                }
            }

            project.packages.push(pkg);
            Ok(())
        })
        .await?;

    // Locate the freshly inserted package to return it. The mutator
    // moved a value so we look it up by the id we minted above.
    let created = updated
        .packages
        .into_iter()
        .find(|p| p.id == new_id_for_search)
        .ok_or_else(|| ApiError::Internal("created package missing from tree after mutate".into()))?;

    Ok((StatusCode::CREATED, Json(created)))
}

/// `PATCH /api/projects/:slug/packages/:pkg` — rename / relabel.
///
/// Slug rename atomically moves the on-disk folder
/// (`packages/<old>/` → `packages/<new>/`) under the per-slug lock so
/// graph data is preserved.
async fn rename_package(
    State(state): State<AppState>,
    Path((slug, pkg)): Path<(String, String)>,
    body: Result<Json<PatchPackageBody>, JsonRejection>,
) -> Result<Json<Package>, ApiError> {
    let slug = parse_slug(&slug)?;
    let pkg_slug = parse_pkg_slug(&pkg)?;
    let Json(body) = body?;

    // Clones keep the borrow checker happy: `pkg_slug` is consumed by the
    // closure but needed later for the disk rename + return-value
    // lookup; `body.slug` is consumed inside the closure but needed
    // afterwards to decide whether to move the on-disk folder.
    let pkg_slug_for_closure = pkg_slug.clone();
    let new_slug_for_closure = body.slug.clone();
    let label_for_closure = body.label;

    let updated = state
        .store
        .mutate_project(&slug, move |project| {
            // Locate target without holding a mutable borrow across the
            // collision scan.
            let (current_parent, current_slug) = {
                let target = project
                    .packages
                    .iter()
                    .find(|p| p.slug == pkg_slug_for_closure)
                    .ok_or(ApiError::NotFound)?;
                (target.parent_id.clone(), target.slug.clone())
            };

            // Apply slug rename if requested and actually different.
            if let Some(new_slug) = new_slug_for_closure.clone() {
                if new_slug != current_slug {
                    let collision = project.packages.iter().any(|p| {
                        p.parent_id == current_parent && p.slug == new_slug
                    });
                    if collision {
                        return Err(ApiError::Conflict(format!(
                            "package slug {} already exists under this parent",
                            new_slug.as_str()
                        )));
                    }
                    // The same lookup succeeded a few lines above under
                    // the same exclusive borrow, so this branch is
                    // logically unreachable — but no-expect rule
                    // applies on the request path, so map to Internal
                    // explicitly rather than panic.
                    let target = project
                        .packages
                        .iter_mut()
                        .find(|p| p.slug == pkg_slug_for_closure)
                        .ok_or_else(|| {
                            ApiError::Internal(
                                "package vanished between collision check and update"
                                    .into(),
                            )
                        })?;
                    target.slug = new_slug;
                }
            }

            // Apply label change. After a slug rename above, locate the
            // package by its (possibly new) slug.
            if let Some(label) = label_for_closure {
                let lookup = new_slug_for_closure
                    .as_ref()
                    .unwrap_or(&pkg_slug_for_closure)
                    .clone();
                let target = project
                    .packages
                    .iter_mut()
                    .find(|p| p.slug == lookup)
                    .ok_or(ApiError::NotFound)?;
                target.label = if label.is_empty() { None } else { Some(label) };
            }
            Ok(())
        })
        .await?;

    let new_slug_opt = body.slug;

    // Move the on-disk folder if the slug actually changed. Done after
    // the tree update so a disk-move failure leaves the tree in a
    // consistent state (we then attempt to roll the tree back).
    if let Some(ref ns) = new_slug_opt {
        if ns != &pkg_slug {
            if let Err(err) = state.store.rename_package_dir(&slug, &pkg_slug, ns).await {
                // Best-effort rollback: revert the slug in the tree so
                // disk and tree remain in agreement. If the rollback
                // itself fails, surface the original error — the
                // operator can reconcile manually from `.history/`.
                let pkg_slug_for_rollback = pkg_slug.clone();
                let ns_clone = ns.clone();
                let _ = state
                    .store
                    .mutate_project(&slug, |project| {
                        if let Some(t) = project.packages.iter_mut().find(|p| p.slug == ns_clone) {
                            t.slug = pkg_slug_for_rollback;
                        }
                        Ok(())
                    })
                    .await;
                return Err(err);
            }
        }
    }

    // Return the package by its post-rename slug (or unchanged).
    let lookup_slug = new_slug_opt.as_ref().unwrap_or(&pkg_slug);
    updated
        .packages
        .into_iter()
        .find(|p| &p.slug == lookup_slug)
        .map(Json)
        .ok_or(ApiError::NotFound)
}

/// `DELETE /api/projects/:slug/packages/:pkg` — delete a package and all
/// its descendants. 204 on success.
///
/// Cannot delete the root package (would leave the project with no
/// entry point); attempts return 409 `conflict`.
async fn delete_package(
    State(state): State<AppState>,
    Path((slug, pkg)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let slug = parse_slug(&slug)?;
    let pkg_slug = parse_pkg_slug(&pkg)?;

    // Capture every descendant's slug so we can clean their disk
    // folders after the tree mutation. The mutator drains them from the
    // tree into the shared Mutex; we drain them from disk after. The
    // Mutex is needed because the closure is `FnOnce` and so cannot
    // borrow a stack-local `&mut Vec` across the await boundary.
    let to_remove_ref = std::sync::Arc::new(std::sync::Mutex::new(Vec::<PackageSlug>::new()));
    let to_remove_writer = to_remove_ref.clone();

    state
        .store
        .mutate_project(&slug, move |project| {
            let target = project
                .packages
                .iter()
                .find(|p| p.slug == pkg_slug)
                .ok_or(ApiError::NotFound)?;
            if target.parent_id.is_none() {
                return Err(ApiError::Conflict(
                    "cannot delete the root package of a project".into(),
                ));
            }
            let target_id = target.id.clone();

            // Compute the descendant set via BFS so a deep subtree
            // (`a → b → c → d`) is removed in one shot.
            let mut doomed_ids: Vec<PackageId> = vec![target_id];
            let mut i = 0;
            while i < doomed_ids.len() {
                let parent = doomed_ids[i].clone();
                for p in &project.packages {
                    if p.parent_id.as_ref() == Some(&parent)
                        && !doomed_ids.iter().any(|d| d == &p.id)
                    {
                        doomed_ids.push(p.id.clone());
                    }
                }
                i += 1;
            }

            // Collect slugs for the disk-cleanup pass before mutating
            // the vec.
            let mut slugs: Vec<PackageSlug> = Vec::with_capacity(doomed_ids.len());
            for id in &doomed_ids {
                if let Some(p) = project.packages.iter().find(|p| &p.id == id) {
                    slugs.push(p.slug.clone());
                }
            }
            // Surface slugs to the outer scope without violating the
            // closure's `FnOnce` contract.
            if let Ok(mut w) = to_remove_writer.lock() {
                w.extend(slugs);
            }

            project.packages.retain(|p| !doomed_ids.contains(&p.id));
            Ok(())
        })
        .await?;

    // Surface the descendant slugs from the closure-owned Mutex back
    // into a plain Vec for the disk-cleanup loop.
    let to_remove: Vec<PackageSlug> = to_remove_ref
        .lock()
        .map(|w| w.iter().cloned().collect())
        .unwrap_or_default();

    // Disk cleanup: best-effort. A failure here leaves the tree without
    // the package (already persisted) but with the folder still present
    // — recoverable. Errors are logged but not propagated, mirroring
    // the existing post-save metadata-bump pattern in `save_graph`.
    for s in to_remove {
        if let Err(err) = state.store.delete_package_dir(&slug, &s).await {
            tracing::warn!(
                slug = %slug,
                pkg = %s,
                ?err,
                "package folder cleanup failed; tree is updated but disk folder remains"
            );
        }
    }

    Ok(StatusCode::NO_CONTENT)
}

/// `GET /api/projects/:slug/packages/:pkg/graph` — load a single
/// package's flow graph.
async fn get_package_graph(
    State(state): State<AppState>,
    Path((slug, pkg)): Path<(String, String)>,
) -> Result<Json<Graph>, ApiError> {
    let slug = parse_slug(&slug)?;
    let pkg_slug = parse_pkg_slug(&pkg)?;
    let graph = state.store.load_graph_for_package(&slug, &pkg_slug).await?;
    Ok(Json(graph))
}

/// `PUT /api/projects/:slug/packages/:pkg/graph` — replace a single
/// package's flow graph atomically. Same validation pipeline as the
/// top-level `PUT /graph` shim.
async fn put_package_graph(
    State(state): State<AppState>,
    Path((slug, pkg)): Path<(String, String)>,
    graph: Result<Json<Graph>, JsonRejection>,
) -> Result<Json<Graph>, ApiError> {
    let slug = parse_slug(&slug)?;
    let pkg_slug = parse_pkg_slug(&pkg)?;
    let Json(graph) = graph?;
    state
        .store
        .save_graph_for_package(&slug, &pkg_slug, &graph, &state.registry)
        .await?;
    Ok(Json(graph))
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
        if std::env::var("RUST_NO_CODE_TEST").is_ok() {
            llm::LlmProvider::Anthropic
        } else if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            llm::LlmProvider::Anthropic
        } else if std::env::var("OPENAI_API_KEY").is_ok() {
            llm::LlmProvider::OpenAi
        } else if std::env::var("DEEPSEEK_API_KEY").is_ok() {
            llm::LlmProvider::DeepSeek
        } else if std::env::var("KIMI_API_KEY").is_ok() {
            llm::LlmProvider::Kimi
        } else {
            llm::LlmProvider::ClaudeCli
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
        if std::env::var("RUST_NO_CODE_TEST").is_ok() {
            llm::LlmProvider::Anthropic
        } else if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            llm::LlmProvider::Anthropic
        } else if std::env::var("OPENAI_API_KEY").is_ok() {
            llm::LlmProvider::OpenAi
        } else if std::env::var("DEEPSEEK_API_KEY").is_ok() {
            llm::LlmProvider::DeepSeek
        } else if std::env::var("KIMI_API_KEY").is_ok() {
            llm::LlmProvider::Kimi
        } else {
            llm::LlmProvider::ClaudeCli
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



