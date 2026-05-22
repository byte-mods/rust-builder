//! End-to-end integration tests for the studio's project CRUD surface.
//!
//! Every test runs the live `router()` (with a fresh tempdir-backed
//! `ProjectStore`) through `tower::ServiceExt::oneshot`. No sockets are
//! bound — tests are deterministic, parallel-safe, and don't depend on
//! ambient filesystem state.
//!
//! The case set deliberately mirrors the wire contract documented in
//! Section 2's plan (the six endpoints, every status code, every error
//! variant the handlers can return).

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use std::sync::Arc;

use rust_no_code_studio::{projects::ProjectStore, router, templates::TemplateRegistry, AppState};
use serde_json::{json, Value};
use tempfile::TempDir;
use tower::ServiceExt;

/// Build a router backed by a fresh tempdir-backed store. The `TempDir` is
/// returned so the test binding keeps it alive — dropping it deletes the
/// project root on disk. Returns a fresh tuple per call because
/// `oneshot` consumes the `Router`.
async fn harness() -> (Router, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ProjectStore::new(dir.path())
        .await
        .expect("project store builds");
    let registry = Arc::new(TemplateRegistry::with_builtins());
    (router(AppState::new(store, registry)), dir)
}

/// Helper — send a JSON request through `oneshot` and return (status,
/// body_value). Centralises the boilerplate so each case reads as the
/// contract assertion, not the HTTP plumbing.
async fn send_json(
    app: Router,
    method: Method,
    uri: &str,
    body: Option<&Value>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    let body = match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            Body::from(serde_json::to_vec(v).expect("body serialises"))
        }
        None => Body::empty(),
    };
    let req = builder.body(body).expect("request builds");
    let res = app.oneshot(req).await.expect("router responds");
    let status = res.status();
    let bytes = res
        .into_body()
        .collect()
        .await
        .expect("body collects")
        .to_bytes();
    // Tolerate non-JSON bodies (e.g. Axum's default plain-text rejections,
    // pre-T5 fix) so a test asserting status_is_client_error still has
    // something to inspect via the returned value.
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

#[tokio::test]
async fn test_post_create_returns_201_with_project_body() {
    let (app, _dir) = harness().await;
    let (status, body) = send_json(
        app,
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "user-service", "name": "User service"})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["slug"], "user-service");
    assert_eq!(body["name"], "User service");
    assert_eq!(body["schema_version"], 1);
    assert!(body["created_at"].is_string());
}

#[tokio::test]
async fn test_post_create_rejects_malformed_slug_with_400_invalid_body() {
    let (app, _dir) = harness().await;
    let (status, body) = send_json(
        app,
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "Bad Slug!", "name": "x"})),
    )
    .await;
    // Slug fails `Deserialize` (its hand-rolled impl routes through the
    // validator). The custom JsonRejection → ApiError::InvalidBody mapping
    // turns Axum's default plain-text 400 into a sanitised 400 with the
    // documented JSON envelope and the `invalid_body` code. The raw slug
    // validator message must NOT leak into the client-facing payload.
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid_body");
    let msg = body["message"].as_str().expect("message string");
    assert!(
        !msg.contains("lowercase letter"),
        "client message must not leak Slug validator detail; got: {msg}"
    );
    assert!(
        !msg.contains("column"),
        "client message must not leak serde column numbers; got: {msg}"
    );
}

#[tokio::test]
async fn test_post_create_duplicate_returns_409_already_exists() {
    let (app, dir) = harness().await;
    let body = json!({"slug": "dupe", "name": "first"});
    let (status1, _) = send_json(app, Method::POST, "/api/projects", Some(&body)).await;
    assert_eq!(status1, StatusCode::CREATED);

    // Rebuild the harness's router on the same tempdir so the second call
    // hits the same store.
    let store = ProjectStore::new(dir.path()).await.unwrap();
    let app2 = router(AppState::new(store, Arc::new(TemplateRegistry::with_builtins())));
    let (status2, body2) = send_json(app2, Method::POST, "/api/projects", Some(&body)).await;
    assert_eq!(status2, StatusCode::CONFLICT);
    assert_eq!(body2["error"], "already_exists");
}

#[tokio::test]
async fn test_get_list_reflects_created_projects() {
    let (app, dir) = harness().await;
    let (s1, _) = send_json(
        app,
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "alpha", "name": "Alpha"})),
    )
    .await;
    assert_eq!(s1, StatusCode::CREATED);

    let store = ProjectStore::new(dir.path()).await.unwrap();
    let app2 = router(AppState::new(store, Arc::new(TemplateRegistry::with_builtins())));
    let (s2, body) = send_json(app2, Method::GET, "/api/projects", None).await;
    assert_eq!(s2, StatusCode::OK);
    let arr = body["projects"].as_array().expect("projects array");
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["slug"], "alpha");
}

#[tokio::test]
async fn test_get_single_project_returns_body() {
    let (app, dir) = harness().await;
    send_json(
        app,
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "one", "name": "One"})),
    )
    .await;

    let store = ProjectStore::new(dir.path()).await.unwrap();
    let app2 = router(AppState::new(store, Arc::new(TemplateRegistry::with_builtins())));
    let (status, body) = send_json(app2, Method::GET, "/api/projects/one", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["slug"], "one");
}

#[tokio::test]
async fn test_get_unknown_project_returns_404() {
    let (app, _dir) = harness().await;
    let (status, body) = send_json(app, Method::GET, "/api/projects/ghost", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn test_get_with_invalid_path_slug_returns_422() {
    let (app, _dir) = harness().await;
    // `..` fails BadStart at the Slug validator long before any FS access.
    let (status, body) = send_json(app, Method::GET, "/api/projects/..", None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "invalid_slug");
}

#[tokio::test]
async fn test_get_initial_graph_contains_entry_point() {
    let (app, dir) = harness().await;
    send_json(
        app,
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "graf", "name": "g"})),
    )
    .await;

    let store = ProjectStore::new(dir.path()).await.unwrap();
    let app2 = router(AppState::new(store, Arc::new(TemplateRegistry::with_builtins())));
    let (status, body) = send_json(app2, Method::GET, "/api/projects/graf/graph", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["schema_version"], 1);
    let nodes = body["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 1, "new project must initialise with an entry-point node");
    assert_eq!(nodes[0]["template_id"], "core.entry_point");
    assert_eq!(nodes[0]["label"], "main.rs");
    assert_eq!(body["edges"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_put_then_get_graph_round_trips() {
    let (app, dir) = harness().await;
    send_json(
        app,
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "rt", "name": "RT"})),
    )
    .await;

    let store = ProjectStore::new(dir.path()).await.unwrap();
    let app2 = router(AppState::new(store, Arc::new(TemplateRegistry::with_builtins())));
    // Send the S2 legacy shape (`kind`) to prove the backward-compat
    // Deserialise still loads it; the round-trip GET below confirms the
    // store rewrote the wire to the new shape (`template_id`).
    let graph_in = json!({
        "schema_version": 1,
        "nodes": [{
            "id": "n1",
            "kind": "route",
            "position": {"x": 10.0, "y": 20.0},
            "config": {"path": "/hello", "method": "GET"},
            "label": "hello"
        }],
        "edges": []
    });
    let (sp, body_put) = send_json(
        app2,
        Method::PUT,
        "/api/projects/rt/graph",
        Some(&graph_in),
    )
    .await;
    assert_eq!(sp, StatusCode::OK);
    assert_eq!(body_put["nodes"][0]["id"], "n1");

    let store = ProjectStore::new(dir.path()).await.unwrap();
    let app3 = router(AppState::new(store, Arc::new(TemplateRegistry::with_builtins())));
    let (sg, body_get) = send_json(app3, Method::GET, "/api/projects/rt/graph", None).await;
    assert_eq!(sg, StatusCode::OK);
    assert_eq!(body_get["nodes"][0]["config"]["path"], "/hello");
    // Post-S3: stored graph uses `template_id`, not `kind`. The legacy
    // `kind: "route"` we PUT was translated on load and rewritten on save.
    assert_eq!(body_get["nodes"][0]["template_id"], "http.route");
    assert!(
        body_get["nodes"][0]["kind"].is_null(),
        "stored shape must not carry the legacy `kind` field"
    );
}

#[tokio::test]
async fn test_put_graph_with_bad_schema_version_returns_422() {
    let (app, dir) = harness().await;
    send_json(
        app,
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "ver", "name": "v"})),
    )
    .await;

    let store = ProjectStore::new(dir.path()).await.unwrap();
    let app2 = router(AppState::new(store, Arc::new(TemplateRegistry::with_builtins())));
    let (status, body) = send_json(
        app2,
        Method::PUT,
        "/api/projects/ver/graph",
        Some(&json!({"schema_version": 999, "nodes": [], "edges": []})),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "invalid_graph");
}

#[tokio::test]
async fn test_put_graph_with_dangling_edge_returns_422() {
    let (app, dir) = harness().await;
    send_json(
        app,
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "bad-edge", "name": "BE"})),
    )
    .await;

    let store = ProjectStore::new(dir.path()).await.unwrap();
    let app2 = router(AppState::new(store, Arc::new(TemplateRegistry::with_builtins())));
    let graph = json!({
        "schema_version": 1,
        "nodes": [{"id": "n1", "template_id": "http.route", "position": {"x": 0, "y": 0}, "config": {"path": "/", "method": "GET"}}],
        "edges": [{"id": "e1", "source": "n1", "source_port": "request", "target": "ghost", "target_port": "request"}]
    });
    let (status, body) = send_json(
        app2,
        Method::PUT,
        "/api/projects/bad-edge/graph",
        Some(&graph),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "invalid_graph");
}

#[tokio::test]
async fn test_put_graph_with_over_connected_single_port_returns_422() {
    let (app, dir) = harness().await;
    send_json(
        app,
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "multi", "name": "M"})),
    )
    .await;

    let store = ProjectStore::new(dir.path()).await.unwrap();
    let app2 = router(AppState::new(store, Arc::new(TemplateRegistry::with_builtins())));
    // http.handler input port "request" is Single — wiring two routes to it
    // should violate multiplicity.
    let graph = json!({
        "schema_version": 1,
        "nodes": [
            {"id": "r1", "template_id": "http.route", "position": {"x": 0, "y": 0}, "config": {"path": "/a", "method": "GET"}},
            {"id": "r2", "template_id": "http.route", "position": {"x": 0, "y": 0}, "config": {"path": "/b", "method": "GET"}},
            {"id": "h1", "template_id": "http.handler", "position": {"x": 0, "y": 0}, "config": {"name": "hello"}}
        ],
        "edges": [
            {"id": "e1", "source": "r1", "source_port": "request", "target": "h1", "target_port": "request"},
            {"id": "e2", "source": "r2", "source_port": "request", "target": "h1", "target_port": "request"}
        ]
    });
    let (status, body) = send_json(
        app2,
        Method::PUT,
        "/api/projects/multi/graph",
        Some(&graph),
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "invalid_graph");
}

#[tokio::test]
async fn test_put_graph_on_missing_project_returns_404() {
    let (app, _dir) = harness().await;
    let (status, body) = send_json(
        app,
        Method::PUT,
        "/api/projects/nope/graph",
        Some(&json!({"schema_version": 1, "nodes": [], "edges": []})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn test_delete_project_returns_204_then_404_on_load() {
    let (app, dir) = harness().await;
    send_json(
        app,
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "doomed", "name": "x"})),
    )
    .await;

    let store = ProjectStore::new(dir.path()).await.unwrap();
    let app2 = router(AppState::new(store, Arc::new(TemplateRegistry::with_builtins())));
    let (sd, _) = send_json(app2, Method::DELETE, "/api/projects/doomed", None).await;
    assert_eq!(sd, StatusCode::NO_CONTENT);

    let store = ProjectStore::new(dir.path()).await.unwrap();
    let app3 = router(AppState::new(store, Arc::new(TemplateRegistry::with_builtins())));
    let (sg, body) = send_json(app3, Method::GET, "/api/projects/doomed", None).await;
    assert_eq!(sg, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn test_delete_missing_project_returns_404() {
    let (app, _dir) = harness().await;
    let (status, body) = send_json(app, Method::DELETE, "/api/projects/never-was", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn test_post_regen_generates_source_for_graph() {
    let (app, dir) = harness().await;
    send_json(
        app,
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "codegen", "name": "Codegen"})),
    )
    .await;

    let store = ProjectStore::new(dir.path()).await.unwrap();
    let app2 = router(AppState::new(store, Arc::new(TemplateRegistry::with_builtins())));
    let graph = json!({
        "schema_version": 1,
        "nodes": [
            {
                "id": "n1",
                "template_id": "core.dto",
                "position": {"x": 0.0, "y": 0.0},
                "config": {"name": "User", "fields": [{"name": "id", "ty": "u64"}]}
            },
            {
                "id": "n2",
                "template_id": "http.handler",
                "position": {"x": 0.0, "y": 0.0},
                "config": {"name": "hello"}
            },
            {
                "id": "n3",
                "template_id": "http.route",
                "position": {"x": 0.0, "y": 0.0},
                "config": {"path": "/hello", "method": "GET"}
            },
            {
                "id": "n4",
                "template_id": "integration.consumer.placeholder",
                "position": {"x": 0.0, "y": 0.0},
                "config": {"topic": "orders", "group": "group-a"}
            },
            {
                "id": "n5",
                "template_id": "observability.logger",
                "position": {"x": 0.0, "y": 0.0},
                "config": {"level": "info", "format": "pretty", "name": "app_logger"}
            }
        ],
        "edges": [
            {
                "id": "e1",
                "source": "n3",
                "target": "n2",
                "source_port": "request",
                "target_port": "request"
            }
        ]
    });
    let (sp, _) = send_json(
        app2,
        Method::PUT,
        "/api/projects/codegen/graph",
        Some(&graph),
    )
    .await;
    assert_eq!(sp, StatusCode::OK);

    let store = ProjectStore::new(dir.path()).await.unwrap();
    let app3 = router(AppState::new(store, Arc::new(TemplateRegistry::with_builtins())));
    let (status, body) = send_json(
        app3,
        Method::POST,
        "/api/projects/codegen/regen",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let written = body["files_written"].as_array().expect("files_written array");
    let paths: Vec<_> = written.iter().map(|v| v.as_str().unwrap()).collect();
    assert!(paths.contains(&"src/dto/user.rs"), "dto file missing: {paths:?}");
    assert!(paths.contains(&"src/handlers/hello.rs"));
    assert!(paths.contains(&"src/consumers/orders.rs"), "consumer file missing: {paths:?}");
    assert!(paths.contains(&"src/loggers/app_logger.rs"), "logger file missing: {paths:?}");
    assert!(paths.contains(&"src/lib.rs"));
    assert!(paths.contains(&"src/main.rs"));
    assert!(paths.contains(&"Cargo.toml"));

    // All built-in templates now emit real code — pending_templates must be empty.
    let pending = body["pending_templates"].as_array().unwrap();
    assert!(
        pending.is_empty(),
        "no templates should be pending after S7: {pending:?}"
    );

    // Verify generated lib.rs contains the route and module declarations.
    let lib_path = dir.path().join("codegen/src/lib.rs");
    let lib_src = tokio::fs::read_to_string(&lib_path).await.unwrap();
    assert!(lib_src.contains(".route(\"/hello\""), "lib.rs must contain route: {lib_src}");
    assert!(lib_src.contains("mod consumers;"), "lib.rs must declare consumers module");

    // Verify generated main.rs contains supervisor + consumer spawn.
    let main_path = dir.path().join("codegen/src/main.rs");
    let main_src = tokio::fs::read_to_string(&main_path).await.unwrap();
    assert!(main_src.contains("async fn supervise"), "main.rs must contain supervise");
    assert!(main_src.contains("crate::consumers::orders::run()"));
    assert!(syn::parse_file(&main_src).is_ok(), "main.rs must be valid Rust");

    // Verify generated logger file parses as Rust and lib.rs declares the module.
    let logger_path = dir.path().join("codegen/src/loggers/app_logger.rs");
    let logger_src = tokio::fs::read_to_string(&logger_path).await.unwrap();
    assert!(syn::parse_file(&logger_src).is_ok(), "logger must be valid Rust");
    assert!(lib_src.contains("mod loggers;"), "lib.rs must declare loggers module");

    // Verify generated dto file parses as Rust.
    let dto_path = dir.path().join("codegen/src/dto/user.rs");
    let dto_src = tokio::fs::read_to_string(&dto_path).await.unwrap();
    assert!(syn::parse_file(&dto_src).is_ok(), "dto must be valid Rust");
}

#[tokio::test]
async fn test_post_build_returns_202_for_existing_project() {
    let (app, dir) = harness().await;
    send_json(
        app,
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "buildable", "name": "Buildable"})),
    )
    .await;

    let store = ProjectStore::new(dir.path()).await.unwrap();
    let app2 = router(AppState::new(store, Arc::new(TemplateRegistry::with_builtins())));
    let (status, _body) = send_json(
        app2,
        Method::POST,
        "/api/projects/buildable/build",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
}

#[tokio::test]
async fn test_post_build_on_missing_project_returns_404() {
    let (app, _dir) = harness().await;
    let (status, body) = send_json(
        app,
        Method::POST,
        "/api/projects/ghost/build",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}
