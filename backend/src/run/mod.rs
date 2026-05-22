//! Run lifecycle manager — starts, stops, and monitors user-project binaries.
//!
//! Section 12 introduces the ability to run generated projects directly from
//! the studio. The orchestrator spawns `cargo run` in the project directory
//! and streams stdout/stderr to WebSocket subscribers via the same
//! broadcast-channel pattern used by the build orchestrator (Section 6).
//!
//! ## Design notes
//!
//! - **Per-slug exclusivity.** At most one run per project. Starting a second
//!   run returns `RunError::AlreadyRunning`.
//! - **Channels are persistent.** Like `BuildManager`, broadcast senders live
//!   for the lifetime of the manager so a WebSocket client can connect before
//!   the run starts and still receive events.
//! - **Stop is forceful.** `child.kill()` sends SIGKILL on Unix and
//!   `TerminateProcess` on Windows. A graceful SIGTERM → SIGKILL dance is
//!   deferred to a future section (no `nix` dependency today).
//! - **Cleanup on unexpected exit.** A background task waits on the child.
//!   When it exits (naturally or killed), the slug is removed from the
//!   `processes` map so subsequent status queries are accurate.

use std::path::PathBuf;
use std::sync::Arc;

use dashmap::DashMap;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

/// One line or lifecycle event emitted by a running process.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "stream")]
pub enum RunEvent {
    /// Run process has started.
    Start { command: String },
    /// Line emitted on stdout.
    Stdout { line: String },
    /// Line emitted on stderr.
    Stderr { line: String },
    /// Run process exited with the given status code.
    Exit { code: Option<i32> },
    /// Run was manually stopped by the user.
    Stop { reason: String },
}

/// Errors the run lifecycle manager can return.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// Another run for the same project is already active.
    #[error("run already active for project `{0}`")]
    AlreadyRunning(String),

    /// No run is active for the requested project.
    #[error("no active run for project `{0}`")]
    NotRunning(String),

    /// Spawning the process or setting up pipes failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Snapshot of the current run state for a project.
#[derive(Debug, Clone, Serialize)]
pub struct RunStatus {
    pub running: bool,
    pub slug: String,
}

/// Global run manager shared across handlers via `AppState`.
///
/// Tracks at most one active run per project slug. Starting a run while
/// another is active returns [`RunError::AlreadyRunning`].
///
/// Each project gets a persistent broadcast channel so WebSocket clients can
/// connect before a run starts and still receive output once it begins.
/// Channels are never dropped — they live for the lifetime of the manager.
#[derive(Clone)]
pub struct RunManager {
    channels: Arc<DashMap<String, broadcast::Sender<RunEvent>>>,
    processes: Arc<DashMap<String, tokio::process::Child>>,
}

impl RunManager {
    pub fn new() -> Self {
        Self {
            channels: Arc::new(DashMap::new()),
            processes: Arc::new(DashMap::new()),
        }
    }

    /// Return the broadcast sender for `slug`, creating one if necessary.
    fn get_or_create_channel(&self, slug: &str) -> broadcast::Sender<RunEvent> {
        if let Some(entry) = self.channels.get(slug) {
            entry.clone()
        } else {
            let (tx, _rx) = broadcast::channel::<RunEvent>(256);
            self.channels.insert(slug.to_string(), tx.clone());
            tx
        }
    }

    /// Spawn `cargo run` for `slug` in `project_dir` and broadcast output to
    /// any WebSocket subscribers.
    ///
    /// Returns immediately once the process is spawned; the actual run runs
    /// in background tasks.
    pub async fn start_run(
        &self,
        slug: &str,
        project_dir: PathBuf,
    ) -> Result<(), RunError> {
        if self.processes.contains_key(slug) {
            return Err(RunError::AlreadyRunning(slug.to_string()));
        }

        let tx = self.get_or_create_channel(slug);
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&project_dir)
            .arg("run")
            .arg("--message-format=short");
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn()?;
        let command_str = "cargo run --message-format=short".to_string();
        info!(%slug, ?project_dir, "started cargo run");

        // Take the pipes BEFORE inserting the child into the map — once
        // behind the DashMap we cannot mutably borrow it.
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        self.processes.insert(slug.to_string(), child);
        let _ = tx.send(RunEvent::Start { command: command_str });

        let tx_stdout = tx.clone();
        let tx_stderr = tx.clone();
        let _slug_owned = slug.to_string();

        let stdout_task = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tx_stdout.send(RunEvent::Stdout { line });
            }
        });

        let stderr_task = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tx_stderr.send(RunEvent::Stderr { line });
            }
        });

        let tx_exit = tx.clone();
        let processes = Arc::clone(&self.processes);
        let slug_cleanup = slug.to_string();

        tokio::spawn(async move {
            let _ = stdout_task.await;
            let _ = stderr_task.await;

            // If stop_run() already removed the child, we have nothing to
            // wait on — the stop_run task handles reaping and the Exit event.
            if let Some((_, mut child)) = processes.remove(&slug_cleanup) {
                match child.wait().await {
                    Ok(status) => {
                        let code = status.code();
                        let _ = tx_exit.send(RunEvent::Exit { code });
                        info!(slug = %slug_cleanup, ?code, "cargo run finished");
                    }
                    Err(e) => {
                        warn!(slug = %slug_cleanup, error = %e, "failed to wait on cargo run process");
                        let _ = tx_exit.send(RunEvent::Exit { code: None });
                    }
                }
            }
        });

        Ok(())
    }

    /// Stop the active run for `slug`.
    ///
    /// Sends `SIGKILL` (Unix) or terminates the process (Windows). A
    /// background task reaps the child so we do not leak zombie processes,
    /// and emits an `Exit` event once `wait()` completes.
    pub async fn stop_run(&self, slug: &str) -> Result<(), RunError> {
        let Some((_, mut child)) = self.processes.remove(slug) else {
            return Err(RunError::NotRunning(slug.to_string()));
        };

        let tx = self.get_or_create_channel(slug);
        let _ = tx.send(RunEvent::Stop {
            reason: "user requested stop".to_string(),
        });

        if let Err(e) = child.kill().await {
            warn!(slug = %slug, error = %e, "failed to kill cargo run process");
        }
        info!(%slug, "stopped cargo run");

        // Reap the child in a background task so we don't block the HTTP
        // handler on wait(). If the natural-exit cleanup task also tries to
        // remove the child, it will find the map empty and do nothing — this
        // task is the one that sends the Exit event for a manual stop.
        let tx_exit = tx.clone();
        let slug_owned = slug.to_string();
        tokio::spawn(async move {
            match child.wait().await {
                Ok(status) => {
                    let _ = tx_exit.send(RunEvent::Exit { code: status.code() });
                    info!(slug = %slug_owned, "stopped process reaped");
                }
                Err(e) => {
                    warn!(slug = %slug_owned, error = %e, "failed to reap stopped process");
                    let _ = tx_exit.send(RunEvent::Exit { code: None });
                }
            }
        });

        Ok(())
    }

    /// Return the current run status for `slug`.
    pub fn status(&self, slug: &str) -> RunStatus {
        RunStatus {
            running: self.processes.contains_key(slug),
            slug: slug.to_string(),
        }
    }

    /// Subscribe to run events for `slug`.
    ///
    /// Returns a receiver even if no run is currently active — the
    /// receiver will block until the next run starts and emits events.
    pub fn subscribe(&self, slug: &str) -> broadcast::Receiver<RunEvent> {
        self.get_or_create_channel(slug).subscribe()
    }
}

// ---------------------------------------------------------------------------
// WebSocket handler
// ---------------------------------------------------------------------------

use axum::extract::{Path, State, ws::{WebSocket, WebSocketUpgrade}};
use axum::response::Response;

/// `GET /ws/run/:slug` — upgrade to WebSocket and stream run events.
pub async fn run_ws(
    ws: WebSocketUpgrade,
    Path(slug): Path<String>,
    State(state): State<crate::AppState>,
) -> Response {
    ws.on_upgrade(move |socket| handle_run_socket(socket, slug, state))
}

async fn handle_run_socket(
    mut socket: WebSocket,
    slug: String,
    state: crate::AppState,
) {
    let mut rx = state.run_manager.subscribe(&slug);

    while let Ok(event) = rx.recv().await {
        let text = match serde_json::to_string(&event) {
            Ok(t) => t,
            Err(e) => {
                error!(error = %e, "failed to serialise RunEvent");
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
    #[allow(unused_assignments)]
    async fn test_run_manager_runs_cargo_run_on_minimal_project() {
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
            r#"fn main() {
    println!("hello from run");
    eprintln!("warn from run");
}
"#,
        )
        .await
        .expect("write main.rs");

        let manager = RunManager::new();

        // Subscribe BEFORE starting the run so we don't race the Start event.
        let mut rx = manager.subscribe("test");

        manager
            .start_run("test", project_dir.clone())
            .await
            .expect("start_run succeeds");

        // A second start while the first is running must fail.
        let err = manager
            .start_run("test", project_dir)
            .await
            .expect_err("second start_run should fail");
        assert!(
            matches!(err, RunError::AlreadyRunning(ref s) if s == "test"),
            "expected AlreadyRunning, got {err:?}"
        );

        // Collect events until Exit.
        let mut saw_start = false;
        let mut saw_stdout = false;
        let mut saw_stderr = false;
        let mut exit_code: Option<i32> = None;

        loop {
            let event = timeout(Duration::from_secs(60), rx.recv())
                .await
                .expect("run should finish within 60s")
                .expect("channel should not close");

            match event {
                RunEvent::Start { .. } => saw_start = true,
                RunEvent::Stdout { ref line } if line.contains("hello from run") => {
                    saw_stdout = true;
                }
                RunEvent::Stderr { ref line } if line.contains("warn from run") => {
                    saw_stderr = true;
                }
                RunEvent::Exit { code } => {
                    exit_code = code;
                    break;
                }
                _ => {}
            }
        }

        assert!(saw_start, "should have received Start event");
        assert!(saw_stdout, "should have received stdout");
        assert!(saw_stderr, "should have received stderr");
        assert_eq!(exit_code, Some(0), "cargo run should succeed");

        // Status should reflect stopped.
        assert!(!manager.status("test").running, "status should be stopped after exit");
    }

    #[tokio::test]
    async fn test_stop_run_kills_active_process() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project_dir = dir.path().join("sleepy");
        tokio::fs::create_dir_all(project_dir.join("src"))
            .await
            .expect("create src dir");

        tokio::fs::write(
            project_dir.join("Cargo.toml"),
            r#"[package]
name = "sleepy"
version = "0.1.0"
edition = "2021"
"#,
        )
        .await
        .expect("write Cargo.toml");

        // A program that sleeps for 5 minutes.
        tokio::fs::write(
            project_dir.join("src/main.rs"),
            r#"fn main() {
    std::thread::sleep(std::time::Duration::from_secs(300));
}
"#,
        )
        .await
        .expect("write main.rs");

        let manager = RunManager::new();
        let mut rx = manager.subscribe("sleepy");

        manager
            .start_run("sleepy", project_dir)
            .await
            .expect("start_run succeeds");

        // Give the process a moment to start.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(manager.status("sleepy").running, "should be running");

        // Stop it.
        manager.stop_run("sleepy").await.expect("stop_run succeeds");

        // Collect events until we see Stop and Exit.
        let mut saw_stop = false;
        loop {
            let event = timeout(Duration::from_secs(10), rx.recv())
                .await
                .expect("should receive event")
                .expect("channel should not close");

            match event {
                RunEvent::Stop { .. } => saw_stop = true,
                RunEvent::Exit { .. } => break,
                _ => {}
            }
        }

        assert!(saw_stop, "should have received Stop event");

        assert!(!manager.status("sleepy").running, "should be stopped");

        // Stopping again should fail.
        let err = manager
            .stop_run("sleepy")
            .await
            .expect_err("second stop should fail");
        assert!(
            matches!(err, RunError::NotRunning(ref s) if s == "sleepy"),
            "expected NotRunning, got {err:?}"
        );
    }
}
