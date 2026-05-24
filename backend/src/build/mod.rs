//! Build orchestrator — drives `cargo check` / `cargo build` for user-projects
//! and streams stdout/stderr to WebSocket subscribers.

use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::projects::Graph;


/// Which cargo subcommand the build manager should drive for a given click.
///
/// S16 extends the manager beyond `cargo check` / `cargo build --release` so
/// the `/test` endpoint can stream `cargo test` output through the same
/// broadcast pipeline. New verbs land here; the argv is computed from the
/// variant in one place so the wire-side `BuildEvent::Start.command`
/// description stays in sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildVerb {
    /// `cargo check` — fast type-checking pass, the default for the
    /// editor's "Check" button.
    Check,
    /// `cargo build --release` — full optimised build.
    BuildRelease,
    /// `cargo test` — runs every `#[test]` in the user-project.
    Test,
}

impl BuildVerb {
    /// Append the verb-specific arguments to a `Command`. `--message-format=short`
    /// is appended by the caller after this returns, so output formatting stays
    /// uniform across verbs.
    fn argv(&self, cmd: &mut Command) {
        match self {
            BuildVerb::Check => {
                cmd.arg("check");
            }
            BuildVerb::BuildRelease => {
                cmd.arg("build").arg("--release");
            }
            BuildVerb::Test => {
                cmd.arg("test");
            }
        }
    }

    /// Human-readable command string emitted on the `BuildEvent::Start`
    /// event — the frontend echoes this verbatim into its build log header.
    fn command_str(&self) -> &'static str {
        match self {
            BuildVerb::Check => "cargo check",
            BuildVerb::BuildRelease => "cargo build --release",
            BuildVerb::Test => "cargo test",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParsedDiagnostic {
    pub file_path: String,
    pub line: usize,
    pub column: usize,
    pub severity: String, // "error" | "warning"
    pub message: String,
    pub code: Option<String>,
    pub node_id: Option<String>,
}

/// One line or lifecycle event emitted by a cargo process.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case", tag = "stream")]
pub enum BuildEvent {
    /// Build process has started.
    Start { command: String },
    /// Line emitted on stdout.
    Stdout { line: String },
    /// Line emitted on stderr.
    Stderr { line: String },
    /// Build process exited with the given status code.
    Exit { code: i32 },
    /// A structured compiler diagnostic message.
    Diagnostic { diagnostic: ParsedDiagnostic },
}


/// Errors the build orchestrator can return.
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    /// Another build for the same project is already running.
    #[error("build already running for project `{0}`")]
    AlreadyRunning(String),

    /// Spawning the cargo process or setting up pipes failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Global build manager shared across handlers via `AppState`.
///
/// Tracks at most one active build per project slug. Starting a build while
/// another is running returns [`BuildError::AlreadyRunning`].
///
/// Each project gets a persistent broadcast channel so WebSocket clients can
/// connect before a build starts and still receive output once it begins.
/// Channels are never dropped — they live for the lifetime of the manager —
/// which removes the reconnection race between clicking "Build" and the
/// WebSocket handshake.
#[derive(Clone)]
pub struct BuildManager {
    channels: Arc<DashMap<String, broadcast::Sender<BuildEvent>>>,
    running: Arc<DashMap<String, ()>>,
}

impl BuildManager {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(DashMap::new()),
            running: Arc::new(DashMap::new()),
        }
    }

    /// Return the broadcast sender for `slug`, creating one if necessary.
    fn get_or_create_channel(&self, slug: &str) -> broadcast::Sender<BuildEvent> {
        if let Some(entry) = self.channels.get(slug) {
            entry.clone()
        } else {
            let (tx, _rx) = broadcast::channel::<BuildEvent>(256);
            self.channels.insert(slug.to_string(), tx.clone());
            tx
        }
    }

    /// Spawn the cargo subcommand selected by `verb` for `slug` in
    /// `project_dir` and broadcast output to any WebSocket subscribers.
    ///
    /// Returns immediately once the process is spawned; the actual build
    /// runs in background tasks. The `AlreadyRunning` guard is per-slug and
    /// verb-agnostic — a running `Check` blocks a `Test` start, which is
    /// correct because cargo serialises lockfile access on the same target
    /// directory anyway.
    pub async fn start_build(
        &self,
        slug: &str,
        project_dir: PathBuf,
        verb: BuildVerb,
        graph: Option<Graph>,
    ) -> Result<(), BuildError> {
        if self.running.contains_key(slug) {
            return Err(BuildError::AlreadyRunning(slug.to_string()));
        }

        let tx = self.get_or_create_channel(slug);
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&project_dir);
        verb.argv(&mut cmd);
        cmd.arg("--message-format=json");
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn()?;
        let command_str = verb.command_str().to_string();
        info!(%slug, ?project_dir, ?verb, "started cargo build");

        self.running.insert(slug.to_string(), ());
        let _ = tx.send(BuildEvent::Start { command: command_str });

        let tx_stdout = tx.clone();
        let tx_stderr = tx.clone();
        let slug_owned = slug.to_string();

        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let stdout_task = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // Try to parse compilation JSON structures
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&line) {
                    if val.get("reason").and_then(|v| v.as_str()) == Some("compiler-message") {
                        if let Some(msg) = val.get("message") {
                            let severity = msg.get("level").and_then(|v| v.as_str()).unwrap_or("error").to_string();
                            let message = msg.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let code = msg.get("code").and_then(|c| c.get("code")).and_then(|v| v.as_str()).map(|s| s.to_string());
                            let rendered = msg.get("rendered").and_then(|v| v.as_str()).unwrap_or("");

                            let mut file_path = String::new();
                            let mut line_num = 1;
                            let mut col_num = 1;
                            if let Some(spans) = msg.get("spans").and_then(|v| v.as_array()) {
                                for span in spans {
                                    if span.get("is_primary").and_then(|v| v.as_bool()) == Some(true) {
                                        file_path = span.get("file_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                        line_num = span.get("line_start").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
                                        col_num = span.get("column_start").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
                                        break;
                                    }
                                }
                            }

                            if !file_path.is_empty() {
                                let node_id = if let Some(ref g) = graph {
                                    map_file_to_node(&file_path, g)
                                } else {
                                    None
                                };

                                let diag = ParsedDiagnostic {
                                    file_path,
                                    line: line_num,
                                    column: col_num,
                                    severity,
                                    message,
                                    code,
                                    node_id,
                                };

                                let _ = tx_stdout.send(BuildEvent::Diagnostic { diagnostic: diag });
                            }

                            for r_line in rendered.lines() {
                                let _ = tx_stdout.send(BuildEvent::Stdout { line: r_line.to_string() });
                            }
                            continue;
                        }
                    }
                }

                // Fallback for non-JSON lines
                let _ = tx_stdout.send(BuildEvent::Stdout { line });
            }
        });

        let stderr_task = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tx_stderr.send(BuildEvent::Stderr { line });
            }
        });

        let tx_exit = tx.clone();
        let running = Arc::clone(&self.running);

        tokio::spawn(async move {
            let _ = stdout_task.await;
            let _ = stderr_task.await;

            match child.wait().await {
                Ok(status) => {
                    let code = status.code().unwrap_or(-1);
                    let _ = tx_exit.send(BuildEvent::Exit { code });
                    info!(slug = %slug_owned, code, "cargo build finished");
                }
                Err(e) => {
                    warn!(slug = %slug_owned, error = %e, "failed to wait on cargo process");
                    let _ = tx_exit.send(BuildEvent::Exit { code: -1 });
                }
            }

            running.remove(&slug_owned);
        });

        Ok(())
    }


    /// Subscribe to build events for `slug`.
    ///
    /// Returns a receiver even if no build is currently running — the
    /// receiver will block until the next build starts and emits events.
    pub fn subscribe(&self, slug: &str) -> broadcast::Receiver<BuildEvent> {
        self.get_or_create_channel(slug).subscribe()
    }
}

/// Standalone helper to map a compiler error file path (relative to `src/`)
/// back to the canvas node ID that generated it.
pub fn map_file_to_node(file_path: &str, graph: &Graph) -> Option<String> {
    // Normalise path separators and strip "src/" prefix
    let path = file_path
        .replace('\\', "/")
        .strip_prefix("src/")
        .unwrap_or(&file_path)
        .to_string();

    for node in &graph.nodes {
        // Simple and safe lookup of configured element name
        if let Some(name_val) = node.config.get("name").and_then(|v| v.as_str()) {
            let snake = to_snake_case(name_val);
            let template_id = node.template_id.as_str();

            if template_id.starts_with("language.struct") || template_id.starts_with("language.enum") {
                if path == format!("types/{}.rs", snake) {
                    return Some(node.id.0.clone());
                }
            } else if template_id.starts_with("language.fn")
                || template_id.starts_with("language.if_else")
                || template_id.starts_with("language.match")
                || template_id.starts_with("language.loop")
            {
                if path == format!("functions/{}.rs", snake) {
                    return Some(node.id.0.clone());
                }
            } else if template_id.starts_with("http.handler") {
                if path == format!("handlers/{}.rs", snake) {
                    return Some(node.id.0.clone());
                }
            } else if template_id.starts_with("core.service") {
                if path == format!("services/{}.rs", snake) {
                    return Some(node.id.0.clone());
                }
            } else if template_id.starts_with("core.dto") {
                if path == format!("dto/{}.rs", snake) {
                    return Some(node.id.0.clone());
                }
            } else if template_id.starts_with("observability.logger") {
                if path == format!("loggers/{}.rs", snake) {
                    return Some(node.id.0.clone());
                }
            } else if template_id.starts_with("parser.") {
                if path == format!("parsers/{}.rs", snake) {
                    return Some(node.id.0.clone());
                }
            } else if template_id.starts_with("integration.consumer") {
                if path == format!("consumers/{}.rs", snake) {
                    return Some(node.id.0.clone());
                }
            } else if template_id.starts_with("integration.scheduler") {
                if path == format!("schedulers/{}.rs", snake) {
                    return Some(node.id.0.clone());
                }
            } else if template_id.starts_with("tokio.") {
                if path == format!("runtime/{}.rs", snake) {
                    return Some(node.id.0.clone());
                }
            }
        }
    }

    None
}

/// Simple and robust camelCase/PascalCase to snake_case converter
fn to_snake_case(s: &str) -> String {
    let mut snake = String::new();
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i > 0 {
                snake.push('_');
            }
            snake.push(ch.to_ascii_lowercase());
        } else {
            snake.push(ch);
        }
    }
    snake
}


// ---------------------------------------------------------------------------
// WebSocket handler
// ---------------------------------------------------------------------------

use axum::extract::{Path, State, ws::{WebSocket, WebSocketUpgrade}};
use axum::response::Response;

/// `GET /ws/build/:slug` — upgrade to WebSocket and stream build events.
pub async fn build_ws(
    ws: WebSocketUpgrade,
    Path(slug): Path<String>,
    State(state): State<crate::AppState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_build_socket(socket, slug, state))
}

async fn handle_build_socket(
    mut socket: WebSocket,
    slug: String,
    state: crate::AppState,
) {
    let mut rx = state.build_manager.subscribe(&slug);

    while let Ok(event) = rx.recv().await {
        let text = match serde_json::to_string(&event) {
            Ok(t) => t,
            Err(e) => {
                error!(error = %e, "failed to serialise BuildEvent");
                continue;
            }
        };
        if socket
            .send(axum::extract::ws::Message::Text(text))
            .await
            .is_err()
        {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{timeout, Duration};

    #[test]
    fn test_map_file_to_node_matches_proper_paths() {
        use crate::projects::types::{Graph, Node, NodeId};
        use crate::templates::TemplateId;
        use serde_json::json;

        let graph = Graph {
            schema_version: 1,
            nodes: vec![
                Node {
                    id: NodeId("n1".to_string()),
                    template_id: TemplateId::new("language.fn").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: json!({ "name": "getUserInfo" }),
                    label: None,
                    comment: None,
                },
                Node {
                    id: NodeId("n2".to_string()),
                    template_id: TemplateId::new("core.service").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: json!({ "name": "process_orders" }),
                    label: None,
                    comment: None,
                },
                Node {
                    id: NodeId("n3".to_string()),
                    template_id: TemplateId::new("parser.json").unwrap(),
                    position: crate::projects::types::Position { x: 0.0, y: 0.0 },
                    config: json!({ "name": "PersonParser" }),
                    label: None,
                    comment: None,
                },
            ],
            edges: vec![],
        };

        // language.fn with camelCase name getUserInfo maps to functions/get_user_info.rs
        assert_eq!(
            map_file_to_node("src/functions/get_user_info.rs", &graph),
            Some("n1".to_string())
        );

        // core.service process_orders maps to services/process_orders.rs
        assert_eq!(
            map_file_to_node("src/services/process_orders.rs", &graph),
            Some("n2".to_string())
        );

        // parser.json PersonParser maps to parsers/person_parser.rs
        assert_eq!(
            map_file_to_node("src/parsers/person_parser.rs", &graph),
            Some("n3".to_string())
        );

        // No match
        assert_eq!(
            map_file_to_node("src/handlers/unknown.rs", &graph),
            None
        );
    }

    #[test]
    fn test_parse_cargo_json_diagnostics_message() {
        let json_line = r#"{
            "reason": "compiler-message",
            "package_id": "user_proj 0.1.0",
            "target": { "kind": ["bin"], "crate_types": ["bin"], "name": "user_proj", "src_path": "src/main.rs", "edition": "2021" },
            "message": {
                "rendered": "error[E0308]: mismatched types\n  --> src/functions/get_user_info.rs:5:10\n",
                "code": { "code": "E0308", "explanation": "..." },
                "level": "error",
                "spans": [
                    {
                        "file_name": "src/functions/get_user_info.rs",
                        "line_start": 5,
                        "line_end": 5,
                        "column_start": 10,
                        "column_end": 15,
                        "is_primary": true
                    }
                ],
                "message": "mismatched types"
            }
        }"#;

        let val: serde_json::Value = serde_json::from_str(json_line).unwrap();
        assert_eq!(val.get("reason").and_then(|v| v.as_str()), Some("compiler-message"));

        let msg = val.get("message").unwrap();
        let severity = msg.get("level").and_then(|v| v.as_str()).unwrap().to_string();
        let message = msg.get("message").and_then(|v| v.as_str()).unwrap().to_string();
        let code = msg.get("code").and_then(|c| c.get("code")).and_then(|v| v.as_str()).map(|s| s.to_string());

        let mut file_path = String::new();
        let mut line_num = 1;
        let mut col_num = 1;
        let spans = msg.get("spans").unwrap().as_array().unwrap();
        for span in spans {
            if span.get("is_primary").unwrap().as_bool() == Some(true) {
                file_path = span.get("file_name").unwrap().as_str().unwrap().to_string();
                line_num = span.get("line_start").unwrap().as_u64().unwrap() as usize;
                col_num = span.get("column_start").unwrap().as_u64().unwrap() as usize;
            }
        }

        assert_eq!(severity, "error");
        assert_eq!(message, "mismatched types");
        assert_eq!(code, Some("E0308".to_string()));
        assert_eq!(file_path, "src/functions/get_user_info.rs");
        assert_eq!(line_num, 5);
        assert_eq!(col_num, 10);
    }


    #[tokio::test]
    async fn test_build_manager_runs_cargo_check_on_minimal_project() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project_dir = dir.path().join("test_proj");
        tokio::fs::create_dir_all(project_dir.join("src"))
            .await
            .expect("create src dir");

        // Minimal valid Rust project with no external dependencies.
        tokio::fs::write(
            project_dir.join("Cargo.toml"),
            r#"[package]
name = "test_proj"
version = "0.1.0"
edition = "2021"
"#,
        )
        .await
        .expect("write Cargo.toml");

        tokio::fs::write(
            project_dir.join("src/main.rs"),
            "fn main() {}\n",
        )
        .await
        .expect("write main.rs");

        let manager = BuildManager::new();

        // Subscribe BEFORE starting the build so we don't race the Start event.
        let mut rx = manager.subscribe("test");

        manager
            .start_build("test", project_dir.clone(), BuildVerb::Check, None)
            .await
            .expect("start_build succeeds");

        // A second start while the first is running must fail.
        let err = manager
            .start_build("test", project_dir, BuildVerb::Check, None)
            .await
            .expect_err("second start_build should fail");
        assert!(
            matches!(err, BuildError::AlreadyRunning(ref s) if s == "test"),
            "expected AlreadyRunning, got {err:?}"
        );

        // Collect events until Exit.
        let mut saw_start = false;
        let mut saw_check_command = false;
        #[allow(unused_assignments)]
        let mut exit_code = None;

        loop {
            let event = timeout(Duration::from_secs(60), rx.recv())
                .await
                .expect("build should finish within 60s")
                .expect("channel should not close");

            match event {
                BuildEvent::Start { ref command } => {
                    saw_start = true;
                    if command.contains("check") {
                        saw_check_command = true;
                    }
                }
                BuildEvent::Exit { code } => {
                    exit_code = Some(code);
                    break;
                }
                _ => {}
            }
        }

        assert!(saw_start, "should have received Start event");
        assert!(saw_check_command, "Start.command must reflect 'check' verb");
        assert_eq!(exit_code, Some(0), "cargo check should succeed");
    }

    #[tokio::test]
    async fn test_already_running_is_verb_agnostic() {
        // A running Check must block a Test start on the same slug. The
        // guard is per-slug, not per-verb, by design — cargo serialises
        // lockfile access on the same target dir anyway.
        let dir = tempfile::tempdir().expect("tempdir");
        let project_dir = dir.path().join("verb_agnostic");
        tokio::fs::create_dir_all(project_dir.join("src"))
            .await
            .expect("create src dir");
        tokio::fs::write(
            project_dir.join("Cargo.toml"),
            r#"[package]
name = "verb_agnostic"
version = "0.1.0"
edition = "2021"
"#,
        )
        .await
        .expect("write Cargo.toml");
        // A program that sleeps long enough for the second start_build to
        // observe AlreadyRunning before the first build completes.
        tokio::fs::write(
            project_dir.join("src/lib.rs"),
            "fn _x() { std::thread::sleep(std::time::Duration::from_secs(30)); }",
        )
        .await
        .expect("write lib.rs");

        let manager = BuildManager::new();
        manager
            .start_build("agnos", project_dir.clone(), BuildVerb::Check, None)
            .await
            .expect("first start_build (Check) succeeds");

        // Test verb on the same slug must hit the AlreadyRunning guard.
        let err = manager
            .start_build("agnos", project_dir, BuildVerb::Test, None)
            .await
            .expect_err("Test on a slug already running Check must fail");
        assert!(
            matches!(err, BuildError::AlreadyRunning(ref s) if s == "agnos"),
            "expected AlreadyRunning across verbs, got {err:?}"
        );
    }

    #[tokio::test]
    async fn test_build_release_verb_emits_release_command() {
        // Pins that BuildRelease's Start.command actually contains "--release"
        // and not just "build" — guards against future verb-table edits
        // that drop the flag from command_str but keep it in argv (or vice
        // versa).
        let dir = tempfile::tempdir().expect("tempdir");
        let project_dir = dir.path().join("rel_proj");
        tokio::fs::create_dir_all(project_dir.join("src"))
            .await
            .expect("create src dir");
        tokio::fs::write(
            project_dir.join("Cargo.toml"),
            r#"[package]
name = "rel_proj"
version = "0.1.0"
edition = "2021"
"#,
        )
        .await
        .expect("write Cargo.toml");
        tokio::fs::write(project_dir.join("src/main.rs"), "fn main() {}\n")
            .await
            .expect("write main.rs");

        let manager = BuildManager::new();
        let mut rx = manager.subscribe("relverb");
        manager
            .start_build("relverb", project_dir, BuildVerb::BuildRelease, None)
            .await
            .expect("start_build succeeds");

        // We only need the Start event — kill receiving once we see it.
        let event = timeout(Duration::from_secs(60), rx.recv())
            .await
            .expect("Start event should arrive")
            .expect("channel should not close");
        match event {
            BuildEvent::Start { command } => {
                assert!(
                    command.contains("--release"),
                    "Start.command must include --release: {command}"
                );
                assert!(command.contains("build"), "Start.command must include build: {command}");
            }
            other => panic!("expected Start, got {other:?}"),
        }
        // Drain to completion so the background tasks reap cleanly.
        while let Ok(Ok(ev)) = timeout(Duration::from_secs(60), rx.recv()).await {
            if matches!(ev, BuildEvent::Exit { .. }) {
                break;
            }
        }
    }

    #[tokio::test]
    async fn test_build_manager_runs_cargo_test_on_minimal_project() {
        // Same shape as the check test, but spawns BuildVerb::Test against
        // a project with one passing #[test]. Pins:
        // 1. Test verb routes to `cargo test` (Start.command contains it).
        // 2. The process exits 0 when the test passes.
        let dir = tempfile::tempdir().expect("tempdir");
        let project_dir = dir.path().join("test_verb_proj");
        tokio::fs::create_dir_all(project_dir.join("src"))
            .await
            .expect("create src dir");

        tokio::fs::write(
            project_dir.join("Cargo.toml"),
            r#"[package]
name = "test_verb_proj"
version = "0.1.0"
edition = "2021"
"#,
        )
        .await
        .expect("write Cargo.toml");

        // One trivial passing #[test] in lib.rs.
        tokio::fs::write(
            project_dir.join("src/lib.rs"),
            r#"
#[cfg(test)]
mod t {
    #[test]
    fn passes() {
        assert_eq!(2 + 2, 4);
    }
}
"#,
        )
        .await
        .expect("write lib.rs");

        let manager = BuildManager::new();
        let mut rx = manager.subscribe("verbtest");

        manager
            .start_build("verbtest", project_dir, BuildVerb::Test, None)
            .await
            .expect("start_build with Test verb succeeds");

        let mut saw_test_command = false;
        #[allow(unused_assignments)]
        let mut exit_code = None;
        loop {
            let event = timeout(Duration::from_secs(120), rx.recv())
                .await
                .expect("cargo test should finish within 120s")
                .expect("channel should not close");
            match event {
                BuildEvent::Start { ref command } => {
                    if command.contains("test") && !command.contains("check") {
                        saw_test_command = true;
                    }
                }
                BuildEvent::Exit { code } => {
                    exit_code = Some(code);
                    break;
                }
                _ => {}
            }
        }

        assert!(saw_test_command, "Start.command must reflect 'test' verb");
        assert_eq!(exit_code, Some(0), "cargo test on a passing test must exit 0");
    }
}
