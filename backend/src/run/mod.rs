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

/// Performance profiling statistics per visual node (S21).
#[derive(Debug, Clone, Serialize)]
pub struct PerformanceStats {
    pub throughput: usize,
    pub avg_latency_us: u64,
    pub p99_latency_us: u64,
}

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
    /// Step-debugger event (S13).
    DebugState {
        node_id: String,
        state: String,
        value: String,
    },
    /// Performance profiling metrics update (S21).
    Metrics {
        metrics: std::collections::HashMap<String, PerformanceStats>,
    },
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
    /// `env` is appended to the child's environment unchanged. S16 uses this
    /// to route `/debug` through the same code path as `/run` while injecting
    /// `RUST_LOG=debug` + `RUST_BACKTRACE=full`; production `/run` callers
    /// pass an empty slice and get the unmodified environment.
    ///
    /// Returns immediately once the process is spawned; the actual run runs
    /// in background tasks.
    pub async fn start_run(
        &self,
        slug: &str,
        project_dir: PathBuf,
        env: &[(&str, &str)],
    ) -> Result<(), RunError> {
        if self.processes.contains_key(slug) {
            return Err(RunError::AlreadyRunning(slug.to_string()));
        }

        let tx = self.get_or_create_channel(slug);
        let mut cmd = Command::new("cargo");
        cmd.current_dir(&project_dir)
            .arg("run")
            .arg("--message-format=short");
        for (k, v) in env {
            cmd.env(k, v);
        }
        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(std::process::Stdio::piped());

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

        // S21: Performance profiling aggregator state
        let metrics_data = std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::<String, Vec<u128>>::new()));
        let metrics_data_clone = std::sync::Arc::clone(&metrics_data);
        let tx_metrics = tx.clone();
        let slug_str = slug.to_string();
        let processes_map = std::sync::Arc::clone(&self.processes);

        // Spawn background aggregator daemon ticking every 1000ms
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(1000));
            loop {
                interval.tick().await;
                if !processes_map.contains_key(&slug_str) {
                    break;
                }

                let mut raw_data = metrics_data_clone.lock().await;
                if raw_data.is_empty() {
                    continue;
                }

                let mut metrics = std::collections::HashMap::new();
                for (node_id, latencies) in raw_data.drain() {
                    let count = latencies.len();
                    if count == 0 {
                        continue;
                    }
                    let sum: u128 = latencies.iter().sum();
                    let avg_latency_us = (sum / count as u128) as u64;

                    // Calculate P99 percentile latency
                    let mut sorted = latencies.clone();
                    sorted.sort_unstable();
                    let p99_idx = (count * 99 / 100).min(count - 1);
                    let p99_latency_us = sorted[p99_idx] as u64;

                    metrics.insert(node_id, PerformanceStats {
                        throughput: count,
                        avg_latency_us,
                        p99_latency_us,
                    });
                }

                let _ = tx_metrics.send(RunEvent::Metrics { metrics });
            }
        });

        let metrics_data_clone_for_stdout = std::sync::Arc::clone(&metrics_data);
        let stdout_task = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(payload_str) = line.strip_prefix("__studio_debug__:") {
                    #[derive(serde::Deserialize)]
                    struct DebugPayload {
                        node_id: String,
                        state: String,
                        value: String,
                    }
                    if let Ok(payload) = serde_json::from_str::<DebugPayload>(payload_str) {
                        let _ = tx_stdout.send(RunEvent::DebugState {
                            node_id: payload.node_id,
                            state: payload.state,
                            value: payload.value,
                        });
                        continue;
                    }
                }
                if let Some(payload_str) = line.strip_prefix("__studio_profile__:") {
                    #[derive(serde::Deserialize)]
                    struct ProfilePayload {
                        node_id: String,
                        latency_us: u128,
                    }
                    if let Ok(payload) = serde_json::from_str::<ProfilePayload>(payload_str) {
                        let mut data = metrics_data_clone_for_stdout.lock().await;
                        data.entry(payload.node_id).or_default().push(payload.latency_us);
                        continue;
                    }
                }
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

    /// Write input data directly to the stdin pipe of the running process for step/resume commands.
    pub async fn send_stdin(&self, slug: &str, data: &str) -> Result<(), RunError> {
        if let Some(mut entry) = self.processes.get_mut(slug) {
            let child = entry.value_mut();
            if let Some(ref mut stdin) = child.stdin {
                use tokio::io::AsyncWriteExt;
                stdin.write_all(data.as_bytes()).await?;
                stdin.flush().await?;
                Ok(())
            } else {
                Err(RunError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "stdin not piped",
                )))
            }
        } else {
            Err(RunError::NotRunning(slug.to_string()))
        }
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
            .start_run("test", project_dir.clone(), &[])
            .await
            .expect("start_run succeeds");

        // A second start while the first is running must fail.
        let err = manager
            .start_run("test", project_dir, &[])
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
            .start_run("sleepy", project_dir, &[])
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

    #[tokio::test]
    async fn test_start_run_propagates_env_vars_to_child() {
        // The user-project echoes a custom env var. If `start_run` passes the
        // env slice through correctly, the value reaches the child and is
        // visible on stdout.
        let dir = tempfile::tempdir().expect("tempdir");
        let project_dir = dir.path().join("envproj");
        tokio::fs::create_dir_all(project_dir.join("src"))
            .await
            .expect("create src dir");
        tokio::fs::write(
            project_dir.join("Cargo.toml"),
            r#"[package]
name = "envproj"
version = "0.1.0"
edition = "2021"
"#,
        )
        .await
        .expect("write Cargo.toml");
        tokio::fs::write(
            project_dir.join("src/main.rs"),
            r#"fn main() {
    println!("STUDIO_PROBE={}", std::env::var("STUDIO_PROBE").unwrap_or_default());
}
"#,
        )
        .await
        .expect("write main.rs");

        let manager = RunManager::new();
        let mut rx = manager.subscribe("envtest");
        manager
            .start_run("envtest", project_dir, &[("STUDIO_PROBE", "ok-42")])
            .await
            .expect("start_run with env succeeds");

        let mut saw_echo = false;
        loop {
            let event = timeout(Duration::from_secs(60), rx.recv())
                .await
                .expect("run should finish within 60s")
                .expect("channel should not close");
            match event {
                RunEvent::Stdout { ref line } if line.contains("STUDIO_PROBE=ok-42") => {
                    saw_echo = true;
                }
                RunEvent::Exit { .. } => break,
                _ => {}
            }
        }
        assert!(saw_echo, "child must see the STUDIO_PROBE env var");
    }

    #[tokio::test]
    async fn test_metrics_calculation_averages_and_p99() {
        // Create sample latencies vector
        let latencies = vec![10, 20, 30, 40, 50, 60, 70, 80, 90, 100]; // 10 items
        let count = latencies.len();
        let sum: u128 = latencies.iter().sum();
        let avg_latency_us = (sum / count as u128) as u64;
        
        let mut sorted = latencies.clone();
        sorted.sort_unstable();
        let p99_idx = (count * 99 / 100).min(count - 1);
        let p99_latency_us = sorted[p99_idx] as u64;
        
        assert_eq!(avg_latency_us, 55);
        assert_eq!(p99_latency_us, 100);
        
        // Assert for a larger dataset
        let latencies_100: Vec<u128> = (1..=100).collect(); // 1 to 100
        let count_100 = latencies_100.len();
        let sum_100: u128 = latencies_100.iter().sum();
        let avg_100 = (sum_100 / count_100 as u128) as u64;
        
        let mut sorted_100 = latencies_100.clone();
        sorted_100.sort_unstable();
        let p99_idx_100 = (count_100 * 99 / 100).min(count_100 - 1);
        let p99_latency_100 = sorted_100[p99_idx_100] as u64;
        
        assert_eq!(avg_100, 50); // 5050 / 100 = 50 (truncated)
        assert_eq!(p99_latency_100, 100);
    }
}
