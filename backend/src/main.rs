//! Binary entry point for the `rust_no_code` studio server.
//!
//! Responsibilities, in order:
//! 1. Initialise `tracing` so every subsystem can emit structured logs.
//! 2. Resolve the bind address from `RUST_NO_CODE_BACKEND_ADDR`, falling back
//!    to `127.0.0.1:7878`. A malformed env var degrades to the default with a
//!    warning rather than aborting — the studio must remain bootable even if
//!    the operator typo'd a config knob.
//! 3. Resolve the projects root from `RUST_NO_CODE_PROJECTS_ROOT`, default
//!    `./projects` (relative to the working directory the binary was started in).
//! 4. Construct the `ProjectStore` and serve the router.
//!
//! `expect` is used only on conditions that are unrecoverable at startup
//! (cannot bind the listener; cannot run the server; cannot create the
//! projects root). This is consistent with the CLAUDE.md rule: `expect` is
//! permitted in binary glue, never on request paths.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};

use rust_no_code_studio::{projects::ProjectStore, templates::TemplateRegistry, AppState};

const DEFAULT_ADDR: &str = "127.0.0.1:7878";
const ADDR_ENV: &str = "RUST_NO_CODE_BACKEND_ADDR";
const PROJECTS_ROOT_ENV: &str = "RUST_NO_CODE_PROJECTS_ROOT";
const DEFAULT_PROJECTS_ROOT: &str = "./projects";

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,tower_http=info")),
        )
        .init();

    let addr = resolve_addr();
    let projects_root = resolve_projects_root();
    let store = ProjectStore::new(&projects_root)
        .await
        .expect("failed to initialise project store");
    info!(root = %projects_root.display(), "project store initialised");

    // Templates registry — compiled-in built-ins at v1 (S3 locked
    // filesystem-loadable templates as a future extension). Built once
    // here and shared via `AppState` across every handler.
    let registry = Arc::new(TemplateRegistry::with_builtins());
    info!(builtins = registry.len(), "template registry initialised");

    let app = rust_no_code_studio::router(AppState::new(store, registry));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("failed to bind studio backend listener");
    info!(%addr, "rust_no_code studio backend listening");

    axum::serve(listener, app)
        .await
        .expect("studio backend server crashed");
}

/// Resolve the bind address from the env var, falling back to the default on
/// any failure mode (unset, empty, unparseable). Logs the chosen address so
/// the operator can confirm what the server actually picked.
fn resolve_addr() -> SocketAddr {
    match std::env::var(ADDR_ENV) {
        Ok(raw) if !raw.is_empty() => match raw.parse::<SocketAddr>() {
            Ok(parsed) => parsed,
            Err(err) => {
                warn!(
                    %err,
                    raw = %raw,
                    default = DEFAULT_ADDR,
                    "{ADDR_ENV} could not be parsed as a SocketAddr; falling back to default"
                );
                default_addr()
            }
        },
        _ => default_addr(),
    }
}

/// Single owner of the default `SocketAddr` literal — collapses what used to
/// be a duplicated `DEFAULT_ADDR.parse().expect(...)` pair in `resolve_addr`
/// into one helper. The constant is statically valid, so `expect` here is
/// equivalent to `unreachable!()` (FOLLOWUP-S1-A close).
fn default_addr() -> SocketAddr {
    DEFAULT_ADDR
        .parse()
        .expect("DEFAULT_ADDR constant must be a valid SocketAddr")
}

/// Resolve the projects root path from the env var, defaulting to
/// `./projects` relative to the CWD the binary was launched from. Unlike the
/// bind address there's no parse failure mode — a relative or absolute
/// path is just passed through; `ProjectStore::new` will create it if
/// missing.
fn resolve_projects_root() -> PathBuf {
    match std::env::var(PROJECTS_ROOT_ENV) {
        Ok(raw) if !raw.is_empty() => PathBuf::from(raw),
        _ => PathBuf::from(DEFAULT_PROJECTS_ROOT),
    }
}
