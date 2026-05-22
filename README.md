# rust_no_code — visual studio for building high-performance Rust backends

`rust_no_code` is an enterprise-grade visual IDE for building **any** backend service in Rust without writing code by hand — API servers, web services, IoT ingestors, microservices, event consumers, job schedulers, pub/sub brokers, log pipelines, and real-time data processors. You design the system as a flow graph in the browser; the studio generates clean, idiomatic, async Rust source code on Tokio that you can ship, audit, and extend.

**Code is the fallback, not the default.** Every common pattern is a draggable node. You reach for Rust only when you need something truly custom — and even then, you write it inside the studio's code editor, expose configurable parameters to the UI, and reuse it like any built-in node.

Think TIBCO StreamBase meets AWS Lambda Designer meets Rust — high performance, memory-safe, async-first, and designed from day one to cooperate with LLM coding agents.

---

## What it does

1. **You design the system as a flow graph in the browser.** Drag nodes for HTTP routes, Kafka consumers, SQL queries, Redis caches, MongoDB reads, scheduled jobs, pub/sub topics, and more. Wire them together with typed edges. No boilerplate.
2. **The studio generates production-grade Rust code** — `Cargo.toml`, `src/main.rs`, real structs, real async handlers, real Tokio-spawned consumers, real connection pools. The output is a normal Rust crate you can `cargo build --release` and deploy.
3. **Hot validation as you type.** The build orchestrator runs `cargo check` in the background. Errors surface directly on the offending node in the canvas — red badges, underlined ports, and a diagnostics panel. Fix the config, watch the error disappear.
4. **Debug the flow, not the binary.** Toggle "Debug Mode" in the UI to step through the graph execution: see events enter a Kafka consumer, transform in a Map node, get filtered, land in a SQL insert. Inspect payloads per edge. Set breakpoints on nodes.
5. **Write tests visually.** Add "Test Case" nodes that feed mock input into any subgraph and assert on output. Run the full suite from the UI. Green checkmarks on passing nodes, red crosses on failures with diffs.
6. **Custom code when you need it.** Drop a "Custom Node", write Rust in the built-in editor, annotate constants with `#[expose]` to make them UI-configurable. It compiles and runs alongside built-in nodes.
7. **Claude CLI is a first-class citizen.** Every project is a normal Rust workspace. `cd` into it, use Claude CLI to edit generated code, and the studio respects your changes on the next regen.

---

## Repository layout

```
rust_no_code/
├── backend/        Studio server — Axum, Tokio, manages projects, generates code, drives builds.
├── frontend/       Studio UI — Vite + React + TypeScript + ReactFlow.
├── projects/       User-projects live here. One folder per project. Generated, but human/Claude-editable.
├── templates/      Codegen templates for node types (populated from Section 3 onward).
├── CLAUDE.md       Project contract for Claude when working on the studio itself.
├── README.md       This file.
├── current-tasks.md  In-flight task ledger (skill-managed).
└── .claude/        Skill state (gitignored).
```

Crucially: **the studio crate (`backend/`) and the generated user-project crates (`projects/<slug>/`) follow different conventions.** See `CLAUDE.md` for the rules.

---

## Status

**Foundation complete (Sections 1–9).** The studio boots, persists projects, renders a ReactFlow canvas, and generates real Rust code:
- `cargo check` / `cargo build` via WebSocket streaming (S6)
- `thiserror` error-handling with `Result<T, AppError>` everywhere (S7)
- Real parser codegen: JSON Schema → `typify`, Protobuf → `prost-build` (S9)
- 137 backend tests pass (111 lib + 26 integration); frontend builds clean.

**What you can build today:** API servers with routes, handlers, DTOs, and basic services. Not yet: real database connectors, Kafka consumers, visual debugging, or test authoring.

**What we are building toward:** A complete visual backend platform where you never write a `main.rs` by hand unless you want to.

---

## Quick start

```bash
# Backend
cd backend
cargo run                       # boots Axum on 127.0.0.1:7878; serves /health + /api/projects/*
curl http://127.0.0.1:7878/health

# Frontend (in a second terminal)
cd frontend
npm install
npm run dev                     # Vite dev server on 127.0.0.1:5173
```

Open `http://127.0.0.1:5173` — the backend badge turns green, the project list appears, and you can create your first project. The created project becomes a folder under `projects/<slug>/` with `project.json` (metadata) and `graph.json` (the empty flow graph).

Override the projects root with `RUST_NO_CODE_PROJECTS_ROOT=/path/to/dir` if you want projects to land somewhere other than `./projects`.

---

## Roadmap

| # | Section | Status |
|---|---|---|
| 1 | Workspace scaffold | ✅ done |
| 2 | Project + Graph model + REST CRUD | ✅ done |
| 3 | Node template registry (plugin contract) | ✅ done |
| 4 | Code generator core (`syn` / `quote` / `prettyplease`) | ✅ done |
| 5 | Long-runner nodes (Kafka, schedulers, generic Tokio tasks) | ✅ done |
| 6 | Build + check orchestration with WebSocket streaming | ✅ done |
| 7 | Logger nodes + `thiserror` error-handling pass | ✅ done |
| 8 | ReactFlow canvas + per-node config drawer | ✅ done |
| 9 | Serialization / parser node pack (JSON, XML, Protobuf, FlatBuffers, Cap'n Proto, bit-packer) | ✅ done (JSON + Protobuf real; XML stub; FlatBuffers/Cap'n Proto/bit-packer deferred) |
| 10 | System-engineering node pack (pub/sub, log-stream, embedded DB) | ✅ done |
| 11 | Claude CLI chat surface (per-project subprocess) | queued |
| 12 | Run lifecycle + per-project `CLAUDE.md` template | queued |
| 13 | Visual step debugger / flow player | queued |
| 14 | Polish + end-to-end demo | queued |
| 15 | Visual Rust programming — language constructs + Tokio runtime nodes | queued |
| 16 | Custom Node SDK — write Rust code, expose constants/variables to UI | queued |
| 17 | Framework deep customization — middleware, layers, hooks for all pre-installed modules | queued |
| 18 | Universal Connector Pack — Kafka, Redis, SQL, MongoDB, ScyllaDB, ClickHouse, and more | queued |
| 19 | Visual Test Runner — write test cases in the UI, mock inputs, assert outputs, run suite | queued |
| 20 | Diagnostics Panel — inline errors, warnings, and suggestions on every node | queued |
| 21 | Performance Profiler — flamegraph per node, memory usage, throughput metrics | queued |
| 22 | Security Audit — dependency vulnerability scan, secret leakage detection, OWASP checks | queued |

---

## License

TBD.
