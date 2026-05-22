# projects/

User-projects live here. Each subdirectory is one project. **The studio backend writes into this directory; humans and Claude CLI can read and edit it freely (subject to the `@generated` region rule).**

This README describes the layout the studio **will** create starting in Section 2 (project CRUD) and Section 4 (codegen). At the moment Section 1 only reserves the directory.

---

## Per-project layout (target)

```
projects/<slug>/
├── graph.json        Source of truth. The flow graph the user designed in the UI.
├── Cargo.toml        Generated. Crate name = <slug>. Deps depend on which adapter the user picked (axum / actix) and which long-runners are wired.
├── CLAUDE.md         Generated. Tells Claude CLI which files in this project are regen-managed vs. user-owned.
├── README.md         Generated. Project overview + how to run / build.
├── .gitignore        Generated. Ignores target/.
├── src/
│   ├── main.rs       Generated. Boots Tokio, binds HTTP (if any routes), spawns long-runners.
│   ├── lib.rs        Generated. Exposes the router builder + task launchers.
│   ├── dto/          Generated. One module per DTO node in the graph.
│   ├── handlers/     Generated. One module per route handler.
│   ├── services/     Generated. One module per service node. User-editable inside @generated regions.
│   ├── consumers/    Generated. Kafka / pub-sub consumers (each spawned via tokio::spawn from main).
│   ├── schedulers/   Generated. Cron-driven jobs (each spawned via tokio::spawn from main).
│   └── errors.rs     Generated. Project-local thiserror enum + HTTP IntoResponse impl.
└── tests/            Generated stubs the user can fill in.
```

---

## Conventions inside a user-project

- **`@generated:begin <id>` / `@generated:end <id>`** comment bracket regen-managed regions. Code outside those brackets is preserved across regenerations.
- **No `unwrap` / `expect` / `panic!`** on any reachable path. Same rule as the studio itself — see the root `CLAUDE.md`.
- **No `Mutex` on hot paths**, no blocking calls in `async fn`. Same rule as the studio.
- **Doc comments are mandatory** on every generated function, struct, and module. Generated code is documentation-grade.

---

## How Claude CLI works on a user-project

```bash
cd projects/<slug>
claude            # starts Claude CLI in the project's working dir
```

Claude reads `CLAUDE.md` at the project root for the per-project contract, sees `graph.json` as the source of truth, and edits source under `src/` honouring the `@generated` regions. The studio's next regen will preserve any edits Claude made outside those regions.

---

## Current state

Section 1: directory reserved. Nothing generated yet. Wait for Section 2 (`POST /api/projects` lands the first concrete folder).
