# CLAUDE.md — project contract for `rust_no_code`

This file is the contract you (Claude) must respect when working in this repository. Read it at every P1.

---

## What this project is

`rust_no_code` is a visual studio that lets users design Rust applications as flow graphs and generates clean async Rust code from those graphs. It targets the full range of enterprise applications: web services, system daemons, pub/sub brokers, log streamers, embedded databases, scheduled jobs, message consumers.

The roadmap and high-level architecture live in `README.md`. Treat this CLAUDE.md as load-bearing: rules here override defaults.

---

## The two-codebase rule (load-bearing)

This repository contains **two distinct codebases** with **different conventions**:

### 1. Studio code — `backend/`, `frontend/`, `templates/`

This is the studio platform itself — the server that manages projects, the UI that draws graphs, the templates that generate user code. Conventions:

- **Rust (backend):** `async fn` everywhere, Tokio runtime, Axum for HTTP, `tracing` for logs, `thiserror` for typed errors at module boundaries, `anyhow` only in `main` and binary glue.
- **No `unwrap` / `expect` / `panic!` on any path reachable from a running server.** `unwrap` inside `#[test]` and inside `build.rs` is fine.
- **No `Mutex` on a request hot path.** Prefer `Arc<RwLock<...>>` for read-mostly state, sharded maps (`dashmap`) when contention matters, or message-passing via `tokio::sync::mpsc` when state ownership can move.
- **No blocking calls inside `async fn`.** Use `tokio::fs`, `tokio::process`, `reqwest`, etc. For genuinely CPU-bound work, use `tokio::task::spawn_blocking`.
- **TypeScript (frontend):** strict mode on. No `any` without an inline justification comment. Prefer named exports. ReactFlow nodes are functional components; state lives in zustand stores (introduced when first needed, S2+).
- **Errors at the HTTP boundary** are mapped to a single `ApiError` enum that implements `IntoResponse`. Never leak `anyhow::Error` to the client.

### 2. Generated user-project code — `projects/<slug>/`

This is code the studio generates for end users. It is real Rust that compiles, runs, and is meant to be read and edited (by humans and by Claude CLI). Conventions:

- **Same no-`unwrap` / no-`Mutex`-on-hot-path / no-blocking-in-async rules apply** — generated code must model the same discipline the studio enforces on itself.
- **Generated regions are bracketed** by `// @generated:begin <id>` / `// @generated:end <id>` comments. Code outside those regions is user-owned and **must be preserved** across regenerations (this is enforced from Section 4 onward).
- **Each user-project ships with its own `CLAUDE.md`** (introduced in Section 10) telling Claude CLI which files are safe to edit freely and which are regen-managed.
- **Doc comments are mandatory** on every generated function, struct, and module — generated code is documentation-grade because both humans and LLMs will read it.

When you work in `backend/` or `frontend/`, follow rule set 1. When you work in `projects/<slug>/`, follow rule set 2. Never apply one to the other.

---

## Architecture invariants (apply to the studio)

- **Axum is the studio's HTTP server.** Generated user-projects may pick Axum or Actix per project; the studio itself does not.
- **Persistence is the filesystem.** Projects are folders. `graph.json` is the source of truth for each project. No database in the studio (this is a deliberate v1 decision — revisit only when collaboration features land).
- **Codegen is deterministic and idempotent.** Same graph → byte-identical output. Regen never loses user edits outside `@generated` regions.
- **Long-running user-project tasks** (Kafka consumers, schedulers, custom Tokio tasks) are spawned with `tokio::spawn` from the user-project's `main` after the HTTP server (if any) is bound. Each spawn site is wrapped in a supervisor that logs panics via `tracing::error!` and restarts with exponential backoff (introduced in Section 5).

---

## Build & test commands

```bash
# Studio backend
cd backend && cargo check
cd backend && cargo test
cd backend && cargo run

# Studio frontend
cd frontend && npm install
cd frontend && npm run dev
cd frontend && npm run build

# A specific user-project (once they exist, S2+)
cd projects/<slug> && cargo check
cd projects/<slug> && cargo build --release
```

The studio also drives `cargo check` / `cargo build --release` for user-projects programmatically via `tokio::process` (Section 6).

---

## Default ports

- Backend HTTP: `127.0.0.1:7878` (override with `RUST_NO_CODE_BACKEND_ADDR`)
- Frontend dev server: `127.0.0.1:5173` (Vite default)
- Frontend talks to backend via `VITE_API_BASE`, defaulting to `http://127.0.0.1:7878`.

---

## File conventions

- `README.md` at the project root is human-oriented.
- `CLAUDE.md` (this file) is the agent contract — keep it tight, factual, and grounded in code that exists.
- `current-tasks.md` is the in-flight task ledger maintained by the pro-coder skill. Read it at every P1.
- `.claude/state/` holds skill state (section snapshots, code-map, proposals). Gitignored.
- `.history/` is a write-only audit trail of changed files per task. Never read from it unless the user explicitly asks.

---

## What NOT to do

- Do not introduce a database into the studio for v1.
- Do not generate user-project code that uses `unwrap`/`expect`/`panic!` on any reachable path.
- Do not edit code outside `@generated` regions in `projects/<slug>/` during a regen.
- Do not pick Axum vs Actix on behalf of the user — the user picks per project (Section 4).
- Do not write CLAUDE.md edits directly. Propose via `.claude/state/claude_md_proposals.md` and let the user merge.

---

## Open items (will firm up in upcoming sections)

- Graph schema (Section 2)
- Node template plugin contract (Section 3)
- WebSocket protocol for build streaming (Section 6)
- Per-user-project `CLAUDE.md` template (Section 10)
