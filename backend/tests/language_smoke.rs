//! End-to-end smoke test for the S15a Visual Rust language templates.
//!
//! Builds a `Graph` with one `language.struct` node and one `language.fn`
//! node where the function constructs the struct, drives the full codegen
//! orchestrator into a tempdir, and runs `cargo check` against the
//! generated tree. Pass = the three new templates compose end-to-end with
//! the existing orchestrator (lib.rs declares the new directories,
//! `types/mod.rs` and `functions/mod.rs` reference the emitted files,
//! and the generated code compiles).
//!
//! Slow by nature — first invocation will compile the generated crate's
//! transitive dependencies. Subsequent runs reuse `~/.cargo/registry`.

use rust_no_code_studio::codegen::Generator;
use rust_no_code_studio::projects::types::{
    Edge, EdgeId, Graph, Node, NodeId, Position, Slug, GRAPH_SCHEMA_VERSION,
};
use rust_no_code_studio::templates::{TemplateId, TemplateRegistry};
use std::sync::Arc;
use tempfile::tempdir;
use tokio::process::Command;

/// Build a minimal graph: `User` struct + `make_user` function that
/// constructs it. The function references the struct via fully-qualified
/// path (`crate::types::user::User`) so we don't rely on `use` injection.
fn smoke_graph() -> Graph {
    Graph {
        schema_version: GRAPH_SCHEMA_VERSION,
        nodes: vec![
            Node {
                id: NodeId("n_struct".into()),
                template_id: TemplateId::new("language.struct").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "name": "User",
                    "derives": ["Debug", "Clone"],
                    "fields": [
                        { "name": "id",   "ty": "u64" },
                        { "name": "name", "ty": "String" }
                    ]
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_fn".into()),
                template_id: TemplateId::new("language.fn").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "name": "make_user",
                    "params": [
                        { "name": "id",   "ty": "u64" },
                        { "name": "name", "ty": "String" }
                    ],
                    "return_type": "crate::types::user::User",
                    "body": "crate::types::user::User { id, name }"
                }),
                label: None,
                comment: None,
            },
        ],
        edges: Vec::<Edge>::new(),
    }
}

/// Sanity check that proves the orchestrator wrote both files and merged
/// the new directories into lib.rs / per-dir mod.rs. Cheap — no `cargo
/// check`, just file inspection. Catches orchestrator-integration
/// regressions without the multi-minute compile cost.
#[tokio::test]
async fn test_language_templates_emit_files_into_project_tree() {
    let dir = tempdir().expect("tempdir creates");
    let registry = Arc::new(TemplateRegistry::with_builtins());
    let gen = Generator::new(registry, dir.path().to_path_buf());
    let slug = Slug::new("lang-smoke-files").expect("slug valid");
    tokio::fs::create_dir(dir.path().join(slug.as_str()))
        .await
        .expect("project dir creates");

    let report = gen
        .generate_project(&slug, &smoke_graph())
        .await
        .expect("generate_project succeeds");

    // Both template emissions present in the report.
    assert!(
        report.files_written.contains(&"src/types/user.rs".to_string()),
        "expected struct file in report: {:?}",
        report.files_written
    );
    assert!(
        report.files_written.contains(&"src/functions/make_user.rs".to_string()),
        "expected fn file in report: {:?}",
        report.files_written
    );

    let project_root = dir.path().join(slug.as_str());

    // The struct file contains the configured derives and fields.
    let struct_src = tokio::fs::read_to_string(project_root.join("src/types/user.rs"))
        .await
        .expect("struct file readable");
    assert!(
        struct_src.contains("pub struct User"),
        "missing struct decl:\n{struct_src}"
    );
    assert!(
        struct_src.contains("pub id: u64"),
        "missing id field:\n{struct_src}"
    );
    assert!(
        struct_src.contains("pub name: String"),
        "missing name field:\n{struct_src}"
    );

    // The fn file contains the constructed return expression.
    let fn_src = tokio::fs::read_to_string(project_root.join("src/functions/make_user.rs"))
        .await
        .expect("fn file readable");
    assert!(
        fn_src.contains("pub fn make_user"),
        "missing fn decl:\n{fn_src}"
    );
    // Tight pinning: the body must contain a struct-literal expression,
    // i.e. `User {` opening a block. The signature alone references the
    // type by name; only the body opens a brace, so this catches a
    // regression where the body would be empty or replaced by `todo!()`.
    assert!(
        fn_src.contains("User {"),
        "fn body must contain a struct literal for User, found:\n{fn_src}"
    );
    assert!(fn_src.contains("id") && fn_src.contains("name"));

    // lib.rs declares both new directories (mod types; mod functions;).
    let lib_src = tokio::fs::read_to_string(project_root.join("src/lib.rs"))
        .await
        .expect("lib.rs readable");
    assert!(lib_src.contains("mod types;"), "lib.rs missing types mod:\n{lib_src}");
    assert!(
        lib_src.contains("mod functions;"),
        "lib.rs missing functions mod:\n{lib_src}"
    );

    // Per-directory mod.rs files were created with pub-use entries.
    let types_mod = tokio::fs::read_to_string(project_root.join("src/types/mod.rs"))
        .await
        .expect("types/mod.rs readable");
    assert!(
        types_mod.contains("pub mod user;") || types_mod.contains("mod user;"),
        "types/mod.rs missing user decl:\n{types_mod}"
    );
    let fns_mod = tokio::fs::read_to_string(project_root.join("src/functions/mod.rs"))
        .await
        .expect("functions/mod.rs readable");
    assert!(
        fns_mod.contains("pub mod make_user;") || fns_mod.contains("mod make_user;"),
        "functions/mod.rs missing make_user decl:\n{fns_mod}"
    );

    // Cargo.toml exists (basic sanity).
    assert!(
        project_root.join("Cargo.toml").exists(),
        "Cargo.toml was not generated"
    );

    // CLAUDE.md exists and is correctly structured
    assert!(
        project_root.join("CLAUDE.md").exists(),
        "CLAUDE.md was not generated"
    );
    let claude_src = tokio::fs::read_to_string(project_root.join("CLAUDE.md"))
        .await
        .expect("CLAUDE.md readable");
    assert!(
        claude_src.contains("# lang_smoke_files - Visual Rust Project Contract"),
        "CLAUDE.md had unexpected content:\n{}",
        claude_src
    );
}

/// Hard contract test: drive `cargo check` against the generated tree and
/// assert it succeeds. This is the S15a closure — proves a Struct + Fn
/// graph round-trips to real compilable Rust.
///
/// Gated behind the env var `LANG_SMOKE_CARGO_CHECK=1` because invoking
/// `cargo check` on a fresh user-project compiles ~30 transitive
/// dependencies (~30s-2min cold). The cheap file-shape test above runs
/// unconditionally; this exhaustive version runs on demand in CI or when
/// a human wants the full smoke. The plumbing is identical; only the
/// final assertion changes.
///
/// To run: `LANG_SMOKE_CARGO_CHECK=1 cargo test --test language_smoke
/// -- --include-ignored`.
#[tokio::test]
#[ignore = "slow (cargo check on a generated user-project); set LANG_SMOKE_CARGO_CHECK=1 + --include-ignored"]
async fn test_language_templates_generated_project_compiles() {
    if std::env::var("LANG_SMOKE_CARGO_CHECK").as_deref() != Ok("1") {
        // Belt and braces: if someone passes `--include-ignored` without
        // the env var, still skip rather than wasting a CI slot. Print
        // so the omission is visible.
        eprintln!("test_language_templates_generated_project_compiles: skipped (set LANG_SMOKE_CARGO_CHECK=1)");
        return;
    }

    let dir = tempdir().expect("tempdir creates");
    let registry = Arc::new(TemplateRegistry::with_builtins());
    let gen = Generator::new(registry, dir.path().to_path_buf());
    let slug = Slug::new("lang-smoke-compile").expect("slug valid");
    tokio::fs::create_dir(dir.path().join(slug.as_str()))
        .await
        .expect("project dir creates");

    gen.generate_project(&slug, &smoke_graph())
        .await
        .expect("generate_project succeeds");

    let project_root = dir.path().join(slug.as_str());
    let manifest = project_root.join("Cargo.toml");
    assert!(manifest.exists(), "Cargo.toml must exist before cargo check");

    // `cargo check` rather than `cargo build`: faster, same level of
    // type-check rigour. Offline-friendly when the registry cache is warm.
    let output = Command::new("cargo")
        .arg("check")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--quiet")
        .output()
        .await
        .expect("cargo invocation succeeds");

    if !output.status.success() {
        panic!(
            "cargo check failed for generated project:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

// Dummy reference to silence dead-code lint when Edge is unused above
// (the import is needed because the type's path is `crate::Edge` rather
// than from a sub-module).
#[allow(dead_code)]
fn _edge_id_used() -> EdgeId {
    EdgeId(String::new())
}

#[tokio::test]
async fn test_language_clone_edge_and_dataflow_inference() {
    let dir = tempdir().expect("tempdir creates");
    let registry = Arc::new(TemplateRegistry::with_builtins());
    let gen = Generator::new(registry, dir.path().to_path_buf());
    let slug = Slug::new("clone-integration").expect("slug valid");
    tokio::fs::create_dir(dir.path().join(slug.as_str()))
        .await
        .expect("project dir creates");

    let graph = Graph {
        schema_version: GRAPH_SCHEMA_VERSION,
        nodes: vec![
            Node {
                id: NodeId("n_json".into()),
                template_id: TemplateId::new("parser.json").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "schema_file": "person.json",
                    "name": "my_parser"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_clone".into()),
                template_id: TemplateId::new("language.clone").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "name": "my_clone"
                }),
                label: None,
                comment: None,
            },
        ],
        edges: vec![
            Edge {
                id: EdgeId("e1".into()),
                source: NodeId("n_json".into()),
                target: NodeId("n_clone".into()),
                source_port: "value".into(),
                target_port: "input".into(),
            },
        ],
    };

    // Write a dummy person.json in the project root so parser.json can read it
    tokio::fs::write(
        dir.path().join("clone-integration/person.json"),
        r#"{"title":"Person","type":"object","properties":{"name":{"type":"string"}},"required":["name"]}"#,
    ).await.unwrap();

    let report = gen
        .generate_project(&slug, &graph)
        .await
        .expect("generate_project succeeds");

    assert!(
        report.files_written.contains(&"src/functions/my_clone.rs".to_string()),
        "expected clone function file in report: {:?}",
        report.files_written
    );

    let clone_src = tokio::fs::read_to_string(dir.path().join("clone-integration/src/functions/my_clone.rs"))
        .await
        .expect("clone file readable");

    assert!(
        clone_src.contains("my_parser_value.clone()"),
        "expected cloned variable in source, found:\n{clone_src}"
    );
}

#[tokio::test]
async fn test_language_tokio_spawn_dataflow_inference() {
    let dir = tempdir().expect("tempdir creates");
    let registry = Arc::new(TemplateRegistry::with_builtins());
    let gen = Generator::new(registry, dir.path().to_path_buf());
    let slug = Slug::new("tokio-spawn-integration").expect("slug valid");
    tokio::fs::create_dir(dir.path().join(slug.as_str()))
        .await
        .expect("project dir creates");

    let graph = Graph {
        schema_version: GRAPH_SCHEMA_VERSION,
        nodes: vec![
            Node {
                id: NodeId("n_json".into()),
                template_id: TemplateId::new("parser.json").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "schema_file": "person.json",
                    "name": "my_parser"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_spawn".into()),
                template_id: TemplateId::new("tokio.spawn").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "name": "worker",
                    "params": [
                        { "name": "my_parser_value", "ty": "crate::types::person::Person" }
                    ],
                    "body": "let _ = my_parser_value.name;"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_spawn_blocking".into()),
                template_id: TemplateId::new("tokio.spawn_blocking").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "name": "blocking_worker",
                    "params": [
                        { "name": "my_parser_value", "ty": "crate::types::person::Person" }
                    ],
                    "body": "let _ = my_parser_value.name;",
                    "return_type": "()"
                }),
                label: None,
                comment: None,
            },
        ],
        edges: vec![
            Edge {
                id: EdgeId("e1".into()),
                source: NodeId("n_json".into()),
                target: NodeId("n_spawn".into()),
                source_port: "value".into(),
                target_port: "my_parser_value".into(),
            },
            Edge {
                id: EdgeId("e2".into()),
                source: NodeId("n_json".into()),
                target: NodeId("n_spawn_blocking".into()),
                source_port: "value".into(),
                target_port: "my_parser_value".into(),
            },
        ],
    };

    // Write a dummy person.json in the project root so parser.json can read it
    tokio::fs::write(
        dir.path().join("tokio-spawn-integration/person.json"),
        r#"{"title":"Person","type":"object","properties":{"name":{"type":"string"}},"required":["name"]}"#,
    ).await.unwrap();

    let report = gen
        .generate_project(&slug, &graph)
        .await
        .expect("generate_project succeeds");

    assert!(
        report.files_written.contains(&"src/runtime/worker.rs".to_string()),
        "expected spawn function file in report: {:?}",
        report.files_written
    );
    assert!(
        report.files_written.contains(&"src/runtime/blocking_worker.rs".to_string()),
        "expected spawn_blocking function file in report: {:?}",
        report.files_written
    );

    let spawn_src = tokio::fs::read_to_string(dir.path().join("tokio-spawn-integration/src/runtime/worker.rs"))
        .await
        .expect("spawn file readable");

    let spawn_blocking_src = tokio::fs::read_to_string(dir.path().join("tokio-spawn-integration/src/runtime/blocking_worker.rs"))
        .await
        .expect("spawn_blocking file readable");

    // The dataflow analyzer should infer ArcShared for my_parser_value since it is shared across multiple concurrent tasks.
    // Therefore, the clone comments should be replaced by a .clone() call on the parameter.
    assert!(
        spawn_src.contains("let my_parser_value = my_parser_value.clone();"),
        "expected clone call in spawn source, found:\n{spawn_src}"
    );
    assert!(
        spawn_blocking_src.contains("let my_parser_value = my_parser_value.clone();"),
        "expected clone call in spawn_blocking source, found:\n{spawn_blocking_src}"
    );
}

#[tokio::test]
async fn test_adapters_emit_files_into_project_tree() {
    let dir = tempdir().expect("tempdir creates");
    let registry = Arc::new(TemplateRegistry::with_builtins());
    let gen = Generator::new(registry, dir.path().to_path_buf());
    let slug = Slug::new("adapters-smoke").expect("slug valid");
    tokio::fs::create_dir(dir.path().join(slug.as_str()))
        .await
        .expect("project dir creates");

    let graph = Graph {
        schema_version: GRAPH_SCHEMA_VERSION,
        nodes: vec![
            Node {
                id: NodeId("n_cron".into()),
                template_id: TemplateId::new("integration.scheduler").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "cron": "*/2 * * * * *",
                    "name": "my_cron"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_tail".into()),
                template_id: TemplateId::new("integration.file_tail").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "file_path": "tail.log",
                    "poll_interval_millis": 200,
                    "name": "my_tail"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_webhook".into()),
                template_id: TemplateId::new("integration.http_client").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "url": "https://httpbin.org/post",
                    "method": "POST",
                    "name": "my_webhook"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_db".into()),
                template_id: TemplateId::new("integration.db_writer").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "db_path": "app.db",
                    "query": "INSERT INTO logs (line) VALUES (?1)",
                    "name": "my_db"
                }),
                label: None,
                comment: None,
            },
        ],
        edges: vec![
            Edge {
                id: EdgeId("e1".into()),
                source: NodeId("n_tail".into()),
                target: NodeId("n_db".into()),
                source_port: "line".into(),
                target_port: "params".into(),
            },
            Edge {
                id: EdgeId("e2".into()),
                source: NodeId("n_cron".into()),
                target: NodeId("n_webhook".into()),
                source_port: "tick".into(),
                target_port: "body".into(),
            },
        ],
    };

    let report = gen
        .generate_project(&slug, &graph)
        .await
        .expect("generate_project succeeds");

    // Check files are generated
    assert!(report.files_written.contains(&"src/schedulers/my_cron.rs".to_string()));
    assert!(report.files_written.contains(&"src/consumers/my_tail.rs".to_string()));
    assert!(report.files_written.contains(&"src/integrations/my_webhook.rs".to_string()));
    assert!(report.files_written.contains(&"src/integrations/my_db.rs".to_string()));
    assert!(report.files_written.contains(&"Cargo.toml".to_string()));
    assert!(report.files_written.contains(&"CLAUDE.md".to_string()));

    let project_root = dir.path().join(slug.as_str());

    // Cargo.toml must merge the required dependencies
    let cargo_toml = tokio::fs::read_to_string(project_root.join("Cargo.toml")).await.unwrap();
    assert!(cargo_toml.contains("cron = "));
    assert!(cargo_toml.contains("chrono = "));
    assert!(cargo_toml.contains("reqwest = "));
    assert!(cargo_toml.contains("tokio-rusqlite = "));
    assert!(cargo_toml.contains("rusqlite = "));

    // Verify CLAUDE.md exists and is correctly structured
    let claude_md = tokio::fs::read_to_string(project_root.join("CLAUDE.md")).await.unwrap();
    assert!(claude_md.contains("# adapters_smoke - Visual Rust Project Contract"), "CLAUDE.md had unexpected content:\n{}", claude_md);

    // Verify main.rs registers cron and file tail tasks as spawns
    let main_rs = tokio::fs::read_to_string(project_root.join("src/main.rs")).await.unwrap();
    assert!(main_rs.contains("adapters_smoke::schedulers::my_cron::run()"), "main_rs did not contain expected cron run, content:\n{}", main_rs);
    assert!(main_rs.contains("adapters_smoke::consumers::my_tail::run()"), "main_rs did not contain expected tail run, content:\n{}", main_rs);
}

#[tokio::test]
#[ignore = "slow (cargo check on a generated user-project); set LANG_SMOKE_CARGO_CHECK=1 + --include-ignored"]
async fn test_adapters_generated_project_compiles() {
    if std::env::var("LANG_SMOKE_CARGO_CHECK").as_deref() != Ok("1") {
        eprintln!("test_adapters_generated_project_compiles: skipped (set LANG_SMOKE_CARGO_CHECK=1)");
        return;
    }

    let dir = tempdir().expect("tempdir creates");
    let registry = Arc::new(TemplateRegistry::with_builtins());
    let gen = Generator::new(registry, dir.path().to_path_buf());
    let slug = Slug::new("adapters-smoke-compile").expect("slug valid");
    tokio::fs::create_dir(dir.path().join(slug.as_str()))
        .await
        .expect("project dir creates");

    let graph = Graph {
        schema_version: GRAPH_SCHEMA_VERSION,
        nodes: vec![
            Node {
                id: NodeId("n_cron".into()),
                template_id: TemplateId::new("integration.scheduler").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "cron": "*/2 * * * * *",
                    "name": "my_cron"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_tail".into()),
                template_id: TemplateId::new("integration.file_tail").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "file_path": "tail.log",
                    "poll_interval_millis": 200,
                    "name": "my_tail"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_webhook".into()),
                template_id: TemplateId::new("integration.http_client").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "url": "https://httpbin.org/post",
                    "method": "POST",
                    "name": "my_webhook"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_db".into()),
                template_id: TemplateId::new("integration.db_writer").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "db_path": "app.db",
                    "query": "CREATE TABLE IF NOT EXISTS logs (line TEXT)",
                    "name": "my_db"
                }),
                label: None,
                comment: None,
            },
        ],
        edges: vec![
            Edge {
                id: EdgeId("e1".into()),
                source: NodeId("n_tail".into()),
                target: NodeId("n_db".into()),
                source_port: "line".into(),
                target_port: "params".into(),
            },
            Edge {
                id: EdgeId("e2".into()),
                source: NodeId("n_cron".into()),
                target: NodeId("n_webhook".into()),
                source_port: "tick".into(),
                target_port: "body".into(),
            },
        ],
    };

    gen.generate_project(&slug, &graph)
        .await
        .expect("generate_project succeeds");

    let project_root = dir.path().join(slug.as_str());
    let manifest = project_root.join("Cargo.toml");

    let output = Command::new("cargo")
        .arg("check")
        .arg("--manifest-path")
        .arg(&manifest)
        .arg("--quiet")
        .output()
        .await
        .expect("cargo check succeeds");

    if !output.status.success() {
        panic!(
            "cargo check failed for generated adapters project:\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[tokio::test]
async fn test_custom_block_and_universal_connectors_emit_files() {
    let dir = tempdir().expect("tempdir creates");
    let registry = Arc::new(TemplateRegistry::with_builtins());
    let gen = Generator::new(registry, dir.path().to_path_buf());
    let slug = Slug::new("custom-connectors-smoke").expect("slug valid");
    tokio::fs::create_dir(dir.path().join(slug.as_str()))
        .await
        .expect("project dir creates");

    let graph = Graph {
        schema_version: GRAPH_SCHEMA_VERSION,
        nodes: vec![
            Node {
                id: NodeId("n_custom".into()),
                template_id: TemplateId::new("custom.block").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "name": "my_custom_fn",
                    "code": "pub fn my_custom_fn(x: i32) -> i32 { x + 1 }"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_kafka_consumer".into()),
                template_id: TemplateId::new("integration.kafka_consumer").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "brokers": "localhost:9092",
                    "topic": "test-topic",
                    "group": "test-group",
                    "name": "my_kafka_consumer"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_kafka_producer".into()),
                template_id: TemplateId::new("integration.kafka_producer").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "brokers": "localhost:9092",
                    "topic": "test-topic",
                    "name": "my_kafka_producer"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_redis".into()),
                template_id: TemplateId::new("integration.redis").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "connection_string": "redis://127.0.0.1:6379",
                    "operation": "SET",
                    "name": "my_redis"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_sql".into()),
                template_id: TemplateId::new("integration.sql_connector").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "connection_string": "postgresql://postgres:postgres@localhost:5432/db",
                    "query": "SELECT * FROM users WHERE id = $1",
                    "name": "my_sql"
                }),
                label: None,
                comment: None,
            },
        ],
        edges: Vec::new(),
    };

    let report = gen
        .generate_project(&slug, &graph)
        .await
        .expect("generate_project succeeds");

    // Assert that files are generated in correct directories
    assert!(report.files_written.contains(&"src/functions/my_custom_fn.rs".to_string()));
    assert!(report.files_written.contains(&"src/consumers/my_kafka_consumer.rs".to_string()));
    assert!(report.files_written.contains(&"src/integrations/my_kafka_producer.rs".to_string()));
    assert!(report.files_written.contains(&"src/integrations/my_redis.rs".to_string()));
    assert!(report.files_written.contains(&"src/integrations/my_sql.rs".to_string()));
}

#[tokio::test]
async fn test_cep_operators_pipeline_smoke() {
    let dir = tempdir().expect("tempdir creates");
    let registry = Arc::new(TemplateRegistry::with_builtins());
    let gen = Generator::new(registry, dir.path().to_path_buf());
    let slug = Slug::new("cep-smoke-pipeline").expect("slug valid");
    tokio::fs::create_dir(dir.path().join(slug.as_str()))
        .await
        .expect("project dir creates");

    let graph = Graph {
        schema_version: GRAPH_SCHEMA_VERSION,
        nodes: vec![
            Node {
                id: NodeId("n_struct".into()),
                template_id: TemplateId::new("language.struct").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "name": "StatusEvent",
                    "derives": ["Debug", "Clone", "Serialize", "Deserialize"],
                    "fields": [
                        { "name": "id",   "ty": "String" },
                        { "name": "status", "ty": "String" },
                        { "name": "value", "ty": "f64" }
                    ]
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_filter".into()),
                template_id: TemplateId::new("stream.filter").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "name": "critical_filter",
                    "predicate": "event.value > 100.0",
                    "item_type": "crate::types::status_event::StatusEvent",
                    "parallel": true
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_map".into()),
                template_id: TemplateId::new("stream.map").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "name": "doubler_map",
                    "expression": "crate::types::status_event::StatusEvent { id: event.id.clone(), status: event.status.clone(), value: event.value * 2.0 }",
                    "input_type": "crate::types::status_event::StatusEvent",
                    "output_type": "crate::types::status_event::StatusEvent",
                    "parallel": false
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_select".into()),
                template_id: TemplateId::new("stream.select").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "name": "proj_select",
                    "fields": "id, status",
                    "input_type": "crate::types::status_event::StatusEvent"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_union".into()),
                template_id: TemplateId::new("stream.union").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "name": "merge_union",
                    "item_type": "crate::types::status_event::StatusEvent"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_join".into()),
                template_id: TemplateId::new("stream.join").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "name": "event_join",
                    "left_key": "left.id",
                    "right_key": "right.id",
                    "window_seconds": 10,
                    "left_type": "crate::types::status_event::StatusEvent",
                    "right_type": "crate::types::status_event::StatusEvent"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_window".into()),
                template_id: TemplateId::new("stream.window").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "name": "avg_window",
                    "window_type": "Tumbling",
                    "trigger_type": "Count",
                    "trigger_value": 5,
                    "aggregation_fn": "AVG",
                    "field_to_aggregate": "event.value",
                    "input_type": "crate::types::status_event::StatusEvent"
                }),
                label: None,
                comment: None,
            },
            Node {
                id: NodeId("n_pattern".into()),
                template_id: TemplateId::new("stream.pattern").unwrap(),
                position: Position { x: 0.0, y: 0.0 },
                config: serde_json::json!({
                    "name": "status_pattern",
                    "predicate_a": "event.status == \"CRITICAL\"",
                    "predicate_b": "event.status == \"RESOLVED\"",
                    "window_seconds": 15,
                    "input_type": "crate::types::status_event::StatusEvent"
                }),
                label: None,
                comment: None,
            },
        ],
        edges: vec![
            Edge {
                id: EdgeId("e1".into()),
                source: NodeId("n_filter".into()),
                source_port: "output".into(),
                target: NodeId("n_map".into()),
                target_port: "input".into(),
            },
            Edge {
                id: EdgeId("e2".into()),
                source: NodeId("n_map".into()),
                source_port: "output".into(),
                target: NodeId("n_select".into()),
                target_port: "input".into(),
            },
        ],
    };

    let report = gen
        .generate_project(&slug, &graph)
        .await
        .expect("generate_project succeeds");

    // Verify all stream modules were emitted in the report.
    assert!(report.files_written.contains(&"src/streams/critical_filter.rs".to_string()));
    assert!(report.files_written.contains(&"src/streams/doubler_map.rs".to_string()));
    assert!(report.files_written.contains(&"src/streams/proj_select.rs".to_string()));
    assert!(report.files_written.contains(&"src/streams/merge_union.rs".to_string()));
    assert!(report.files_written.contains(&"src/streams/event_join.rs".to_string()));
    assert!(report.files_written.contains(&"src/streams/avg_window.rs".to_string()));
    assert!(report.files_written.contains(&"src/streams/status_pattern.rs".to_string()));
}

