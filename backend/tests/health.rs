//! Integration tests for the studio's `/health` endpoint.
//!
//! These tests exercise the live router (`rust_no_code_studio::router`) via
//! `tower::ServiceExt::oneshot` rather than binding a network socket. That
//! keeps the suite deterministic (no port collisions in CI), fast (no
//! listener teardown between cases), and faithful (the same `Router` value
//! that `main` serves is the one under test).

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use std::sync::Arc;

use rust_no_code_studio::{projects::ProjectStore, router, templates::TemplateRegistry, AppState};
use serde_json::{json, Value};
use tempfile::tempdir;
use tower::ServiceExt;

/// `GET /health` must return HTTP 200 with an exact JSON body of
/// `{"status":"ok","version":"<crate version>"}`. This pins the wire
/// contract — the frontend parses these exact fields to decide whether the
/// studio is reachable and to detect version skew during development.
///
/// The router now requires a `ProjectStore` (introduced in Section 2). The
/// store is constructed against a per-test tempdir so the health probe is
/// fully isolated from any on-disk project state.
#[tokio::test]
async fn test_health_endpoint_returns_ok_with_version() {
    let dir = tempdir().expect("tempdir creates");
    let store = ProjectStore::new(dir.path())
        .await
        .expect("project store builds");
    let registry = Arc::new(TemplateRegistry::with_builtins());
    let app = router(AppState::new(store, registry));

    let response = app
        .oneshot(
            Request::builder()
                .uri("/health")
                .body(Body::empty())
                .expect("health request builds"),
        )
        .await
        .expect("router responds without error");

    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("body collects")
        .to_bytes();

    let body_json: Value = serde_json::from_slice(&body_bytes).expect("body is valid JSON");

    let expected = json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
    });

    assert_eq!(
        body_json, expected,
        "/health JSON body must match the documented shape exactly"
    );
}
