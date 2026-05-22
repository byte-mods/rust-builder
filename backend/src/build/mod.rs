//! Build orchestrator — drives `cargo check` / `cargo build` for user-projects
//! and streams stdout/stderr to WebSocket subscribers.

use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

/// One line or lifecycle event emitted by a cargo process.
#[derive(Debug, Clone, Serialize)]
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

    /// Spawn `cargo check` (or `cargo build --release`) for `slug` in
    /// `project_dir` and broadcast output to any WebSocket subscribers.
    ///
    /// Returns immediately once the process is spawned; the actual build runs
    /// in background tasks.
    pub async fn start_build(
        &self,
        slug: &str,
        project_dir: PathBuf,
        release: bool,
    ) -> Result<(), BuildError> {
        if self.running.contains_key(slug) {
            return Err(BuildError::AlreadyRunning(slug.to_string()));
        }

        let tx = self.get_or_create_channel(slug);
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&project_dir)
            .arg(if release { "build" } else { "check" })
            .arg("--message-format=short");
        if release {
            cmd.arg("--release");
        }
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn()?;
        let command_str = format!(
            "cargo {} --message-format=short",
            if release { "build --release" } else { "check" }
        );
        info!(%slug, ?project_dir, release, "started cargo build");

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
            .start_build("test", project_dir.clone(), false)
            .await
            .expect("start_build succeeds");

        // A second start while the first is running must fail.
        let err = manager
            .start_build("test", project_dir, false)
            .await
            .expect_err("second start_build should fail");
        assert!(
            matches!(err, BuildError::AlreadyRunning(ref s) if s == "test"),
            "expected AlreadyRunning, got {err:?}"
        );

        // Collect events until Exit.
        let mut saw_start = false;
        let mut exit_code = None;

        loop {
            let event = timeout(Duration::from_secs(60), rx.recv())
                .await
                .expect("build should finish within 60s")
                .expect("channel should not close");

            match event {
                BuildEvent::Start { .. } => saw_start = true,
                BuildEvent::Exit { code } => {
                    exit_code = Some(code);
                    break;
                }
                _ => {}
            }
        }

        assert!(saw_start, "should have received Start event");
        assert_eq!(exit_code, Some(0), "cargo check should succeed");
    }
}
