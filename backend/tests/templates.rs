//! End-to-end integration tests for the studio's template HTTP surface.
//!
//! Two endpoints: `GET /api/templates` (list summaries) and
//! `GET /api/templates/:id` (one summary). Both are pure registry lookups
//! — the tests pin the wire contract (sorted order, body shape, error
//! codes) so the frontend node palette can rely on it.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Method, Request, StatusCode},
    Router,
};
use http_body_util::BodyExt;
use rust_no_code_studio::{projects::ProjectStore, router, templates::TemplateRegistry, AppState};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

async fn harness() -> (Router, TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = ProjectStore::new(dir.path())
        .await
        .expect("project store builds");
    let registry = Arc::new(TemplateRegistry::with_builtins());
    (router(AppState::new(store, registry)), dir)
}

async fn get_json(app: Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(Body::empty())
        .expect("request");
    let res = app.oneshot(req).await.expect("response");
    let status = res.status();
    let bytes = res.into_body().collect().await.expect("body").to_bytes();
    let value: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

#[tokio::test]
async fn test_list_templates_returns_expected_builtin_inventory_sorted() {
    let (app, _dir) = harness().await;
    let (status, body) = get_json(app, "/api/templates").await;
    assert_eq!(status, StatusCode::OK);
    let templates = body["templates"].as_array().expect("templates array");
    let ids: Vec<&str> = templates.iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert_eq!(
        ids,
        vec![
            "core.dto",
            "core.entry_point",
            "core.service",
            "custom.block",
            "grpc.client",
            "grpc.server",
            "http.handler",
            "http.route",
            "integration.consumer.placeholder",
            "integration.db_writer",
            "integration.file_tail",
            "integration.http_client",
            "integration.kafka_consumer",
            "integration.kafka_producer",
            "integration.redis",
            "integration.scheduler",
            "integration.scheduler.placeholder",
            "integration.sql_connector",
            "language.await",
            "language.clone",
            "language.enum",
            "language.fn",
            "language.if_else",
            "language.loop",
            "language.match",
            "language.pointer",
            "language.propagate",
            "language.struct",
            "observability.logger",
            "parser.json",
            "parser.protobuf",
            "parser.xml",
            "stream.filter",
            "stream.join",
            "stream.map",
            "stream.pattern",
            "stream.select",
            "stream.union",
            "stream.window",
            "tokio.broadcast",
            "tokio.interval",
            "tokio.join",
            "tokio.mpsc",
            "tokio.mutex",
            "tokio.notify",
            "tokio.rwlock",
            "tokio.select",
            "tokio.semaphore",
            "tokio.sleep",
            "tokio.spawn",
            "tokio.spawn_blocking",
            "wasm.runner",
        ],
        "templates must be returned in lexicographic id order"
    );
}

#[tokio::test]
async fn test_list_templates_shape_includes_display_ports_modes() {
    let (app, _dir) = harness().await;
    let (status, body) = get_json(app, "/api/templates").await;
    assert_eq!(status, StatusCode::OK);
    let first = &body["templates"][0];
    assert!(first["display"]["name"].is_string());
    assert!(first["display"]["category"].is_string());
    assert!(first["input_ports"].is_array());
    assert!(first["output_ports"].is_array());
    assert!(first["codegen_mode"].is_string());
    assert!(first["debug_bridge"].is_string());
}

#[tokio::test]
async fn test_get_known_template_returns_summary() {
    let (app, _dir) = harness().await;
    let (status, body) = get_json(app, "/api/templates/http.route").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], "http.route");
    assert_eq!(body["display"]["category"], "HTTP");
    // http.route has one input (entry, wired from core.entry_point) and one output (the request).
    assert_eq!(body["input_ports"].as_array().unwrap().len(), 1);
    assert_eq!(body["input_ports"][0]["name"], "entry");
    assert_eq!(body["output_ports"].as_array().unwrap().len(), 1);
    assert_eq!(body["output_ports"][0]["name"], "request");
}

#[tokio::test]
async fn test_get_unknown_template_returns_404() {
    let (app, _dir) = harness().await;
    let (status, body) = get_json(app, "/api/templates/ghost.template").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "not_found");
}

#[tokio::test]
async fn test_get_template_with_malformed_id_returns_422() {
    let (app, _dir) = harness().await;
    // Single-segment id (no `.`) fails TemplateId::new before any lookup.
    let (status, body) = get_json(app, "/api/templates/notnamespaced").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "invalid_graph");
}

#[tokio::test]
async fn test_dto_template_declares_codegen_mode() {
    let (app, _dir) = harness().await;
    let (status, body) = get_json(app, "/api/templates/core.dto").await;
    assert_eq!(status, StatusCode::OK);
    // S9's parser pack and S4's generator both branch on this discriminator.
    assert_eq!(body["codegen_mode"], "codegen");
}

#[tokio::test]
async fn test_long_runner_template_declares_long_runner_bridge() {
    let (app, _dir) = harness().await;
    let (status, body) = get_json(app, "/api/templates/integration.consumer.placeholder").await;
    assert_eq!(status, StatusCode::OK);
    // S13's step debugger keys per-instance behaviour off this.
    assert_eq!(body["debug_bridge"], "long_runner");
}

#[tokio::test]
async fn test_put_graph_rejects_node_with_unregistered_template() {
    // Verifies the S3-T6 wiring end-to-end: the registry's validation
    // gate sits in front of the filesystem write. A graph whose node
    // references an unknown template must surface as 422 invalid_graph,
    // not crash the server and not touch disk.
    use serde_json::json;
    let (_app, dir) = harness().await;
    // Create a project first.
    let store = ProjectStore::new(dir.path()).await.unwrap();
    let registry = Arc::new(TemplateRegistry::with_builtins());
    let create_req = Request::builder()
        .method(Method::POST)
        .uri("/api/projects")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({"slug": "tpl", "name": "tpl"})).unwrap(),
        ))
        .unwrap();
    let create_app = router(AppState::new(store.clone(), registry.clone()));
    let create_res = create_app.oneshot(create_req).await.unwrap();
    assert_eq!(create_res.status(), StatusCode::CREATED);

    let put_req = Request::builder()
        .method(Method::PUT)
        .uri("/api/projects/tpl/graph")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&json!({
                "schema_version": 1,
                "nodes": [{
                    "id": "n1",
                    "template_id": "ghost.template",
                    "position": {"x": 0.0, "y": 0.0},
                    "config": {}
                }],
                "edges": []
            }))
            .unwrap(),
        ))
        .unwrap();
    let put_app = router(AppState::new(store, registry));
    let put_res = put_app.oneshot(put_req).await.unwrap();
    assert_eq!(put_res.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let bytes = put_res.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["error"], "invalid_graph");
}
