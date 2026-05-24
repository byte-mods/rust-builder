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
use sqlx::{Connection, Executor};

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
    assert!(main_src.contains("codegen::consumers::orders::run()"), "main_src did not contain expected orders consumer, content:\n{}", main_src);
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

#[tokio::test]
async fn test_deferred_codegen_e2e_flow() {
    let (app, dir) = harness().await;

    // 1. Create a project
    let (status1, _) = send_json(
        app.clone(),
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "deferred-e2e", "name": "Deferred E2E"})),
    )
    .await;
    assert_eq!(status1, StatusCode::CREATED);

    // 2. PUT a graph with a DTO node
    let graph = json!({
        "schema_version": 1,
        "nodes": [
            {
                "id": "entry",
                "template_id": "core.entry_point",
                "position": {"x": 0.0, "y": 0.0},
                "config": {}
            },
            {
                "id": "dto",
                "template_id": "core.dto",
                "position": {"x": 100.0, "y": 0.0},
                "config": {"name": "TestDto", "fields": [{"name": "value", "ty": "u32"}]}
            }
        ],
        "edges": []
    });

    let (status2, _) = send_json(
        app.clone(),
        Method::PUT,
        "/api/projects/deferred-e2e/graph",
        Some(&graph),
    )
    .await;
    assert_eq!(status2, StatusCode::OK);

    // 3. Verify that the files DO NOT exist yet (codegen is deferred)
    let dto_path = dir.path().join("deferred-e2e/src/dto/test_dto.rs");
    assert!(!dto_path.exists(), "DTO file must NOT exist before build click");

    // 4. Trigger build (POST /api/projects/:slug/build)
    let store = ProjectStore::new(dir.path()).await.unwrap();
    let registry = Arc::new(TemplateRegistry::with_builtins());
    let state = AppState::new(store, registry);
    let build_manager = state.build_manager.clone();
    let app2 = router(state);

    let (status3, _) = send_json(
        app2,
        Method::POST,
        "/api/projects/deferred-e2e/build",
        None,
    )
    .await;
    assert_eq!(status3, StatusCode::ACCEPTED);

    // 5. Wait for the build task to exit 0
    let mut rx = build_manager.subscribe("deferred-e2e");
    let mut exit_code = None;
    tokio::select! {
        res = async {
            loop {
                if let Ok(event) = rx.recv().await {
                    if let rust_no_code_studio::build::BuildEvent::Exit { code } = event {
                        return Some(code);
                    }
                }
            }
        } => exit_code = res,
        _ = tokio::time::sleep(tokio::time::Duration::from_secs(60)) => {}
    }

    assert_eq!(exit_code, Some(0), "cargo check build must succeed");

    // 6. Verify that files now exist on disk and were regenerated!
    assert!(dto_path.exists(), "DTO file must exist after build click");
}

#[tokio::test]
async fn test_llm_endpoints_return_api_key_missing_when_env_not_set() {
    let (app, _dir) = harness().await;

    // First ensure project is created
    let (status1, _) = send_json(
        app.clone(),
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "llm-test", "name": "LLM Test"})),
    )
    .await;
    assert_eq!(status1, StatusCode::CREATED);

    // Make sure ANTHROPIC_API_KEY is not set in this test context
    std::env::remove_var("ANTHROPIC_API_KEY");

    // 1. Test generate-flow
    let (status2, body2) = send_json(
        app.clone(),
        Method::POST,
        "/api/projects/llm-test/llm/generate-flow",
        Some(&json!({"prompt": "create a background task"})),
    )
    .await;
    assert_eq!(status2, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body2["error"], "api_key_missing");

    // 2. Test refine-flow
    let (status3, body3) = send_json(
        app,
        Method::POST,
        "/api/projects/llm-test/llm/refine-flow",
        Some(&json!({"prompt": "change interval to 10 seconds"})),
    )
    .await;
    assert_eq!(status3, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body3["error"], "api_key_missing");
}

#[tokio::test]
async fn test_step_debugger_lifecycle() {
    let (app, dir) = harness().await;

    // 1. Create a project debug-e2e
    let (status1, _) = send_json(
        app.clone(),
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "debug-e2e", "name": "Debug E2E"})),
    )
    .await;
    assert_eq!(status1, StatusCode::CREATED);

    // 2. PUT a graph with a route, handler, and service wired
    let graph = json!({
        "schema_version": 1,
        "nodes": [
            {
                "id": "entry",
                "template_id": "core.entry_point",
                "position": {"x": 0.0, "y": 0.0},
                "config": {}
            },
            {
                "id": "route",
                "template_id": "http.route",
                "position": {"x": 100.0, "y": 0.0},
                "config": {"path": "/hello", "method": "GET"}
            },
            {
                "id": "hello",
                "template_id": "http.handler",
                "position": {"x": 200.0, "y": 0.0},
                "config": {"name": "hello"}
            },
            {
                "id": "get_user",
                "template_id": "core.service",
                "position": {"x": 300.0, "y": 0.0},
                "config": {"name": "get_user"}
            }
        ],
        "edges": [
            {
                "id": "e1",
                "source": "route",
                "target": "hello",
                "source_port": "request",
                "target_port": "request"
            },
            {
                "id": "e2",
                "source": "get_user",
                "target": "hello",
                "source_port": "output",
                "target_port": "request"
            }
        ]
    });

    // Write the graph.json directly to disk to bypass the multiplicity validation check
    let project_dir = dir.path().join("debug-e2e");
    tokio::fs::create_dir_all(&project_dir).await.unwrap();
    tokio::fs::write(
        project_dir.join("graph.json"),
        serde_json::to_string(&graph).unwrap(),
    )
    .await
    .unwrap();

    // 3. Trigger regen
    let (status3, _) = send_json(
        app.clone(),
        Method::POST,
        "/api/projects/debug-e2e/regen",
        None,
    )
    .await;
    assert_eq!(status3, StatusCode::OK);

    // 4. Set up RunManager state and start run manually with studio debug active
    let store = ProjectStore::new(dir.path()).await.unwrap();
    let registry = Arc::new(TemplateRegistry::with_builtins());
    let state = AppState::new(store, registry);
    let run_manager = state.run_manager.clone();

    let project_dir = dir.path().join("debug-e2e");
    let mut rx = run_manager.subscribe("debug-e2e");

    run_manager
        .start_run(
            "debug-e2e",
            project_dir,
            &[
                ("RUST_LOG", "debug"),
                ("RUST_BACKTRACE", "full"),
                ("STUDIO_DEBUG", "1"),
                ("STUDIO_BREAKPOINTS", "get_user"),
                ("BIND_ADDR", "127.0.0.1:8089"),
            ],
        )
        .await
        .unwrap();

    // 5. Trigger the HTTP endpoint in a background task
    let client = reqwest::Client::new();
    let http_task = tokio::spawn(async move {
        let mut attempts = 0;
        loop {
            match client.get("http://127.0.0.1:8089/hello").send().await {
                Ok(resp) => return resp.status(),
                Err(_) => {
                    attempts += 1;
                    if attempts > 120 {
                        panic!("Failed to connect to user-project server");
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
            }
        }
    });

    // 6. Loop and assert we hit the breakpoints and can resume cleanly
    let mut saw_paused = false;
    #[allow(unused_assignments)]
    let mut saw_after = false;

    loop {
        let event = tokio::time::timeout(tokio::time::Duration::from_secs(60), rx.recv())
            .await
            .expect("debugger lifecycle should complete within 60s")
            .expect("channel should not close");

        match event {
            rust_no_code_studio::run::RunEvent::Stdout { line } => {
                println!("[CHILD STDOUT] {}", line);
            }
            rust_no_code_studio::run::RunEvent::Stderr { line } => {
                println!("[CHILD STDERR] {}", line);
            }
            rust_no_code_studio::run::RunEvent::DebugState { node_id, state, value } => {
                println!("[CHILD DEBUG] {} -> {} (val: {})", node_id, state, value);
                if node_id == "get_user" && state == "paused" {
                    saw_paused = true;
                    // Resume execution
                    run_manager.send_stdin("debug-e2e", "resume\n").await.unwrap();
                } else if node_id == "get_user" && state == "after" {
                    saw_after = true;
                    break;
                }
            }
            other => {
                println!("[CHILD EVENT] {:?}", other);
            }
        }
    }

    assert!(saw_paused, "should have hit the paused state at get_user");
    assert!(saw_after, "should have hit the after state at get_user");

    // Await the HTTP request success
    let http_status = http_task.await.unwrap();
    assert_eq!(http_status, axum::http::StatusCode::OK);

    // Stop run
    run_manager.stop_run("debug-e2e").await.unwrap();
}

#[tokio::test]
async fn test_security_audit_endpoint_e2e() {
    let (app, _dir) = harness().await;

    // 1. Create a project
    let (status1, _) = send_json(
        app.clone(),
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "audit-e2e", "name": "Security Audit E2E"})),
    )
    .await;
    assert_eq!(status1, StatusCode::CREATED);

    // 2. Put a graph with leaked AWS key and SQL injection risk
    let graph = json!({
        "schema_version": 1,
        "nodes": [
            {
                "id": "node_1",
                "template_id": "custom.block",
                "position": {"x": 0.0, "y": 0.0},
                "config": {
                    "name": "dangerous_query",
                    "aws_key": "AKIA1234567890ABCDEF",
                    "code": "pub async fn dangerous_query(name: String) -> Result<String, AppError> { let sql = format!(\"SELECT * FROM users WHERE name = '{}'\", name);\nconn.execute(&sql, ()).await?;\nOk(\"ok\".to_string()) }"
                }
            }
        ],
        "edges": []
    });

    let (status2, _) = send_json(
        app.clone(),
        Method::PUT,
        "/api/projects/audit-e2e/graph",
        Some(&graph),
    )
    .await;
    assert_eq!(status2, StatusCode::OK);

    // 3. Trigger Security Audit POST request
    let (status3, body3) = send_json(
        app.clone(),
        Method::POST,
        "/api/projects/audit-e2e/audit",
        None,
    )
    .await;

    assert_eq!(status3, StatusCode::OK);
    
    let report: rust_no_code_studio::projects::security::SecurityAuditReport =
        serde_json::from_value(body3).unwrap();

    // Verify secret leak is detected
    assert!(!report.leaked_secrets.is_empty(), "Should detect leaked AWS Access Key ID");
    assert_eq!(report.leaked_secrets[0].secret_type, "AWS Access Key ID");
    assert_eq!(report.leaked_secrets[0].node_id, "node_1");
    assert_eq!(report.leaked_secrets[0].masked_value, "AKIA****************");

    // Verify SQL injection is caught
    assert!(!report.secure_code_violations.is_empty(), "Should detect SQL Injection Risk");
    assert_eq!(report.secure_code_violations[0].violation_type, "SQL Injection Risk (OWASP A03)");
    assert_eq!(report.secure_code_violations[0].node_id, "node_1");
    assert_eq!(report.secure_code_violations[0].severity, "CRITICAL");

    // Verify score is lowered
    assert!(report.security_score < 100);
}

#[tokio::test]
async fn test_flow_export_and_import_endpoints_e2e() {
    let (app, _dir) = harness().await;

    // 1. Create a project
    let (status1, _) = send_json(
        app.clone(),
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "e2e-flow", "name": "E2E Flow Project"})),
    )
    .await;
    assert_eq!(status1, StatusCode::CREATED);

    // 2. Export it via GET /api/projects/:slug/export
    let req_export = Request::builder()
        .method(Method::GET)
        .uri("/api/projects/e2e-flow/export")
        .body(Body::empty())
        .unwrap();
    let res_export = app.clone().oneshot(req_export).await.unwrap();
    assert_eq!(res_export.status(), StatusCode::OK);
    
    // Check headers
    let content_type = res_export.headers().get(axum::http::header::CONTENT_TYPE).unwrap();
    assert_eq!(content_type, "application/octet-stream");
    let content_disp = res_export.headers().get(axum::http::header::CONTENT_DISPOSITION).unwrap();
    assert!(content_disp.to_str().unwrap().contains("e2e-flow.flow"));

    let zip_bytes = res_export
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert!(!zip_bytes.is_empty(), "Exported flow bytes should not be empty");

    // 3. Import it back via POST /api/projects/import!
    // Since "e2e-flow" exists, this will collide and get slug "e2e-flow-import"
    let req_import = Request::builder()
        .method(Method::POST)
        .uri("/api/projects/import")
        .header("content-type", "application/octet-stream")
        .body(Body::from(zip_bytes))
        .unwrap();
    
    let res_import = app.clone().oneshot(req_import).await.unwrap();
    assert_eq!(res_import.status(), StatusCode::OK);

    let import_bytes = res_import
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    let imported_project: Value = serde_json::from_slice(&import_bytes).unwrap();
    
    assert_eq!(imported_project["slug"], "e2e-flow-import");
    assert_eq!(imported_project["name"], "E2E Flow Project (Imported)");
}

#[tokio::test]
async fn test_db_schema_introspection_endpoint() {
    let (app, _dir) = harness().await;

    // 1. Create a project
    let (status1, _) = send_json(
        app.clone(),
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "db-test", "name": "DB Introspect Project"})),
    )
    .await;
    assert_eq!(status1, StatusCode::CREATED);

    // 2. Set up a temporary SQLite database
    let temp_db_dir = tempfile::tempdir().unwrap();
    let db_path = temp_db_dir.path().join("test.sqlite");
    std::fs::File::create(&db_path).unwrap();
    let conn_str = format!("sqlite://{}", db_path.to_str().unwrap());

    // Create a table inside the temporary SQLite database
    {
        sqlx::any::install_default_drivers();
        let mut conn = sqlx::AnyConnection::connect(&conn_str).await.unwrap();
        conn.execute("CREATE TABLE products (id INTEGER PRIMARY KEY NOT NULL, name TEXT NOT NULL, price REAL);").await.unwrap();
    }

    // 3. Request introspection via POST /api/projects/:slug/db/schema
    let (status2, body2) = send_json(
        app.clone(),
        Method::POST,
        "/api/projects/db-test/db/schema",
        Some(&json!({
            "connection_string": conn_str
        })),
    )
    .await;
    
    assert_eq!(status2, StatusCode::OK);
    
    let tables = body2["tables"].as_array().unwrap();
    assert_eq!(tables.len(), 1);
    
    let product_table = &tables[0];
    assert_eq!(product_table["name"], "products");
    
    let cols = product_table["columns"].as_array().unwrap();
    assert_eq!(cols.len(), 3);
    
    let id_col = cols.iter().find(|c| c["name"] == "id").unwrap();
    assert_eq!(id_col["data_type"], "integer");
    assert!(id_col["primary_key"].as_bool().unwrap());
    assert!(!id_col["nullable"].as_bool().unwrap());

    let price_col = cols.iter().find(|c| c["name"] == "price").unwrap();
    assert_eq!(price_col["data_type"], "real");
    assert!(!price_col["primary_key"].as_bool().unwrap());
    assert!(price_col["nullable"].as_bool().unwrap());
}

#[tokio::test]
async fn test_grpc_scaffolder_pipeline() {
    let (app, dir) = harness().await;

    // 1. Create project grpc-test
    let (status1, _) = send_json(
        app.clone(),
        Method::POST,
        "/api/projects",
        Some(&json!({"slug": "grpc-test", "name": "gRPC Test"})),
    )
    .await;
    assert_eq!(status1, StatusCode::CREATED);

    // 2. PUT a graph containing gRPC Server and a custom block handler
    let graph = json!({
        "schema_version": 1,
        "nodes": [
            {
                "id": "entry",
                "template_id": "core.entry_point",
                "position": {"x": 0.0, "y": 0.0},
                "config": {}
            },
            {
                "id": "grpc_server",
                "template_id": "grpc.server",
                "position": {"x": 100.0, "y": 0.0},
                "config": {
                    "address": "[::1]:50051",
                    "service_name": "Greeter",
                    "proto_definition": "syntax = \"proto3\";\npackage hello;\nservice Greeter {\n  rpc SayHello (HelloRequest) returns (HelloReply);\n}\nmessage HelloRequest {\n  string name = 1;\n}\nmessage HelloReply {\n  string message = 1;\n}"
                }
            },
            {
                "id": "custom",
                "template_id": "custom.block",
                "position": {"x": 250.0, "y": 0.0},
                "config": {
                    "name": "say_hello_handler",
                    "code": "pub fn execute(req: crate::grpc::server_grpc_server::proto_grpc_server::HelloRequest) -> Result<crate::grpc::server_grpc_server::proto_grpc_server::HelloReply, crate::errors::AppError> {\n    Ok(crate::grpc::server_grpc_server::proto_grpc_server::HelloReply { message: format!(\"Hello, {}!\", req.name) })\n}"
                }
            }
        ],
        "edges": [
            {
                "id": "e1",
                "source": "entry",
                "source_port": "service",
                "target": "grpc_server",
                "target_port": "entry"
            },
            {
                "id": "e2",
                "source": "grpc_server",
                "source_port": "SayHello",
                "target": "custom",
                "target_port": "req"
            }
        ]
    });

    let (status2, body2) = send_json(
        app.clone(),
        Method::PUT,
        "/api/projects/grpc-test/graph",
        Some(&graph),
    )
    .await;
    if status2 != StatusCode::OK {
        panic!("PUT graph failed: status={}, body={:?}", status2, body2);
    }
    assert_eq!(status2, StatusCode::OK);

    // Verify dynamic ports generated on the grpc.server node!
    let nodes = body2["nodes"].as_array().unwrap();
    let server_node = nodes.iter().find(|n| n["id"] == "grpc_server").unwrap();
    let server_outputs = server_node["config"]["outputs"].as_array().unwrap();
    assert_eq!(server_outputs.len(), 1);
    assert_eq!(server_outputs[0]["name"], "SayHello");
    assert_eq!(server_outputs[0]["ty"], "HelloRequest");

    // 3. Trigger project regeneration
    let (status3, body3) = send_json(
        app.clone(),
        Method::POST,
        "/api/projects/grpc-test/regen",
        None,
    )
    .await;
    if status3 != StatusCode::OK {
        panic!("Regen failed! status={}, body={:#?}", status3, body3);
    }
    assert_eq!(status3, StatusCode::OK);

    let files_written = body3["files_written"].as_array().unwrap();
    let file_paths: Vec<&str> = files_written.iter().map(|f| f.as_str().unwrap()).collect();

    // Verify all gRPC files were written!
    assert!(file_paths.contains(&"src/../build.rs"));
    assert!(file_paths.contains(&"src/../proto/grpc_server_grpc_server.proto"));
    assert!(file_paths.contains(&"src/grpc/server_grpc_server.rs"));
    assert!(file_paths.contains(&"Cargo.toml"));

    // 4. Verify build.rs contents
    let build_rs_path = dir.path().join("grpc-test/build.rs");
    assert!(build_rs_path.exists());
    let build_rs_src = std::fs::read_to_string(build_rs_path).unwrap();
    assert!(build_rs_src.contains("tonic_build::compile_protos"));

    // 5. Verify Cargo.toml build-dependencies section was written
    let cargo_toml_path = dir.path().join("grpc-test/Cargo.toml");
    assert!(cargo_toml_path.exists());
    let cargo_toml_src = std::fs::read_to_string(cargo_toml_path).unwrap();
    assert!(cargo_toml_src.contains("[build-dependencies]"));
    assert!(cargo_toml_src.contains("tonic-build = \"0.10\""));
}




